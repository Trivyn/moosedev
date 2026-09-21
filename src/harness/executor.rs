//! The runner's only filesystem mutation and command execution boundary.
mod command_line;
#[cfg(unix)]
mod fs_ops;
mod output;
mod sandbox;
mod scratch;
#[cfg(test)]
mod tests;
mod workspace;

use super::progress::ProgressSender;
use anyhow::{Context, Result};
pub use command_line::unrunnable_reason;
#[cfg(unix)]
use fs_ops::remove_child;
use output::bounded_output_into;
use sandbox::{confined_command, readable_directories, trusted_path};
pub use scratch::cleanup_task;
#[cfg(unix)]
use scratch::{prepare_cargo_home, FixedDirectoryCleanup, TaskScratch, TemporaryDirectory};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use workspace::snapshot_source;
pub use workspace::Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
    /// The shell's exit status; absent when a signal ended the command.
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// Narrow, user-approved additions to the baseline command sandbox.
///
/// Paths are canonical host paths. Existing files grant access to that exact
/// file; existing directories grant recursive access. Construct requests with
/// [`CommandPermissions::requested`] rather than trusting model-supplied paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPermissions {
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
    pub network: bool,
}

impl CommandPermissions {
    /// Canonicalize and validate a requested permission set for one task.
    ///
    /// Permissions may only name existing files or directories outside both
    /// the live workspace and task scratch. Rejecting ancestors as well as
    /// descendants prevents a broad grant from exposing either protected tree.
    pub fn requested(
        root: &Path,
        scratch: &Path,
        read_paths: &[String],
        write_paths: &[String],
        network: bool,
    ) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("canonicalize command workspace")?;
        anyhow::ensure!(root.is_dir(), "command workspace must be a directory");
        let scratch = canonical_or_absolute(scratch)?;
        let read_paths = validated_permission_paths(read_paths, &root, &scratch, "read")?;
        let write_paths = validated_permission_paths(write_paths, &root, &scratch, "write")?;
        Ok(Self {
            read_paths,
            write_paths,
            network,
        })
    }

    fn revalidate(&self, root: &Path, scratch: &Path) -> Result<Self> {
        let reads = self
            .read_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let writes = self
            .write_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        Self::requested(root, scratch, &reads, &writes, self.network)
    }
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    anyhow::ensure!(path.is_absolute(), "task scratch path must be absolute");
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut canonical) => {
                for child in missing.iter().rev() {
                    canonical.push(child);
                }
                return Ok(canonical);
            }
            Err(error) if cursor.symlink_metadata().is_ok() => {
                return Err(error)
                    .with_context(|| format!("canonicalize task scratch path {}", path.display()));
            }
            Err(_) => {
                let child = cursor.file_name().with_context(|| {
                    format!("find existing ancestor of task scratch {}", path.display())
                })?;
                missing.push(child.to_os_string());
                cursor = cursor.parent().with_context(|| {
                    format!("find existing ancestor of task scratch {}", path.display())
                })?;
            }
        }
    }
}

fn validated_permission_paths(
    requested: &[String],
    root: &Path,
    scratch: &Path,
    access: &str,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::with_capacity(requested.len());
    for raw in requested {
        let path = Path::new(raw);
        anyhow::ensure!(
            path.is_absolute(),
            "external {access} permission path must be absolute: {raw:?}"
        );
        let path = path
            .canonicalize()
            .with_context(|| format!("canonicalize external {access} path {raw:?}"))?;
        anyhow::ensure!(
            path.to_str().is_some(),
            "external {access} permission path must be UTF-8"
        );
        let metadata = path
            .metadata()
            .with_context(|| format!("inspect external {access} path {}", path.display()))?;
        anyhow::ensure!(
            metadata.is_file() || metadata.is_dir() || is_socket(&metadata),
            "external {access} path must be an existing file, directory, or Unix socket: {}",
            path.display()
        );
        anyhow::ensure!(
            path.parent().is_some(),
            "external {access} permission cannot grant the filesystem root"
        );
        for (name, protected) in [("workspace", root), ("task scratch", scratch)] {
            anyhow::ensure!(
                !paths_overlap(&path, protected),
                "external {access} permission overlaps the protected {name}: {}",
                path.display()
            );
        }
        validate_permission_tree(&path, access)?;
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    // A recursive directory grant subsumes paths beneath it. Compacting here
    // makes approval displays stable and avoids redundant sandbox rules.
    let mut compact: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if compact
            .iter()
            .any(|parent| parent.is_dir() && path.starts_with(parent))
        {
            continue;
        }
        compact.push(path);
    }
    Ok(compact)
}

/// A socket may be granted by its exact path, so reaching one service does
/// not require exposing the directory around it.
#[cfg(unix)]
pub(super) fn is_socket(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(not(unix))]
pub(super) fn is_socket(_metadata: &fs::Metadata) -> bool {
    false
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(unix)]
fn validate_permission_tree(scope: &Path, access: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    const MAX_PERMISSION_ENTRIES: usize = 100_000;
    let metadata = fs::metadata(scope)?;
    if !metadata.is_dir() {
        anyhow::ensure!(
            metadata.nlink() == 1,
            "external {access} permission rejects hardlinked file {}",
            scope.display()
        );
        return Ok(());
    }
    let mut pending = vec![scope.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).with_context(|| {
            format!(
                "inspect external {access} directory {}",
                directory.display()
            )
        })? {
            let entry = entry.with_context(|| {
                format!(
                    "inspect external {access} directory {}",
                    directory.display()
                )
            })?;
            entries += 1;
            anyhow::ensure!(
                entries <= MAX_PERMISSION_ENTRIES,
                "external {access} directory exceeds the {MAX_PERMISSION_ENTRIES} entry validation limit: {}",
                scope.display()
            );
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect external {access} entry {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                let target = path.canonicalize().with_context(|| {
                    format!("resolve external {access} symlink {}", path.display())
                })?;
                anyhow::ensure!(
                    target.starts_with(scope),
                    "external {access} directory contains an escaping symlink: {}",
                    path.display()
                );
            } else if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                anyhow::ensure!(
                    metadata.nlink() == 1,
                    "external {access} directory contains a hardlinked file: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_permission_tree(_scope: &Path, _access: &str) -> Result<()> {
    Ok(())
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
    command_with_permissions(
        root,
        scratch,
        command,
        &CommandPermissions::default(),
        progress,
    )
    .await
}

/// Execute with task-scoped, user-approved additions to the baseline sandbox.
/// The permissions are revalidated here so journal corruption or an unsafe
/// direct caller cannot turn persisted strings into ambient host access.
pub async fn command_with_permissions(
    root: &Path,
    scratch: &Path,
    command: &str,
    permissions: &CommandPermissions,
    progress: Option<ProgressSender>,
) -> Result<CommandResult> {
    let permissions = permissions.revalidate(root, scratch)?;
    run_command(
        root,
        scratch,
        command,
        command_timeout()?,
        &permissions,
        progress,
    )
    .await
}

#[cfg(unix)]
async fn run_command(
    root: &Path,
    scratch: &Path,
    command: &str,
    timeout: Duration,
    permissions: &CommandPermissions,
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
    let mut process = confined_command(&source, scratch, &temporary.path, command, permissions)?;
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
        .env(
            "CARGO_NET_OFFLINE",
            if permissions.network { "false" } else { "true" },
        )
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
        exit_code: status.code(),
    })
}

#[cfg(not(unix))]
async fn run_command(
    _root: &Path,
    _scratch: &Path,
    _command: &str,
    _timeout: Duration,
    _permissions: &CommandPermissions,
    _progress: Option<ProgressSender>,
) -> Result<CommandResult> {
    anyhow::bail!("command confinement is unsupported on this platform")
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
