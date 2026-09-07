//! The runner's only filesystem mutation and command execution boundary.
use super::progress::{Progress, ProgressSender};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_OUTPUT: usize = 128 * 1024;

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
fn cache_directory(directory: &File) -> bool {
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
fn cvt(value: libc::c_int) -> std::io::Result<libc::c_int> {
    if value < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

#[cfg(unix)]
fn open_at(
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
}

/// Execute against a filtered, read-only source snapshot with no network.
/// The task build cache and per-command temporary directories are writable;
/// host reads are allowlisted and the source path/mtimes remain stable.
pub async fn command(root: &Path, scratch: &Path, command: &str) -> Result<CommandResult> {
    command_with_progress(root, scratch, command, None).await
}

/// Output notifications are bounded like the durable command result. Dropping
/// the receiver never changes execution or confinement semantics.
pub async fn command_with_progress(
    root: &Path,
    scratch: &Path,
    command: &str,
    progress: Option<ProgressSender>,
) -> Result<CommandResult> {
    run_command(root, scratch, command, command_timeout()?, progress).await
}

#[cfg(unix)]
async fn run_command(
    root: &Path,
    scratch: &Path,
    command: &str,
    timeout: Duration,
    progress: Option<ProgressSender>,
) -> Result<CommandResult> {
    anyhow::ensure!(
        !command.is_empty() && command.len() <= 32 * 1024,
        "command must contain 1–32768 bytes"
    );
    let root = root.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "command workspace must be a directory");
    for allowed in readable_directories() {
        let allowed = allowed.canonicalize()?;
        anyhow::ensure!(
            !root.starts_with(&allowed),
            "command workspace must be outside trusted system/toolchain directories"
        );
    }
    let task = TaskScratch::open(scratch)?;
    #[cfg(target_os = "macos")]
    task.rotate_build()?;
    let scratch = &task.path;
    let source = scratch.join("source");
    // Publish a fresh trusted snapshot at a stable path, preserving source mtimes
    // so Cargo can reuse artifacts. Never copy from command-writable scratch.
    let staging = TemporaryDirectory::new(scratch, "snapshot")?;
    snapshot_source(&Workspace::new(&root)?, &staging.path.join("source"))?;
    remove_child(&task.directory, "source")?;
    fs::rename(staging.path.join("source"), &source)?;
    let _cargo_cleanup = FixedDirectoryCleanup {
        parent: task.directory.try_clone()?,
        name: CString::new("cargo-home")?,
    };
    let temporary = TemporaryDirectory::new(scratch, "command")?;
    for directory in ["home", "tmp"] {
        fs::create_dir(temporary.path.join(directory))?;
    }
    // Cargo registry paths also participate in fingerprints. Keep their path
    // stable but rebuild the small configuration-free home before every command.
    let cargo_home = scratch.join("cargo-home");
    #[cfg(target_os = "macos")]
    {
        let backing = temporary.path.join("cargo-home");
        prepare_cargo_home(&backing)?;
        std::os::unix::fs::symlink(backing.strip_prefix(scratch)?, &cargo_home)?;
    }
    #[cfg(not(target_os = "macos"))]
    prepare_cargo_home(&cargo_home)?;
    let mut process = confined_command(&source, scratch, &temporary.path, command)?;
    process
        .current_dir(&source)
        .env_clear()
        .env("PATH", trusted_path()?)
        .env("HOME", temporary.path.join("home"))
        .env("TMPDIR", temporary.path.join("tmp"))
        .env("TMP", temporary.path.join("tmp"))
        .env("TEMP", temporary.path.join("tmp"))
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", scratch.join("build"))
        .env("CARGO_NET_OFFLINE", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Only toolchains/settings and registry data are exposed, never the user's
    // Cargo configuration or credentials. Cargo writes locks in its scratch home.
    if let Some(home) = std::env::var_os("HOME") {
        process.env("RUSTUP_HOME", Path::new(&home).join(".rustup"));
    }
    #[cfg(unix)]
    process.process_group(0);
    let child = process
        .spawn()
        .context("start confined command (sandbox support is required)")?;
    let mut child = ChildGroup::new(child);
    let stdout = child
        .child
        .stdout
        .take()
        .context("missing command stdout")?;
    let stderr = child
        .child
        .stderr
        .take()
        .context("missing command stderr")?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let (exited, leader_exit) = tokio::sync::oneshot::channel();
    let wait = async {
        let status = tokio::time::timeout(timeout, child.child.wait()).await;
        child.kill_group();
        let _ = exited.send(());
        status
    };
    let drain = async {
        let output = async {
            tokio::try_join!(
                bounded_output_into(stdout, progress.clone(), &mut stdout_bytes),
                bounded_output_into(stderr, progress, &mut stderr_bytes),
            )
        };
        tokio::pin!(output);
        tokio::select! {
            result = &mut output => result.map(|_| false),
            _ = leader_exit => match tokio::time::timeout(Duration::from_millis(500), output).await {
                Ok(result) => result.map(|_| false),
                Err(_) => Ok(true),
            },
        }
    };
    let (status, unfinished_output) = tokio::join!(wait, drain);
    let unfinished_output = unfinished_output?;
    let mut output = String::from_utf8_lossy(&stdout_bytes).into_owned();
    if !stderr_bytes.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&String::from_utf8_lossy(&stderr_bytes));
    }
    if unfinished_output {
        output.push_str("\n[command output remained open after its leader exited; drain stopped after 500 ms and the command is incomplete]");
    }
    if let Some(notice) = &task.cleanup_notice {
        output.push_str(&format!("\n[{notice}]"));
    }
    let status = status
        .with_context(|| format!("command exceeded {} second timeout", timeout.as_secs()))??;
    if !status.success() && output.is_empty() {
        output = format!("command failed: {status}");
    }
    Ok(CommandResult {
        success: status.success() && !unfinished_output,
        output,
    })
}

#[cfg(not(unix))]
async fn run_command(
    _root: &Path,
    _scratch: &Path,
    _command: &str,
    _timeout: Duration,
    _progress: Option<ProgressSender>,
) -> Result<CommandResult> {
    bail!("command confinement is unsupported on this platform")
}

/// Human configuration, never model-supplied. Cold local builds need minutes;
/// explicit limits still prevent an unattended command from running forever.
fn command_timeout() -> Result<Duration> {
    parse_command_timeout(std::env::var("MOOSEDEV_COMMAND_TIMEOUT_SECONDS"))
}

fn parse_command_timeout(
    value: std::result::Result<String, std::env::VarError>,
) -> Result<Duration> {
    let seconds = match value {
        Ok(value) => value
            .parse::<u64>()
            .context("MOOSEDEV_COMMAND_TIMEOUT_SECONDS must be an integer")?,
        Err(std::env::VarError::NotPresent) => 900,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        (1..=86400).contains(&seconds),
        "MOOSEDEV_COMMAND_TIMEOUT_SECONDS must be between 1 and 86400"
    );
    Ok(Duration::from_secs(seconds))
}

#[cfg(unix)]
struct TaskScratch {
    path: PathBuf,
    directory: File,
    cleanup_notice: Option<String>,
    #[cfg(target_os = "macos")]
    cache_valid: bool,
}

#[cfg(unix)]
impl TaskScratch {
    fn open(path: &Path) -> Result<Self> {
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
fn build_backing(directory: &File) -> Result<CString> {
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
    bail!("task build cache cannot be a symlink");
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
    fn rotate_build(&self) -> Result<()> {
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
struct TemporaryDirectory {
    parent: File,
    name: CString,
    path: PathBuf,
}

#[cfg(unix)]
impl TemporaryDirectory {
    fn new(parent: &Path, prefix: &str) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let name = CString::new(format!("{prefix}-{}", uuid::Uuid::new_v4()))?;
        let path = parent.join(name.to_str()?);
        let parent = File::open(parent)?;
        cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })?;
        Ok(Self { parent, name, path })
    }
}

#[cfg(unix)]
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = remove_child_name(&self.parent, &self.name);
    }
}

#[cfg(unix)]
struct FixedDirectoryCleanup {
    parent: File,
    name: CString,
}

#[cfg(unix)]
impl Drop for FixedDirectoryCleanup {
    fn drop(&mut self) {
        let _ = remove_child_name(&self.parent, &self.name);
    }
}

/// fdopendir owns its descriptor, so duplicate instead of consuming the caller's
/// locked/open directory. Names, recursion and unlink remain descriptor-relative.
#[cfg(unix)]
fn child_names(directory: &File) -> Result<Vec<CString>> {
    child_names_bounded(directory, &mut WalkBudget::new(Duration::from_secs(5)))
}

#[cfg(unix)]
struct WalkBudget {
    deadline: std::time::Instant,
    remaining: usize,
}

#[cfg(unix)]
impl WalkBudget {
    fn new(duration: Duration) -> Self {
        Self {
            deadline: std::time::Instant::now() + duration,
            remaining: 1_000_000,
        }
    }

    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            std::time::Instant::now() < self.deadline,
            "scratch traversal reached its time limit; retry cleanup or rebuild the cache"
        );
        Ok(())
    }
}

#[cfg(unix)]
fn child_names_bounded(directory: &File, budget: &mut WalkBudget) -> Result<Vec<CString>> {
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
fn remove_child(directory: &File, name: &str) -> Result<()> {
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
fn open_owned_directory(parent: &File, name: &CString) -> std::io::Result<File> {
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
fn remove_child_name(directory: &File, name: &CString) -> Result<()> {
    remove_child_before(
        directory,
        name,
        std::time::Instant::now() + Duration::from_secs(5),
    )
}

#[cfg(unix)]
fn remove_child_before(
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
fn remove_child_bounded(directory: &File, name: &CString, budget: &mut WalkBudget) -> Result<()> {
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

#[cfg(unix)]
fn sanitize_build(directory: &File) -> Result<()> {
    sanitize_build_bounded(directory, &mut WalkBudget::new(Duration::from_secs(30)))
}

#[cfg(unix)]
fn sanitize_build_bounded(directory: &File, budget: &mut WalkBudget) -> Result<()> {
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

fn trusted_path() -> Result<std::ffi::OsString> {
    let mut paths = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        paths.insert(0, Path::new(&home).join(".cargo/bin"));
    }
    std::env::join_paths(paths)
        .context("trusted command PATH contains a path separator (check HOME)")
}

#[cfg(test)]
async fn bounded_output(
    input: impl AsyncRead + Unpin,
    progress: Option<ProgressSender>,
) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    bounded_output_into(input, progress, &mut result).await?;
    Ok(result)
}

async fn bounded_output_into(
    mut input: impl AsyncRead + Unpin,
    progress: Option<ProgressSender>,
    result: &mut Vec<u8>,
) -> std::io::Result<()> {
    let mut emitted = 0;
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(MAX_OUTPUT.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
        // Retain an incomplete UTF-8 suffix until the next read so a terminal
        // never receives a replacement character for a split Unicode scalar.
        let end = emitted + complete_utf8_prefix(&result[emitted..]);
        send_output(progress.as_ref(), &result[emitted..end]);
        emitted = end;
        truncated |= keep < count;
    }
    if truncated {
        result.extend_from_slice(b"\n[output truncated]\n");
    }
    send_output(progress.as_ref(), &result[emitted..]);
    Ok(())
}

fn send_output(progress: Option<&ProgressSender>, bytes: &[u8]) {
    if let Some(progress) = progress.filter(|_| !bytes.is_empty()) {
        let _ = progress.send(Progress::CommandOutput(
            String::from_utf8_lossy(bytes).into_owned(),
        ));
    }
}

fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            Err(error) => {
                offset += error.valid_up_to();
                match error.error_len() {
                    Some(invalid) => offset += invalid,
                    None => return offset,
                }
            }
        }
    }
    offset
}

struct ChildGroup {
    child: tokio::process::Child,
    group: Option<u32>,
}
impl ChildGroup {
    fn new(child: tokio::process::Child) -> Self {
        let group = child.id();
        Self { child, group }
    }
    fn kill_group(&mut self) {
        if let Some(group) = self.group.take() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(group as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// Copy regular source files without following any symlink component. Hardlinks
/// are omitted too: an innocuous project name may alias a protected host file.
/// Binary fixtures are supported; explicit bounds make oversized workspaces fail
/// before a command is started instead of silently giving checks partial inputs.
#[cfg(unix)]
fn snapshot_source(workspace: &Workspace, destination: &Path) -> Result<()> {
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
fn snapshot_source(_workspace: &Workspace, _destination: &Path) -> Result<()> {
    bail!("command confinement is unsupported on this platform")
}

fn prepare_cargo_home(destination: &Path) -> Result<()> {
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

/// Platform runtime and installed tools only. In particular, no /, /Users,
/// /home, /private, ~/.cargo, ~/.ssh, or live project subtree is exposed.
fn readable_directories() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let system = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/libexec",
        "/System/Library",
        "/System/Cryptexes/OS",
        "/System/Volumes/Preboot/Cryptexes/OS",
        "/private/var/db/dyld",
        "/Library/Developer/CommandLineTools",
        "/Applications/Xcode.app/Contents/Developer",
        "/opt/homebrew/bin",
        "/opt/homebrew/Cellar",
        "/opt/homebrew/opt",
        "/opt/homebrew/lib",
        "/opt/homebrew/share",
        "/usr/local/bin",
        "/usr/local/Cellar",
        "/usr/local/opt",
        "/usr/local/lib",
        "/usr/local/share",
    ];
    #[cfg(not(target_os = "macos"))]
    let system = ["/bin", "/sbin", "/lib", "/lib64", "/usr"];
    let mut paths: Vec<PathBuf> = system
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .collect();
    if let Some(home) = std::env::var_os("HOME") {
        for suffix in [
            ".cargo/bin",
            ".cargo/registry/cache",
            ".cargo/registry/index",
            ".cargo/registry/src",
            ".rustup/toolchains",
        ] {
            let path = Path::new(&home).join(suffix);
            if path.is_dir() && !path.is_symlink() {
                paths.push(path);
            }
        }
    }
    paths
}

fn readable_files() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/etc/localtime")];
    // Apple's libcurl initializes LibreSSL even for offline Cargo operations.
    // Grant only its system configuration file, not /etc or the SSL directory.
    #[cfg(target_os = "macos")]
    paths.push(PathBuf::from("/private/etc/ssl/openssl.cnf"));
    #[cfg(target_os = "linux")]
    paths.push(PathBuf::from("/etc/ld.so.cache"));
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(Path::new(&home).join(".rustup/settings.toml"));
    }
    paths.into_iter().filter(|path| path.is_file()).collect()
}

#[cfg(target_os = "macos")]
fn sandbox_literal(path: &Path) -> Result<String> {
    let path = path.to_str().context("sandbox path must be UTF-8")?;
    anyhow::ensure!(
        !path.chars().any(char::is_control),
        "sandbox path contains control characters"
    );
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('\"', "\\\"")
    ))
}

#[cfg(target_os = "macos")]
fn confined_command(
    root: &Path,
    scratch: &Path,
    temporary: &Path,
    command: &str,
) -> Result<tokio::process::Command> {
    // Metadata is not file contents. Runtime path resolution needs ancestor
    // metadata even though those ancestors must not grant recursive data reads.
    let mut profile = String::from("(version 1)(deny default)(allow file-read-metadata)(allow process-fork)(allow sysctl-read)(allow signal (target same-sandbox))");
    for path in readable_directories()
        .into_iter()
        .chain([scratch.to_path_buf()])
    {
        profile.push_str(&format!(
            "(allow file-read* process-exec (subpath {}))",
            sandbox_literal(&path.canonicalize()?)?
        ));
    }
    for path in readable_files().into_iter().chain([
        // libignition opens / as an openat root during dyld bootstrap. This
        // literal directory grant does not expose descendants.
        PathBuf::from("/"),
        PathBuf::from("/dev/null"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/random"),
    ]) {
        profile.push_str(&format!(
            "(allow file-read* (literal {}))",
            sandbox_literal(&path)?
        ));
    }
    for writable in [
        scratch.join("build"),
        scratch.join("cargo-home"),
        temporary.to_path_buf(),
    ] {
        profile.push_str(&format!(
            "(allow file-write* (subpath {}))",
            sandbox_literal(&writable.canonicalize()?)?
        ));
    }
    profile.push_str("(deny file-write-flags)");
    profile.push_str(&format!("(deny file-write* (subpath {}))(deny file-write-unlink (literal {}) (literal {}) (literal {}))(allow file-write-data (literal \"/dev/null\"))", sandbox_literal(root)?, sandbox_literal(&scratch.join("build").canonicalize()?)?, sandbox_literal(temporary)?, sandbox_literal(&scratch.join("cargo-home").canonicalize()?)?));
    let mut process = tokio::process::Command::new("/usr/bin/sandbox-exec");
    process.args(["-p", &profile, "/bin/sh", "-c", command]);
    Ok(process)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn confined_command(
    root: &Path,
    scratch: &Path,
    temporary: &Path,
    command: &str,
) -> Result<tokio::process::Command> {
    use std::os::fd::AsRawFd;
    let binary = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .context("bubblewrap is required for confined commands")?;
    let mut process = tokio::process::Command::new(binary);
    process
        .args([
            "--die-with-parent",
            "--new-session",
            "--unshare-all",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--ro-bind",
        ])
        .arg(scratch)
        .arg(scratch);
    for writable in [
        scratch.join("build"),
        scratch.join("cargo-home"),
        temporary.to_path_buf(),
    ] {
        process.arg("--bind").arg(&writable).arg(&writable);
    }
    for path in readable_directories().into_iter().chain(readable_files()) {
        // Resolve system symlinks at the source only; their sandbox destinations
        // remain the tool paths expected by the dynamic loader and PATH.
        process.arg("--ro-bind").arg(path.canonicalize()?).arg(path);
    }
    process
        .arg("--ro-bind")
        .arg(root)
        .arg(root)
        .arg("--chdir")
        .arg(root)
        .args(["--seccomp", "198", "/bin/sh", "-c", command]);
    // Network namespaces do not block pathname-based Unix sockets. Deny socket
    // creation in seccomp as well, including foreign syscall architectures.
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc00000b7;
    let filter: [(u16, u8, u8, u32); 9] = [
        (0x20, 0, 0, 4),
        (0x15, 1, 0, ARCH),
        (0x06, 0, 0, 0x80000000),
        (0x20, 0, 0, 0),
        (0x35, 0, 1, 0x40000000),
        (0x06, 0, 0, 0x80000000),
        (0x15, 0, 1, libc::SYS_socket as u32),
        (0x06, 0, 0, 0x00050000 | libc::EPERM as u32),
        (0x06, 0, 0, 0x7fff0000),
    ];
    let filter_path = temporary.join("seccomp.bpf");
    let mut bytes = Vec::new();
    for (code, jt, jf, k) in filter {
        bytes.extend(code.to_ne_bytes());
        bytes.extend([jt, jf]);
        bytes.extend(k.to_ne_bytes());
    }
    fs::write(&filter_path, bytes)?;
    let filter = File::open(filter_path)?;
    unsafe {
        process.pre_exec(move || {
            if libc::dup2(filter.as_raw_fd(), 198) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(process)
}

#[cfg(not(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
fn confined_command(
    _root: &Path,
    _scratch: &Path,
    _temporary: &Path,
    _command: &str,
) -> Result<tokio::process::Command> {
    bail!("command confinement is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn scratch_flags_are_repaired_only_on_exclusively_owned_entries() {
        use std::os::fd::AsRawFd;
        let scratch = Fixture::new();
        let outside = Fixture::new();
        let task = TaskScratch::open(&scratch.0).unwrap();
        fs::write(task.path.join("build/flagged"), "warm").unwrap();
        let flagged = File::open(task.path.join("build/flagged")).unwrap();
        cvt(unsafe { libc::fchflags(flagged.as_raw_fd(), libc::UF_IMMUTABLE | libc::UF_APPEND) })
            .unwrap();
        let build = File::open(task.path.join("build")).unwrap();
        cvt(unsafe { libc::fchflags(build.as_raw_fd(), libc::UF_IMMUTABLE) }).unwrap();
        drop(task);
        cleanup_task(&scratch.0).unwrap();
        assert!(!scratch.0.exists());
        fs::write(outside.0.join("flagged"), "outside").unwrap();
        let external = File::open(outside.0.join("flagged")).unwrap();
        let task = TaskScratch::open(&scratch.0).unwrap();
        fs::hard_link(outside.0.join("flagged"), task.path.join("build/alias")).unwrap();
        cvt(unsafe { libc::fchflags(external.as_raw_fd(), libc::UF_IMMUTABLE) }).unwrap();
        drop(task);
        let result = cleanup_task(&scratch.0);
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        cvt(unsafe { libc::fstat(external.as_raw_fd(), &mut stat) }).unwrap();
        // Reset our fake external fixture before assertions so failure cannot
        // leave an immutable file behind in the test environment.
        cvt(unsafe { libc::fchflags(external.as_raw_fd(), 0) }).unwrap();
        assert!(
            result.is_err(),
            "cleanup modified an external hardlinked inode"
        );
        assert_ne!(stat.st_flags & libc::UF_IMMUTABLE, 0);
        cleanup_task(&scratch.0).unwrap();
    }

    #[test]
    fn command_timeout_defaults_and_bounds_are_explicit() {
        assert_eq!(
            parse_command_timeout(Err(std::env::VarError::NotPresent)).unwrap(),
            Duration::from_secs(900)
        );
        for (input, seconds) in [("1", 1), ("900", 900), ("86400", 86400)] {
            assert_eq!(
                parse_command_timeout(Ok(input.into())).unwrap(),
                Duration::from_secs(seconds)
            );
        }
        for input in ["0", "86401", "-1", "NaN", "18446744073709551616", ""] {
            assert!(
                parse_command_timeout(Ok(input.into())).is_err(),
                "accepted {input}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn scratch_parent_symlinks_cannot_redirect_creation_or_cleanup() {
        use std::os::unix::fs::symlink;
        let project = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("keep"), "untouched").unwrap();
        for parent in [
            ".moosedev",
            ".moosedev/harness",
            ".moosedev/harness/scratch",
        ] {
            let alias = project.0.join(parent);
            fs::create_dir_all(alias.parent().unwrap()).unwrap();
            symlink(&outside.0, &alias).unwrap();
            let scratch = project.0.join(".moosedev/harness/scratch/task");
            assert!(TaskScratch::open(&scratch).is_err(), "followed {parent}");
            assert!(cleanup_task(&scratch).is_err(), "cleanup followed {parent}");
            assert_eq!(
                fs::read_to_string(outside.0.join("keep")).unwrap(),
                "untouched"
            );
            assert!(!outside.0.join("task").exists());
            fs::remove_file(alias).unwrap();
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_generations_copy_inodes_and_reject_fifo_socket_and_external_aliases() {
        use std::os::{
            fd::AsRawFd,
            unix::{
                fs::{symlink, MetadataExt},
                net::UnixListener,
            },
        };
        let scratch = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret"), "private").unwrap();
        let task = TaskScratch::open(&scratch.0).unwrap();
        fs::write(task.path.join("build/artifact"), "warm").unwrap();
        task.rotate_build().unwrap();
        let old = File::open(task.path.join("build/artifact")).unwrap();
        let old_metadata = old.metadata().unwrap();
        let build = open_owned_directory(&task.directory, &build_backing(&task.directory).unwrap())
            .unwrap();
        cvt(unsafe {
            libc::mkfifoat(
                build.as_raw_fd(),
                CString::new("fifo").unwrap().as_ptr(),
                0o600,
            )
        })
        .unwrap();
        let _socket = UnixListener::bind(task.path.join("build/socket")).unwrap();
        symlink(outside.0.join("secret"), task.path.join("build/alias")).unwrap();
        fs::hard_link(outside.0.join("secret"), task.path.join("build/hardalias")).unwrap();
        drop(task);
        let task = TaskScratch::open(&scratch.0).unwrap();
        task.rotate_build().unwrap();
        let new_metadata = fs::metadata(task.path.join("build/artifact")).unwrap();
        assert_ne!(old_metadata.ino(), new_metadata.ino());
        assert_eq!(
            old_metadata.modified().unwrap(),
            new_metadata.modified().unwrap()
        );
        assert_eq!(
            fs::read_to_string(task.path.join("build/artifact")).unwrap(),
            "warm"
        );
        for name in ["fifo", "socket", "alias", "hardalias"] {
            assert!(!task.path.join("build").join(name).exists());
        }
        drop(task);
        cleanup_task(&scratch.0).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_enumeration_shares_entry_and_time_limits_across_passes() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("a"), "one").unwrap();
        fs::write(fixture.0.join("b"), "two").unwrap();
        let directory = File::open(&fixture.0).unwrap();
        let mut budget = WalkBudget::new(Duration::from_secs(5));
        budget.remaining = 1;
        assert!(child_names_bounded(&directory, &mut budget)
            .unwrap_err()
            .to_string()
            .contains("entry limit"));
        let mut budget = WalkBudget::new(Duration::from_secs(5));
        budget.remaining = 2;
        assert!(sanitize_build_bounded(&directory, &mut budget)
            .unwrap_err()
            .to_string()
            .contains("entry limit"));
        assert_eq!(
            budget.remaining, 0,
            "the second pass must share the first pass's budget"
        );
        let mut budget = WalkBudget::new(Duration::ZERO);
        assert!(child_names_bounded(&directory, &mut budget)
            .unwrap_err()
            .to_string()
            .contains("time limit"));
        sanitize_build(&directory).unwrap();
        assert_eq!(fs::read_to_string(fixture.0.join("a")).unwrap(), "one");
    }

    #[cfg(unix)]
    #[test]
    fn large_directory_cleanup_makes_progress_across_bounded_batches() {
        let fixture = Fixture::new();
        let cache = fixture.0.join("cache");
        fs::create_dir(&cache).unwrap();
        for index in 0..4100 {
            fs::write(cache.join(index.to_string()), "disposable").unwrap();
        }
        let parent = File::open(&fixture.0).unwrap();
        assert!(remove_child(&parent, "cache").is_err());
        let remaining = fs::read_dir(&cache).unwrap().count();
        assert_eq!(
            remaining, 4,
            "first cleanup must delete its batch before returning"
        );
        remove_child(&parent, "cache").unwrap();
        assert!(!cache.exists());
    }

    #[tokio::test]
    async fn output_progress_precedes_eof_and_preserves_split_unicode() {
        use tokio::io::AsyncWriteExt;
        let (input, mut writer) = tokio::io::duplex(64);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let read = tokio::spawn(bounded_output(input, Some(sender)));
        writer.write_all(b"hello \xf0\x9f").await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, Progress::CommandOutput(text) if text == "hello "));
        writer.write_all(b"\xab\x8e!").await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, Progress::CommandOutput(text) if text == "🫎!"));
        drop(writer);
        assert_eq!(
            String::from_utf8(read.await.unwrap().unwrap()).unwrap(),
            "hello 🫎!"
        );
    }

    #[tokio::test]
    async fn output_progress_is_bounded_but_input_is_drained() {
        let bytes = vec![b'x'; MAX_OUTPUT * 3];
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let result = bounded_output(bytes.as_slice(), Some(sender))
            .await
            .unwrap();
        let mut observed = String::new();
        while let Some(Progress::CommandOutput(text)) = receiver.recv().await {
            observed.push_str(&text);
        }
        assert_eq!(observed.as_bytes(), result);
        assert_eq!(result.len(), MAX_OUTPUT + b"\n[output truncated]\n".len());
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // macOS's default temporary-directory prefix exceeds sockaddr_un's
            // path budget once a UUID and socket name are appended.
            let base = if cfg!(unix) {
                PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            };
            let path = base.join(format!("mdx-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn workspace(&self) -> Workspace {
            Workspace::new(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cas_create_update_delete_and_permissions() {
        let fixture = Fixture::new();
        let workspace = fixture.workspace();
        assert_eq!(workspace.read("code.rs").unwrap(), None);
        workspace.apply("code.rs", None, Some("old")).unwrap();
        assert!(workspace.apply("code.rs", None, Some("clobber")).is_err());
        assert!(workspace.apply("code.rs", Some("stale"), None).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(fixture.0.join("code.rs"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        workspace
            .apply("code.rs", Some("old"), Some("new"))
            .unwrap();
        assert_eq!(workspace.read("code.rs").unwrap().as_deref(), Some("new"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(fixture.0.join("code.rs"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        workspace.apply("code.rs", Some("new"), None).unwrap();
        assert_eq!(workspace.read("code.rs").unwrap(), None);
        assert_eq!(workspace.read("new/deep/file.rs").unwrap(), None);
        workspace
            .apply("new/deep/file.rs", None, Some("new file"))
            .unwrap();
        assert_eq!(
            workspace.read("new/deep/file.rs").unwrap().as_deref(),
            Some("new file")
        );
    }

    #[test]
    fn rejects_protected_paths_and_traversal() {
        let fixture = Fixture::new();
        let workspace = fixture.workspace();
        for file in [
            "",
            "../secret",
            "/etc/passwd",
            "a/../../b",
            ".git/config",
            "src/.GIT/config",
            ".moosedev/kg.nq",
            "target/build",
            "node_modules/package/index.js",
            ".env",
            ".env.local",
            ".codex/config.toml",
            ".ssh/id_ed25519",
            ".gnupg/private-keys-v1.d/key",
            ".aws/credentials",
            ".azure/accessTokens.json",
            ".kube/config",
            "a\\b",
            "nul\0",
        ] {
            assert!(workspace.read(file).is_err(), "read accepted {file:?}");
            assert!(
                workspace.apply(file, None, Some("bad")).is_err(),
                "write accepted {file:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_ancestors_and_hardlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret"), "untouched").unwrap();
        symlink(&outside.0, fixture.0.join("ancestor")).unwrap();
        symlink(outside.0.join("secret"), fixture.0.join("link")).unwrap();
        fs::hard_link(outside.0.join("secret"), fixture.0.join("hard")).unwrap();
        let workspace = fixture.workspace();
        for file in ["ancestor/secret", "ancestor/new", "link", "hard"] {
            assert!(workspace.read(file).is_err());
            assert!(workspace.apply(file, None, Some("changed")).is_err());
        }
        assert!(workspace.files().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(outside.0.join("secret")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn bounds_files_and_omits_binary_or_protected_entries() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("source"), "source").unwrap();
        fs::write(fixture.0.join("binary"), [0xff, 0xfe]).unwrap();
        File::create(fixture.0.join("large"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        fs::create_dir(fixture.0.join(".moosedev")).unwrap();
        fs::write(fixture.0.join(".moosedev/kg.nq"), "knowledge").unwrap();
        assert_eq!(fixture.workspace().files().unwrap(), vec!["source"]);
        assert!(fixture.workspace().read("large").is_err());
        assert!(fixture.workspace().read("binary").is_err());
    }

    #[tokio::test]
    async fn output_drains_after_reaching_limit() {
        let input = vec![b'x'; MAX_OUTPUT * 3];
        let output = bounded_output(input.as_slice(), None).await.unwrap();
        assert!(output.len() < MAX_OUTPUT + 100);
        assert!(output.ends_with(b"[output truncated]\n"));
    }

    #[test]
    fn runtime_allowlist_excludes_user_credentials_and_unrelated_system_files() {
        let mut denied = vec![
            PathBuf::from("/etc/hosts"),
            PathBuf::from("/private/etc/hosts"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            for suffix in [
                ".ssh/id_ed25519",
                ".cargo/credentials.toml",
                ".cargo/config.toml",
                ".aws/credentials",
            ] {
                denied.push(Path::new(&home).join(suffix));
            }
        }
        for denied in denied {
            assert!(
                !readable_directories()
                    .iter()
                    .any(|allowed| denied.starts_with(allowed)),
                "runtime directory grants credential read: {}",
                denied.display()
            );
            assert!(!readable_files().contains(&denied));
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_snapshot_omits_protected_paths_and_aliases_and_preserves_binary_fixtures() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let output = Fixture::new();
        fs::write(fixture.0.join("source.rs"), "source").unwrap();
        fs::write(fixture.0.join("binary.fixture"), [0xff, 0x00, 0xfe]).unwrap();
        fs::write(fixture.0.join(".env"), "fake project credential").unwrap();
        fs::create_dir(fixture.0.join(".ssh")).unwrap();
        fs::write(
            fixture.0.join(".ssh/id_ed25519"),
            "fake project private key",
        )
        .unwrap();
        fs::write(outside.0.join("credential"), "fake host credential").unwrap();
        symlink(&outside.0, fixture.0.join("linked-directory")).unwrap();
        symlink(outside.0.join("credential"), fixture.0.join("linked-file")).unwrap();
        fs::hard_link(
            outside.0.join("credential"),
            fixture.0.join("hardlinked-file"),
        )
        .unwrap();
        let destination = output.0.join("source");
        snapshot_source(&fixture.workspace(), &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("binary.fixture")).unwrap(),
            [0xff, 0x00, 0xfe]
        );
        assert_eq!(
            fs::read_to_string(destination.join("source.rs")).unwrap(),
            "source"
        );
        for name in [
            ".env",
            ".ssh",
            "linked-directory",
            "linked-file",
            "hardlinked-file",
        ] {
            assert!(!destination.join(name).exists(), "snapshot exposed {name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_skip_tagged_caches_but_preserve_nested_source_and_mtimes() {
        let fixture = Fixture::new();
        let output = Fixture::new();
        for name in ["build", "dist", "target"] {
            fs::create_dir_all(fixture.0.join("src").join(name)).unwrap();
            fs::write(fixture.0.join("src").join(name).join("code.rs"), "source").unwrap();
        }
        for path in ["clients/zed/target", "nested/cache-data"] {
            fs::create_dir_all(fixture.0.join(path)).unwrap();
            fs::write(
                fixture.0.join(path).join("CACHEDIR.TAG"),
                b"Signature: 8a477f597d28d172789f06886806bc55\n# Generated cache\n",
            )
            .unwrap();
            // Exclusion must happen before charging file sizes or copying data.
            File::create(fixture.0.join(path).join("huge.rlib"))
                .unwrap()
                .set_len(600 * 1024 * 1024)
                .unwrap();
            fs::write(fixture.0.join(path).join("generated.rs"), "cached").unwrap();
        }
        snapshot_source(&fixture.workspace(), &output.0.join("source")).unwrap();
        let files = fixture.workspace().files().unwrap();
        assert_eq!(files.len(), 3);
        for path in ["clients/zed/target", "nested/cache-data"] {
            assert!(!output.0.join("source").join(path).exists());
        }
        for name in ["build", "dist", "target"] {
            let path = Path::new("src").join(name).join("code.rs");
            assert!(files.contains(&path.to_string_lossy().into_owned()));
            assert_eq!(
                fixture
                    .workspace()
                    .read(path.to_str().unwrap())
                    .unwrap()
                    .as_deref(),
                Some("source")
            );
            assert_eq!(
                fs::metadata(fixture.0.join(&path))
                    .unwrap()
                    .modified()
                    .unwrap(),
                fs::metadata(output.0.join("source").join(path))
                    .unwrap()
                    .modified()
                    .unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn cache_markers_require_an_unaliased_regular_file_and_exact_signature() {
        use std::os::{unix::fs::symlink, unix::net::UnixListener};
        let fixture = Fixture::new();
        let directory = File::open(&fixture.0).unwrap();
        let marker = fixture.0.join("CACHEDIR.TAG");
        let signature = b"Signature: 8a477f597d28d172789f06886806bc55";
        assert!(!cache_directory(&directory));
        for invalid in [
            b"".as_slice(),
            &signature[..42],
            b"signature: 8a477f597d28d172789f06886806bc55",
            b" Signature: 8a477f597d28d172789f06886806bc55",
        ] {
            fs::write(&marker, invalid).unwrap();
            assert!(!cache_directory(&directory));
        }
        fs::write(&marker, signature).unwrap();
        assert!(cache_directory(&directory), "newline is not required");
        fs::remove_file(&marker).unwrap();

        let external = Fixture::new();
        fs::write(external.0.join("tag"), signature).unwrap();
        symlink(external.0.join("tag"), &marker).unwrap();
        assert!(!cache_directory(&directory));
        fs::remove_file(&marker).unwrap();
        fs::hard_link(external.0.join("tag"), &marker).unwrap();
        assert!(!cache_directory(&directory));
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(!cache_directory(&directory));
        fs::remove_dir(&marker).unwrap();
        let socket = UnixListener::bind(&marker).unwrap();
        assert!(!cache_directory(&directory));
        drop(socket);
        fs::remove_file(&marker).unwrap();
        let name = CString::new(marker.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(!cache_directory(&directory), "FIFO markers must not block");
        fs::remove_file(&marker).unwrap();

        // An invalid marker is ordinary source, not a reason to hide a directory.
        fs::create_dir(fixture.0.join("src")).unwrap();
        fs::write(fixture.0.join("src/CACHEDIR.TAG"), "not a cache").unwrap();
        fs::write(fixture.0.join("src/code.rs"), "source").unwrap();
        let output = Fixture::new();
        snapshot_source(&fixture.workspace(), &output.0.join("source")).unwrap();
        assert!(fixture
            .workspace()
            .files()
            .unwrap()
            .contains(&"src/code.rs".into()));
        assert!(output.0.join("source/src/code.rs").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "copies the current repository and requires functional OS confinement"]
    async fn populated_repository_command_excludes_nested_cargo_cache() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let scratch = Fixture::new();
        let result = command(root, &scratch.0,
            "test -f Cargo.toml && test -f src/harness/executor.rs && test -f clients/zed/Cargo.toml && test ! -e clients/zed/target").await.unwrap();
        assert!(result.success, "{}", result.output);
        let source = scratch.0.join("source");
        assert!(!source.join("clients/zed/target").exists());
        let mut directories = vec![source];
        let mut bytes = 0;
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    directories.push(entry.path());
                } else {
                    bytes += entry.metadata().unwrap().len();
                }
            }
        }
        eprintln!("Current repository source snapshot: {bytes} bytes; nested Cargo cache excluded");
        assert!(bytes <= 512 * 1024 * 1024);
        cleanup_task(&scratch.0).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn scratch_reuse_and_cleanup_never_follow_command_created_aliases() {
        use std::os::unix::fs::symlink;
        let scratch = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret"), "untouched").unwrap();
        let task = TaskScratch::open(&scratch.0).unwrap();
        fs::write(task.path.join("build/artifact"), "warm").unwrap();
        fs::hard_link(
            task.path.join("build/artifact"),
            task.path.join("build/artifact-link"),
        )
        .unwrap();
        symlink(&outside.0, task.path.join("build/directory-alias")).unwrap();
        fs::hard_link(outside.0.join("secret"), task.path.join("build/hard-alias")).unwrap();
        fs::create_dir(task.path.join("legacy-command")).unwrap();
        symlink(&outside.0, task.path.join("legacy-command/alias")).unwrap();
        drop(task);
        let task = TaskScratch::open(&scratch.0).unwrap();
        assert_eq!(
            fs::read_to_string(task.path.join("build/artifact")).unwrap(),
            "warm"
        );
        assert_eq!(
            fs::read_to_string(task.path.join("build/artifact-link")).unwrap(),
            "warm"
        );
        assert!(!task.path.join("build/directory-alias").exists());
        assert!(!task.path.join("build/hard-alias").exists());
        assert!(!task.path.join("legacy-command").exists());
        assert!(
            TaskScratch::open(&scratch.0).is_err(),
            "concurrent scratch use was allowed"
        );
        assert!(
            cleanup_task(&scratch.0).is_err(),
            "active scratch was removed"
        );
        drop(task);
        cleanup_task(&scratch.0).unwrap();
        assert_eq!(
            fs::read_to_string(outside.0.join("secret")).unwrap(),
            "untouched"
        );
        fs::create_dir(&scratch.0).unwrap();
        symlink(&outside.0, scratch.0.join("build")).unwrap();
        assert!(
            TaskScratch::open(&scratch.0).is_err(),
            "replaced build root was followed"
        );
        cleanup_task(&scratch.0).unwrap();
        assert_eq!(
            fs::read_to_string(outside.0.join("secret")).unwrap(),
            "untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scratch_repairs_owned_permissions_and_cleans_deep_trees_without_recursion() {
        use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
        let scratch = Fixture::new();
        let task = TaskScratch::open(&scratch.0).unwrap();
        fs::create_dir(task.path.join("build/locked")).unwrap();
        fs::write(task.path.join("build/locked/artifact"), "warm").unwrap();
        for path in ["build/locked", "build"] {
            fs::set_permissions(task.path.join(path), fs::Permissions::from_mode(0o0)).unwrap();
        }
        drop(task);
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o0)).unwrap();
        let task = TaskScratch::open(&scratch.0).unwrap();
        assert_eq!(
            fs::read_to_string(task.path.join("build/locked/artifact")).unwrap(),
            "warm"
        );
        let temporary = TemporaryDirectory::new(&task.path, "command").unwrap();
        let mut parent = File::open(&temporary.path).unwrap();
        let name = CString::new("d").unwrap();
        // Descriptor-relative construction reaches beyond ordinary PATH_MAX and
        // the old recursion guard without keeping one fd open per ancestor.
        for _ in 0..600 {
            cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) }).unwrap();
            let child = open_owned_directory(&parent, &name).unwrap();
            parent
                .set_permissions(fs::Permissions::from_mode(0o0))
                .unwrap();
            parent = child;
        }
        drop(parent);
        let temporary_path = temporary.path.clone();
        drop(temporary);
        // Drop has a deadline. Explicit cleanup retries must make progress even
        // when repairing an unusually deep tree takes more than one budget.
        for _ in 0..3 {
            if !temporary_path.exists() {
                break;
            }
            let name =
                CString::new(temporary_path.file_name().unwrap().as_encoded_bytes()).unwrap();
            let _ = remove_child_name(&task.directory, &name);
        }
        assert!(
            !temporary_path.exists(),
            "deep command scratch did not recover through bounded cleanup retries"
        );
        for path in ["build/locked", "build"] {
            fs::set_permissions(task.path.join(path), fs::Permissions::from_mode(0o0)).unwrap();
        }
        drop(task);
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o0)).unwrap();
        cleanup_task(&scratch.0).unwrap();
        assert!(!scratch.0.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
    async fn confinement_denies_host_and_project_secret_reads_including_aliases() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let scratch = Fixture::new();
        fs::create_dir_all(outside.0.join(".ssh")).unwrap();
        fs::create_dir_all(outside.0.join(".cargo")).unwrap();
        fs::write(
            outside.0.join(".ssh/id_ed25519"),
            "FAKE_HOST_SECRET_SENTINEL",
        )
        .unwrap();
        fs::write(
            outside.0.join(".cargo/credentials.toml"),
            "FAKE_CARGO_SECRET_SENTINEL",
        )
        .unwrap();
        fs::write(fixture.0.join(".env"), "FAKE_PROJECT_SECRET_SENTINEL").unwrap();
        fs::create_dir_all(fixture.0.join(".moosedev")).unwrap();
        fs::write(fixture.0.join(".moosedev/kg.nq"), "FAKE_GRAPH_SENTINEL").unwrap();
        fs::create_dir_all(fixture.0.join(".git")).unwrap();
        fs::write(fixture.0.join(".git/config"), "FAKE_GIT_SECRET_SENTINEL").unwrap();
        fs::write(fixture.0.join("source"), "public-source").unwrap();
        symlink(outside.0.join(".ssh/id_ed25519"), fixture.0.join("alias")).unwrap();
        fs::hard_link(
            outside.0.join(".ssh/id_ed25519"),
            fixture.0.join("hardalias"),
        )
        .unwrap();
        let outside = outside.0.canonicalize().unwrap();
        let original = fixture.0.canonicalize().unwrap();
        let command_text = format!(
            "set -e; cat source; \
            for denied in .env .moosedev/kg.nq .git/config alias hardalias /etc/hosts \
                '{outside}/.ssh/id_ed25519' '{outside}/.cargo/credentials.toml' '{original}/.env'; do \
                if cat \"$denied\"; then exit 21; fi; done; \
            ln -s '{outside}/.ssh/id_ed25519' \"$TMPDIR/alias\"; \
            if cat \"$TMPDIR/alias\"; then exit 22; fi; \
            if ln '{outside}/.ssh/id_ed25519' \"$TMPDIR/hardalias\"; then exit 23; fi; \
            printf success",
            outside = outside.display(), original = original.display(),
        );
        let result = command(&fixture.0, &scratch.0, &command_text)
            .await
            .unwrap();
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("public-source"), "{}", result.output);
        assert!(result.output.contains("success"), "{}", result.output);
        assert!(
            !result.output.contains("SENTINEL"),
            "secret escaped: {}",
            result.output
        );
    }

    /// Must run outside a parent sandbox which forbids installing OS sandboxes.
    #[tokio::test]
    #[ignore = "requires Rust toolchain and functional OS sandbox; run explicitly"]
    async fn confinement_can_run_rust_checks_without_writing_source() {
        let fixture = Fixture::new();
        let scratch = Fixture::new();
        fs::write(fixture.0.join("Cargo.toml"), "[package]\nname = \"harness-check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
        fs::write(
            fixture.0.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"harness-check\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            fixture.0.join("lib.rs"),
            "#[test]\nfn arithmetic() { assert_eq!(2 + 2, 4); }\n",
        )
        .unwrap();
        let before = fixture.workspace().files().unwrap();
        let result = command(&fixture.0, &scratch.0, "cargo test --offline --locked")
            .await
            .unwrap();
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("1 passed"), "{}", result.output);
        assert_eq!(fixture.workspace().files().unwrap(), before);
        assert!(!fixture.0.join("target").exists());
        let snapshot = scratch.0.join("source/lib.rs");
        let modified = fs::metadata(&snapshot).unwrap().modified().unwrap();
        fs::write(fixture.0.join("obsolete.txt"), "remove me").unwrap();
        let warm = command(
            &fixture.0,
            &scratch.0,
            "cargo test --offline --locked --verbose",
        )
        .await
        .unwrap();
        assert!(warm.success, "{}", warm.output);
        assert!(
            warm.output.contains("Fresh harness-check"),
            "cache was cold: {}",
            warm.output
        );
        assert_eq!(
            fs::metadata(&snapshot).unwrap().modified().unwrap(),
            modified
        );
        assert!(scratch.0.join("source/obsolete.txt").exists());
        fs::remove_file(fixture.0.join("obsolete.txt")).unwrap();
        fs::write(
            fixture.0.join("lib.rs"),
            "#[test] fn changed() { assert_eq!(2 + 2, 5); }\n",
        )
        .unwrap();
        let changed = command(&fixture.0, &scratch.0, "cargo test --offline --locked")
            .await
            .unwrap();
        assert!(
            !changed.success,
            "changed source was ignored: {}",
            changed.output
        );
        assert!(changed.output.contains("changed"), "{}", changed.output);
        assert!(!scratch.0.join("source/obsolete.txt").exists());
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            if cfg!(target_os = "macos") { 3 } else { 2 },
            "ephemeral command directories leaked"
        );
        cleanup_task(&scratch.0).unwrap();
        assert!(!scratch.0.exists());
    }

    /// Must run outside a parent sandbox which forbids installing OS sandboxes.
    #[tokio::test]
    #[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
    async fn confinement_blocks_source_graph_network_and_allows_scratch() {
        let fixture = Fixture::new();
        let scratch = Fixture::new();
        fs::write(fixture.0.join("source"), "unchanged").unwrap();
        fs::create_dir(fixture.0.join(".moosedev")).unwrap();
        fs::write(fixture.0.join(".moosedev/kg.nq"), "unchanged").unwrap();
        // The final status proves scratch writes worked, not just sandbox failure.
        let result = command(&fixture.0, &scratch.0,
            "test -z \"$SECRET_TOKEN\" && ! (echo bad > source) && ! (echo bad > .moosedev/kg.nq) && echo good > \"$TMPDIR/allowed\" && cat \"$TMPDIR/allowed\"").await.unwrap();
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("good"));
        assert_eq!(
            fs::read_to_string(fixture.0.join("source")).unwrap(),
            "unchanged"
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join(".moosedev/kg.nq")).unwrap(),
            "unchanged"
        );
        for escape in [
            "mv \"$CARGO_TARGET_DIR\" \"$CARGO_TARGET_DIR-moved\"",
            "mv \"$PWD\" \"$PWD-moved\"",
            "ln source \"$TMPDIR/alias\" && echo bad > \"$TMPDIR/alias\"",
            "ln -s \"$PWD/.moosedev/kg.nq\" \"$TMPDIR/alias\" && echo bad > \"$TMPDIR/alias\"",
        ] {
            let result = command(&fixture.0, &scratch.0, escape).await.unwrap();
            assert!(
                !result.success,
                "scratch alias bypassed confinement: {}",
                result.output
            );
        }
        assert_eq!(
            fs::read_to_string(fixture.0.join("source")).unwrap(),
            "unchanged"
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join(".moosedev/kg.nq")).unwrap(),
            "unchanged"
        );
        let curl = command(&fixture.0, &scratch.0, "command -v curl")
            .await
            .unwrap();
        assert!(curl.success, "integration test requires curl");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let network = command(
            &fixture.0,
            &scratch.0,
            &format!(
                "curl --max-time 2 http://{}",
                listener.local_addr().unwrap()
            ),
        )
        .await
        .unwrap();
        assert!(!network.success);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        #[cfg(unix)]
        {
            let socket_path = fixture.0.join("daemon.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let result = command(
                &fixture.0,
                &scratch.0,
                "curl --max-time 2 --unix-socket daemon.sock http://localhost",
            )
            .await
            .unwrap();
            assert!(!result.success);
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
    async fn timeout_kills_process_group() {
        let fixture = Fixture::new();
        let scratch = Fixture::new();
        let result = run_command(
            &fixture.0,
            &scratch.0,
            "sleep 30 & echo $! > \"$CARGO_TARGET_DIR/child\"; wait",
            Duration::from_millis(500),
            None,
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("timeout"));
        assert_child_terminated(&scratch).await;
    }

    #[cfg(unix)]
    async fn assert_child_terminated(scratch: &Fixture) {
        let pid: libc::pid_t = fs::read_to_string(scratch.0.join("build/child"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..40 {
            if unsafe { libc::kill(pid, 0) } < 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("command descendant {pid} survived cancellation");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
    async fn cancellation_kills_process_group() {
        let fixture = Fixture::new();
        let scratch = Fixture::new();
        let root = fixture.0.clone();
        let temporary = scratch.0.clone();
        let task = tokio::spawn(async move {
            command(
                &root,
                &temporary,
                "sleep 30 & echo $! > \"$CARGO_TARGET_DIR/child\"; wait",
            )
            .await
        });
        let mut started = false;
        for _ in 0..100 {
            started = scratch.0.join("build/child").is_file();
            if started {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        task.abort();
        let _ = task.await;
        assert!(started, "sandboxed command never started");
        assert_child_terminated(&scratch).await;
    }
}
