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
//! - `lsp`: run as a thin Knowledge-LSP shim — relay editor stdio to the
//!   daemon-owned LSP socket.
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
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use moosedev::code::substrate::{self, Position, ResolutionMode, Substrate};
use moosedev::export::{export_graph, ExportFormat, ExportScope};
use moosedev::graph;
use moosedev::graph_import::{import_graph as import_rdf_graph, ImportFormat, ImportMode};
use moosedev::init;
use moosedev::runtime;

mod temporal;

/// Selected transport mode plus its optional explicit socket path.
enum Mode {
    Stdio,
    Serve {
        socket: Option<PathBuf>,
        open: bool,
    },
    Connect(Option<PathBuf>),
    /// Relay editor stdio to the daemon-owned Knowledge-LSP socket.
    Lsp,
    /// Report backend + web UI status without opening the store (socket-only).
    Status(Option<PathBuf>),
    /// Open the running backend's web UI in a browser (auto-spawning if needed).
    Ui(Option<PathBuf>),
    Export(ExportArgs),
    Import(ImportArgs),
    /// Configure a project to use MOOSEDev as memory (`.mcp.json`, `.gitignore`,
    /// `CLAUDE.md`); no store, no backend.
    Init(InitArgs),
    /// Temporal git-walk bootstrap: replay trunk history into the graph.
    Bootstrap(temporal::BootstrapArgs),
    /// Build the code substrate index.
    Index,
    /// Plan/apply CodeEntity minting from the code substrate.
    Mint {
        apply: bool,
    },
    /// Resolve a source position through the code substrate.
    Resolve(ResolveArgs),
    /// Print the resolved `skills/` dir + the agent workflow docs it holds.
    Skills,
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
    zed: bool,
    stdio: bool,
    binary: Option<PathBuf>,
    data_dir: Option<String>,
}

struct ResolveArgs {
    file: PathBuf,
    line: u32,
    col: u32,
}

const USAGE: &str = "\
moosedev — neurosymbolic MCP memory server

USAGE:
    moosedev                  Serve MCP over stdio (default, single client)
    moosedev --serve [SOCK] [--open]
                              Run the shared backend on a Unix socket; --open
                              launches the web UI in a browser once it is up
    moosedev --connect [SOCK] Proxy stdio to a backend; auto-spawn if needed
    moosedev lsp              Proxy editor stdio to the daemon Knowledge-LSP
    moosedev --status [SOCK]  Report backend + web UI status (no store lock)
    moosedev ui [SOCK]        Open the backend's web UI in a browser (auto-spawn)
    moosedev export [PATH]    Export the graph; no running backend required
    moosedev import PATH      Import RDF into the graph; no running backend required
    moosedev init [DIR]       Configure DIR (default .) to use MOOSEDev as memory
    moosedev bootstrap --temporal
                              Replay git history into the graph (per-commit dates)
    moosedev index            Build the code substrate index (runs rust-analyzer scip)
    moosedev mint [--apply]   Mint CodeEntity continuants from the substrate
                              (dry-run unless --apply is present)
    moosedev resolve FILE LINE:COL
                              Resolve a source position to a code entity (debug)
    moosedev skills           List the shipped agent workflow docs (bootstrap, …)
    moosedev --help           Show this help

SOCKET defaults to MOOSEDEV_SOCKET, else <MOOSEDEV_DATA_DIR>/moosedev.sock.
The web UI binds an ephemeral loopback port by default (discoverable via
--status / ui); set MOOSEDEV_HTTP_ADDR for a stable port or network exposure,
or MOOSEDEV_NO_HTTP=1 to disable it.
Configuration: repo-root .env plus environment variables. Explicit environment
values win. Keys: MOOSEDEV_DATA_DIR, MOOSEDEV_ONTOLOGY_DIR, MOOSEDEV_SOCKET,
MOOSEDEV_HTTP_ADDR, MOOSEDEV_NO_HTTP, MOOSEDEV_NO_LSP,
MOOSEDEV_NO_AUTOSPAWN.
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

RESOLVE:
    Input positions are 1-based. Columns are UTF-8 byte columns.
    MOOSEDEV_SCIP_PRODUCER overrides the SCIP producer binary used by index.

INIT OPTIONS:
    --stdio                   Generate a bare-stdio MCP config (single client)
                              instead of the default shared --connect config
    --codex                   Also write .codex/config.toml for the Codex CLI
    --opencode                Also install the opencode push plugin (.opencode/plugins/)
    --zed                     Also write project-local .zed/settings.json
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
        Some("lsp") => parse_no_args(iter, "lsp").map(|()| Mode::Lsp),
        Some("--status") => Ok(Mode::Status(parse_optional_path(&mut iter, "--status")?)),
        Some("ui") => Ok(Mode::Ui(parse_optional_path(&mut iter, "ui")?)),
        Some("export") => parse_export(iter).map(Mode::Export),
        Some("import") => parse_import(iter).map(Mode::Import),
        Some("init") => parse_init(iter).map(Mode::Init),
        Some("bootstrap") => parse_bootstrap(iter).map(Mode::Bootstrap),
        Some("index") => parse_index(iter).map(|()| Mode::Index),
        Some("mint") => parse_mint(iter).map(|apply| Mode::Mint { apply }),
        Some("resolve") => parse_resolve(iter).map(Mode::Resolve),
        Some("skills") => Ok(Mode::Skills),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => anyhow::bail!(
            "unknown argument {other:?} — expected export, import, init, bootstrap, index, mint, resolve, skills, lsp, --serve, --connect, --status, ui, --help, or no arguments (stdio)"
        ),
    }
}

fn parse_no_args<'a>(mut iter: impl Iterator<Item = &'a String>, mode: &str) -> anyhow::Result<()> {
    if let Some(extra) = iter.next() {
        anyhow::bail!("{mode} accepts no arguments; unexpected {extra:?}");
    }
    Ok(())
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

fn parse_index<'a>(mut iter: impl Iterator<Item = &'a String>) -> anyhow::Result<()> {
    if let Some(extra) = iter.next() {
        anyhow::bail!("index accepts no arguments; unexpected {extra:?}");
    }
    Ok(())
}

/// Parse `mint`'s single optional flag; the command is dry-run unless `--apply`.
fn parse_mint<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<bool> {
    let mut apply = false;
    for arg in iter {
        if arg == "--apply" {
            apply = true;
        } else if arg.starts_with('-') {
            anyhow::bail!("unknown mint option {arg:?}; expected optional --apply");
        } else {
            anyhow::bail!("mint accepts only optional --apply; unexpected {arg:?}");
        }
    }
    Ok(apply)
}

fn parse_resolve<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<ResolveArgs> {
    let mut file = None;
    let mut line = None;
    let mut col = None;
    let mut args = iter.peekable();

    while let Some(arg) = args.next() {
        if arg.starts_with('-') {
            anyhow::bail!("unknown resolve option {arg:?}");
        } else if file.is_none() {
            file = Some(PathBuf::from(arg.as_str()));
        } else if line.is_none() {
            if let Some((line_value, col_value)) = arg.split_once(':') {
                line = Some(parse_positive_position("line", line_value)?);
                col = Some(parse_positive_position("column", col_value)?);
            } else {
                line = Some(parse_positive_position("line", arg)?);
            }
        } else if col.is_none() {
            col = Some(parse_positive_position("column", arg)?);
        } else {
            anyhow::bail!("resolve accepts FILE LINE:COL or FILE LINE COL; unexpected {arg:?}");
        }
    }

    let file = file.ok_or_else(|| anyhow::anyhow!("resolve requires FILE LINE:COL"))?;
    let line = line.ok_or_else(|| anyhow::anyhow!("resolve requires FILE LINE:COL"))?;
    let col = col.ok_or_else(|| anyhow::anyhow!("resolve requires FILE LINE:COL"))?;
    Ok(ResolveArgs { file, line, col })
}

fn parse_positive_position(name: &str, value: &str) -> anyhow::Result<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("{name} must be a positive integer, got {value:?}"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be 1-based, got 0");
    }
    Ok(parsed)
}

/// Parse `init`'s arguments: an optional target dir plus flags. Mirrors the
/// long/`=` option style of [`parse_export`]/[`parse_import`].
fn parse_init<'a>(iter: impl Iterator<Item = &'a String>) -> anyhow::Result<InitArgs> {
    let mut target_dir = None;
    let mut force = false;
    let mut codex = false;
    let mut opencode = false;
    let mut zed = false;
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
        } else if arg == "--zed" {
            zed = true;
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
        zed,
        stdio,
        binary,
        data_dir,
    })
}

/// Parse `bootstrap`'s arguments. `--temporal` is required for a real run;
/// without it, print guidance and exit 0. Remaining flags are all optional.
fn parse_bootstrap<'a>(
    iter: impl Iterator<Item = &'a String>,
) -> anyhow::Result<temporal::BootstrapArgs> {
    let mut temporal = false;
    let mut repo = None::<PathBuf>;
    let mut data_dir = None::<String>;
    let mut trunk = None::<String>;
    let mut resume = false;
    let mut agent = temporal::Agent::Claude;
    let mut model = None::<String>;
    let mut limit = None::<usize>;
    let mut dry_run = false;
    let mut milestone_every = 10usize;
    let mut args = iter.peekable();

    while let Some(arg) = args.next() {
        if arg == "--temporal" {
            temporal = true;
        } else if arg == "--resume" {
            resume = true;
        } else if arg == "--dry-run" {
            dry_run = true;
        } else if let Some(value) = arg.strip_prefix("--repo=") {
            repo = Some(PathBuf::from(value));
        } else if arg == "--repo" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--repo requires a path"))?;
            repo = Some(PathBuf::from(v.as_str()));
        } else if let Some(value) = arg.strip_prefix("--data-dir=") {
            data_dir = Some(value.to_string());
        } else if arg == "--data-dir" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--data-dir requires a value"))?;
            data_dir = Some(v.clone());
        } else if let Some(value) = arg.strip_prefix("--trunk=") {
            trunk = Some(value.to_string());
        } else if arg == "--trunk" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--trunk requires a branch name"))?;
            trunk = Some(v.clone());
        } else if let Some(value) = arg.strip_prefix("--agent=") {
            agent = parse_agent(value)?;
        } else if arg == "--agent" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--agent requires claude or codex"))?;
            agent = parse_agent(v)?;
        } else if let Some(value) = arg.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if arg == "--model" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--model requires a model name"))?;
            model = Some(v.clone());
        } else if let Some(value) = arg.strip_prefix("--limit=") {
            limit = Some(
                value
                    .parse::<usize>()
                    .map_err(|e| anyhow::anyhow!("--limit: {e}"))?,
            );
        } else if arg == "--limit" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--limit requires a number"))?;
            limit = Some(
                v.parse::<usize>()
                    .map_err(|e| anyhow::anyhow!("--limit: {e}"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--milestone-every=") {
            milestone_every = value
                .parse::<usize>()
                .map_err(|e| anyhow::anyhow!("--milestone-every: {e}"))?;
        } else if arg == "--milestone-every" {
            let v = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--milestone-every requires a number"))?;
            milestone_every = v
                .parse::<usize>()
                .map_err(|e| anyhow::anyhow!("--milestone-every: {e}"))?;
        } else if arg.starts_with('-') {
            anyhow::bail!("unknown bootstrap option {arg:?}");
        } else {
            anyhow::bail!("bootstrap does not accept positional arguments; unexpected {arg:?}");
        }
    }

    if !temporal {
        // No --temporal: print guidance and exit 0 (snapshot bootstrap is an agent skill).
        println!(
            "Snapshot bootstrap is an interactive agent skill — run `moosedev skills` to find it."
        );
        println!(
            "For temporal git-walk bootstrap (replay git history with per-commit dates), use:"
        );
        println!(
            "  moosedev bootstrap --temporal [--repo .] [--data-dir .moosedev] [--dry-run] ..."
        );
        std::process::exit(0);
    }

    let repo =
        repo.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let data_dir = data_dir.unwrap_or_else(|| ".moosedev".to_string());

    // Resolve the skill path via the same lookup as skills_mode.
    let skill_path = skills_dir()
        .map(|d| d.join("temporal-episode-capture.md"))
        .filter(|p| p.is_file());

    Ok(temporal::BootstrapArgs {
        temporal,
        repo,
        data_dir,
        trunk,
        resume,
        agent,
        model,
        limit,
        dry_run,
        milestone_every,
        ontology_dir: ontology_dir(),
        skill_path,
    })
}

fn parse_agent(value: &str) -> anyhow::Result<temporal::Agent> {
    match value {
        "claude" => Ok(temporal::Agent::Claude),
        "codex" => Ok(temporal::Agent::Codex),
        other => anyhow::bail!("unknown agent {other:?}; expected claude or codex"),
    }
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
        Mode::Lsp => {
            let mcp_socket = prepare_socket(None, &data_dir)?;
            let lsp_socket = moosedev::lsp::lsp_socket_path_for(&data_dir);
            runtime::connect_lsp_unix(&lsp_socket, &mcp_socket, &data_dir).await
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
            let http_addr = runtime::spawn_http_if_enabled(state.clone(), &data_dir).await;
            let lsp_socket = moosedev::lsp::spawn_lsp_listener(state, &data_dir).await;
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
                "MOOSEDev backend startup: data_dir={}, socket={}, lsp_socket={}, pidfile={}",
                data_dir.display(),
                socket.display(),
                lsp_socket
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<disabled>".to_string()),
                pidfile.display()
            );
            let result = runtime::serve_unix(server, &socket).await;
            let _ = std::fs::remove_file(&pidfile);
            let _ = std::fs::remove_file(runtime::http_addr_file_path_for(&data_dir));
            if let Some(lsp_socket) = lsp_socket {
                let _ = std::fs::remove_file(lsp_socket);
            }
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
        Mode::Bootstrap(args) => temporal::run(args),
        Mode::Index => index_mode(&data_dir),
        Mode::Mint { apply } => mint_mode(&data_dir, apply).await,
        Mode::Resolve(args) => resolve_mode(&data_dir, args),
        Mode::Skills => skills_mode(),
        Mode::Stdio => {
            let server = runtime::build_server(&data_dir, &ontology_dir()).await?;
            runtime::serve_stdio(server).await
        }
    }
}

fn index_mode(data_dir: &Path) -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()?;
    let report = substrate::producer::run_index(&repo_root, data_dir)?;
    println!("substrate index");
    println!("  commit:      {}", report.commit);
    println!("  duration:    {:.3}s", report.duration.as_secs_f64());
    println!("  documents:   {}", report.documents);
    println!("  occurrences: {}", report.occurrences);
    println!("  definitions: {}", report.definitions);
    println!(
        "  index size:  {} bytes\n  {}",
        report.index_bytes,
        substrate::producer::diagnostic_summary(data_dir)
    );
    println!(
        "  output:      {}",
        substrate::index_path(data_dir).display()
    );
    Ok(())
}

/// Plan or apply CodeEntity minting from the previously built code substrate.
async fn mint_mode(data_dir: &Path, apply: bool) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let repo_root = std::env::current_dir()?;
    let substrate = Substrate::load(data_dir, &repo_root)
        .with_context(|| "load code substrate for mint; run `moosedev index` first")?;
    let state = match runtime::build_state(data_dir, &ontology_dir()).await {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "failed to open MOOSEDev state at {}: {e}\n\
                 Stop the daemon first, for example: kill $(cat {}/moosedev-serve.pid)",
                data_dir.display(),
                data_dir.display()
            );
            return Err(e);
        }
    };

    let terms = graph::CodeTerms::resolve(&state)?;
    let components = graph::load_components(&state)?;
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(&state, &definitions, &terms, &components)?;
    report_mint_plan(&plan, &components);

    if !apply {
        println!("dry-run only; re-run with --apply");
        return Ok(());
    }

    let outcome = graph::apply_mint(&state, &plan, &terms)?;
    let validation_started = std::time::Instant::now();
    state.ensure_enriched();
    moosedev::canonical::write_through(&state.store, data_dir)?;
    let report = moosedev::validation::validate_project(&state)?;
    println!("\n{}", moosedev::validation::format_report(&report));
    println!(
        "enrich+validate: {:.3}s",
        validation_started.elapsed().as_secs_f64()
    );
    if !report.conforms() {
        anyhow::bail!("post-mint validation failed");
    }
    println!(
        "mint applied: created {}, updated {} in {:.3}s",
        outcome.created.len(),
        outcome.updated,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Print the human-facing dry-run/apply plan summary.
fn report_mint_plan(plan: &graph::MintPlan, components: &[graph::ComponentEntry]) {
    let component_names = components
        .iter()
        .filter_map(|component| {
            component
                .iri
                .as_ref()
                .map(|iri| (iri.as_str(), component.name.as_str()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    for planned in &plan.create {
        report_planned_entity("CREATE", planned, &component_names);
    }
    for planned in &plan.update {
        report_planned_entity("UPDATE", planned, &component_names);
    }

    if !plan.collisions.is_empty() {
        println!("collisions:");
        for collision in &plan.collisions {
            println!(
                "  {} kept {} dropped {}",
                collision.normalized_symbol,
                collision.kept_file,
                collision.dropped.join(", ")
            );
        }
    }
    if !plan.unmapped_paths.is_empty() {
        println!("unmapped paths:");
        for path in &plan.unmapped_paths {
            println!("  {path}");
        }
    }
    if !plan.orphaned.is_empty() {
        println!("orphaned entities:");
        for (iri, symbol) in &plan.orphaned {
            println!("  {symbol} {iri}");
        }
    }

    println!(
        "summary: create={} update={} unchanged={} skipped-scope={} skipped-tests={} collisions={} unmapped={} orphaned={}",
        plan.create.len(),
        plan.update.len(),
        plan.unchanged,
        plan.skipped_scope,
        plan.skipped_tests,
        plan.collisions.len(),
        plan.unmapped_paths.len(),
        plan.orphaned.len()
    );
}

/// Print one planned create/update line, including the component link if present.
fn report_planned_entity(
    action: &str,
    planned: &graph::PlannedEntity,
    component_names: &std::collections::BTreeMap<&str, &str>,
) {
    print!(
        "{} {} {} ({})",
        action,
        planned.display_kind(),
        planned.display_name(),
        planned.entry.file
    );
    if let Some(component_iri) = planned.realizes.as_deref() {
        let name = component_names
            .get(component_iri)
            .copied()
            .unwrap_or(component_iri);
        print!(" -> realizes {name}");
    }
    println!();
}

fn resolve_mode(data_dir: &Path, args: ResolveArgs) -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()?;
    let substrate = Substrate::load(data_dir, &repo_root)?;
    let relative_path = normalize_resolve_path(&repo_root, &args.file);
    let pos = Position {
        line: args.line - 1,
        col: args.col - 1,
    };

    let Some(resolution) = substrate.resolve(&relative_path, pos) else {
        eprintln!(
            "no entity at {}:{}:{}",
            args.file.display(),
            args.line,
            args.col
        );
        std::process::exit(1);
    };

    let role = if resolution.is_definition {
        "definition"
    } else {
        "reference"
    };
    let mode = match resolution.mode {
        ResolutionMode::Scip => "scip",
        ResolutionMode::TreeSitter => "tree-sitter",
    };
    println!("symbol:         {}", resolution.symbol);
    println!(
        "display name:   {}",
        resolution.display_name.as_deref().unwrap_or("<none>")
    );
    println!(
        "kind:           {}",
        resolution.kind.as_deref().unwrap_or("<none>")
    );
    println!("role:           {role}");
    println!(
        "local:          {}",
        if resolution.is_local { "yes" } else { "no" }
    );
    println!(
        "range:          {}:{}-{}:{}",
        resolution.range.start.line + 1,
        resolution.range.start.col + 1,
        resolution.range.end.line + 1,
        resolution.range.end.col + 1
    );
    println!("mode:           {mode}");
    println!("indexed commit: {}", substrate.meta().indexed_commit);
    println!("stale:          {}", resolution.stale);
    Ok(())
}

fn normalize_resolve_path(repo_root: &Path, file: &Path) -> String {
    let normalized_separators = file.to_string_lossy().replace('\\', "/");
    let path = Path::new(&normalized_separators);
    let relative = if path.is_absolute() {
        path.strip_prefix(repo_root).unwrap_or(path)
    } else {
        path
    };

    relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
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
        // Resolve symlinks (Homebrew's bin/moosedev → libexec/moosedev; the curl
        // installer's ~/.local/bin symlink) so `ontologies/` is found next to the
        // REAL binary, not the symlink's dir.
        .and_then(|exe| std::fs::canonicalize(exe).ok())
        .and_then(|exe| exe.parent().map(|p| p.join("ontologies")))
        .filter(|p| p.is_dir())
    {
        return dir;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

/// Locate the shipped `skills/` dir (agent workflow docs), mirroring
/// [`ontology_dir`]'s exe-relative, symlink-resolved lookup.
fn skills_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MOOSEDEV_SKILLS_DIR") {
        let dir = PathBuf::from(dir);
        return dir.is_dir().then_some(dir);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::canonicalize(exe).ok())
        .and_then(|exe| exe.parent().map(|p| p.join("skills")))
        .filter(|p| p.is_dir())
    {
        return Some(dir);
    }
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    crate_dir.is_dir().then_some(crate_dir)
}

/// `moosedev skills` — print the resolved `skills/` dir and the workflow docs it
/// holds, with absolute paths a user can hand straight to their coding agent.
fn skills_mode() -> anyhow::Result<()> {
    let Some(dir) = skills_dir() else {
        anyhow::bail!(
            "no skills/ dir found next to the binary (set MOOSEDEV_SKILLS_DIR to override)"
        );
    };
    println!("MOOSEDev skills: {}", dir.display());
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| n.to_string_lossy().ends_with(".md"))
        .collect();
    names.sort();
    for name in &names {
        println!("  {}", dir.join(name).display());
    }
    let bootstrap = dir.join("bootstrap-existing-codebase.md");
    if bootstrap.is_file() {
        println!(
            "\nWith the moosedev MCP attached, point your coding agent at one, e.g.:\n  \"Follow {} to bootstrap this repo's project memory.\"",
            bootstrap.display()
        );
    }
    Ok(())
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
        zed: args.zed,
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
    fn parse_lsp_accepts_no_args() {
        assert!(matches!(parse_mode(&argv(&["lsp"])).unwrap(), Mode::Lsp));
        assert!(parse_mode(&argv(&["lsp", "extra"])).is_err());
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
    fn parse_index_accepts_no_args_and_rejects_extras() {
        assert!(matches!(
            parse_mode(&argv(&["index"])).unwrap(),
            Mode::Index
        ));
        assert!(parse_mode(&argv(&["index", "extra"])).is_err());
    }

    #[test]
    fn parse_mint_accepts_optional_apply_and_rejects_extras() {
        assert!(matches!(
            parse_mode(&argv(&["mint"])).unwrap(),
            Mode::Mint { apply: false }
        ));
        assert!(matches!(
            parse_mode(&argv(&["mint", "--apply"])).unwrap(),
            Mode::Mint { apply: true }
        ));
        assert!(parse_mode(&argv(&["mint", "extra"])).is_err());
    }

    #[test]
    fn parse_resolve_accepts_line_col_pair() {
        match parse_mode(&argv(&["resolve", "src/main.rs", "144:4"])).unwrap() {
            Mode::Resolve(args) => {
                assert_eq!(args.file, PathBuf::from("src/main.rs"));
                assert_eq!(args.line, 144);
                assert_eq!(args.col, 4);
            }
            _ => panic!("expected resolve mode"),
        }
    }

    #[test]
    fn parse_resolve_accepts_split_line_col() {
        match parse_mode(&argv(&["resolve", "src/main.rs", "144", "4"])).unwrap() {
            Mode::Resolve(args) => {
                assert_eq!(args.file, PathBuf::from("src/main.rs"));
                assert_eq!(args.line, 144);
                assert_eq!(args.col, 4);
            }
            _ => panic!("expected resolve mode"),
        }
    }

    #[test]
    fn parse_resolve_rejects_zero_and_garbage() {
        assert!(parse_mode(&argv(&["resolve", "src/main.rs", "0:4"])).is_err());
        assert!(parse_mode(&argv(&["resolve", "src/main.rs", "144:0"])).is_err());
        assert!(parse_mode(&argv(&["resolve", "src/main.rs", "abc:4"])).is_err());
        assert!(parse_mode(&argv(&["resolve", "src/main.rs", "144", "garbage"])).is_err());
    }

    #[test]
    fn normalize_resolve_path_accepts_debug_cli_path_forms() {
        let repo_root = std::env::current_dir().unwrap();
        let cases = [
            PathBuf::from("./src/runtime.rs"),
            repo_root.join("src/runtime.rs"),
            PathBuf::from("src\\runtime.rs"),
        ];

        for path in cases {
            assert_eq!(normalize_resolve_path(&repo_root, &path), "src/runtime.rs");
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
