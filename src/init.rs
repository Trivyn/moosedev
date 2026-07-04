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
//! The filesystem-facing inputs (the resolved binary command, the templates dir)
//! are passed in by `main.rs`, keeping [`init_project`] a pure function of its
//! [`InitOptions`] so it is unit-testable against a temp dir.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{json, Value};

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

/// Inputs to [`init_project`]. `main.rs` resolves the runtime-derived fields
/// (`command`, `templates_dir`); everything else comes from argv.
pub struct InitOptions {
    /// Directory to initialize (the target project root).
    pub target_dir: PathBuf,
    /// The command MCP clients should spawn — bare `"moosedev"` when it is on
    /// `PATH` (upgrade-stable), else an absolute binary path.
    pub command: String,
    /// Directory holding `CLAUDE.md` (the shipped `templates/`), or `None` when
    /// it could not be located — the CLAUDE.md step is then skipped with a note.
    pub templates_dir: Option<PathBuf>,
    /// Value written for `MOOSEDEV_DATA_DIR`; relative keeps clones portable.
    pub data_dir: String,
    /// MCP invocation style.
    pub server_mode: ServerMode,
    /// Overwrite existing files / server entries instead of preserving them.
    pub force: bool,
    /// Also write `.codex/config.toml` for the Codex CLI.
    pub codex: bool,
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

/// Copy the CLAUDE.md adoption template in, but never touch an existing one.
fn write_claude_md(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join("CLAUDE.md");
    let existed = path.exists();

    if existed && !opts.force {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        let where_from = match &opts.templates_dir {
            Some(dir) => dir.join("CLAUDE.md").display().to_string(),
            None => "MOOSEDev's templates/CLAUDE.md".to_string(),
        };
        report.notes.push(format!(
            "CLAUDE.md exists; add the MOOSEDev memory section from {where_from}"
        ));
        return Ok(());
    }

    let Some(templates) = &opts.templates_dir else {
        report.notes.push(
            "templates/ not found next to the binary; skipped CLAUDE.md (copy MOOSEDev's templates/CLAUDE.md manually)".to_string(),
        );
        return Ok(());
    };
    let src = templates.join("CLAUDE.md");
    let content = std::fs::read_to_string(&src)
        .with_context(|| format!("read template {}", src.display()))?;
    std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("moosedev-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_templates(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moosedev-init-tpl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "# TEMPLATE CLAUDE\n").unwrap();
        dir
    }

    fn opts(target: &Path, templates: &Path) -> InitOptions {
        InitOptions {
            target_dir: target.to_path_buf(),
            command: "moosedev".to_string(),
            templates_dir: Some(templates.to_path_buf()),
            data_dir: ".moosedev".to_string(),
            server_mode: ServerMode::Connect,
            force: false,
            codex: false,
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
        let templates = temp_templates("fresh");
        let report = init_project(&opts(&target, &templates)).unwrap();

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

        assert!(std::fs::read_to_string(target.join("CLAUDE.md"))
            .unwrap()
            .contains("TEMPLATE CLAUDE"));
        assert!(target.join(".moosedev").is_dir());
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Created));

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn merges_into_an_existing_mcp_json() {
        let target = temp_project("merge");
        let templates = temp_templates("merge");
        std::fs::write(
            target.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();

        let report = init_project(&opts(&target, &templates)).unwrap();

        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(
            mcp["mcpServers"]["other"]["command"], "x",
            "preserves others"
        );
        assert_eq!(mcp["mcpServers"]["moosedev"]["command"], "moosedev");
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Merged));

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn is_idempotent() {
        let target = temp_project("idem");
        let templates = temp_templates("idem");
        let o = opts(&target, &templates);
        init_project(&o).unwrap();
        let report = init_project(&o).unwrap();

        let gitignore = std::fs::read_to_string(target.join(".gitignore")).unwrap();
        assert_eq!(
            gitignore.matches("/.moosedev/*").count(),
            1,
            "gitignore line not duplicated"
        );
        assert_eq!(outcome_for(&report, ".mcp.json"), Some(&Outcome::Skipped));
        assert_eq!(outcome_for(&report, "CLAUDE.md"), Some(&Outcome::Skipped));

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn preserves_an_existing_claude_md() {
        let target = temp_project("claude");
        let templates = temp_templates("claude");
        std::fs::write(target.join("CLAUDE.md"), "SENTINEL").unwrap();

        let report = init_project(&opts(&target, &templates)).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.join("CLAUDE.md")).unwrap(),
            "SENTINEL"
        );
        assert_eq!(outcome_for(&report, "CLAUDE.md"), Some(&Outcome::Skipped));
        assert!(report.notes.iter().any(|n| n.contains("CLAUDE.md exists")));

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn force_overwrites_existing_files() {
        let target = temp_project("force");
        let templates = temp_templates("force");
        std::fs::write(target.join("CLAUDE.md"), "SENTINEL").unwrap();
        std::fs::write(
            target.join(".mcp.json"),
            r#"{"mcpServers":{"moosedev":{"command":"old"}}}"#,
        )
        .unwrap();

        let mut o = opts(&target, &templates);
        o.force = true;
        init_project(&o).unwrap();

        assert!(std::fs::read_to_string(target.join("CLAUDE.md"))
            .unwrap()
            .contains("TEMPLATE"));
        let mcp = read_json(&target.join(".mcp.json"));
        assert_eq!(mcp["mcpServers"]["moosedev"]["command"], "moosedev");

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn stdio_mode_writes_empty_args() {
        let target = temp_project("stdio");
        let templates = temp_templates("stdio");
        let mut o = opts(&target, &templates);
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
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn rejects_invalid_mcp_json() {
        let target = temp_project("badjson");
        let templates = temp_templates("badjson");
        std::fs::write(target.join(".mcp.json"), "not json {{{").unwrap();
        assert!(init_project(&opts(&target, &templates)).is_err());
        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
    }

    #[test]
    fn writes_codex_config_when_requested() {
        let target = temp_project("codex");
        let templates = temp_templates("codex");
        let mut o = opts(&target, &templates);
        o.codex = true;
        init_project(&o).unwrap();

        let toml = std::fs::read_to_string(target.join(".codex/config.toml")).unwrap();
        assert!(toml.contains("[mcp_servers.moosedev]"));
        assert!(toml.contains("command = \"moosedev\""));

        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::remove_dir_all(&templates);
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
