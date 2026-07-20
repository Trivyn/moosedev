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
const CLAUDE_GATE_HOOK: &str = include_str!("../.claude/hooks/moosedev-gate.sh");
const CLAUDE_PUSH_HOOK: &str = include_str!("../.claude/hooks/moosedev-push.sh");
const CLAUDE_CAPTURE_HOOK: &str = include_str!("../.claude/hooks/moosedev-capture.sh");

/// Metadata for one agent skill file synthesised at `moosedev init` time from the
/// shipped `skills/*.md` doc body plus a YAML frontmatter header that tells Claude
/// Code / opencode / Codex what the skill does and when to invoke it automatically.
struct SkillMeta {
    name: &'static str,
    description: &'static str,
    body: &'static str,
}

/// The three agent workflow skills shipped with MOOSEDev. Each is installed as a
/// `SKILL.md` into the conventional auto-discovery location (`.claude/skills/`).
/// The source `skills/*.md` files are left unchanged — a bench harness and
/// cross-references depend on those paths; this only synthesises the harness files.
const SKILLS: &[SkillMeta] = &[
    SkillMeta {
        name: "bootstrap-existing-codebase",
        description: "Recover and record the architectural decisions, constraints, lessons, and patterns behind an existing codebase into MOOSEDev's project knowledge graph as typed, linked records. Use when the user asks to bootstrap, seed, or initialize MOOSEDev project memory / the knowledge graph for a repository, or to recover design rationale from existing code.",
        body: include_str!("../skills/bootstrap-existing-codebase.md"),
    },
    SkillMeta {
        name: "generate-adrs-from-graph",
        description: "Render the architectural decisions captured in MOOSEDev's knowledge graph as a set of Architecture Decision Records (ADRs). Use when the user asks to generate, export, or write ADRs from the MOOSEDev graph.",
        body: include_str!("../skills/generate-adrs-from-graph.md"),
    },
    SkillMeta {
        name: "temporal-episode-capture",
        description: "Capture the decisions, constraints, and lessons from a single unit of work (a commit, PR, or work session) into MOOSEDev as typed, linked records. Use when the user asks to record or capture what changed in this episode/commit/PR into project memory.",
        body: include_str!("../skills/temporal-episode-capture.md"),
    },
];

/// Render a skill as a `SKILL.md` file: YAML frontmatter (`name` + `description`)
/// followed by a blank line and the doc body. The frontmatter is what tells
/// Claude Code / opencode the skill's purpose so agents auto-invoke it by
/// description without the user having to supply the path.
fn render_skill(s: &SkillMeta) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}",
        s.name, s.description, s.body
    )
}

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
    /// Also write project-local Zed LSP settings.
    pub zed: bool,
    /// Also write project-local VS Code settings for the `clients/vscode`
    /// extension.
    pub vscode: bool,
    /// Also install the Claude Code gate/push/capture hooks (`.claude/hooks/`
    /// + a `hooks` block merged into `.claude/settings.json`).
    pub claude_hooks: bool,
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
    write_skills(opts, &mut report)?;
    if opts.codex {
        write_codex_config(opts, &mut report)?;
    }
    if opts.opencode {
        write_opencode_plugin(opts, &mut report)?;
    }
    if opts.zed {
        write_zed_settings(opts, &mut report)?;
    }
    if opts.vscode {
        write_vscode_settings(opts, &mut report)?;
    }
    if opts.claude_hooks {
        write_claude_hooks(opts, &mut report)?;
    }
    offer_reindex_hooks(opts, &mut report)?;

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

/// The `lsp.moosedev` value written into `.zed/settings.json`.
fn zed_lsp_entry() -> Value {
    json!({
        "initialization_options": {
            "diagnostics": {
                "constraints": true,
                "staleRationale": true,
            },
        },
    })
}

/// Merge MOOSEDev's optional diagnostics into project-local Zed settings.
/// Existing JSON that cannot be parsed is never overwritten.
fn write_zed_settings(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join(".zed/settings.json");
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
    // Zed defaults code lens off; enable it so MOOSEDev's ambient badges render.
    // Respect an existing user preference if one is already set.
    obj.entry("code_lens").or_insert_with(|| json!("on"));
    let lsp = obj
        .entry("lsp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} `lsp` must be an object", path.display()))?;

    report.notes.push(
        "Zed needs the thin extension in `clients/zed` of the MOOSEDev source checkout (or from the Zed registry once published): zed: install dev extension → clients/zed. Requires `rustup target add wasm32-wasip1`."
            .to_string(),
    );
    if lsp.contains_key("moosedev") && !opts.force {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        report.notes.push(
            "`.zed/settings.json` already configures `lsp.moosedev`; left as-is (use --force to overwrite)"
                .to_string(),
        );
        return Ok(());
    }
    lsp.insert("moosedev".to_string(), zed_lsp_entry());

    let dir = path.parent().expect("settings path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
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

/// The `moosedev.*` keys written into `.vscode/settings.json`. A
/// discoverability artifact, exactly parallel to [`zed_lsp_entry`]: the
/// extension defaults to these values, but seeding them makes the dials
/// visible in the project.
fn vscode_settings_entries() -> Vec<(&'static str, Value)> {
    vec![
        ("moosedev.diagnostics.constraints", json!(true)),
        ("moosedev.diagnostics.staleRationale", json!(true)),
    ]
}

/// Merge MOOSEDev's optional diagnostics into project-local VS Code settings.
/// VS Code settings are JSONC (comments and trailing commas are valid and
/// common), which strict JSON parsing rejects — an unparseable file is
/// SKIPPED with a note rather than aborting the whole init run, and is never
/// overwritten.
fn write_vscode_settings(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let path = opts.target_dir.join(".vscode/settings.json");
    let existed = path.exists();

    let mut root: Value = if existed {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        match serde_json::from_str(&text) {
            Ok(root) => root,
            Err(_) => {
                report.entries.push(Entry::new(path, Outcome::Skipped));
                report.notes.push(
                    "`.vscode/settings.json` uses JSONC (comments/trailing commas) or is invalid, so init cannot merge into it; add the `moosedev.*` settings yourself (they all default to on — see clients/vscode/README.md)"
                        .to_string(),
                );
                return Ok(());
            }
        }
    } else {
        json!({})
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must be a JSON object", path.display()))?;

    report.notes.push(
        "VS Code needs the thin extension in `clients/vscode` of the MOOSEDev source checkout: `npm install && npm run package`, then `code --install-extension moosedev-<version>.vsix`."
            .to_string(),
    );
    if obj.keys().any(|key| key.starts_with("moosedev.")) && !opts.force {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        report.notes.push(
            "`.vscode/settings.json` already configures `moosedev.*`; left as-is (use --force to overwrite)"
                .to_string(),
        );
        return Ok(());
    }
    for (key, value) in vscode_settings_entries() {
        obj.insert(key.to_string(), value);
    }

    let dir = path.parent().expect("settings path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
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

/// The three Claude Code active-agency hooks: event name, matcher, script.
const CLAUDE_HOOKS: &[(&str, Option<&str>, &str, &str)] = &[
    (
        "PreToolUse",
        Some("Edit|Write|MultiEdit|NotebookEdit"),
        "moosedev-gate.sh",
        CLAUDE_GATE_HOOK,
    ),
    (
        "PostToolUse",
        Some("Read|Edit|Write"),
        "moosedev-push.sh",
        CLAUDE_PUSH_HOOK,
    ),
    ("Stop", None, "moosedev-capture.sh", CLAUDE_CAPTURE_HOOK),
];

/// Install the Claude Code adapter: write the three hook scripts (executable)
/// and merge their `hooks` registrations into `.claude/settings.json`, never
/// clobbering existing user hooks (the merge only appends missing MOOSEDev
/// groups). The scripts contain zero policy — they call the daemon over HTTP
/// and translate its verdict into the Claude Code hook contract.
fn write_claude_hooks(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    // 1. The scripts.
    let hooks_dir = opts.target_dir.join(".claude/hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("create {}", hooks_dir.display()))?;
    for (_, _, script, content) in CLAUDE_HOOKS {
        let path = hooks_dir.join(script);
        if path.exists() && !opts.force {
            report.entries.push(Entry::new(path, Outcome::Skipped));
            continue;
        }
        let existed = path.exists();
        std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        set_executable(&path)?;
        report.entries.push(Entry::new(
            path,
            if existed {
                Outcome::Merged
            } else {
                Outcome::Created
            },
        ));
    }

    // 2. The settings registration.
    let path = opts.target_dir.join(".claude/settings.json");
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
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} `hooks` must be an object", path.display()))?;

    let mut changed = false;
    for (event, matcher, script, _) in CLAUDE_HOOKS {
        let groups = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow::anyhow!("{} `hooks.{event}` must be an array", path.display())
            })?;
        let already = groups.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|hooks| {
                hooks
                    .iter()
                    .any(|h| h["command"].as_str().is_some_and(|c| c.contains(script)))
            })
        });
        if already {
            continue;
        }
        let mut group = serde_json::Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".to_string(), json!(matcher));
        }
        group.insert(
            "hooks".to_string(),
            json!([{
                "type": "command",
                "command": format!("\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/{script}"),
            }]),
        );
        groups.push(Value::Object(group));
        changed = true;
    }

    if !changed {
        report.entries.push(Entry::new(path, Outcome::Skipped));
        return Ok(());
    }
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
    report.notes.push(
        "Claude Code hooks installed: gate (PreToolUse), push (PostToolUse), capture (Stop). They need a running backend (`moosedev --serve`) plus `jq` and `curl`; without one they stay silent (fail-open)."
            .to_string(),
    );
    Ok(())
}

/// Git events whose new working-tree content should refresh the substrate:
/// commits, plus HEAD moves without one (pull/merge, checkout, rebase).
const REINDEX_HOOK_EVENTS: [&str; 3] = ["post-commit", "post-merge", "post-checkout"];

/// One shared async template for every re-index hook. Backgrounded so the git
/// operation never blocks on the full producer run; the daemon hot-reloads the
/// published index and live staleness covers the window in between. Concurrent
/// events serialize on this repository's `index.lock`; waiting is intentional,
/// because the later event may describe newer working-tree content and must not
/// be discarded.
const REINDEX_HOOK: &str = "#!/bin/sh\n\
# moosedev: refresh the code substrate in the background after this git event —\n\
# the daemon hot-reloads the new index when it lands. Concurrent events wait\n\
# on this repository's index lock so the newest tree is always indexed.\n\
nohup moosedev index >/dev/null 2>&1 &\n";

/// The exact bytes older `moosedev init` versions wrote (a synchronous run
/// that blocked each commit); safe to upgrade in place.
const LEGACY_SYNC_HOOK: &str =
    "#!/bin/sh\n# moosedev: refresh the code substrate after each commit\nmoosedev index || true\n";

/// Offer safe, project-local re-index hooks without changing user hook content.
fn offer_reindex_hooks(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let git_dir = opts.target_dir.join(".git");
    if !git_dir.is_dir() {
        report
            .notes
            .push("no .git directory found; re-index hooks were not installed".to_string());
        return Ok(());
    }

    for name in REINDEX_HOOK_EVENTS {
        let path = git_dir.join("hooks").join(name);
        let hook_exists = match std::fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => return Err(err).with_context(|| format!("inspect {}", path.display())),
        };
        if hook_exists {
            let content =
                std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            if content == LEGACY_SYNC_HOOK.as_bytes() {
                // Our own old synchronous template: upgrading in place is
                // non-clobbering because we authored those exact bytes.
                std::fs::write(&path, REINDEX_HOOK)
                    .with_context(|| format!("write {}", path.display()))?;
                set_executable(&path)?;
                report.entries.push(Entry::new(path, Outcome::Merged));
            } else if content
                .windows(b"moosedev index".len())
                .any(|part| part == b"moosedev index")
            {
                report.entries.push(Entry::new(path, Outcome::Skipped));
            } else {
                report.notes.push(format!(
                    "{name} hook exists without MOOSEDev; append this line manually: nohup moosedev index >/dev/null 2>&1 &"
                ));
            }
            continue;
        }

        let dir = path.parent().expect("hook path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        std::fs::write(&path, REINDEX_HOOK).with_context(|| format!("write {}", path.display()))?;
        set_executable(&path)?;
        report.entries.push(Entry::new(path, Outcome::Created));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("mark {} executable", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> anyhow::Result<()> {
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

/// Install agent skill `SKILL.md` files into the conventional auto-discovery
/// locations. Claude Code and opencode read `.claude/skills/<name>/SKILL.md`; Codex
/// reads `.agents/skills/<name>/SKILL.md`. The `.claude/skills/` tree is written on
/// every run; `.agents/skills/` is additionally written when `opts.codex`. Both are
/// non-clobbering unless `--force`. A summary note is pushed when at least one file
/// in `.claude/skills/` is (re-)written.
fn write_skills(opts: &InitOptions, report: &mut InitReport) -> anyhow::Result<()> {
    let mut claude_written = 0usize;
    let mut agents_written = false;

    for skill in SKILLS {
        let content = render_skill(skill);

        // Always install into .claude/skills/<name>/SKILL.md
        {
            let dir = opts
                .target_dir
                .join(".claude")
                .join("skills")
                .join(skill.name);
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            let path = dir.join("SKILL.md");
            let existed = path.exists();
            if existed && !opts.force {
                report.entries.push(Entry::new(path, Outcome::Skipped));
            } else {
                std::fs::write(&path, &content)
                    .with_context(|| format!("write {}", path.display()))?;
                report.entries.push(Entry::new(
                    path,
                    if existed {
                        Outcome::Merged
                    } else {
                        Outcome::Created
                    },
                ));
                claude_written += 1;
            }
        }

        // Additionally install into .agents/skills/<name>/SKILL.md when --codex
        if opts.codex {
            let dir = opts
                .target_dir
                .join(".agents")
                .join("skills")
                .join(skill.name);
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            let path = dir.join("SKILL.md");
            let existed = path.exists();
            if existed && !opts.force {
                report.entries.push(Entry::new(path, Outcome::Skipped));
            } else {
                std::fs::write(&path, &content)
                    .with_context(|| format!("write {}", path.display()))?;
                report.entries.push(Entry::new(
                    path,
                    if existed {
                        Outcome::Merged
                    } else {
                        Outcome::Created
                    },
                ));
                agents_written = true;
            }
        }
    }

    if claude_written > 0 {
        let mut note = format!(
            "installed {} agent skill(s) into .claude/skills/ — your agent can auto-discover them (e.g. ask it to bootstrap this repo's memory)",
            claude_written
        );
        if agents_written {
            note.push_str("; also installed into .agents/skills/ for Codex");
        }
        report.notes.push(note);
    }

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
            zed: false,
            vscode: false,
            claude_hooks: false,
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
        assert!(plugin.contains("journalCheckpoint(root, files, warnOnce)"));
        assert!(plugin.contains("isMooseDevStatePath(file)"));
        assert!(plugin.contains("resolve(root, dataDir, \"http.addr\")"));
        assert!(plugin.contains("/api/v1/health"));
        assert!(plugin.contains("realpathSync.native"));
        assert!(plugin.contains("redirect: \"manual\""));
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn zed_settings_created_fresh() {
        let target = temp_project("zed-fresh");
        let mut o = opts(&target);
        o.zed = true;
        let report = init_project(&o).unwrap();

        let settings = read_json(&target.join(".zed/settings.json"));
        assert_eq!(
            settings["lsp"]["moosedev"]["initialization_options"]["diagnostics"]["constraints"],
            true
        );
        assert_eq!(
            settings["lsp"]["moosedev"]["initialization_options"]["diagnostics"]["staleRationale"],
            true
        );
        assert_eq!(
            settings["code_lens"], "on",
            "init should enable Zed code lens (off by default) so badges render"
        );
        assert_eq!(
            outcome_for(&report, ".zed/settings.json"),
            Some(&Outcome::Created)
        );
        assert!(
            report.notes.iter().any(|note| note.contains("clients/zed")),
            "report should point to the dev extension"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn zed_settings_merge_preserves_existing() {
        let target = temp_project("zed-merge");
        let zed_dir = target.join(".zed");
        std::fs::create_dir_all(&zed_dir).unwrap();
        std::fs::write(
            zed_dir.join("settings.json"),
            r#"{"theme":"One Dark","lsp":{"rust-analyzer":{"x":1}}}"#,
        )
        .unwrap();
        let mut o = opts(&target);
        o.zed = true;
        let report = init_project(&o).unwrap();

        let settings = read_json(&zed_dir.join("settings.json"));
        assert_eq!(settings["theme"], "One Dark");
        assert_eq!(settings["lsp"]["rust-analyzer"]["x"], 1);
        assert_eq!(
            settings["lsp"]["moosedev"]["initialization_options"]["diagnostics"]["constraints"],
            true
        );
        assert_eq!(
            outcome_for(&report, ".zed/settings.json"),
            Some(&Outcome::Merged)
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn vscode_settings_created_fresh() {
        let target = temp_project("vscode-fresh");
        let mut o = opts(&target);
        o.vscode = true;
        let report = init_project(&o).unwrap();

        let settings = read_json(&target.join(".vscode/settings.json"));
        assert_eq!(settings["moosedev.diagnostics.constraints"], true);
        assert_eq!(settings["moosedev.diagnostics.staleRationale"], true);
        assert_eq!(
            outcome_for(&report, ".vscode/settings.json"),
            Some(&Outcome::Created)
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("clients/vscode")),
            "report should point to the extension packaging steps"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn vscode_settings_merge_preserves_existing() {
        let target = temp_project("vscode-merge");
        let vscode_dir = target.join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("settings.json"),
            r#"{"editor.formatOnSave":true,"rust-analyzer.checkOnSave":false}"#,
        )
        .unwrap();
        let mut o = opts(&target);
        o.vscode = true;
        let report = init_project(&o).unwrap();

        let settings = read_json(&vscode_dir.join("settings.json"));
        assert_eq!(settings["editor.formatOnSave"], true);
        assert_eq!(settings["rust-analyzer.checkOnSave"], false);
        assert_eq!(settings["moosedev.diagnostics.constraints"], true);
        assert_eq!(
            outcome_for(&report, ".vscode/settings.json"),
            Some(&Outcome::Merged)
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn vscode_settings_jsonc_is_skipped_not_fatal() {
        let target = temp_project("vscode-jsonc");
        let vscode_dir = target.join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        let jsonc = "{\n  // format on save\n  \"editor.formatOnSave\": true,\n}\n";
        std::fs::write(vscode_dir.join("settings.json"), jsonc).unwrap();
        let mut o = opts(&target);
        o.vscode = true;

        let report = init_project(&o).expect("JSONC settings must not abort init");
        assert_eq!(
            outcome_for(&report, ".vscode/settings.json"),
            Some(&Outcome::Skipped)
        );
        assert!(
            report.notes.iter().any(|note| note.contains("JSONC")),
            "the skip must be explained: {:?}",
            report.notes
        );
        assert_eq!(
            std::fs::read_to_string(vscode_dir.join("settings.json")).unwrap(),
            jsonc,
            "the user's JSONC file is left untouched"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn vscode_settings_existing_moosedev_skipped_without_force() {
        let target = temp_project("vscode-skip");
        let vscode_dir = target.join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("settings.json"),
            r#"{"moosedev.diagnostics.constraints":false}"#,
        )
        .unwrap();
        let mut o = opts(&target);
        o.vscode = true;
        let report = init_project(&o).unwrap();

        let settings = read_json(&vscode_dir.join("settings.json"));
        assert_eq!(
            settings["moosedev.diagnostics.constraints"], false,
            "an existing user value is preserved without --force"
        );
        assert_eq!(
            outcome_for(&report, ".vscode/settings.json"),
            Some(&Outcome::Skipped)
        );

        o.force = true;
        let report = init_project(&o).unwrap();
        let settings = read_json(&vscode_dir.join("settings.json"));
        assert_eq!(settings["moosedev.diagnostics.constraints"], true);
        assert_eq!(
            outcome_for(&report, ".vscode/settings.json"),
            Some(&Outcome::Merged)
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn claude_hooks_created_fresh() {
        let target = temp_project("hooks-fresh");
        let mut o = opts(&target);
        o.claude_hooks = true;
        let report = init_project(&o).unwrap();

        // The three scripts exist and are executable.
        for script in [
            "moosedev-gate.sh",
            "moosedev-push.sh",
            "moosedev-capture.sh",
        ] {
            let path = target.join(".claude/hooks").join(script);
            assert!(path.is_file(), "{script} written");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{script} is executable");
            }
            assert_eq!(outcome_for(&report, script), Some(&Outcome::Created));
        }

        let capture =
            std::fs::read_to_string(target.join(".claude/hooks/moosedev-capture.sh")).unwrap();
        assert!(!capture.contains("since_unix_seconds"));
        assert!(capture.contains("api/v1/capture"));
        assert!(capture.contains("exclude).moosedev/**"));

        // The settings registration carries all three events.
        let settings = read_json(&target.join(".claude/settings.json"));
        let gate = settings["hooks"]["PreToolUse"][0].clone();
        assert_eq!(gate["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert!(gate["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("moosedev-gate.sh"));
        assert!(settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("moosedev-push.sh"));
        let stop = settings["hooks"]["Stop"][0].clone();
        assert!(stop.get("matcher").is_none(), "Stop takes no matcher");
        assert!(stop["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("moosedev-capture.sh"));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn claude_hooks_merge_is_idempotent_and_preserves_user_hooks() {
        let target = temp_project("hooks-merge");
        let claude_dir = target.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"]},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-own-hook.sh"}]}]}}"#,
        )
        .unwrap();
        let mut o = opts(&target);
        o.claude_hooks = true;
        init_project(&o).unwrap();

        let settings = read_json(&claude_dir.join("settings.json"));
        // User content preserved; MOOSEDev group appended after it.
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "my-own-hook.sh"
        );
        assert!(settings["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("moosedev-gate.sh"));

        // Re-running init adds nothing (idempotent merge).
        let report = init_project(&o).unwrap();
        let settings = read_json(&claude_dir.join("settings.json"));
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            outcome_for(&report, ".claude/settings.json"),
            Some(&Outcome::Skipped)
        );
        assert_eq!(
            outcome_for(&report, "moosedev-gate.sh"),
            Some(&Outcome::Skipped),
            "existing scripts skipped without --force"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn zed_settings_existing_moosedev_skipped_without_force() {
        let target = temp_project("zed-existing");
        let zed_dir = target.join(".zed");
        std::fs::create_dir_all(&zed_dir).unwrap();
        let path = zed_dir.join("settings.json");
        std::fs::write(&path, r#"{"lsp":{"moosedev":{"custom":true}}}"#).unwrap();
        let mut o = opts(&target);
        o.zed = true;

        let report = init_project(&o).unwrap();
        assert_eq!(
            outcome_for(&report, ".zed/settings.json"),
            Some(&Outcome::Skipped)
        );
        assert_eq!(read_json(&path)["lsp"]["moosedev"]["custom"], true);

        o.force = true;
        let report = init_project(&o).unwrap();
        let settings = read_json(&path);
        assert_eq!(
            outcome_for(&report, ".zed/settings.json"),
            Some(&Outcome::Merged)
        );
        assert_eq!(
            settings["lsp"]["moosedev"]["initialization_options"]["diagnostics"]["staleRationale"],
            true
        );
        assert!(settings["lsp"]["moosedev"].get("custom").is_none());

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn reindex_hooks_created_when_missing() {
        let target = temp_project("hook-fresh");
        std::fs::create_dir_all(target.join(".git/hooks")).unwrap();

        let report = init_project(&opts(&target)).unwrap();
        for name in REINDEX_HOOK_EVENTS {
            let path = target.join(".git/hooks").join(name);
            let hook = std::fs::read_to_string(&path).unwrap();
            assert_eq!(hook, REINDEX_HOOK, "{name}");
            assert!(!hook.contains("--if-idle"), "{name}: {hook}");
            assert!(!hook.contains("pgrep"), "{name}: {hook}");
            assert_eq!(
                outcome_for(&report, &format!(".git/hooks/{name}")),
                Some(&Outcome::Created),
                "{name}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_ne!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                    0
                );
            }
        }

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn reindex_hooks_existing_untouched() {
        let target = temp_project("hook-existing");
        let hook_dir = target.join(".git/hooks");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let path = hook_dir.join("post-commit");
        let custom = b"#!/bin/sh\necho custom\n";
        std::fs::write(&path, custom).unwrap();

        let report = init_project(&opts(&target)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), custom);
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("post-commit hook exists without MOOSEDev")));
        assert!(outcome_for(&report, ".git/hooks/post-commit").is_none());
        // A foreign hook on one event never blocks installing the others.
        assert_eq!(
            outcome_for(&report, ".git/hooks/post-merge"),
            Some(&Outcome::Created)
        );
        assert_eq!(
            outcome_for(&report, ".git/hooks/post-checkout"),
            Some(&Outcome::Created)
        );

        let existing = b"#!/bin/sh\nmoosedev index --custom-flags || true\n";
        std::fs::write(&path, existing).unwrap();
        let report = init_project(&opts(&target)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), existing);
        assert_eq!(
            outcome_for(&report, ".git/hooks/post-commit"),
            Some(&Outcome::Skipped)
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn legacy_sync_post_commit_upgraded_in_place() {
        let target = temp_project("hook-legacy");
        let hook_dir = target.join(".git/hooks");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let path = hook_dir.join("post-commit");
        std::fs::write(&path, LEGACY_SYNC_HOOK).unwrap();

        let report = init_project(&opts(&target)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), REINDEX_HOOK.as_bytes());
        assert_eq!(
            outcome_for(&report, ".git/hooks/post-commit"),
            Some(&Outcome::Merged)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                0
            );
        }

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn no_git_dir_skips_hook() {
        let target = temp_project("hook-no-git");
        let report = init_project(&opts(&target)).unwrap();

        assert!(!target.join(".git/hooks/post-commit").exists());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("no .git directory")));

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

    #[test]
    fn installs_skills_into_dot_claude() {
        let target = temp_project("skills-claude");
        let report = init_project(&opts(&target)).unwrap();

        let skill_path = target.join(".claude/skills/bootstrap-existing-codebase/SKILL.md");
        assert!(
            skill_path.exists(),
            "bootstrap skill file should be created"
        );

        let content = std::fs::read_to_string(&skill_path).unwrap();
        assert!(
            content.contains("description:"),
            "SKILL.md should contain YAML frontmatter with `description:`"
        );
        assert!(
            content.contains("Bootstrap"),
            "SKILL.md should contain body text from the source doc"
        );

        // Without --codex, .agents/skills/ must NOT be written.
        assert!(
            !target
                .join(".agents/skills/bootstrap-existing-codebase/SKILL.md")
                .exists(),
            ".agents/skills/ must not be written without --codex"
        );

        assert_eq!(
            outcome_for(&report, "bootstrap-existing-codebase/SKILL.md"),
            Some(&Outcome::Created)
        );
        assert!(
            report.notes.iter().any(|n| n.contains(".claude/skills/")),
            "report should include a note about installed skills"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn codex_also_installs_agents_skills() {
        let target = temp_project("skills-codex");
        let mut o = opts(&target);
        o.codex = true;
        init_project(&o).unwrap();

        let agents_path = target.join(".agents/skills/bootstrap-existing-codebase/SKILL.md");
        assert!(
            agents_path.exists(),
            ".agents/skills/ should be written when --codex is set"
        );
        let content = std::fs::read_to_string(&agents_path).unwrap();
        assert!(
            content.contains("description:"),
            "agents skill file should contain YAML frontmatter"
        );

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn skills_are_idempotent() {
        let target = temp_project("skills-idem");
        let o = opts(&target);
        init_project(&o).unwrap();
        let report = init_project(&o).unwrap();

        // Second run must report Skipped, not Created.
        assert_eq!(
            outcome_for(&report, "bootstrap-existing-codebase/SKILL.md"),
            Some(&Outcome::Skipped),
            "second run should skip an already-installed skill"
        );
        // Content must still be valid.
        let content = std::fs::read_to_string(
            target.join(".claude/skills/bootstrap-existing-codebase/SKILL.md"),
        )
        .unwrap();
        assert!(content.contains("description:"));

        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn skills_force_overwrites_existing() {
        let target = temp_project("skills-force");

        // Pre-place a sentinel SKILL.md so we can detect whether it is clobbered.
        let skill_dir = target.join(".claude/skills/bootstrap-existing-codebase");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "SENTINEL").unwrap();

        // Without --force: sentinel must be preserved (Skipped).
        let report = init_project(&opts(&target)).unwrap();
        assert_eq!(
            outcome_for(&report, "bootstrap-existing-codebase/SKILL.md"),
            Some(&Outcome::Skipped),
            "without --force the existing SKILL.md should be skipped"
        );
        let content = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(
            content.contains("SENTINEL"),
            "sentinel must be preserved without --force"
        );

        // With --force: sentinel must be overwritten (Merged).
        let mut o = opts(&target);
        o.force = true;
        let report = init_project(&o).unwrap();
        assert_eq!(
            outcome_for(&report, "bootstrap-existing-codebase/SKILL.md"),
            Some(&Outcome::Merged),
            "with --force the existing SKILL.md should be overwritten"
        );
        let content = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(
            !content.contains("SENTINEL"),
            "sentinel must be gone after --force"
        );
        assert!(
            content.contains("description:"),
            "valid SKILL.md content expected after --force overwrite"
        );

        let _ = std::fs::remove_dir_all(&target);
    }
}
