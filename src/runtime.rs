//! Server runtime: build the shared server state and run it under one of the
//! transport modes (`stdio`, `--serve`, `--connect`).
//!
//! RocksDB (the durable store's backend) is single-writer: only one process may
//! hold the store open read-write. The per-client stdio model therefore can't
//! support two MCP clients at once — the second `Store::open` fails and the
//! client never completes its handshake. The fix is a **single backend** that
//! owns the store and serves many clients:
//!
//! - [`serve_unix`] — the backend: own the store, listen on a Unix socket, and
//!   serve the MCP protocol on every accepted connection over one shared
//!   [`AppState`] (writes are serialized by RocksDB transactions;
//!   `EntityIndexCache` is `Send + Sync`).
//! - [`connect_unix`] — a thin client: a transparent byte relay between this
//!   process's stdio and the backend's socket. No MCP parsing — both ends speak
//!   the same framing, so a bidirectional copy is sufficient and hard to get
//!   subtly wrong.
//!
//! The socket path is derived **per data dir** ([`socket_path_for`]), so each
//! project gets its own backend + store + socket with no cross-talk. `--connect`
//! auto-spawns a detached `--serve` backend when no daemon is listening, unless
//! `MOOSEDEV_NO_AUTOSPAWN` opts out.

use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::{transport::stdio, ServiceExt};
use tokio::net::{UnixListener, UnixStream};

use crate::graph::AppState;
use crate::mcp::MooseDevServer;

/// Filename of the per-project rendezvous socket, co-located in the data dir.
const SOCKET_FILE_NAME: &str = "moosedev.sock";

/// Filename for detached backend logs, co-located in the data dir.
const SERVE_LOG_FILE_NAME: &str = "moosedev-serve.log";

/// Filename for the detached backend pidfile, co-located in the data dir.
const PIDFILE_NAME: &str = "moosedev-serve.pid";

/// Conservative cap on a Unix-socket path length. `sockaddr_un.sun_path` is 104
/// bytes on macOS (108 on Linux) including the NUL terminator; stay well under
/// the smaller limit so a deeply nested data dir falls back to a hashed path
/// rather than failing to bind at runtime.
const MAX_SOCKET_PATH_LEN: usize = 100;

/// Bootstrap the durable state and build the MCP server — the shared setup every
/// transport mode runs. Opening the store here is what acquires the RocksDB
/// write lock, so exactly one process (the stdio server or the `--serve`
/// backend) does this per data dir; `--connect` clients never call it.
pub async fn build_server(data_dir: &Path, ontology_dir: &Path) -> anyhow::Result<MooseDevServer> {
    tracing::info!(
        "MOOSEDev: bootstrapping state (data dir: {})…",
        data_dir.display()
    );
    let mut state = AppState::bootstrap(data_dir, ontology_dir)?;
    // Build the alignment index (loads the embedding model). Non-fatal by design:
    // if the model can't load (e.g. offline with no bundled weights), the
    // alignment tools report it per call, but the rest of the server (capture,
    // query, context, provenance) must still start.
    tracing::info!("MOOSEDev: building ontology alignment index (embedding vectors)…");
    if let Err(e) = state.build_alignment_index().await {
        tracing::warn!(
            "alignment index unavailable — align_concepts/suggest_mappings disabled: {e}"
        );
    }
    Ok(MooseDevServer::new(Arc::new(state)))
}

/// Derive the rendezvous socket path for a data dir. Both `--serve` and
/// `--connect` compute this from the same input, so they always agree.
///
/// Preferred: `<data_dir>/moosedev.sock` (per-project by construction, removed
/// with the project). If that path is too long for the platform's socket-path
/// limit, fall back to a hashed name under the OS temp dir — still deterministic
/// from the (canonical) data dir, so both modes still agree.
pub fn socket_path_for(data_dir: &Path) -> PathBuf {
    // Canonicalize so the same data dir reached via different relative paths maps
    // to one socket. Falls back to the path as-given if it doesn't exist yet.
    let canonical = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let in_dir = canonical.join(SOCKET_FILE_NAME);
    if in_dir.as_os_str().len() <= MAX_SOCKET_PATH_LEN {
        return in_dir;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    std::env::temp_dir().join(format!("moosedev-{:016x}.sock", hasher.finish()))
}

/// Path to the per-data-dir detached backend log file.
pub fn serve_log_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(SERVE_LOG_FILE_NAME)
}

/// Path to the per-data-dir detached backend pidfile.
pub fn pidfile_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(PIDFILE_NAME)
}

/// Run the MCP server over stdio (the default mode; unchanged single-client
/// behavior). The calling client owns this process's lifetime.
pub async fn serve_stdio(server: MooseDevServer) -> anyhow::Result<()> {
    tracing::info!("MOOSEDev MCP server starting (stdio transport)…");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    tracing::info!("MOOSEDev MCP server shut down cleanly.");
    Ok(())
}

/// Refuse to start a second backend beside a live one: connecting succeeds only
/// if something is already accepting on this socket. Call this **before**
/// [`build_server`] — opening the store would otherwise fail first with the raw
/// RocksDB lock error (and waste a model load) for a same-data-dir conflict.
pub async fn ensure_no_live_backend(socket: &Path) -> anyhow::Result<()> {
    if UnixStream::connect(socket).await.is_ok() {
        anyhow::bail!(
            "a MOOSEDev backend is already listening on {} — refusing to start a second",
            socket.display()
        );
    }
    Ok(())
}

fn should_spawn(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::NotFound | ErrorKind::ConnectionRefused)
}

fn autospawn_disabled() -> bool {
    std::env::var_os("MOOSEDEV_NO_AUTOSPAWN")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

/// Start a detached backend using the exact socket path the proxy resolved.
/// Stdio is isolated from the proxy's JSON-RPC channel and appended to the
/// per-data-dir daemon log.
pub fn spawn_detached_backend(socket: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("resolve current exe: {e}"))?;
    std::fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;

    let log_path = serve_log_path_for(data_dir);
    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| anyhow::anyhow!("open backend log {}: {e}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| anyhow::anyhow!("clone backend log {}: {e}", log_path.display()))?;

    let child = std::process::Command::new(exe)
        .arg("--serve")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .process_group(0)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "spawn MOOSEDev backend for {} (log: {}): {e}",
                socket.display(),
                log_path.display()
            )
        })?;
    drop(child);
    Ok(())
}

/// Connect to a backend, auto-spawning a detached one when the rendezvous socket
/// is absent or stale. Permission and other hard errors are returned unchanged.
pub async fn connect_or_spawn(socket: &Path, data_dir: &Path) -> anyhow::Result<UnixStream> {
    match UnixStream::connect(socket).await {
        Ok(stream) => return Ok(stream),
        Err(e) if should_spawn(e.kind()) => {
            if autospawn_disabled() {
                anyhow::bail!(
                    "connect {}: {e} — no MOOSEDev backend is listening and MOOSEDEV_NO_AUTOSPAWN is set; start one with `moosedev --serve {}`",
                    socket.display(),
                    socket.display()
                );
            }
            tracing::info!(
                "MOOSEDev proxy: no backend listening on {}; auto-spawning detached backend",
                socket.display()
            );
            spawn_detached_backend(socket, data_dir)?;
        }
        Err(e) => return Err(anyhow::anyhow!("connect {}: {e}", socket.display())),
    }

    let log_path = serve_log_path_for(data_dir);
    let deadline = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match UnixStream::connect(socket).await {
                Ok(stream) => return Ok(stream),
                Err(e) if should_spawn(e.kind()) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(anyhow::anyhow!("connect {}: {e}", socket.display())),
            }
        }
    })
    .await;

    match deadline {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "timed out waiting for auto-spawned MOOSEDev backend on {} (see log: {})",
            socket.display(),
            log_path.display()
        ),
    }
}

/// Run the backend: own the store and serve MCP on every connection accepted on
/// the Unix socket. Each connection gets its own session over a clone of the
/// server that shares the single `Arc<AppState>`. Probe with
/// [`ensure_no_live_backend`] before building the server.
pub async fn serve_unix(server: MooseDevServer, socket: &Path) -> anyhow::Result<()> {
    // A leftover socket file from a crashed backend would make bind fail with
    // AddrInUse; nothing live owns it (caller probed), so clear it.
    if socket.exists() {
        std::fs::remove_file(socket)
            .map_err(|e| anyhow::anyhow!("remove stale socket {}: {e}", socket.display()))?;
    }

    let listener = UnixListener::bind(socket)
        .map_err(|e| anyhow::anyhow!("bind {}: {e}", socket.display()))?;
    tracing::info!(
        "MOOSEDev backend serving on {} (Unix socket) — connect clients with `--connect`.",
        socket.display()
    );
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| anyhow::anyhow!("install SIGTERM handler: {e}"))?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted
                    .map_err(|e| anyhow::anyhow!("accept on {}: {e}", socket.display()))?;
                let session = server.clone();
                tokio::spawn(async move {
                    match session.serve(stream).await {
                        Ok(running) => {
                            if let Err(e) = running.waiting().await {
                                tracing::warn!("client session ended with error: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("failed to serve connection: {e}"),
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown signal — removing socket {}", socket.display());
                let _ = std::fs::remove_file(socket);
                return Ok(());
            }
            _ = terminate.recv() => {
                tracing::info!("SIGTERM — removing socket {}", socket.display());
                let _ = std::fs::remove_file(socket);
                return Ok(());
            }
        }
    }
}

/// Run as a thin client (proxy): relay this process's stdio to/from the backend
/// socket. The spawning MCP client speaks to us over stdio exactly as it would
/// to a stdio server; we forward those bytes to the shared backend and stream
/// its replies back. No MCP awareness needed — both ends share the framing.
pub async fn connect_unix(socket: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let mut stream = connect_or_spawn(socket, data_dir).await?;
    tracing::info!("MOOSEDev proxy: relaying stdio ⇄ {}", socket.display());

    // `join` turns the separate stdin/stdout halves into one duplex so
    // `copy_bidirectional` can pump both directions, flush promptly, and
    // half-close correctly when either side hangs up.
    let mut client = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    tokio::io::copy_bidirectional(&mut client, &mut stream)
        .await
        .map_err(|e| anyhow::anyhow!("proxy relay failed: {e}"))?;
    tracing::info!("MOOSEDev proxy: connection closed.");
    Ok(())
}
