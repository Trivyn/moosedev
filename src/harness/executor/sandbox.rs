//! Trusted runtime inputs and platform command confinement policies.
use super::CommandPermissions;
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(super) fn trusted_path() -> Result<std::ffi::OsString> {
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

/// Platform runtime and installed tools only. In particular, no /, /Users,
/// /home, /private, ~/.cargo, ~/.ssh, or live project subtree is exposed.
pub(super) fn readable_directories() -> Vec<PathBuf> {
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

pub(super) fn readable_files() -> Vec<PathBuf> {
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
fn permitted_unix_sockets(permissions: &CommandPermissions) -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::FileTypeExt;

    let granted = || {
        permissions
            .read_paths
            .iter()
            .chain(&permissions.write_paths)
    };
    let mut sockets = Vec::new();
    for path in granted() {
        if super::is_socket(&std::fs::metadata(path)?) {
            sockets.push(path.clone());
        }
    }
    let mut pending = granted()
        .filter(|path| path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                let target = path.canonicalize()?;
                if std::fs::metadata(&target)?.file_type().is_socket() {
                    sockets.push(target);
                }
            } else if metadata.is_dir() {
                pending.push(path);
            } else if metadata.file_type().is_socket() {
                sockets.push(path.canonicalize()?);
            }
        }
    }
    sockets.sort();
    sockets.dedup();
    Ok(sockets)
}

#[cfg(target_os = "macos")]
pub(super) fn confined_command(
    source: &Path,
    scratch: &Path,
    temporary: &Path,
    command: &str,
    permissions: &CommandPermissions,
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
    for path in &permissions.read_paths {
        let selector = if path.is_dir() { "subpath" } else { "literal" };
        profile.push_str(&format!(
            "(allow file-read* ({selector} {}))",
            sandbox_literal(path)?
        ));
    }
    for path in &permissions.write_paths {
        let selector = if path.is_dir() { "subpath" } else { "literal" };
        profile.push_str(&format!(
            "(allow file-read* file-write* ({selector} {}))",
            sandbox_literal(path)?
        ));
    }
    if permissions.network {
        // Keep pathname-based Unix sockets behind filesystem-scoped rules;
        // a general network grant covers only TCP and UDP.
        profile.push_str("(allow network-outbound (remote tcp) (remote udp))(allow network-inbound (local tcp) (local udp))");
        // TCP alone reaches only IP literals over plaintext. Name resolution
        // goes through the system resolver's socket and TLS verification reads
        // the system CA bundle; both are fixed system paths, not user data.
        profile.push_str("(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))(allow file-read* (literal \"/private/etc/ssl/cert.pem\"))");
        for socket in permitted_unix_sockets(permissions)? {
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (path-literal {})))",
                sandbox_literal(&socket)?
            ));
        }
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
    profile.push_str(&format!("(deny file-write* (subpath {}))(deny file-write-unlink (literal {}) (literal {}) (literal {}))(allow file-write-data (literal \"/dev/null\"))", sandbox_literal(source)?, sandbox_literal(&scratch.join("build").canonicalize()?)?, sandbox_literal(temporary)?, sandbox_literal(&scratch.join("cargo-home").canonicalize()?)?));
    let mut process = tokio::process::Command::new("/usr/bin/sandbox-exec");
    process.args(["-p", &profile, "/bin/sh", "-c", command]);
    Ok(process)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn confined_command(
    source: &Path,
    scratch: &Path,
    temporary: &Path,
    command: &str,
    permissions: &CommandPermissions,
) -> Result<tokio::process::Command> {
    use std::os::fd::AsRawFd;
    let binary = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .context("bubblewrap is required for confined commands")?;
    let mut process = tokio::process::Command::new(binary);
    process.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--cap-drop",
        "ALL",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]);
    if permissions.network {
        process.arg("--share-net");
    }
    process.arg("--ro-bind").arg(scratch).arg(scratch);
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
    for path in &permissions.read_paths {
        process.arg("--ro-bind").arg(path).arg(path);
    }
    for path in &permissions.write_paths {
        process.arg("--bind").arg(path).arg(path);
    }
    process
        .arg("--ro-bind")
        .arg(source)
        .arg(source)
        .arg("--chdir")
        .arg(source)
        .args(["--seccomp", "198", "/bin/sh", "-c", command]);
    // Network namespaces do not block pathname-based Unix sockets. Deny socket
    // creation in seccomp as well, including foreign syscall architectures.
    // A network grant shares the host network namespace, where abstract Unix
    // sockets (X11, some session buses) live beside TCP; it therefore permits
    // only IP sockets plus the netlink queries name resolution makes.
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc00000b7;
    // Jump offsets count from the following instruction.
    let mut filter: Vec<(u16, u8, u8, u32)> = vec![
        (0x20, 0, 0, 4),
        (0x15, 1, 0, ARCH),
        (0x06, 0, 0, 0x80000000),
        (0x20, 0, 0, 0),
        (0x35, 0, 1, 0x40000000),
        (0x06, 0, 0, 0x80000000),
    ];
    if permissions.network {
        filter.extend([
            // Not socket(): allow. Otherwise load the address family, the low
            // word of the first argument on these little-endian targets.
            (0x15, 0, 5, libc::SYS_socket as u32),
            (0x20, 0, 0, 16),
            (0x15, 3, 0, libc::AF_INET as u32),
            (0x15, 2, 0, libc::AF_INET6 as u32),
            (0x15, 1, 0, libc::AF_NETLINK as u32),
        ]);
    } else {
        filter.push((0x15, 0, 1, libc::SYS_socket as u32));
    }
    filter.extend([
        (0x06, 0, 0, 0x00050000 | libc::EPERM as u32),
        (0x06, 0, 0, 0x7fff0000),
    ]);
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
pub(super) fn confined_command(
    _source: &Path,
    _scratch: &Path,
    _temporary: &Path,
    _command: &str,
    _permissions: &CommandPermissions,
) -> Result<tokio::process::Command> {
    anyhow::bail!("command confinement is unsupported on this platform")
}
