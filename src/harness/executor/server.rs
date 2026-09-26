//! A long-lived language server confined like a command, over a per-task
//! source mirror. The mirror lives beside, not inside, the command scratch:
//! every command clears its scratch of everything but the build cache and the
//! snapshot, which would pull the files out from under a running server.
use super::sandbox::{readable_directories, trusted_path};
use super::scratch::prepare_cargo_home;
use super::workspace::{snapshot_source, Workspace};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The directories a server uses: `source` (the mirror), `build`, `cargo-home`,
/// `home`, `tmp`, and its stderr log.
#[derive(Debug, Clone)]
pub struct ServerDirectory {
    pub root: PathBuf,
    /// The live project the mirror copies.
    pub project: PathBuf,
}

impl ServerDirectory {
    pub fn mirror(&self) -> PathBuf {
        self.root.join("source")
    }

    /// A fresh directory with a snapshot of `project`, discarding any earlier
    /// one, so a restarted server never sees a half-updated mirror.
    pub fn prepare(project: &Path, root: &Path) -> Result<Self> {
        let project = project.canonicalize()?;
        for allowed in readable_directories() {
            anyhow::ensure!(
                !project.starts_with(allowed.canonicalize()?),
                "language server project must be outside trusted system/toolchain directories"
            );
        }
        if root.exists() {
            fs::remove_dir_all(root).context("remove the previous language server directory")?;
        }
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        snapshot_source(&Workspace::new(&project)?, &root.join("source"))?;
        // Every lockfile root gets a lockfile the toolchain may fill; the
        // mirror is otherwise read-only to the server.
        super::carry_generated_lockfile(&root, &root.join("no-previous"), &root.join("source"))?;
        for directory in ["build", "home", "tmp"] {
            fs::create_dir(root.join(directory))?;
        }
        prepare_cargo_home(&root.join("cargo-home"))?;
        Ok(Self { root, project })
    }

    /// Mirror one applied edit: `after` is the file's new text, `None` a
    /// deletion. Paths go through the workspace's descriptor-safe writes.
    pub fn mirror_edit(&self, file: &str, after: Option<&str>) -> Result<()> {
        let mirror = Workspace::new(&self.mirror())?;
        let before = mirror.read(file)?;
        if before.as_deref() == after {
            return Ok(());
        }
        mirror.apply(file, before.as_deref(), after)
    }

    /// The confined server process: `argv` under the platform sandbox, working
    /// in the mirror, with piped stdio and stderr kept in `stderr.log`.
    /// `read_paths` are the standing sandbox read grants (for example a
    /// Node installation outside the system directories).
    pub fn command(&self, argv: &[String], read_paths: &[PathBuf]) -> Result<Command> {
        anyhow::ensure!(!argv.is_empty(), "language server command is empty");
        let mut process = confined(self, argv, read_paths)?;
        let stderr = fs::File::create(self.root.join("stderr.log"))?;
        process
            .current_dir(self.mirror())
            .env_clear()
            .env("PATH", trusted_path()?)
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("TEMP", self.root.join("tmp"))
            .env("CARGO_HOME", self.root.join("cargo-home"))
            .env("CARGO_TARGET_DIR", self.root.join("build"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANG", "en_US.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        if let Some(home) = std::env::var_os("HOME") {
            process.env("RUSTUP_HOME", Path::new(&home).join(".rustup"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }
        Ok(process)
    }
}

#[cfg(target_os = "macos")]
fn confined(
    directory: &ServerDirectory,
    argv: &[String],
    read_paths: &[PathBuf],
) -> Result<Command> {
    let writable: Vec<PathBuf> = ["build", "cargo-home", "home", "tmp"]
        .iter()
        .map(|name| directory.root.join(name))
        .collect();
    let profile = super::sandbox::server_profile(
        &directory.project,
        &directory.root,
        &directory.mirror(),
        &writable,
        read_paths,
    )?;
    let mut process = Command::new("/usr/bin/sandbox-exec");
    process.args(["-p", &profile]).args(argv);
    Ok(process)
}

#[cfg(not(target_os = "macos"))]
fn confined(
    _directory: &ServerDirectory,
    _argv: &[String],
    _read_paths: &[PathBuf],
) -> Result<Command> {
    anyhow::bail!("confined language servers are supported on macOS only so far")
}

/// Where a program named `name` resolves on the trusted command PATH.
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    let path = trusted_path().ok()?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}
