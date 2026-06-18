//! MOOSEDev — neurosymbolic MCP sidecar built on the MOOSE engine.
//!
//! Thin entry point. Picks a transport mode from argv, then hands off to
//! [`moosedev::runtime`]. **stdout is the JSON-RPC channel**, so all diagnostics
//! are routed to stderr. The server surface and tools live in the `moosedev`
//! library crate (see `src/lib.rs` and `spec/MOOSEDev_design.md`).
//!
//! Modes:
//! - (no args): serve MCP over stdio (default; single client owns this process).
//!   Unchanged, backward-compatible behavior.
//! - `--serve [SOCKET]`: run the shared backend — own the store, listen on a Unix
//!   socket, serve every connecting client. Run one per project so Claude Code +
//!   Codex can share one live graph concurrently (RocksDB is single-writer).
//! - `--connect [SOCKET]`: run as a thin proxy — relay this process's stdio to a
//!   running backend, auto-spawning a detached backend when none is listening.
//!   This is what each MCP client spawns.
//!
//! `SOCKET` is optional; it otherwise comes from `MOOSEDEV_SOCKET`, else is
//! derived per data dir. Configure with `MOOSEDEV_DATA_DIR` / `MOOSEDEV_ONTOLOGY_DIR`.
//! Set `MOOSEDEV_NO_AUTOSPAWN=1` to make `--connect` require a pre-running backend.

use std::path::{Path, PathBuf};

use moosedev::runtime;

/// Selected transport mode plus its optional explicit socket path.
enum Mode {
    Stdio,
    Serve(Option<PathBuf>),
    Connect(Option<PathBuf>),
}

const USAGE: &str = "\
moosedev — neurosymbolic MCP memory server

USAGE:
    moosedev                 Serve MCP over stdio (default, single client)
    moosedev --serve [SOCK]  Run the shared backend on a Unix socket
    moosedev --connect [SOCK] Proxy stdio to a backend; auto-spawn if needed
    moosedev --help          Show this help

SOCKET defaults to MOOSEDEV_SOCKET, else <MOOSEDEV_DATA_DIR>/moosedev.sock.
Environment: MOOSEDEV_DATA_DIR, MOOSEDEV_ONTOLOGY_DIR, MOOSEDEV_SOCKET,
MOOSEDEV_NO_AUTOSPAWN.";

/// Parse argv (excluding argv[0]) into a [`Mode`]. Modes are mutually exclusive;
/// each takes one optional positional socket path.
fn parse_mode(args: &[String]) -> anyhow::Result<Mode> {
    let mut iter = args.iter();
    match iter.next().map(String::as_str) {
        None => Ok(Mode::Stdio),
        Some("--serve") => Ok(Mode::Serve(iter.next().map(PathBuf::from))),
        Some("--connect") => Ok(Mode::Connect(iter.next().map(PathBuf::from))),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => anyhow::bail!(
            "unknown argument {other:?} — expected --serve, --connect, --help, or no arguments (stdio)"
        ),
    }
}

/// Resolve the socket path: explicit arg wins, then `MOOSEDEV_SOCKET`, then the
/// per-data-dir derivation (which `--serve` and `--connect` compute identically).
fn resolve_socket(explicit: Option<PathBuf>, data_dir: &Path) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("MOOSEDEV_SOCKET").map(PathBuf::from))
        .unwrap_or_else(|| runtime::socket_path_for(data_dir))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs MUST go to stderr — stdout carries the MCP JSON-RPC framing.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moosedev=info,rmcp=warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = parse_mode(&args)?;

    let data_dir =
        PathBuf::from(std::env::var("MOOSEDEV_DATA_DIR").unwrap_or_else(|_| "data".to_string()));

    match mode {
        // The proxy never opens the store (no RocksDB lock, no model load) — it
        // only needs the socket/data-dir rendezvous, so resolve it and relay.
        Mode::Connect(sock) => {
            // Match `--serve`: make the data dir exist before socket derivation
            // so canonicalization and the long-path fallback are stable from the
            // first auto-spawned run.
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
            let socket = resolve_socket(sock, &data_dir);
            runtime::connect_unix(&socket, &data_dir).await
        }
        Mode::Serve(sock) => {
            // Create the data dir up front so socket derivation canonicalizes the
            // same way `--connect` will (otherwise the length-guard fallback could
            // pick a different path on a first-ever serve). `bootstrap` also
            // creates it; doing it here too is harmless.
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
            let socket = resolve_socket(sock, &data_dir);
            // Probe before the (expensive) bootstrap: a same-data-dir conflict
            // would otherwise fail first with the raw RocksDB lock error after a
            // wasted model load. This gives a clear message and exits fast.
            runtime::ensure_no_live_backend(&socket).await?;
            let server = runtime::build_server(&data_dir, &ontology_dir()).await?;
            let pidfile = runtime::pidfile_path_for(&data_dir);
            std::fs::write(&pidfile, format!("{}\n", std::process::id()))
                .map_err(|e| anyhow::anyhow!("write pidfile {}: {e}", pidfile.display()))?;
            tracing::info!(
                "MOOSEDev backend startup: data_dir={}, socket={}, pidfile={}",
                data_dir.display(),
                socket.display(),
                pidfile.display()
            );
            let result = runtime::serve_unix(server, &socket).await;
            let _ = std::fs::remove_file(&pidfile);
            result
        }
        Mode::Stdio => {
            let server = runtime::build_server(&data_dir, &ontology_dir()).await?;
            runtime::serve_stdio(server).await
        }
    }
}

/// Where the shipped ontologies live. Defaults to the crate's `ontologies/` dir
/// (dev/`cargo run`); override with `MOOSEDEV_ONTOLOGY_DIR` for a deployed binary
/// that ships them elsewhere.
fn ontology_dir() -> PathBuf {
    match std::env::var("MOOSEDEV_ONTOLOGY_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
    }
}
