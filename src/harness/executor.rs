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
                if protected(&name.to_string_lossy()) {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
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

fn protected(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if (lower == ".env" || lower.starts_with(".env.")) && lower != ".env.example" {
        return true;
    }
    matches!(
        lower.as_str(),
        ".git"
            | ".moosedev"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
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
                anyhow::ensure!(!protected(name), "protected workspace path: {file}");
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
/// Only a fresh child of `scratch` is writable; host reads are allowlisted.
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
    run_command(root, scratch, command, Duration::from_secs(120), progress).await
}

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
    fs::create_dir_all(scratch)?;
    let scratch = scratch
        .canonicalize()?
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))?;
    }
    for directory in ["home", "tmp", "build"] {
        fs::create_dir(scratch.join(directory))?;
    }
    // Never grant reads to the live project: protected files and filesystem
    // aliases must not become readable through a shell action. Copy through the
    // same descriptor-relative boundary used by the direct file actions.
    let source = scratch.join("source");
    snapshot_source(&Workspace::new(&root)?, &source)?;
    let cargo_home = scratch.join("home/.cargo");
    prepare_cargo_home(&cargo_home)?;
    let mut process = confined_command(&source, &scratch, command)?;
    process
        .current_dir(&source)
        .env_clear()
        .env("PATH", trusted_path())
        .env("HOME", scratch.join("home"))
        .env("TMPDIR", scratch.join("tmp"))
        .env("TMP", scratch.join("tmp"))
        .env("TEMP", scratch.join("tmp"))
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
    let execution = async {
        let wait = async {
            let result = child.child.wait().await;
            child.kill_group(); // Close pipes held by background descendants, too.
            result
        };
        let (status, stdout, stderr) = tokio::join!(
            wait,
            bounded_output(stdout, progress.clone()),
            bounded_output(stderr, progress),
        );
        let mut output = String::from_utf8_lossy(&stdout?).into_owned();
        let errors = stderr?;
        if !errors.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&String::from_utf8_lossy(&errors));
        }
        let status = status?;
        if !status.success() && output.is_empty() {
            output = format!("command failed: {status}");
        }
        Ok::<_, anyhow::Error>(CommandResult {
            success: status.success(),
            output,
        })
    };
    match tokio::time::timeout(timeout, execution).await {
        Ok(result) => result,
        Err(_) => {
            child.kill_group();
            bail!("command exceeded {} second timeout", timeout.as_secs())
        }
    }
}

fn trusted_path() -> String {
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
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

async fn bounded_output(
    mut input: impl AsyncRead + Unpin,
    progress: Option<ProgressSender>,
) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
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
    Ok(result)
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
    group: u32,
}
impl ChildGroup {
    fn new(child: tokio::process::Child) -> Self {
        let group = child.id().unwrap();
        Self { child, group }
    }
    fn kill_group(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.group as libc::pid_t), libc::SIGKILL);
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
            if protected(&name.to_string_lossy()) {
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
                let _directory = open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                fs::create_dir_all(destination.join(&relative))?;
                pending.push((relative, depth + 1));
            } else if kind.is_file() {
                let (parent, name) = workspace.parent(file, false)?;
                let input = open_at(&parent, &name, libc::O_RDONLY, 0)?;
                let metadata = input.metadata()?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    continue;
                }
                const FILE_LIMIT: u64 = 64 * 1024 * 1024;
                const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;
                anyhow::ensure!(
                    metadata.len() <= FILE_LIMIT,
                    "command source file exceeds 64 MiB: {file}"
                );
                let target = destination.join(&relative);
                fs::create_dir_all(target.parent().unwrap())?;
                let mut output = File::create(target)?;
                let copied = std::io::copy(&mut input.take(FILE_LIMIT + 1), &mut output)?;
                total_bytes += copied;
                anyhow::ensure!(
                    copied <= FILE_LIMIT && total_bytes <= TOTAL_LIMIT,
                    "command source snapshot exceeds its size budget"
                );
                output.set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
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
fn confined_command(root: &Path, scratch: &Path, command: &str) -> Result<tokio::process::Command> {
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
    profile.push_str(&format!("(allow file-write* (subpath {}))(deny file-write* (subpath {}))(allow file-write-data (literal \"/dev/null\"))", sandbox_literal(scratch)?, sandbox_literal(root)?));
    let mut process = tokio::process::Command::new("/usr/bin/sandbox-exec");
    process.args(["-p", &profile, "/bin/sh", "-c", command]);
    Ok(process)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn confined_command(root: &Path, scratch: &Path, command: &str) -> Result<tokio::process::Command> {
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
            "--bind",
        ])
        .arg(scratch)
        .arg(scratch);
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
    let filter_path = scratch.join("seccomp.bpf");
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
    _command: &str,
) -> Result<tokio::process::Command> {
    bail!("command confinement is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "sleep 30 & echo $! > \"$TMPDIR/child\"; wait",
            Duration::from_millis(500),
            None,
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("timeout"));
        assert_child_terminated(&scratch).await;
    }

    #[cfg(unix)]
    async fn assert_child_terminated(scratch: &Fixture) {
        let run = fs::read_dir(&scratch.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let pid: libc::pid_t = fs::read_to_string(run.join("tmp/child"))
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
                "sleep 30 & echo $! > \"$TMPDIR/child\"; wait",
            )
            .await
        });
        let mut started = false;
        for _ in 0..100 {
            started = fs::read_dir(&scratch.0)
                .unwrap()
                .flatten()
                .any(|entry| entry.path().join("tmp/child").is_file());
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
