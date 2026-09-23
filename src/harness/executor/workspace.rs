//! Safe live-workspace access and filtered command source snapshots.
#[cfg(unix)]
use super::fs_ops::{cvt, open_at};
use anyhow::{bail, Context, Result};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub(super) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 10_000;

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root.canonicalize().context("resolve workspace")?;
        anyhow::ensure!(root.is_dir(), "workspace must be a directory");
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files(&self) -> Result<Vec<String>> {
        let mut files = Vec::new();
        let mut pending = vec![(self.root.clone(), 0)];
        let mut visited = 0;
        while let Some((dir, depth)) = pending.pop() {
            anyhow::ensure!(depth < 64, "workspace directory depth exceeds 64");
            for entry in fs::read_dir(dir)? {
                visited += 1;
                anyhow::ensure!(
                    visited <= 100_000,
                    "workspace enumeration exceeds 100000 entries"
                );
                let entry = entry?;
                let name = entry.file_name();
                if protected(&name.to_string_lossy(), depth) {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    #[cfg(unix)]
                    {
                        let path = entry.path();
                        let relative = path
                            .strip_prefix(&self.root)?
                            .to_str()
                            .context("workspace directory path must be UTF-8")?;
                        let (parent, name) = self.parent(relative, false)?;
                        let directory =
                            open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                        if cache_directory(&directory) {
                            continue;
                        }
                    }
                    pending.push((entry.path(), depth + 1));
                } else if kind.is_file() {
                    let path = entry.path();
                    let Some(relative) = path.strip_prefix(&self.root)?.to_str() else {
                        continue;
                    };
                    if self.read(relative).is_ok_and(|text| text.is_some()) {
                        anyhow::ensure!(
                            files.len() < MAX_FILES,
                            "workspace exceeds 10000 readable files"
                        );
                        files.push(relative.to_owned());
                    }
                }
            }
        }
        files.sort();
        Ok(files)
    }

    pub fn read(&self, file: &str) -> Result<Option<String>> {
        #[cfg(unix)]
        {
            let (parent, name) = match self.parent(file, false) {
                Ok(value) => value,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None)
                }
                Err(error) => return Err(error),
            };
            read_at(&parent, &name).map(|value| value.map(|(text, _)| text))
        }
        #[cfg(not(unix))]
        bail!("safe workspace access is unsupported on this platform")
    }

    /// Exact whole-file compare-and-swap; creation can add parent directories.
    /// Directory-descriptor operations prevent symlink traversal. The workspace
    /// lock serializes harness writers; users must not edit the target concurrently.
    pub fn apply(&self, file: &str, before: Option<&str>, after: Option<&str>) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
            anyhow::ensure!(
                after.is_none_or(|text| text.len() as u64 <= MAX_FILE_BYTES),
                "replacement exceeds 2 MiB"
            );
            let root = File::open(&self.root)?;
            fs2::FileExt::lock_exclusive(&root)?;
            let (parent, name) = self.parent(file, before.is_none() && after.is_some())?;
            let current = read_at(&parent, &name)?;
            anyhow::ensure!(
                current.as_ref().map(|(text, _)| text.as_str()) == before,
                "file changed since it was read: {file}"
            );
            match after {
                None if current.is_some() => {
                    cvt(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) })?;
                }
                None => {}
                Some(text) => {
                    let temporary =
                        CString::new(format!(".moosedev-edit-{}", uuid::Uuid::new_v4()))?;
                    let mut output = open_at(
                        &parent,
                        &temporary,
                        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                        0o600,
                    )?;
                    let result = (|| -> Result<()> {
                        output.write_all(text.as_bytes())?;
                        let mode = current
                            .as_ref()
                            .map(|(_, metadata)| metadata.permissions().mode() & 0o777)
                            .unwrap_or(0o644);
                        output.set_permissions(fs::Permissions::from_mode(mode))?;
                        output.sync_all()?;
                        // Detect changes made while writing the temporary file.
                        anyhow::ensure!(
                            read_at(&parent, &name)?
                                .as_ref()
                                .map(|(text, _)| text.as_str())
                                == before,
                            "file changed during apply: {file}"
                        );
                        cvt(unsafe {
                            libc::renameat(
                                parent.as_raw_fd(),
                                temporary.as_ptr(),
                                parent.as_raw_fd(),
                                name.as_ptr(),
                            )
                        })?;
                        Ok(())
                    })();
                    if result.is_err() {
                        unsafe {
                            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
                        }
                    }
                    result?;
                }
            }
            parent.sync_all().context("persist directory update")?;
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("safe workspace writes are unsupported on this platform")
    }

    #[cfg(unix)]
    fn parent(&self, file: &str, create: bool) -> Result<(File, CString)> {
        use std::os::fd::AsRawFd;
        let names = checked_components(file)?;
        let mut parent = File::open(&self.root)?;
        for name in &names[..names.len() - 1] {
            let name = CString::new(name.as_bytes())?;
            let opened = open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0);
            parent = match opened {
                Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                    cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) })?;
                    parent.sync_all()?;
                    open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                result => result
                    .with_context(|| format!("open parent of {file} without following links"))?,
            };
        }
        Ok((parent, CString::new(names.last().unwrap().as_bytes())?))
    }
}

fn protected(name: &str, depth: usize) -> bool {
    let lower = name.to_ascii_lowercase();
    // Conventional generated directories are excluded only at the project root;
    // src/build and similar names can contain actual implementation code.
    if depth == 0 && matches!(lower.as_str(), "target" | "build" | "dist") {
        return true;
    }
    if (lower == ".env" || lower.starts_with(".env.")) && lower != ".env.example" {
        return true;
    }
    // `moosedev.toml` configures the sandbox itself, so a model that could edit
    // it could widen its own capabilities. The example file carries no values.
    if depth == 0 && lower == crate::config::FILE_NAME {
        return true;
    }
    matches!(
        lower.as_str(),
        ".git"
            | ".moosedev"
            | "node_modules"
            | ".venv"
            | "venv"
            | ".cache"
            | ".agents"
            | ".codex"
            | ".claude"
            | ".ssh"
            | ".gnupg"
            | ".aws"
            | ".azure"
            | ".kube"
    )
}

/// Cache Directory Tagging Specification: an ordinary marker file whose first
/// 43 bytes match this header. Read a fixed prefix through the directory fd;
/// aliases and special files must neither hide source nor block discovery.
/// https://bford.info/cachedir/
#[cfg(unix)]
pub(super) fn cache_directory(directory: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    const SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";
    let Ok(mut marker) = open_at(
        directory,
        &CString::new("CACHEDIR.TAG").unwrap(),
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    ) else {
        return false;
    };
    if !marker
        .metadata()
        .is_ok_and(|m| m.is_file() && m.nlink() == 1)
    {
        return false;
    }
    let mut header = [0; 43];
    marker.read_exact(&mut header).is_ok() && &header == SIGNATURE
}

fn checked_components(file: &str) -> Result<Vec<&str>> {
    anyhow::ensure!(
        !file.is_empty() && !file.contains('\\') && !file.contains('\0'),
        "invalid file path"
    );
    let mut names = Vec::new();
    for component in Path::new(file).components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().context("path must be UTF-8")?;
                anyhow::ensure!(
                    !protected(name, names.len()),
                    "protected workspace path: {file}"
                );
                names.push(name);
            }
            _ => bail!("path must be relative and contain no traversal: {file}"),
        }
    }
    anyhow::ensure!(!names.is_empty(), "empty file path");
    Ok(names)
}

#[cfg(unix)]
fn read_at(parent: &File, name: &CString) -> Result<Option<(String, fs::Metadata)>> {
    use std::os::unix::fs::MetadataExt;
    let input = match open_at(parent, name, libc::O_RDONLY, 0) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = input.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "target must be a regular file with one link"
    );
    anyhow::ensure!(metadata.len() <= MAX_FILE_BYTES, "file exceeds 2 MiB");
    let mut bytes = Vec::new();
    input.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_FILE_BYTES, "file exceeds 2 MiB");
    Ok(Some((
        String::from_utf8(bytes).context("file is not UTF-8")?,
        metadata,
    )))
}

/// Copy regular source files without following any symlink component. Hardlinks
/// are omitted too: an innocuous project name may alias a protected host file.
/// Binary fixtures are supported; explicit bounds make oversized workspaces fail
/// before a command is started instead of silently giving checks partial inputs.
#[cfg(unix)]
pub(super) fn snapshot_source(workspace: &Workspace, destination: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fs::create_dir(destination)?;
    let mut pending = vec![(PathBuf::new(), 0)];
    let mut entries = 0;
    let mut total_bytes = 0u64;
    while let Some((relative, depth)) = pending.pop() {
        anyhow::ensure!(depth < 64, "command source exceeds 64 directory levels");
        for entry in fs::read_dir(workspace.root().join(&relative))? {
            entries += 1;
            anyhow::ensure!(entries <= 100_000, "command source exceeds 100000 entries");
            let entry = entry?;
            let name = entry.file_name();
            if protected(&name.to_string_lossy(), depth) {
                continue;
            }
            let relative = relative.join(name);
            let file = relative
                .to_str()
                .context("command source path must be UTF-8")?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                // Validate ancestors without following links before enumeration.
                let (parent, name) = workspace.parent(file, false)?;
                let directory = open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                if cache_directory(&directory) {
                    continue;
                }
                fs::create_dir_all(destination.join(&relative))?;
                pending.push((relative, depth + 1));
            } else if kind.is_file() {
                let (parent, name) = workspace.parent(file, false)?;
                let input = open_at(&parent, &name, libc::O_RDONLY, 0)?;
                let metadata = input.metadata()?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    continue;
                }
                const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;
                anyhow::ensure!(
                    metadata.len() <= TOTAL_LIMIT.saturating_sub(total_bytes),
                    "command source exceeds 512 MiB snapshot budget at {file}"
                );
                let target = destination.join(&relative);
                fs::create_dir_all(target.parent().unwrap())?;
                let mut output = File::create(target)?;
                let copied = std::io::copy(
                    &mut input.take(TOTAL_LIMIT.saturating_sub(total_bytes) + 1),
                    &mut output,
                )?;
                total_bytes += copied;
                anyhow::ensure!(
                    total_bytes <= TOTAL_LIMIT,
                    "command source snapshot exceeds its size budget"
                );
                output.set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
                output.set_times(fs::FileTimes::new().set_modified(metadata.modified()?))?;
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn snapshot_source(_workspace: &Workspace, _destination: &Path) -> Result<()> {
    bail!("command confinement is unsupported on this platform")
}
