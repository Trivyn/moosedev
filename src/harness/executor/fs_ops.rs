//! Descriptor-relative traversal, bounded enumeration, and safe scratch removal.
use anyhow::{Context, Result};
use std::{
    ffi::CString,
    fs::{self, File},
    time::Duration,
};

#[cfg(unix)]
pub(super) fn cvt(value: libc::c_int) -> std::io::Result<libc::c_int> {
    if value < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

#[cfg(unix)]
pub(super) fn open_at(
    parent: &File,
    name: &CString,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = cvt(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    })?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// fdopendir owns its descriptor, so duplicate instead of consuming the caller's
/// locked/open directory. Names, recursion and unlink remain descriptor-relative.
#[cfg(unix)]
pub(super) fn child_names(directory: &File) -> Result<Vec<CString>> {
    child_names_bounded(directory, &mut WalkBudget::new(Duration::from_secs(5)))
}

#[cfg(unix)]
pub(super) struct WalkBudget {
    pub(super) deadline: std::time::Instant,
    pub(super) remaining: usize,
}

#[cfg(unix)]
impl WalkBudget {
    pub(super) fn new(duration: Duration) -> Self {
        Self {
            deadline: std::time::Instant::now() + duration,
            remaining: 1_000_000,
        }
    }

    pub(super) fn check(&self) -> Result<()> {
        anyhow::ensure!(
            std::time::Instant::now() < self.deadline,
            "scratch traversal reached its time limit; retry cleanup or rebuild the cache"
        );
        Ok(())
    }
}

#[cfg(unix)]
pub(super) fn child_names_bounded(
    directory: &File,
    budget: &mut WalkBudget,
) -> Result<Vec<CString>> {
    child_names_chunk(directory, budget, usize::MAX)
}

#[cfg(unix)]
fn child_names_chunk(
    directory: &File,
    budget: &mut WalkBudget,
    limit: usize,
) -> Result<Vec<CString>> {
    use std::os::fd::AsRawFd;
    // A fresh open file description avoids sharing the enumeration offset.
    let opened = open_at(
        directory,
        &CString::new(".")?,
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )?;
    let fd = cvt(unsafe { libc::dup(opened.as_raw_fd()) })?;
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(std::io::Error::last_os_error().into());
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        budget.check()?;
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            anyhow::ensure!(
                budget.remaining > 0,
                "scratch traversal reached its entry limit; retry cleanup or rebuild the cache"
            );
            budget.remaining -= 1;
            names.push(name.to_owned());
            if names.len() == limit {
                break;
            }
        }
    }
    Ok(names)
}

#[cfg(unix)]
pub(super) fn remove_child(directory: &File, name: &str) -> Result<()> {
    remove_child_name(directory, &CString::new(name)?)
}

#[cfg(target_os = "macos")]
fn clear_owned_flags(entry: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstat(entry.as_raw_fd(), &mut stat) })?;
    let restrictive = libc::UF_IMMUTABLE | libc::UF_APPEND;
    if stat.st_flags & restrictive == 0 {
        return Ok(());
    }
    let kind = stat.st_mode & libc::S_IFMT;
    if stat.st_uid != unsafe { libc::geteuid() }
        || !(kind == libc::S_IFDIR || (kind == libc::S_IFREG && stat.st_nlink == 1))
    {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "restrictive scratch flags belong to an aliased or foreign inode; clear flags on the original owned entry manually and retry cleanup"));
    }
    cvt(unsafe { libc::fchflags(entry.as_raw_fd(), stat.st_flags & !restrictive) })?;
    Ok(())
}

/// Command-owned directories can have arbitrary modes. Repair only directories
/// owned by this user, never a followed symlink or a multiply-linked file. The
/// pathname chmod is explicitly nofollow; subsequent repair is descriptor-bound.
#[cfg(unix)]
pub(super) fn open_owned_directory(parent: &File, name: &CString) -> std::io::Result<File> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    };
    let opened = open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0);
    let directory = match opened {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            cvt(unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            })?;
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                || stat.st_uid != unsafe { libc::geteuid() }
            {
                return Err(error);
            }
            #[cfg(target_os = "macos")]
            if stat.st_flags & (libc::UF_IMMUTABLE | libc::UF_APPEND) != 0 {
                let directory = open_at(parent, name, libc::O_EVTONLY, 0)?;
                clear_owned_flags(&directory)?;
            }
            cvt(unsafe {
                libc::fchmodat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    (stat.st_mode & 0o777) | 0o700,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            })?;
            open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
        }
        result => result?,
    };
    #[cfg(target_os = "macos")]
    clear_owned_flags(&directory)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "task scratch directory belongs to another user",
        ));
    }
    if metadata.mode() & 0o700 != 0o700 {
        directory.set_permissions(fs::Permissions::from_mode(
            (metadata.mode() & 0o777) | 0o700,
        ))?;
    }
    Ok(directory)
}

#[cfg(unix)]
pub(super) fn remove_child_name(directory: &File, name: &CString) -> Result<()> {
    remove_child_before(
        directory,
        name,
        std::time::Instant::now() + Duration::from_secs(5),
    )
}

#[cfg(unix)]
pub(super) fn remove_child_before(
    directory: &File,
    name: &CString,
    deadline: std::time::Instant,
) -> Result<()> {
    remove_child_bounded(
        directory,
        name,
        &mut WalkBudget {
            deadline,
            remaining: 1_000_000,
        },
    )
}

#[cfg(unix)]
pub(super) fn remove_child_bounded(
    directory: &File,
    name: &CString,
    budget: &mut WalkBudget,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    // Keep names on the heap and reopen ancestors from the trusted descriptor.
    // This uses constant descriptors and stack space even for a hostile deeply
    // nested tree, and never traverses a symlink or a command-supplied '..'.
    let mut work = 0usize;
    let mut current = directory.try_clone()?;
    let mut path = Vec::<CString>::new();
    let mut pending = vec![vec![name.clone()]];
    while let Some(children) = pending.last_mut() {
        work += 1;
        budget.check()?;
        anyhow::ensure!(work <= 1_000_000, "scratch cleanup reached its work limit; retry cleanup to continue removing the remaining owned entries");
        if let Some(name) = children.pop() {
            match open_owned_directory(&current, &name) {
                Ok(child) => {
                    // Delete a bounded batch before retrying a large directory.
                    // Collecting the entire directory before any unlink could
                    // make every cleanup retry hit the same enumeration limit.
                    pending.push(child_names_chunk(&child, budget, 4096)?);
                    path.push(name);
                    current = child;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                    #[cfg(target_os = "macos")]
                    {
                        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                        let result = unsafe { libc::fstatat(current.as_raw_fd(), name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
                        if result == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFREG {
                            let entry = open_at(&current, &name, libc::O_EVTONLY, 0)?;
                            clear_owned_flags(&entry)?;
                        }
                    }
                    cvt(unsafe { libc::unlinkat(current.as_raw_fd(), name.as_ptr(), 0) })?;
                }
                Err(error) => return Err(error).context("scratch entry could not be removed; stop remaining command processes and clear restrictive flags on the owned scratch entry, then retry cleanup"),
            }
        } else {
            pending.pop();
            if let Some(name) = path.pop() {
                current = directory.try_clone()?;
                for component in &path {
                    budget.check()?;
                    current = open_owned_directory(&current, component)?;
                }
                cvt(unsafe {
                    libc::unlinkat(current.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR)
                })?;
            }
        }
    }
    Ok(())
}
