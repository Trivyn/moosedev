//! Task cache generations and disposable command directory lifecycles.
#[cfg(unix)]
use super::fs_ops::*;
use anyhow::{Context, Result};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
pub(super) struct TaskScratch {
    pub(super) path: PathBuf,
    pub(super) directory: File,
    pub(super) cleanup_notice: Option<String>,
    #[cfg(target_os = "macos")]
    cache_valid: bool,
}

#[cfg(unix)]
impl TaskScratch {
    pub(super) fn open(path: &Path) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let (parent_path, parent, name_c) = scratch_parent(path, true)?;
        let name = path.file_name().context("task scratch requires a name")?;
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let directory = open_owned_directory(&parent, &name_c)
            .context("task scratch must be a real directory, not an alias")?;
        fs2::FileExt::try_lock_exclusive(&directory)
            .context("task command scratch is already in use")?;
        let path = parent_path.join(name);
        let backing = build_backing(&directory)?;
        let mut cleanup_notice = None;
        let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(5);
        // Remove abandoned command/snapshot children, including legacy UUID
        // scratch trees. Only the stable build cache is carried across commands.
        for child in child_names(&directory)? {
            if child.as_bytes() != b"build" && child.as_bytes() != b"source" && child != backing {
                if let Err(error) = remove_child_before(&directory, &child, cleanup_deadline) {
                    cleanup_notice =
                        Some(format!("old task scratch remains for cleanup: {error:#}"));
                }
            }
        }
        let build = CString::new("build")?;
        let result = unsafe { libc::mkdirat(directory.as_raw_fd(), build.as_ptr(), 0o700) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let build = open_owned_directory(&directory, &backing)
            .context("task build cache must be a real directory or a validated harness alias")?;
        let inspected = sanitize_build(&build);
        #[cfg(target_os = "macos")]
        let cache_valid = match inspected {
            Ok(()) => true,
            Err(error) => {
                cleanup_notice = Some(format!("previous cache could not be safely inspected; starting a fresh cache: {error:#}"));
                false
            }
        };
        #[cfg(not(target_os = "macos"))]
        inspected?;
        Ok(Self {
            path,
            directory,
            cleanup_notice,
            #[cfg(target_os = "macos")]
            cache_valid,
        })
    }
}

#[cfg(unix)]
pub(super) fn build_backing(directory: &File) -> Result<CString> {
    use std::os::fd::AsRawFd;
    let build = CString::new("build")?;
    let mut buffer = [0u8; 128];
    let count = unsafe {
        libc::readlinkat(
            directory.as_raw_fd(),
            build.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if count < 0 {
        let error = std::io::Error::last_os_error();
        anyhow::ensure!(
            matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)),
            "inspect task build cache: {error}"
        );
        return Ok(build);
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("task build cache cannot be a symlink");
    #[cfg(target_os = "macos")]
    {
        let target = std::str::from_utf8(&buffer[..count as usize])?;
        let id = target
            .strip_prefix(".build-")
            .context("task build alias is not harness-owned")?;
        uuid::Uuid::parse_str(id).context("invalid task build backing identifier")?;
        Ok(CString::new(target)?)
    }
}

#[cfg(target_os = "macos")]
impl TaskScratch {
    pub(super) fn rotate_build(&self) -> Result<()> {
        use std::os::fd::AsRawFd;
        let mut previous = build_backing(&self.directory)?;
        let source = open_owned_directory(&self.directory, &previous)?;
        let mut name = CString::new(format!(".build-{}", uuid::Uuid::new_v4()))?;
        cvt(unsafe { libc::mkdirat(self.directory.as_raw_fd(), name.as_ptr(), 0o700) })?;
        let destination = open_owned_directory(&self.directory, &name)?;
        if self.cache_valid && clone_build(&source, &destination).is_err() {
            // Publish an empty generation even if disposing a partial copy
            // takes another cleanup pass. Cleanup must not prevent a cache miss.
            let _ = remove_child_name(&self.directory, &name);
            name = CString::new(format!(".build-{}", uuid::Uuid::new_v4()))?;
            cvt(unsafe { libc::mkdirat(self.directory.as_raw_fd(), name.as_ptr(), 0o700) })?;
        }
        let alias = CString::new(format!(".build-alias-{}", uuid::Uuid::new_v4()))?;
        cvt(unsafe { libc::symlinkat(name.as_ptr(), self.directory.as_raw_fd(), alias.as_ptr()) })?;
        let build = CString::new("build")?;
        // Move a legacy real directory aside before publishing the stable alias.
        // Deletion can be retried later without blocking the next command.
        if previous == build {
            previous = CString::new(format!(".build-{}", uuid::Uuid::new_v4()))?;
            cvt(unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    build.as_ptr(),
                    self.directory.as_raw_fd(),
                    previous.as_ptr(),
                )
            })?;
        }
        cvt(unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                alias.as_ptr(),
                self.directory.as_raw_fd(),
                build.as_ptr(),
            )
        })?;
        self.directory.sync_all()?;
        let _ = remove_child_name(&self.directory, &previous);
        Ok(())
    }
}

/// Copy every artifact to a distinct inode. APFS fclonefileat shares disk blocks
/// copy-on-write; filesystems without cloning use a bounded ordinary copy.
#[cfg(target_os = "macos")]
fn clone_build(source: &File, destination: &File) -> Result<()> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    };
    fn copy(
        source: &File,
        destination: &File,
        depth: usize,
        fallback_bytes: &mut u64,
        budget: &mut WalkBudget,
    ) -> Result<()> {
        if depth >= 128 {
            return Ok(());
        }
        for name in child_names_bounded(source, budget)? {
            budget.check()?;
            let input = match open_at(source, &name, libc::O_RDONLY, 0) {
                Ok(input) => input,
                Err(_) => continue, // Cache misses rebuild; aliases/unreadable entries never block a command.
            };
            let metadata = input.metadata()?;
            if metadata.is_dir() {
                cvt(unsafe { libc::mkdirat(destination.as_raw_fd(), name.as_ptr(), 0o700) })?;
                let output = open_owned_directory(destination, &name)?;
                copy(&input, &output, depth + 1, fallback_bytes, budget)?;
            } else if metadata.is_file() {
                // sanitize_build already rejected links outside this cache.
                // No source path is followed here, and every copy gets a new inode.
                let cloned = unsafe {
                    libc::fclonefileat(input.as_raw_fd(), destination.as_raw_fd(), name.as_ptr(), 0)
                } == 0;
                let output = if cloned {
                    open_at(destination, &name, libc::O_RDONLY, 0)?
                } else {
                    let mut output = open_at(
                        destination,
                        &name,
                        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                        0o600,
                    )?;
                    anyhow::ensure!(
                        metadata.len()
                            <= (8u64 * 1024 * 1024 * 1024).saturating_sub(*fallback_bytes),
                        "build cache copy exceeds 8 GiB fallback budget"
                    );
                    let mut input = input.take(metadata.len());
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        budget.check()?;
                        let read = input.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        output.write_all(&buffer[..read])?;
                        *fallback_bytes += read as u64;
                    }
                    output
                };
                // Flags can be copied by clonefile even when sandbox chflags is
                // denied. Clear only the new owned single-link clone, never an
                // external hardlinked inode or the previous writable generation.
                let current = output.metadata()?;
                anyhow::ensure!(
                    current.uid() == unsafe { libc::geteuid() } && current.nlink() == 1,
                    "new build artifact is not exclusively owned"
                );
                cvt(unsafe { libc::fchflags(output.as_raw_fd(), 0) })?;
                output.set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
                output.set_times(fs::FileTimes::new().set_modified(metadata.modified()?))?;
            }
        }
        Ok(())
    }
    copy(
        source,
        destination,
        0,
        &mut 0,
        &mut WalkBudget::new(Duration::from_secs(30)),
    )
}

/// Canonicalize the user-selected workspace anchor, then open every managed
/// component descriptor-relatively. Never canonicalize through .moosedev or a
/// task scratch parent: those names must not redirect lifecycle mutations.
#[cfg(unix)]
fn scratch_parent(path: &Path, create: bool) -> Result<(PathBuf, File, CString)> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let components: Vec<_> = absolute.components().collect();
    anyhow::ensure!(
        components
            .iter()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "scratch path must contain no traversal"
    );
    let boundary = components
        .iter()
        .position(|part| part.as_os_str() == ".moosedev")
        .unwrap_or(2)
        .min(components.len().saturating_sub(1));
    let anchor: PathBuf = components[..boundary]
        .iter()
        .map(|part| part.as_os_str())
        .collect();
    let mut location = anchor
        .canonicalize()
        .context("resolve scratch workspace anchor")?;
    let mut parent = File::open(&location)?;
    for part in &components[boundary..components.len() - 1] {
        let name = CString::new(part.as_os_str().as_bytes())?;
        if create {
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        parent = open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
            .context("managed scratch parents must be real directories, not symlinks")?;
        location.push(part.as_os_str());
    }
    let name = CString::new(
        absolute
            .file_name()
            .context("task scratch requires a name")?
            .as_bytes(),
    )?;
    Ok((location, parent, name))
}

/// Delete owned task scratch without following any command-created alias. The
/// runner calls this only after command futures stop at a terminal task state.
#[cfg(unix)]
pub fn cleanup_task(path: &Path) -> Result<()> {
    let (_parent_path, parent, name) = match scratch_parent(path, false) {
        Ok(parent) => parent,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    let directory = match open_owned_directory(&parent, &name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).context("task scratch must be a real directory, not an alias")
        }
    };
    fs2::FileExt::try_lock_exclusive(&directory).context("task command scratch is still in use")?;
    remove_child_name(&parent, &name)
        .with_context(|| format!("cleanup task scratch {}", path.display()))
}

#[cfg(not(unix))]
pub fn cleanup_task(path: &Path) -> Result<()> {
    anyhow::ensure!(
        !path.exists(),
        "command scratch cleanup is unsupported on this platform"
    );
    Ok(())
}

#[cfg(unix)]
pub(super) struct TemporaryDirectory {
    _cleanup: FixedDirectoryCleanup,
    pub(super) path: PathBuf,
}

#[cfg(unix)]
impl TemporaryDirectory {
    pub(super) fn new(parent: &Path, prefix: &str) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let name = CString::new(format!("{prefix}-{}", uuid::Uuid::new_v4()))?;
        let path = parent.join(name.to_str()?);
        let parent = File::open(parent)?;
        cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })?;
        Ok(Self {
            _cleanup: FixedDirectoryCleanup { parent, name },
            path,
        })
    }
}

#[cfg(unix)]
pub(super) struct FixedDirectoryCleanup {
    pub(super) parent: File,
    pub(super) name: CString,
}

#[cfg(unix)]
impl Drop for FixedDirectoryCleanup {
    fn drop(&mut self) {
        let _ = remove_child_name(&self.parent, &self.name);
    }
}

#[cfg(unix)]
pub(super) fn sanitize_build(directory: &File) -> Result<()> {
    sanitize_build_bounded(directory, &mut WalkBudget::new(Duration::from_secs(30)))
}

#[cfg(unix)]
pub(super) fn sanitize_build_bounded(directory: &File, budget: &mut WalkBudget) -> Result<()> {
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;
    type Counts = HashMap<(libc::dev_t, libc::ino_t), (usize, usize)>;
    fn visit(
        directory: &File,
        counts: &mut Counts,
        remove: bool,
        depth: usize,
        budget: &mut WalkBudget,
    ) -> Result<()> {
        for name in child_names_bounded(directory, budget)? {
            budget.check()?;
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            cvt(unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            })?;
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR if depth >= 127 => remove_child_bounded(directory, &name, budget)?,
                libc::S_IFDIR => visit(
                    &open_owned_directory(directory, &name)?,
                    counts,
                    remove,
                    depth + 1,
                    budget,
                )?,
                libc::S_IFREG => {
                    let key = (stat.st_dev, stat.st_ino);
                    if remove {
                        // Cargo hardlinks its own artifacts. Keep an inode only
                        // when every link is contained within this build cache.
                        if counts.get(&key).is_none_or(|(seen, links)| links > seen) {
                            remove_child_bounded(directory, &name, budget)?;
                        }
                    } else {
                        let value = counts.entry(key).or_default();
                        value.0 += 1;
                        value.1 = stat.st_nlink as usize;
                    }
                }
                _ => remove_child_bounded(directory, &name, budget)?,
            }
        }
        Ok(())
    }
    let mut counts = Counts::new();
    visit(directory, &mut counts, false, 0, budget)?;
    visit(directory, &mut counts, true, 0, budget)
}

pub(super) fn prepare_cargo_home(destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME") {
        let registry = destination.join("registry");
        fs::create_dir(&registry)?;
        for name in ["cache", "index", "src"] {
            let source = Path::new(&home).join(".cargo/registry").join(name);
            if source.is_dir() {
                std::os::unix::fs::symlink(source, registry.join(name))?;
            }
        }
    }
    Ok(())
}
