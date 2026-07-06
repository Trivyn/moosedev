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
//! - `--serve [SOCKET] [--open]`: run the shared backend — own the store, listen
//!   on a Unix socket, serve every connecting client. Run one per project so
//!   Claude Code + Codex can share one live graph concurrently (RocksDB is
//!   single-writer). `--open` launches the web UI in a browser once it is up.
//! - `--connect [SOCKET]`: run as a thin proxy — relay this process's stdio to a
//!   running backend, auto-spawning a detached backend when none is listening.
//!   This is what each MCP client spawns.
//! - `--status [SOCKET]`: report backend + web UI status without opening the
//!   store (socket-only liveness probe).
//! - `ui [SOCKET]`: open the backend's web UI in a browser, auto-spawning a
//!   backend if none is live.
//!
//! `SOCKET` is optional; it otherwise comes from `MOOSEDEV_SOCKET`, else is
//! derived per data dir. Configure with `.env` in the repo root, or environment
//! variables such as `MOOSEDEV_DATA_DIR` / `MOOSEDEV_ONTOLOGY_DIR`. Set
//! `MOOSEDEV_NO_AUTOSPAWN=1` to make `--connect` require a pre-running backend.

use std::io::Write;
use std::path::{Path, PathBuf};

use moosedev::export::{export_graph, ExportFormat, ExportScope};
use moosedev::graph;
use moosedev::graph_import::{import_graph as import_rdf_graph, ImportFormat, ImportMode};
use moosedev::init;
use moosedev::runtime;

/// Selected transport mode plus its optional explicit socket path.
enum Mode {
    Stdio,
    Serve {
        socket: Option<PathBuf>,
        open: bool,
    },
    Connect(Option<PathBuf>),
    /// Report backend + web UI status without opening the store (socket-only).
    Status(Option<PathBuf>),
    /// Open the running backend's web UI in a browser (auto-spawning if needed).
    Ui(Option<PathBuf>),
    Export(ExportArgs),
    Import(ImportArgs),
    /// Configure a project to use MOOSEDev as memory (`.mcp.json`, `.gitignore`,
    /// `CLAUDE.md`); no store, no backend.
    Init(InitArgs),
}

struct ExportArgs {
    path: Option<PathBuf>,
    format: ExportFormat,
    scope: ExportScope,
}

struct ImportArgs {
    path: PathBuf,
    format: ImportFormat,
    scope: ExportScope,
    mode: ImportMode,
}

struct InitArgs {
    target_dir: Option<PathBuf>,
    force: bool,
    codex: bool,
    opencode: bool,
    stdio: bool,
    binary: Option<PathBuf>,
    data_dir: Option<String>,
}

const USAGE: &str = "\
moosedev — neurosymbolic MCP memory server

USAGE:
    moosedev                  Serve MCP over stdio (default, single client)
    moosedev --serve [SOCK] [--open]
                              Run the shared backend on a Unix socket; --open
                              launches the web UI in a browser once it is up
    moosedev --connect [SOCK] Proxy stdio to a backend; auto-spawn if needed
    moosedev --status [SOCK]  Report backend + web UI status (no store lock)
    moosedev ui [SOCK]        Open the backend's web UI in a browser (auto-spawn)
    moosedev export [PATH]    Export the graph; no running backend required
    moosedev import PATH      Import RDF into the graph; no running backend required
    moosedev init [DIR]       Configure DIR (default .) to use MOOSEDev as memory
    moosedev --help           Show this help

SOCKET defaults to MOOSEDEV_SOCKET, else <MOOSEDEV_DATA_DIR>/moosedev.sock.
The web UI binds an ephemeral loopback port by default (discoverable via
--status / ui); set MOOSEDEV_HTTP_ADDR for a stable port or network exposure,
or MOOSEDEV_NO_HTTP=1 to disable it.
Configuration: repo-root .env plus environment variables. Explicit environment
values win. Keys: MOOSEDEV_DATA_DIR, MOOSEDEV_ONTOLOGY_DIR, MOOSEDEV_SOCKET,
MOOSEDEV_HTTP_ADDR, MOOSEDEV_NO_HTTP, MOOSEDEV_NO_AUTOSPAWN.
LLM assistance is disabled unless MOOSEDEV_LLM_BASE_URL is explicitly set;
then MOOSEDEV_LLM_API_KEY, MOOSEDEV_LLM_MODEL, and MOOSEDEV_LLM_ASSIST_LEVEL
configure the provider and assist level.

EXPORT OPTIONS:
    --format nq|nt|ttl        Output format (default: nq)
    --graph project|provenance|all
                              Named graph scope (default: project)

IMPORT OPTIONS:
    --format ttl|nt|nq        Input format (default: ttl)
    --graph project|provenance|all
                              Target graph/scope (default: project)
    --mode patch|replace      Patch inserts missing quads; replace fully restores
                              the selected scope (default: patch)

INIT OPTIONS:
    --stdio                   Generate a bare-stdio MCP config (single client)
                              instead of the default shared --connect config
    --codex                   Also write .codex/config.toml for the Codex CLI
    --opencode                Also install the opencode push plugin (.opencode/plugins/)
    --binary PATH             Force this binary path in the config instead of
                              the auto-resolved command (bare `moosedev` on PATH,
                              else this executable's absolute path)
    --data-dir DIR            Data dir written into the config (default .moosedev)
    --force                   Overwrite existing files / server entries

N-Quads is the canonical version-control format. N-Triples is deterministic
after graph names are dropped. Turtle is human-readable, not byte-canonical.";

/// Parse argv (excluding argv[0]) into a [`Mode`]. Modes are mutually exclusive;
/// each takes one optional positional socket path.
fn parse_mode(args: &[String]) -> anyhow::Result<Mode> {
    let mut iter = args.iter();
    match iter.next().map(String::as_str) {
        None => Ok(Mode::Stdio),
        Some("--serve") => parse_serve(iter),
        Some("--connect") => Ok(Mode::Connect(parse_optional_path(&mut iter, "--connect")?)),
        Some("--status") => Ok(Mode::Status(parse_optional_path(&mut iter, "--status")?)),
        Some("ui") => Ok(Mode::Ui(parse_optional_path(&mut iter, "ui")?)),
        Some("export") => parse_export(iter).map(Mode::Export),
        Some("import") => parse_import(iter).map(Mode::Import),
        Some("init") => parse_init(iter).map(Mode::Init),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => anyhow::bail!(
            "unknown argument {other:?} — expected export, import, init, --serve, --connect, --status, ui, --help, or no arguments (stdio)"
        ),
    }
}

/// Parse `--serve`'s arguments: an optional socket path and an optional `--open`
/// flag, in either order.
fn parse_serve<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<Mode> {
    let mut socket = None;
    let mut open = false;
    for arg in iter {
        if arg == "--open" {
            open = true;
        } else if arg.starts_with('-') {
            anyhow::bail!(
                "unknown --serve option {arg:?}; expected an optional socket path and/or --open"
            );
        } else if socket.is_none() {
            socket = Some(PathBuf::from(arg.as_str()));
        } else {
            anyhow::bail!("--serve accepts at most one socket path; unexpected {arg:?}");
        }
    }
    Ok(Mode::Serve { socket, open })
}

fn parse_optional_path<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
    mode: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let path = iter.next().map(|value| PathBuf::from(value.as_str()));
    if let Some(extra) = iter.next() {
        anyhow::bail!("{mode} accepts at most one optional socket path; unexpected {extra:?}");
    }
    Ok(path)
}

fn parse_export<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<ExportArgs> {
    let mut path = None;
    let mut format = ExportFormat::default();
    let mut scope = ExportScope::default();
    let mut args = iter.peekable();

    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--format=") {
            format = ExportFormat::parse(value)?;
        } else if arg == "--format" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--format requires nq, nt, or ttl"))?;
            format = ExportFormat::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--graph=") {
            scope = ExportScope::parse(value)?;
        } else if arg == "--graph" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--graph requires project, provenance, or all"))?;
            scope = ExportScope::parse(value)?;
        } else if arg.starts_with('-') {
            anyhow::bail!("unknown export option {arg:?}");
        } else if path.is_none() {
            path = Some(PathBuf::from(arg.as_str()));
        } else {
            anyhow::bail!(
                "export accepts at most one output path; unexpected extra argument {arg:?}"
            );
        }
    }

    Ok(ExportArgs {
        path,
        format,
        scope,
    })
}

fn parse_import<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<ImportArgs> {
    let mut path = None;
    let mut format = ImportFormat::default();
    let mut scope = ExportScope::default();
    let mut mode = ImportMode::default();
    let mut args = iter.peekable();

    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--format=") {
            format = ImportFormat::parse(value)?;
        } else if arg == "--format" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--format requires ttl, nt, or nq"))?;
            format = ImportFormat::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--graph=") {
            scope = ExportScope::parse(value)?;
        } else if arg == "--graph" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--graph requires project, provenance, or all"))?;
            scope = ExportScope::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--mode=") {
            mode = ImportMode::parse(value)?;
        } else if arg == "--mode" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--mode requires patch or replace"))?;
            mode = ImportMode::parse(value)?;
        } else if arg.starts_with('-') {
            anyhow::bail!("unknown import option {arg:?}");
        } else if path.is_none() {
            path = Some(PathBuf::from(arg.as_str()));
        } else {
            anyhow::bail!(
                "import accepts exactly one input path; unexpected extra argument {arg:?}"
            );
        }
    }

    let path = path.ok_or_else(|| anyhow::anyhow!("import requires an input path"))?;
    Ok(ImportArgs {
        path,
        format,
        scope,
        mode,
    })
}

/// Parse `init`'s arguments: an optional target dir plus flags. Mirrors the
/// long/`=` option style of [`parse_export`]/[`parse_import`].
fn parse_init<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<InitArgs> {
    let mut target_dir = None;
    let mut force = false;
    let mut codex = false;
    let mut opencode = false;
    let mut stdio = false;
    let mut binary = None;
    let mut data_dir = None;
    let mut args = iter.peekable();

    while let Some(arg) = args.next() {
        if arg == "--force" {
            force = true;
        } else if arg == "--codex" {
            codex = true;
        } else if arg == "--opencode" {
            opencode = true;
        } else if arg == "--stdio" {
            stdio = true;
        } else if let Some(value) = arg.strip_prefix("--binary=") {
            binary = Some(PathBuf::from(value));
        } else if arg == "--binary" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--binary requires a path"))?;
            binary = Some(PathBuf::from(value.as_str()));
        } else if let Some(value) = arg.strip_prefix("--data-dir=") {
            data_dir = Some(value.to_string());
        } else if arg == "--data-dir" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--data-dir requires a value"))?;
            data_dir = Some(value.clone());
        } else if arg.starts_with('-') {
            anyhow::bail!("unknown init option {arg:?}");
        } else if target_dir.is_none() {
            target_dir = Some(PathBuf::from(arg.as_str()));
        } else {
            anyhow::bail!("init accepts at most one target directory; unexpected {arg:?}");
        }
    }

    Ok(InitArgs {
        target_dir,
        force,
        codex,
        opencode,
        stdio,
        binary,
        data_dir,
    })
}

/// Resolve the socket path: explicit arg wins, then `MOOSEDEV_SOCKET`, then the
/// per-data-dir derivation (which `--serve` and `--connect` compute identically).
fn resolve_socket(explicit: Option<PathBuf>, data_dir: &Path) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("MOOSEDEV_SOCKET").map(PathBuf::from))
        .unwrap_or_else(|| runtime::socket_path_for(data_dir))
}

/// Make the data dir exist, then derive the rendezvous socket. The directory must
/// exist *before* socket derivation so canonicalization (and the long-path
/// fallback) resolves identically across `--serve`/`--connect`/`--status`/`ui`;
/// otherwise a first-ever run could pick a different socket path than later runs.
fn prepare_socket(explicit: Option<PathBuf>, data_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
    Ok(resolve_socket(explicit, data_dir))
}

fn load_dotenv_file(path: &Path) -> anyhow::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    dotenvy::from_path(path)
        .map(|_| true)
        .map_err(|e| anyhow::anyhow!("load dotenv {}: {e}", path.display()))
}

fn repo_dotenv_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join(".env")];

    for start in [std::env::current_exe().ok(), std::env::current_dir().ok()]
        .into_iter()
        .flatten()
    {
        for ancestor in start.ancestors() {
            if ancestor.join("Cargo.toml").is_file() {
                let candidate = ancestor.join(".env");
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
                break;
            }
        }
    }

    candidates
}

fn load_repo_dotenv() -> anyhow::Result<()> {
    for candidate in repo_dotenv_candidates() {
        if load_dotenv_file(&candidate)? {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_repo_dotenv()?;

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

    // Runtime data lives in a per-repo, gitignored `.moosedev/` dir by convention
    // (mirrored by .mcp.json, .codex/config.toml, start-moosedev.sh, and the README).
    // The default must agree so a bare `--serve` in any repo honors the convention
    // instead of spawning a stray `data/`.
    let data_dir = PathBuf::from(
        std::env::var("MOOSEDEV_DATA_DIR").unwrap_or_else(|_| ".moosedev".to_string()),
    );

    match mode {
        // The proxy never opens the store (no RocksDB lock, no model load) — it
        // only needs the socket/data-dir rendezvous, so resolve it and relay.
        Mode::Connect(sock) => {
            let socket = prepare_socket(sock, &data_dir)?;
            runtime::connect_unix(&socket, &data_dir).await
        }
        Mode::Serve { socket: sock, open } => {
            let socket = prepare_socket(sock, &data_dir)?;
            // Probe before the (expensive) bootstrap: a same-data-dir conflict
            // would otherwise fail first with the raw RocksDB lock error after a
            // wasted model load. This gives a clear message and exits fast.
            runtime::ensure_no_live_backend(&socket).await?;
            let state = runtime::build_state(&data_dir, &ontology_dir()).await?;
            let server = moosedev::mcp::MooseDevServer::new(state.clone());
            // Infallible: a UI bind failure must not abort the MCP backend. The
            // bound address (None if the UI is disabled/failed) drives --open.
            let http_addr = runtime::spawn_http_if_enabled(state, &data_dir).await;
            let pidfile = runtime::pidfile_path_for(&data_dir);
            std::fs::write(&pidfile, format!("{}\n", std::process::id()))
                .map_err(|e| anyhow::anyhow!("write pidfile {}: {e}", pidfile.display()))?;
            if open {
                match http_addr {
                    Some(addr) => open_browser(&format!("http://{addr}")),
                    None => tracing::warn!(
                        "--open requested but the web UI is not running (disabled or failed to bind)"
                    ),
                }
            }
            tracing::info!(
                "MOOSEDev backend startup: data_dir={}, socket={}, pidfile={}",
                data_dir.display(),
                socket.display(),
                pidfile.display()
            );
            let result = runtime::serve_unix(server, &socket).await;
            let _ = std::fs::remove_file(&pidfile);
            let _ = std::fs::remove_file(runtime::http_addr_file_path_for(&data_dir));
            result
        }
        // --status / ui are socket-only: they connect (or auto-spawn) but never
        // open RocksDB, so they cannot deadlock against a running backend.
        Mode::Status(sock) => {
            let socket = prepare_socket(sock, &data_dir)?;
            status_mode(&data_dir, &socket).await
        }
        Mode::Ui(sock) => {
            let socket = prepare_socket(sock, &data_dir)?;
            ui_mode(&data_dir, &socket).await
        }
        Mode::Export(args) => export_mode(&data_dir, args),
        Mode::Import(args) => import_mode(&data_dir, args),
        Mode::Init(args) => init_mode(args),
        Mode::Stdio => {
            let server = runtime::build_server(&data_dir, &ontology_dir()).await?;
            runtime::serve_stdio(server).await
        }
    }
}

fn export_mode(data_dir: &Path, args: ExportArgs) -> anyhow::Result<()> {
    let store_dir = data_dir.join("kg");
    if !store_dir.exists() {
        anyhow::bail!(
            "no MOOSEDev graph store found at {}; start/capture project memory before exporting",
            store_dir.display()
        );
    }
    let store = graph::open_store(data_dir).map_err(|e| {
        anyhow::anyhow!(
            "{e}\nHint: if a MOOSEDev backend is running for this data dir, use the MCP export_graph tool or GET /api/v1/graph/export instead."
        )
    })?;
    let dump = export_graph(&store, args.scope, args.format)?;

    if let Some(path) = args.path {
        std::fs::write(&path, dump.text)
            .map_err(|e| anyhow::anyhow!("write export {}: {e}", path.display()))?;
        eprintln!(
            "exported {} quads from {} to {}",
            dump.quad_count,
            dump.graphs.join(", "),
            path.display()
        );
    } else {
        std::io::stdout()
            .write_all(dump.text.as_bytes())
            .map_err(|e| anyhow::anyhow!("write export to stdout: {e}"))?;
        eprintln!(
            "exported {} quads from {}",
            dump.quad_count,
            dump.graphs.join(", ")
        );
    }

    Ok(())
}

fn import_mode(data_dir: &Path, args: ImportArgs) -> anyhow::Result<()> {
    let store_dir = data_dir.join("kg");
    if !store_dir.exists() {
        anyhow::bail!(
            "no MOOSEDev graph store found at {}; start/capture project memory before importing",
            store_dir.display()
        );
    }
    let text = std::fs::read_to_string(&args.path)
        .map_err(|e| anyhow::anyhow!("read import {}: {e}", args.path.display()))?;
    let store = graph::open_store(data_dir).map_err(|e| {
        anyhow::anyhow!(
            "{e}\nHint: if a MOOSEDev backend is running for this data dir, use the web UI or POST /api/v1/graph/import instead."
        )
    })?;
    let outcome = import_rdf_graph(&store, args.scope, args.format, args.mode, &text)?;
    if outcome.project_changed() {
        // Keep the committed canonical text in step with the store, like every
        // project-graph mutation (Requirement d459cac2).
        moosedev::canonical::write_through(&store, data_dir)?;
    }
    println!(
        "imported {} quad(s), skipped {} existing, removed {} from {}",
        outcome.inserted_quad_count,
        outcome.skipped_existing_count,
        outcome.removed_quad_count,
        outcome.graphs.join(", ")
    );
    Ok(())
}

/// Report whether a backend is running for this data dir and where its web UI is
/// listening — without opening the store. Liveness comes from a socket connect
/// (never RocksDB); the web UI address is read from the published `http.addr`,
/// trusted only while the backend is live so a stale file from a crash is ignored.
async fn status_mode(data_dir: &Path, socket: &Path) -> anyhow::Result<()> {
    let live = runtime::backend_is_live(socket).await;
    println!("socket:  {}", socket.display());
    if !live {
        println!("backend: not running");
        println!("web UI:  not running");
        return Ok(());
    }

    let pidfile = runtime::pidfile_path_for(data_dir);
    let pid = std::fs::read_to_string(&pidfile)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok());
    match pid {
        Some(pid) => println!("backend: running (pid {pid})"),
        None => println!("backend: running"),
    }

    match runtime::read_http_addr(data_dir) {
        Some(addr) => println!("web UI:  http://{addr}"),
        None => println!("web UI:  not running (disabled or failed to bind)"),
    }
    Ok(())
}

/// Open the backend's web UI in a browser, auto-spawning a backend if none is
/// live (honoring `MOOSEDEV_NO_AUTOSPAWN`). Socket-only: it never opens the store.
async fn ui_mode(data_dir: &Path, socket: &Path) -> anyhow::Result<()> {
    // Guarantee a live backend, auto-spawning one if needed (honoring
    // MOOSEDEV_NO_AUTOSPAWN). `connect_or_spawn` returns immediately when a
    // backend is already live; we need only the liveness, so the returned relay
    // connection is dropped at the end of this statement. `spawn_http_if_enabled`
    // writes `http.addr` before `serve_unix` binds the socket, so once the socket
    // is connectable the address file is already present.
    runtime::connect_or_spawn(socket, data_dir).await?;

    let Some(addr) = runtime::read_http_addr(data_dir) else {
        anyhow::bail!(
            "the MOOSEDev web UI is not available (disabled via MOOSEDEV_NO_HTTP or failed to bind); see {}",
            runtime::serve_log_path_for(data_dir).display()
        );
    };
    let url = format!("http://{addr}");
    println!("opening MOOSEDev web UI at {url}");
    open_browser(&url);
    Ok(())
}

/// Best-effort, dependency-free browser launch. Never fatal: on a headless box
/// (no opener binary, no display) it logs and returns so callers don't crash.
fn open_browser(url: &str) {
    let spawned = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).spawn()
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(url).spawn()
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            tracing::warn!("cannot open a browser on this platform; open {url} manually");
            return;
        }
    };
    if let Err(e) = spawned {
        tracing::warn!("could not open a browser for {url}: {e}; open it manually");
    }
}

/// Where the shipped ontologies live, resolved in priority order:
///   1. `MOOSEDEV_ONTOLOGY_DIR` — explicit override (deployments that ship them elsewhere).
///   2. `ontologies/` next to the running binary — the released tarball layout, so a
///      downloaded build works with zero configuration.
///   3. The crate's own `ontologies/` dir — dev / `cargo run`, where the binary lives
///      under `target/` and (2) does not apply.
fn ontology_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MOOSEDEV_ONTOLOGY_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("ontologies")))
        .filter(|p| p.is_dir())
    {
        return dir;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

/// Is `bin` reachable on `PATH`?
fn is_on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

/// Resolve the command MCP clients should spawn for the `moosedev` server, plus
/// an optional PATH advisory. Prefer a bare `moosedev` on `PATH` (upgrade-stable
/// — the install channels put it there) so a version bump does not strand a
/// stale absolute path in the generated config; otherwise fall back to this
/// executable's absolute path and warn. `--binary` forces an explicit path.
fn resolve_init_command(binary: Option<PathBuf>) -> anyhow::Result<(String, Option<String>)> {
    if let Some(path) = binary {
        let abs = std::fs::canonicalize(&path).unwrap_or(path);
        return Ok((abs.to_string_lossy().into_owned(), None));
    }
    if is_on_path("moosedev") {
        return Ok(("moosedev".to_string(), None));
    }
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("resolve current executable for the MCP command: {e}"))?;
    let bindir = exe
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let note = format!(
        "`moosedev` is not on your PATH, so .mcp.json uses an absolute path. Add {bindir} to PATH so the config survives upgrades (or re-run `moosedev init` with it on PATH)."
    );
    Ok((exe.to_string_lossy().into_owned(), Some(note)))
}

fn init_mode(args: InitArgs) -> anyhow::Result<()> {
    let target_dir = args.target_dir.unwrap_or_else(|| PathBuf::from("."));
    let data_dir = args.data_dir.unwrap_or_else(|| ".moosedev".to_string());
    let server_mode = if args.stdio {
        init::ServerMode::Stdio
    } else {
        init::ServerMode::Connect
    };
    let (command, path_note) = resolve_init_command(args.binary)?;

    let opts = init::InitOptions {
        target_dir,
        command,
        data_dir,
        server_mode,
        force: args.force,
        codex: args.codex,
        opencode: args.opencode,
    };
    let report = init::init_project(&opts)?;
    print_init_report(&opts, &report, path_note.as_deref());
    Ok(())
}

/// Print the human-facing summary of an `init` run. `init` is a one-shot terminal
/// command (never the MCP stdio channel), so stdout is the right sink here.
fn print_init_report(opts: &init::InitOptions, report: &init::InitReport, path_note: Option<&str>) {
    println!(
        "Initialized MOOSEDev memory in {}",
        opts.target_dir.display()
    );
    for entry in &report.entries {
        let verb = match entry.outcome {
            init::Outcome::Created => "create",
            init::Outcome::Merged => "update",
            init::Outcome::Skipped => "keep  ",
        };
        println!("  {verb}  {}", entry.path.display());
    }
    if let Some(note) = path_note {
        println!("\nnote: {note}");
    }
    for note in &report.notes {
        println!("note: {note}");
    }
    println!("\nNext steps:");
    println!("  1. Reload MCP servers in your client (restart Claude Code, or /mcp reconnect).");
    println!(
        "  2. Seed project memory: run MOOSEDev's bootstrap skill (skills/bootstrap-existing-codebase.md)."
    );
    println!(
        "  3. Commit {}/kg.nq to version your project's memory with the code.",
        opts.data_dir.trim_end_matches('/')
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_serve_defaults_to_no_socket_and_no_open() {
        assert!(matches!(
            parse_mode(&argv(&["--serve"])).unwrap(),
            Mode::Serve {
                socket: None,
                open: false
            }
        ));
    }

    #[test]
    fn parse_serve_accepts_open_flag_and_socket_in_any_order() {
        for args in [
            ["--serve", "/tmp/m.sock", "--open"],
            ["--serve", "--open", "/tmp/m.sock"],
        ] {
            match parse_mode(&argv(&args)).unwrap() {
                Mode::Serve { socket, open } => {
                    assert!(open, "--open must be recognized regardless of position");
                    assert_eq!(socket, Some(PathBuf::from("/tmp/m.sock")));
                }
                _ => panic!("expected --serve mode for {args:?}"),
            }
        }
    }

    #[test]
    fn parse_serve_rejects_unknown_flag() {
        assert!(parse_mode(&argv(&["--serve", "--nope"])).is_err());
    }

    #[test]
    fn parse_status_and_ui_modes() {
        assert!(matches!(
            parse_mode(&argv(&["--status"])).unwrap(),
            Mode::Status(None)
        ));
        assert!(matches!(
            parse_mode(&argv(&["ui"])).unwrap(),
            Mode::Ui(None)
        ));
        assert!(matches!(
            parse_mode(&argv(&["ui", "/tmp/m.sock"])).unwrap(),
            Mode::Ui(Some(_))
        ));
    }

    #[test]
    fn parse_import_requires_path_and_accepts_options() {
        assert!(parse_mode(&argv(&["import"])).is_err());

        match parse_mode(&argv(&[
            "import",
            "backup.nq",
            "--format",
            "nq",
            "--graph=all",
            "--mode",
            "replace",
        ]))
        .unwrap()
        {
            Mode::Import(args) => {
                assert_eq!(args.path, PathBuf::from("backup.nq"));
                assert_eq!(args.format, ImportFormat::NQuads);
                assert_eq!(args.scope, ExportScope::All);
                assert_eq!(args.mode, ImportMode::Replace);
            }
            _ => panic!("expected import mode"),
        }
    }

    #[test]
    fn dotenv_loads_missing_values_without_overriding_existing_env() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let dir = std::env::temp_dir().join(format!("moosedev-dotenv-test-{}", std::process::id()));
        let dotenv = dir.join(".env");
        let missing_key = "MOOSEDEV_DOTENV_TEST_MISSING";
        let existing_key = "MOOSEDEV_DOTENV_TEST_EXISTING";

        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dotenv dir");
        std::fs::write(
            &dotenv,
            format!("{missing_key}=from-file\n{existing_key}=from-file\n"),
        )
        .expect("write temp dotenv");

        std::env::remove_var(missing_key);
        std::env::set_var(existing_key, "from-env");

        assert!(load_dotenv_file(&dotenv).expect("load dotenv"));
        assert_eq!(std::env::var(missing_key).as_deref(), Ok("from-file"));
        assert_eq!(std::env::var(existing_key).as_deref(), Ok("from-env"));

        std::env::remove_var(missing_key);
        std::env::remove_var(existing_key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ontology_dir_resolves_override_then_exe_relative_then_crate() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let key = "MOOSEDEV_ONTOLOGY_DIR";

        // 1. Explicit override wins.
        std::env::set_var(key, "/custom/onto");
        assert_eq!(ontology_dir(), PathBuf::from("/custom/onto"));
        std::env::remove_var(key);

        // 2. With no override, an `ontologies/` dir next to the binary is used.
        let exe_dir = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("exe has parent")
            .to_path_buf();
        let marker = exe_dir.join("ontologies");
        let created = !marker.exists();
        if created {
            std::fs::create_dir(&marker).expect("create exe-relative ontologies marker");
        }
        assert_eq!(ontology_dir(), marker);

        // 3. Without an exe-relative dir, fall back to the crate's own `ontologies/`.
        if created {
            std::fs::remove_dir(&marker).expect("remove marker");
            assert_eq!(
                ontology_dir(),
                Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
            );
        }
    }

    #[test]
    fn export_mode_rejects_missing_store_without_creating_one() {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-export-missing-store-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let err = export_mode(
            &dir,
            ExportArgs {
                path: None,
                format: ExportFormat::NQuads,
                scope: ExportScope::Project,
            },
        )
        .expect_err("missing store should reject export");

        assert!(
            err.to_string().contains("no MOOSEDev graph store found"),
            "error should explain the missing store: {err}"
        );
        assert!(
            !dir.join("kg").exists(),
            "export must not create a new empty store"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_mode_rejects_missing_store_without_creating_one() {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-import-missing-store-test-{}",
            std::process::id()
        ));
        let input = dir.with_extension("ttl");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&input);
        std::fs::write(
            &input,
            r#"<https://example.test/s> <http://www.w3.org/2000/01/rdf-schema#label> "x" ."#,
        )
        .expect("write import fixture");

        let err = import_mode(
            &dir,
            ImportArgs {
                path: input.clone(),
                format: ImportFormat::Turtle,
                scope: ExportScope::Project,
                mode: ImportMode::Patch,
            },
        )
        .expect_err("missing store should reject import");

        assert!(
            err.to_string().contains("no MOOSEDev graph store found"),
            "error should explain the missing store: {err}"
        );
        assert!(
            !dir.join("kg").exists(),
            "import must not create a new empty store"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&input);
    }
}
