//! `moosedev init` — configure a project to use MOOSEDev as its long-term memory.
//!
//! Collapses the manual onboarding (hand-editing MCP-client JSON, writing the
//! `.gitignore` memory-cache rule, copying the CLAUDE.md template) into one
//! command. Every artifact write is **non-clobbering**: an existing file is
//! preserved (and reported as [`Outcome::Skipped`]) unless `--force`, so
//! re-running `init`, or running it in a repo that already has a `.mcp.json` /
//! `CLAUDE.md`, never destroys the user's work. `.mcp.json` is *merged* — the
//! `moosedev` server is inserted alongside any servers already configured.
//!
//! The CLAUDE.md template is embedded via `include_str!`, so `init` never has to
//! locate a `templates/` dir at runtime (it may be buried in a Homebrew Cellar).
//! An existing CLAUDE.md gets the project-memory block *appended* — managed,
//! idempotent, reversible — instead of the user being told to add it by hand.
//!
//! The one filesystem-facing input (the resolved binary command) is passed in by
//! `main.rs`, keeping [`init_project`] a pure function of its [`InitOptions`] so
//! it is unit-testable against a temp dir.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{json, Value};

/// The CLAUDE.md adoption template, embedded so `init` needs no runtime
/// `templates/` lookup. Its project-memory section is wrapped in the markers
/// below so an existing CLAUDE.md can be detected and augmented idempotently.
const CLAUDE_TEMPLATE: &str = include_str!("../templates/CLAUDE.md");
/// Start of the managed project-memory block. Its presence means a CLAUDE.md is
/// already memory-aware.
const MEMORY_BEGIN: &str = "<!-- moosedev:begin";
/// End of the managed project-memory block.
const MEMORY_END: &str = "moosedev:end -->";

/// The opencode project-memory PUSH plugin, embedded like the CLAUDE.md template.
/// opencode targets local models that under-call MCP tools, so this proactively
/// injects records into their context rather than relying on the model to pull.
const OPENCODE_PLUGIN: &str = include_str!("../.opencode/plugins/moosedev-push.ts");

/// How `init` should render the MCP server invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerMode {
    /// `--connect`: proxy to a shared backend (auto-spawned) that owns the
    /// single-writer store, so Claude Code + Codex + the web UI share one live
    /// graph. The default.
    Connect,
    /// Bare stdio: the client owns the server process directly. Single-client.
    Stdio,
}

impl ServerMode {
    /// The `args` array for the generated MCP config.
    fn args(self) -> Vec<String> {
        match self {
            ServerMode::Connect => vec!["--connect".to_string()],
            ServerMode::Stdio => Vec::new(),
        }
    }
}

/// Inputs to [`init_project`]. `main.rs` resolves the runtime-derived `command`;
/// everything else comes from argv.
pub struct InitOptions {
    /// Directory to initialize (the target project root).
    pub target_dir: PathBuf,
    /// The command MCP clients should spawn — bare `"moosedev"` when it is on
    /// `PATH` (upgrade-stable), else an absolute binary path.
    pub command: String,
    /// Value written for `MOOSEDEV_DATA_DIR`; relative keeps clones portable.
    pub data_dir: String,
    /// MCP invocation style.
    pub server_mode: ServerMode,
    /// Overwrite existing files / server entries instead of preserving them.
    pub force: bool,
    /// Also write `.codex/config.toml` for the Codex CLI.
    pub codex: bool,
    /// Also install the opencode PUSH plugin into `.opencode/plugins/`.
    pub opencode: bool,
}

/// What happened to one artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The file (or dir) did not exist and was written.
    Created,
    /// An existing file was updated in place — e.g. `.mcp.json` gained the
    /// `moosedev` server, or `.gitignore` gained the cache-ignore lines.
    Merged,
    /// The file already existed and was left untouched (no `--force`).
    Skipped,
}

/// One artifact `init` touched.
pub struct Entry {
    pub path: PathBuf,
    pub outcome: Outcome,
}

impl Entry {
    fn new(path: PathBuf, outcome: Outcome) -> Self {
        Self { path, outcome }
    }
}

/// The result of an `init` run: what was written and any advisory notes
/// (existing files left alone, PATH caveats, custom-data-dir warnings).
pub struct InitReport {
    pub entries: Vec<Entry>,
    pub notes: Vec<String>,
}

/// Configure `opts.target_dir` to use MOOSEDev as project memory. Idempotent and
/// non-clobbering (see the module docs). Errors only on unrecoverable IO or an
/// existing `.mcp.json` that is not valid JSON (which we refuse to overwrite).
pub fn init_project(opts: &InitOptions) -> anyhow::Result<InitReport> {
    let mut report = InitReport {
        entries: Vec::new(),
        notes: Vec::new(),
    };

    std::fs::create_dir_all(&opts.target_dir)
        .with_context(|| format!("create target dir {}", opts.target_dir.display()))?;

    write_data_dir(opts, &mut report)?;
    write_mcp_json(opts, &mut report)?;
    write_gitignore(opts, &mut report)?;
    write_claude_md(opts, &mut report)?;
    if opts.codex {
        write_codex_config(opts, &mut report)?;
    }
    if opts.opencode {
        write_opencode_plugin(opts, &mut report)?;
    }

    Ok(report)
}

/// Create the data dir so the memory location exists before first capture.
fn write_data_dir(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join(&opts.data_dir);
    let existed = path.exists();
    std::fs::create_dir_all(&path)
        .with_context(|| format!("create data dir {}", path.display()))?;
    report.entries.push(Entry::new(
        path,
        if existed {
            Outcome::Skipped
        } else {
            Outcome::Created
        },
    ));
    Ok(())
}

/// The `mcpServers.moosedev` value written into `.mcp.json`.
fn mcp_server_entry(opts: &InitOptions) -> Value {
    json!({
        "command": opts.command,
        "args": opts.server_mode.args(),
        "env": { "MOOSEDEV_DATA_DIR": opts.data_dir },
    })
}

/// Merge the `moosedev` server into `.mcp.json`, preserving any other servers.
/// A pre-existing file that is not valid JSON is a hard error — we will not
/// clobber it (it may hold hand-written config or a merge conflict).
fn write_mcp_json(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join(".mcp.json");
    let existed = path.exists();

    let mut root: Value = if existed {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON — fix or move it, then re-run init",
                path.display()
            )
        })?
    } else {
        json!({})
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must be a JSON object", path.display()))?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} `mcpServers` must be an object", path.display()))?;

    if servers.contains_key("moosedev") && !opts.force {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        report
            .notes
            .push("`.mcp.json` already configures a `moosedev` server; left as-is (use --force to overwrite)".to_string());
        return Ok(());
    }
    servers.insert("moosedev".to_string(), mcp_server_entry(opts));

    let serialized = format!("{}\n", serde_json::to_string_pretty(&root)?);
    std::fs::write(&path, serialized).with_context(|| format!("write {}", path.display()))?;
    report.entries.push(Entry::new(
        path,
        if existed {
            Outcome::Merged
        } else {
            Outcome::Created
        },
    ));
    Ok(())
}

/// The two `.gitignore` lines that keep the derived RocksDB/vector cache out of
/// git while committing the canonical project-graph text, derived from the data
/// dir. `None` for an absolute data dir (no single repo-relative rule applies).
fn gitignore_lines(data_dir: &str) -> Option<[String; 2]> {
    if Path::new(data_dir).is_absolute() {
        return None;
    }
    let dir = data_dir.trim_start_matches("./").trim_matches('/');
    if dir.is_empty() {
        return None;
    }
    Some([format!("/{dir}/*"), format!("!/{dir}/kg.nq")])
}

/// Append the cache-ignore lines to `.gitignore` iff missing (idempotent).
fn write_gitignore(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let Some(lines) = gitignore_lines(&opts.data_dir) else {
        report.notes.push(format!(
            "data dir {:?} is absolute — add your own ignore rule to keep the derived cache out of git",
            opts.data_dir
        ));
        return Ok(());
    };
    let path = opts.target_dir.join(".gitignore");
    let existed = path.exists();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let have: HashSet<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&String> = lines
        .iter()
        .filter(|l| !have.contains(l.as_str()))
        .collect();
    if missing.is_empty() {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        return Ok(());
    }

    let fresh_block = missing.len() == lines.len();
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if fresh_block {
        out.push_str(
            "\n# MOOSEDev: ignore the derived store/vector cache,\n# but commit the canonical project-graph text (kg.nq).\n",
        );
    }
    for line in &missing {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    report.entries.push(Entry::new(
        path,
        if existed {
            Outcome::Merged
        } else {
            Outcome::Created
        },
    ));
    Ok(())
}

/// The managed project-memory block (markers inclusive) sliced from the embedded
/// template — this is what gets appended to an existing CLAUDE.md.
fn memory_block() -> &'static str {
    let begin = CLAUDE_TEMPLATE
        .find(MEMORY_BEGIN)
        .expect("embedded CLAUDE.md template must contain the moosedev:begin marker");
    let end = CLAUDE_TEMPLATE
        .find(MEMORY_END)
        .expect("embedded CLAUDE.md template must contain the moosedev:end marker")
        + MEMORY_END.len();
    CLAUDE_TEMPLATE[begin..end].trim_end()
}

/// Make the target's CLAUDE.md memory-aware. Fresh: write the full embedded
/// template. Existing: **append** the managed project-memory block (idempotent —
/// skipped if the marker is already present), never rewriting the user's file.
/// `--force` overwrites the whole file with the fresh template.
fn write_claude_md(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join("CLAUDE.md");
    let existed = path.exists();

    if !existed || opts.force {
        std::fs::write(&path, CLAUDE_TEMPLATE)
            .with_context(|| format!("write {}", path.display()))?;
        report.entries.push(Entry::new(
            path,
            if existed {
                Outcome::Merged
            } else {
                Outcome::Created
            },
        ));
        return Ok(());
    }

    let current =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if current.contains(MEMORY_BEGIN) {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        return Ok(());
    }

    let mut out = current;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(memory_block());
    out.push('\n');
    std::fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    report.entries.push(Entry::new(path, Outcome::Merged));
    report.notes.push(
        "appended the MOOSEDev memory block to your existing CLAUDE.md (delete the moosedev:begin…end block to undo)".to_string(),
    );
    Ok(())
}

/// The `[mcp_servers.moosedev]` TOML block for the Codex CLI.
fn codex_block(opts: &InitOptions) -> String {
    let args = opts
        .server_mode
        .args()
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[mcp_servers.moosedev]\ntype = \"stdio\"\ncommand = {:?}\nargs = [{args}]\nenv = {{ MOOSEDEV_DATA_DIR = {:?} }}\n",
        opts.command, opts.data_dir,
    )
}

/// Append the Codex MCP block iff absent. A safe in-place rewrite of an existing
/// `[mcp_servers.moosedev]` table needs a real TOML parser, so when one is
/// already present we leave the file untouched and say so.
fn write_codex_config(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let dir = opts.target_dir.join(".codex");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("config.toml");
    let existed = path.exists();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    if existing.contains("[mcp_servers.moosedev]") {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        report.notes.push(
            "`.codex/config.toml` already has [mcp_servers.moosedev]; left as-is".to_string(),
        );
        return Ok(());
    }

    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&codex_block(opts));
    std::fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    report.entries.push(Entry::new(
        path,
        if existed {
            Outcome::Merged
        } else {
            Outcome::Created
        },
    ));
    Ok(())
}

/// Install the opencode project-memory PUSH plugin into `.opencode/plugins/`.
/// Unlike the pull-based MCP configs, opencode targets local models that
/// under-call MCP tools, so the plugin proactively injects records into their
/// context. It is self-contained (Node built-ins only — no `npm install`) and
/// reads `MOOSEDEV_DATA_DIR` (defaulting to `.moosedev`). Non-clobbering.
fn write_opencode_plugin(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let dir = opts.target_dir.join(".opencode").join("plugins");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("moosedev-push.ts");
    let existed = path.exists();

    if existed && !opts.force {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        return Ok(());
    }
    std::fs::write(&path, OPENCODE_PLUGIN).with_context(|| format!("write {}", path.display()))?;
    report.entries.push(Entry::new(
        path,
        if existed {
            Outcome::Merged
        } else {
            Outcome::Created
        },
    ));
    report.notes.push(
        "installed the opencode push plugin (.opencode/plugins/moosedev-push.ts) — proactively injects memory for local models that under-call MCP tools".to_string(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("moosedev-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn opts(target: &Path) -> InitOptions {
        InitOptions {
            target_dir: target.to_path_buf(),
            command: "moosedev".to_string(),
            data_dir: ".moosedev".to_string(),
            server_mode: ServerMode::Connect,
            force: false,
            codex: false,
            opencode: false,
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn outcome_for<'a>(report: &'a InitReport, name: &str) -> Option<&'a Outcome> {
        report
            .entries
            .iter()
            .find(|e| e.path.ends_with(name))
            .map(|e| &e.outcome)
    }

    #[test]
    fn writes_a_fresh_project() {
        let target = temp_project("fresh");
        let report = init_project(&opts(&target)).unwrap();

        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(mcp["mcpServers"]["moosedev"]["command"], "moosedev");
        assert_eq!(mcp["mcpServers"]["moosedev"]["args"][0], "--connect");
        assert_eq!(
            mcp["mcpServers"]["moosedev"]["env"]["MOOSEDEV_DATA_DIR"],
            ".moosedev"
        );

        let gitignore = std::fs::read_to_string(target.join(".gitignore")).unwrap();
        assert!(gitignore.contains("/.moosedev/*"));
        assert!(gitignore.contains("!/.moosedev/kg.nq"));

        let claude = std::fs::read_to_string(target.join("CLAUDE.md")).unwrap();
        assert!(claude.contains("Working with project memory"));
        assert!(claude.contains(MEMORY_BEGIN), "carries the managed marker");
        assert!(target.join(".moosedev").is_dir());
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Created));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn merges_into_an_existing_mcp_json() {
        let target = temp_project("merge");
        std::fs::write(
            target.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();

        let report = init_project(&opts(&target)).unwrap();

        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(
            mcp["mcpServers"]["other"]["command"], "x",
            "preserves others"
        );
        assert_eq!(mcp["mcpServers"]["moosedev"]["command"], "moosedev");
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Merged));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn is_idempotent() {
        let target = temp_project("idem");
        let o = opts(&target);
        init_project(&o).unwrap();
        let report = init_project(&o).unwrap();

        let gitignore = std::fs::read_to_string(target.join(".gitignore")).unwrap();
        assert_eq!(
            gitignore.matches("/.moosedev/*").count(),
            1,
            "gitignore line not duplicated"
        );
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Skipped));
        // fresh run wrote the full template (with the marker), so the re-run skips.
        assert_eq!(outcome_for(&report, "CLAUDE.md"), Some(&Outcome::Skipped));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn appends_memory_block_to_an_existing_claude_md() {
        let target = temp_project("claude");
        std::fs::write(
            target.join("CLAUDE.md"),
            "# My Project\n\nExisting notes.\n",
        )
        .unwrap();

        let report = init_project(&opts(&target)).unwrap();

        let claude = std::fs::read_to_string(target.join("CLAUDE.md")).unwrap();
        assert!(
            claude.contains("Existing notes."),
            "preserves the user's content"
        );
        assert!(
            claude.contains(MEMORY_BEGIN),
            "appends the managed memory block"
        );
        assert!(claude.contains("Working with project memory"));
        assert_eq!(outcome_for(&report, "CLAUDE.md"), Some(&Outcome::Merged));
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("appended the MOOSEDev memory block")));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn appending_the_memory_block_is_idempotent() {
        let target = temp_project("claude-idem");
        std::fs::write(target.join("CLAUDE.md"), "# My Project\n").unwrap();
        init_project(&opts(&target)).unwrap();
        let report = init_project(&opts(&target)).unwrap();

        let claude = std::fs::read_to_string(target.join("CLAUDE.md")).unwrap();
        assert_eq!(
            claude.matches(MEMORY_BEGIN).count(),
            1,
            "block not duplicated"
        );
        assert_eq!(outcome_for(&report, "CLAUDE.md"), Some(&Outcome::Skipped));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn force_overwrites_existing_files() {
        let target = temp_project("force");
        std::fs::write(target.join("CLAUDE.md"), "SENTINEL").unwrap();
        std::fs::write(
            target.join(".mcp.json"),
            r#"{"mcpServers":{"moosedev":{"command":"old"}}}"#,
        )
        .unwrap();

        let mut o = opts(&target);
        o.force = true;
        init_project(&o).unwrap();

        let claude = std::fs::read_to_string(target.join("CLAUDE.md")).unwrap();
        assert!(
            claude.contains(MEMORY_BEGIN),
            "force rewrote with the full template"
        );
        assert!(!claude.contains("SENTINEL"));
        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(mcp["mcpServers"]["moosedev"]["command"], "moosedev");

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn stdio_mode_writes_empty_args() {
        let target = temp_project("stdio");
        let mut o = opts(&target);
        o.server_mode = ServerMode::Stdio;
        init_project(&o).unwrap();

        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(
            mcp["mcpServers"]["moosedev"]["args"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn rejects_invalid_mcp_json() {
        let target = temp_project("badjson");
        std::fs::write(target.join(".mcp.json"), "not json {{{").unwrap();
        assert!(init_project(&opts(&target)).is_err());
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn writes_codex_config_when_requested() {
        let target = temp_project("codex");
        let mut o = opts(&target);
        o.codex = true;
        init_project(&o).unwrap();

        let toml = std::fs::read_to_string(target.join(".codex/config.toml")).unwrap();
        assert!(toml.contains("[mcp_servers.moosedev]"));
        assert!(toml.contains("command = \"moosedev\""));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn installs_opencode_plugin_only_when_requested() {
        let off = temp_project("opencode-off");
        init_project(&opts(&off)).unwrap();
        assert!(
            !off.join(".opencode/plugins/moosedev-push.ts").exists(),
            "not installed without the flag"
        );
        let _ = std::fs::remove_dir_all(&off);

        let target = temp_project("opencode");
        let mut o = opts(&target);
        o.opencode = true;
        init_project(&o).unwrap();
        let plugin =
            std::fs::read_to_string(target.join(".opencode/plugins/moosedev-push.ts")).unwrap();
        assert!(plugin.contains("MOOSEDEV_DATA_DIR"));
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn template_carries_the_memory_markers() {
        assert!(CLAUDE_TEMPLATE.contains(MEMORY_BEGIN));
        assert!(CLAUDE_TEMPLATE.contains(MEMORY_END));
        let block = memory_block();
        assert!(block.starts_with(MEMORY_BEGIN));
        assert!(block.trim_end().ends_with(MEMORY_END));
        assert!(block.contains("Working with project memory"));
    }

    #[test]
    fn gitignore_lines_only_for_relative_dirs() {
        assert_eq!(
            gitignore_lines(".moosedev"),
            Some(["/.moosedev/*".to_string(), "!/.moosedev/kg.nq".to_string()])
        );
        assert_eq!(gitignore_lines("/abs/store"), None);
    }
}
