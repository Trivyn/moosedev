//! Temporal (git-walk) bootstrap: replay a repo's trunk history oldest→newest
//! into a MOOSEDev graph with real supersession chains, a real timeline, and
//! real provenance.
//!
//! Native Rust port of `bench/temporal_bootstrap.py` (the executable spec).

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

// ── public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum Agent {
    Claude,
    Codex,
}

pub struct BootstrapArgs {
    pub temporal: bool,
    pub repo: PathBuf,
    pub data_dir: String,
    pub trunk: Option<String>,
    pub resume: bool,
    pub agent: Agent,
    pub model: Option<String>,
    pub limit: Option<usize>,
    pub dry_run: bool,
    pub milestone_every: usize,
    /// Resolved by main.rs via ontology_dir()
    pub ontology_dir: PathBuf,
    /// Resolved by main.rs via skills_dir() + "temporal-episode-capture.md"
    pub skill_path: Option<PathBuf>,
}

// ── Episode ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Episode {
    pub sha: String,
    pub author: String,
    pub email: String,
    /// %aI — RFC3339 author date; passed verbatim as the record timestamp.
    pub date: String,
    pub subject: String,
    pub body: String,
    pub is_merge: bool,
}

// ── git helpers ───────────────────────────────────────────────────────────────

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("spawn git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    // errors="replace" equivalent: lossy UTF-8 (some repos have binary diffs)
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_ok(repo: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

// ── trunk detection ───────────────────────────────────────────────────────────

/// Detect the trunk branch: explicit override → origin/HEAD symbolic-ref →
/// first of main/master/develop/dev/trunk that resolves.
pub fn detect_trunk(repo: &Path, override_: Option<&str>) -> anyhow::Result<String> {
    if let Some(t) = override_ {
        return Ok(t.to_string());
    }
    if let Some(ref_) = git_ok(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        let ref_ = ref_.trim();
        if !ref_.is_empty() {
            if let Some(branch) = ref_.rsplit('/').next() {
                if !branch.is_empty() {
                    return Ok(branch.to_string());
                }
            }
        }
    }
    for name in &["main", "master", "develop", "dev", "trunk"] {
        if git_ok(repo, &["rev-parse", "--verify", name]).is_some() {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!(
        "could not detect trunk for {}; pass --trunk",
        repo.display()
    )
}

// ── enumerate ─────────────────────────────────────────────────────────────────

const US: char = '\x1f'; // unit separator — field delimiter
const RS: char = '\x1e'; // record separator — ends each commit record

/// First-parent trunk history oldest→newest. `--first-parent` excludes commits
/// on branches never merged into trunk; merged work enters via its merge commit.
pub fn enumerate(repo: &Path, trunk: &str) -> anyhow::Result<Vec<Episode>> {
    // Build the --format= string matching the Python driver's _FMT.
    let fmt = format!("--format=%H{US}%an{US}%ae{US}%aI{US}%P{US}%s{US}%b{RS}");
    let raw = git(repo, &["log", "--first-parent", "--reverse", &fmt, trunk])?;
    let mut eps = Vec::new();
    for rec in raw.split(RS) {
        let rec = rec.trim_matches('\n');
        if rec.is_empty() {
            continue;
        }
        // Split on US into exactly 7 fields: sha, author, email, date, parents, subject, body.
        let parts: Vec<&str> = rec.splitn(7, US).collect();
        if parts.len() < 7 {
            continue; // malformed record; skip
        }
        let (sha, author, email, date, parents, subject, body) = (
            parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6],
        );
        let is_merge = parents.split_whitespace().count() > 1;
        eps.push(Episode {
            sha: sha.trim().to_string(),
            author: author.to_string(),
            email: email.to_string(),
            date: date.trim().to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            is_merge,
        });
    }
    Ok(eps)
}

// ── episode diff ──────────────────────────────────────────────────────────────

/// Get the diff for one episode (merge → explicit diff; else git-show).
/// Returns (diff_text, file_list). Truncates to ~24 000 bytes.
pub fn episode_diff(repo: &Path, ep: &Episode) -> anyhow::Result<(String, Vec<String>)> {
    let raw = if ep.is_merge {
        git(repo, &["diff", &format!("{}^1", ep.sha), &ep.sha])?
    } else {
        git(repo, &["show", "--first-parent", "--format=", &ep.sha])?
    };

    let files: Vec<String> = raw
        .lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(String::from)
        .collect();

    const MAX_BYTES: usize = 24_000;
    let diff = if raw.len() > MAX_BYTES {
        // Truncate on a byte boundary and re-validate UTF-8.
        let mut cut = raw.as_bytes()[..MAX_BYTES].to_vec();
        // Walk back to a valid UTF-8 boundary.
        while !cut.is_empty() && std::str::from_utf8(&cut).is_err() {
            cut.pop();
        }
        let mut s = String::from_utf8(cut).unwrap_or_default();
        s.push_str(&format!(
            "\n\n[TRUNCATED to {MAX_BYTES} bytes; {} files changed]",
            files.len()
        ));
        s
    } else {
        raw
    };

    Ok((diff, files))
}

// ── triage ────────────────────────────────────────────────────────────────────

fn is_skip_subject(subject: &str) -> bool {
    let s = subject.to_lowercase();
    let s = s.trim();
    // Fixed-prefix patterns (case-insensitive, mirroring _SKIP_SUBJECT in the Python).
    let prefixes: &[&str] = &[
        "merge branch",
        "merge remote",
        "merge pull",
        "bump ",
        "chore(deps)",
        "cargo fmt",
        "rustfmt",
        "gofmt",
        "prettier",
        "fmt:",
        "typo",
        "fix typo",
        "version bump",
        "update changelog",
        "update lockfile",
        "clippy",
    ];
    for p in prefixes {
        if s.starts_with(p) {
            return true;
        }
    }
    // fmt$ — exactly "fmt"
    if s == "fmt" {
        return true;
    }
    // wip\b — starts with "wip" not followed by an alphanumeric/underscore char
    if let Some(rest) = s.strip_prefix("wip") {
        if rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return true;
        }
    }
    // release v?\d — "release " then optional "v" then an ASCII digit
    if let Some(rest) = s.strip_prefix("release ") {
        let rest = rest.strip_prefix('v').unwrap_or(rest);
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn is_noise_path(path: &str) -> bool {
    let contains_patterns = [
        "Cargo.lock",
        "package-lock.json",
        "go.sum",
        "bun.lock",
        "__pycache__",
    ];
    let ends_patterns = [".gitignore", ".snap"];
    let starts_patterns = [".github/", "vendor/", "node_modules/"];
    for p in &contains_patterns {
        if path.contains(p) {
            return true;
        }
    }
    for p in &ends_patterns {
        if path.ends_with(p) {
            return true;
        }
    }
    for p in &starts_patterns {
        if path.starts_with(p) {
            return true;
        }
    }
    false
}

/// Conservative why-cue detector (favor SEND — false positives here mean we
/// send rather than skip, which is the correct direction).
fn has_why_cue(text: &str) -> bool {
    let t = text.to_lowercase();
    let phrases: &[&str] = &[
        "because",
        "so that",
        "in order to",
        "instead of",
        "we tried",
        "trade-off",
        "tradeoff",
        "never",
        "must not",
        "revert",
        "superse", // supersede/supersedes/superseded
        "replace",
        "no longer",
        "deprecat", // deprecate/deprecated/deprecating
        "remove",
        "drop",
    ];
    phrases.iter().any(|p| t.contains(p))
}

/// Minimum diff lines (added + removed) below which a no-why-cue episode is skipped.
const TB_MIN_LINES: usize = 2;

/// Returns Some(reason) to skip, or None to send. Conservative — favor SEND.
/// Merges are NEVER skipped (feature decisions land at the merge commit).
pub fn triage(ep: &Episode, diff: &str, files: &[String]) -> Option<String> {
    if files.is_empty() {
        return Some("empty-diff".into());
    }
    if ep.is_merge {
        return None; // always SEND non-empty merges
    }
    let combined = format!("{}\n{}", ep.subject, ep.body);
    let why = has_why_cue(&combined);
    if is_skip_subject(&ep.subject) && ep.body.trim().len() < 40 && !why {
        let abbrev: String = ep.subject.chars().take(40).collect();
        return Some(format!("mechanical:{abbrev}"));
    }
    if files.iter().all(|f| is_noise_path(f)) {
        return Some("all-noise-paths".into());
    }
    let added = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    if added + removed <= TB_MIN_LINES && !why {
        return Some("tiny-no-why-cue".into());
    }
    None
}

// ── backend lifecycle ─────────────────────────────────────────────────────────

struct Backend {
    child: std::process::Child,
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_socket_live(path: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

fn start_backend(
    data_dir: &str,
    ontology_dir: &Path,
    ts_file: &Path,
    author_file: &Path,
) -> anyhow::Result<Backend> {
    let data_dir_path = PathBuf::from(data_dir);
    fs::create_dir_all(&data_dir_path)
        .map_err(|e| anyhow::anyhow!("create data dir {data_dir}: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("resolve current exe: {e}"))?;

    // Pass an ABSOLUTE ontology dir: the child inherits our cwd, so a relative
    // MOOSEDEV_ONTOLOGY_DIR (e.g. from a repo `.env`) would resolve against the
    // wrong directory and the backend would fail to load its ontologies.
    let ontology_abs = ontology_dir
        .canonicalize()
        .unwrap_or_else(|_| ontology_dir.to_path_buf());

    // Capture the backend's stderr so an early-exit failure is diagnosable —
    // otherwise a bare "exited early (code 1)" hides the real cause.
    let serve_log = std::env::temp_dir().join("moosedev-temporal-serve.log");
    let stderr_to = std::fs::File::create(&serve_log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());

    let mut child = Command::new(&exe)
        .arg("--serve")
        .env("MOOSEDEV_DATA_DIR", data_dir)
        .env("MOOSEDEV_ONTOLOGY_DIR", &ontology_abs)
        .env("MOOSEDEV_CAPTURE_TS_FILE", ts_file)
        .env("MOOSEDEV_CAPTURE_AUTHOR_FILE", author_file)
        .env("MOOSEDEV_NO_HTTP", "1")
        .stdout(Stdio::null())
        .stderr(stderr_to)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn moosedev --serve: {e}"))?;

    // Poll for the socket to become connectable (up to ~120 s for a fresh
    // store's first vector-index build).
    let socket = moosedev::runtime::socket_path_for(&data_dir_path);
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if is_socket_live(&socket) {
            return Ok(Backend { child });
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log = fs::read_to_string(&serve_log).unwrap_or_default();
            let tail = log
                .lines()
                .rev()
                .take(10)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "moosedev --serve exited early (code {:?}):\n{tail}",
                status.code()
            );
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "moosedev --serve did not become ready (socket never appeared after 120s)"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ── MCP config (agent-side proxy) ─────────────────────────────────────────────

fn write_mcp_config(data_dir: &str, ontology_dir: &Path) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("resolve current exe: {e}"))?;
    let cfg = serde_json::json!({
        "mcpServers": {
            "moosedev": {
                "command": exe.to_string_lossy(),
                "args": ["--connect"],
                "env": {
                    "MOOSEDEV_DATA_DIR": data_dir,
                    "MOOSEDEV_ONTOLOGY_DIR": ontology_dir.to_string_lossy().as_ref(),
                    "MOOSEDEV_NO_AUTOSPAWN": "1"
                }
            }
        }
    });
    let path = std::env::temp_dir().join("moosedev-temporal-mcp.json");
    fs::write(&path, serde_json::to_string_pretty(&cfg)?)
        .map_err(|e| anyhow::anyhow!("write mcp config {}: {e}", path.display()))?;
    Ok(path)
}

/// Build the `-c mcp_servers.moosedev.*` TOML override args for codex, mirroring
/// `_codex_moosedev_overrides` in the Python driver.
fn codex_moosedev_overrides(data_dir: &str, ontology_dir: &Path) -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "moosedev".to_string());
    let toml_args = r#"["--connect"]"#;
    let toml_env = format!(
        r#"{{ MOOSEDEV_DATA_DIR = "{}", MOOSEDEV_ONTOLOGY_DIR = "{}", MOOSEDEV_NO_AUTOSPAWN = "1" }}"#,
        data_dir,
        ontology_dir.display()
    );
    vec![
        "-c".to_string(),
        format!(r#"mcp_servers.moosedev.command="{exe}""#),
        "-c".to_string(),
        format!("mcp_servers.moosedev.args={toml_args}"),
        "-c".to_string(),
        format!("mcp_servers.moosedev.env={toml_env}"),
    ]
}

// ── per-episode prompt ────────────────────────────────────────────────────────

fn episode_prompt(ep: &Episode, diff: &str, repo_name: &str, skill_path: Option<&Path>) -> String {
    let skill_ref = match skill_path {
        Some(p) => p.display().to_string(),
        None => "skills/temporal-episode-capture.md (locate it via `moosedev skills`)".to_string(),
    };
    let body = ep.body.trim();
    let body_display = if body.is_empty() { "(none)" } else { body };
    let sha_short = &ep.sha[..10.min(ep.sha.len())];
    format!(
        "You are the temporal-bootstrap capture agent for commit {sha_short} of `{repo_name}`.\n\
         \n\
         Read {skill_ref} and follow it EXACTLY for THIS single commit. A `moosedev` MCP \
         server is attached to the shared in-progress store; it holds only decisions from \
         EARLIER commits, so recall it first.\n\
         \n\
         COMMIT {sha}\n\
         AUTHOR: {author} <{email}>\n\
         DATE: {date}\n\
         SUBJECT: {subject}\n\
         BODY:\n\
         {body_display}\n\
         \n\
         DIFF:\n\
         {diff}\n\
         \n\
         The record timestamp + author are set automatically to this commit's values by the \
         driver — record normally, do NOT pass timestamp/author yourself. If this commit \
         reverses a decision you find in recall, supersede that existing IRI (do not invent). \
         If it is not decision-bearing, record nothing and say so. End with the skill's report.",
        sha = ep.sha,
        author = ep.author,
        email = ep.email,
        date = ep.date,
        subject = ep.subject,
    )
}

// ── agent spawn ───────────────────────────────────────────────────────────────

const AGENT_TIMEOUT_SECS: u64 = 900;

/// Poll-based wait with a timeout; returns true on success.
fn wait_with_timeout(child: &mut std::process::Child, timeout_secs: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    eprintln!("[capture agent TIMED OUT after {timeout_secs}s]");
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                eprintln!("[capture agent wait error: {e}]");
                return false;
            }
        }
    }
}

fn spawn_claude(prompt: &str, mcp_config: &Path, skill_dir: Option<&Path>, model: &str) -> bool {
    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        prompt,
        "--mcp-config",
        &mcp_config.to_string_lossy(),
        // Use ONLY our --mcp-config backend — never a project .mcp.json's own
        // `moosedev` server, which would send captures into the wrong store.
        "--strict-mcp-config",
        "--model",
        model,
        "--dangerously-skip-permissions",
    ]);
    // The skill doc lives in MOOSEDev's skills dir (not the target repo — the diff
    // is inlined in the prompt), so grant the agent read access to it.
    if let Some(dir) = skill_dir {
        cmd.arg("--add-dir").arg(dir);
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => wait_with_timeout(&mut child, AGENT_TIMEOUT_SECS),
        Err(e) => {
            eprintln!("failed to spawn claude: {e}");
            false
        }
    }
}

fn spawn_codex(
    prompt: &str,
    data_dir: &str,
    ontology_dir: &Path,
    skill_dir: Option<&Path>,
    repo: &Path,
    model: &str,
) -> bool {
    let overrides = codex_moosedev_overrides(data_dir, ontology_dir);
    let mut cmd = Command::new("codex");
    cmd.args([
        "exec",
        "-m",
        model,
        "--dangerously-bypass-approvals-and-sandbox",
        "--skip-git-repo-check",
    ]);
    for o in &overrides {
        cmd.arg(o);
    }
    cmd.arg(prompt)
        // Run where the skill doc is readable (MOOSEDev's skills dir); fall back to
        // the target repo. The diff is inlined, so the target repo isn't required.
        .current_dir(skill_dir.unwrap_or(repo))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => wait_with_timeout(&mut child, AGENT_TIMEOUT_SECS),
        Err(e) => {
            eprintln!("failed to spawn codex: {e}");
            false
        }
    }
}

// ── PATH probe ────────────────────────────────────────────────────────────────

fn is_on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

fn resolve_model(agent: Agent, model: Option<&str>) -> String {
    model.map(str::to_string).unwrap_or_else(|| match agent {
        Agent::Claude => "sonnet".to_string(),
        Agent::Codex => "gpt-5.4-mini".to_string(),
    })
}

// ── main entry point ──────────────────────────────────────────────────────────

/// Run the temporal bootstrap. Called from `main.rs`.
/// `args.temporal` must be true — `parse_bootstrap` guards this, but we check
/// here too so `BootstrapArgs` is usable without `parse_bootstrap`.
pub fn run(args: BootstrapArgs) -> anyhow::Result<()> {
    if !args.temporal {
        println!(
            "Snapshot bootstrap is an interactive agent skill — run `moosedev skills` to find it."
        );
        println!("For temporal git-walk bootstrap (replay git history), use: moosedev bootstrap --temporal");
        return Ok(());
    }
    let trunk = detect_trunk(&args.repo, args.trunk.as_deref())?;
    // Canonicalize so a relative `--repo .` still yields a real directory name
    // (e.g. "moosedev") in the header, not "." — falls back to the raw path.
    let repo_name = args
        .repo
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| args.repo.display().to_string());

    let mut episodes = enumerate(&args.repo, &trunk)?;
    if let Some(limit) = args.limit {
        episodes.truncate(limit);
    }
    let n = episodes.len();
    println!(
        "trunk={trunk}  episodes={n}  repo={repo_name}  store={}",
        args.data_dir
    );

    let applied_log = PathBuf::from(&args.data_dir).join("temporal-applied.log");
    let done: HashSet<String> = if args.resume && applied_log.exists() {
        fs::read_to_string(&applied_log)
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect()
    } else {
        HashSet::new()
    };

    // ── dry-run path ─────────────────────────────────────────────────────────
    if args.dry_run {
        let mut sent = 0usize;
        let mut skipped = 0usize;
        for (i, ep) in episodes.iter().enumerate() {
            let idx = i + 1;
            let (diff, files) = episode_diff(&args.repo, ep)?;
            let reason = triage(ep, &diff, &files);
            if let Some(reason) = reason {
                skipped += 1;
                println!("[{idx}/{n}] SKIP {} ({reason})", &ep.sha[..8]);
            } else {
                sent += 1;
                let tag = if ep.is_merge { "merge" } else { "commit" };
                let subj: String = ep.subject.chars().take(66).collect();
                println!("[{idx}/{n}] SEND {} [{tag}] {subj}", &ep.sha[..8]);
            }
        }
        println!("\nDRY RUN: would send {sent}, skip {skipped} (of {n})");
        return Ok(());
    }

    // ── live run: pre-flight checks ──────────────────────────────────────────
    let data_dir_path = PathBuf::from(&args.data_dir);
    fs::create_dir_all(&data_dir_path)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", args.data_dir))?;
    let socket = moosedev::runtime::socket_path_for(&data_dir_path);
    if is_socket_live(&socket) {
        anyhow::bail!(
            "a MOOSEDev backend is already running on {}; stop it first — \
             the temporal walk needs exclusive single-writer access",
            socket.display()
        );
    }

    let agent_bin = match args.agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    };
    if !is_on_path(agent_bin) {
        anyhow::bail!(
            "{agent_bin} CLI is not on PATH; install it before running temporal bootstrap"
        );
    }

    // ── capture-file temps ───────────────────────────────────────────────────
    let ts_file = std::env::temp_dir().join("moosedev-temporal-ts");
    let author_file = std::env::temp_dir().join("moosedev-temporal-author");

    // ── MCP config (written once before the loop) ────────────────────────────
    let mcp_config_path = match args.agent {
        Agent::Claude => Some(write_mcp_config(&args.data_dir, &args.ontology_dir)?),
        Agent::Codex => None,
    };

    // ── start backend ────────────────────────────────────────────────────────
    eprintln!("starting MOOSEDev backend for {} …", args.data_dir);
    let _backend = start_backend(&args.data_dir, &args.ontology_dir, &ts_file, &author_file)?;
    eprintln!("backend ready");

    let model = resolve_model(args.agent, args.model.as_deref());
    // The capture agent must read `temporal-episode-capture.md`, which ships in
    // MOOSEDev's skills dir — grant access to that dir, not the target repo.
    let skill_dir: Option<PathBuf> = args
        .skill_path
        .as_deref()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    if skill_dir.is_none() {
        eprintln!(
            "warning: temporal-episode-capture skill not found — captures will lack the workflow doc"
        );
    }

    // ── per-commit loop ───────────────────────────────────────────────────────
    let mut sent = 0usize;
    let mut applied = 0usize;
    let mut skipped = 0usize;

    for (i, ep) in episodes.iter().enumerate() {
        let idx = i + 1;
        if done.contains(&ep.sha) {
            continue;
        }
        let (diff, files) = episode_diff(&args.repo, ep)?;
        let reason = triage(ep, &diff, &files);
        if let Some(reason) = reason {
            skipped += 1;
            println!("[{idx}/{n}] SKIP {} ({reason})", &ep.sha[..8]);
            continue;
        }
        sent += 1;

        // Inject commit date + author for the backend's capture defaults.
        // resolve_when / resolve_author in mcp/mod.rs read these files per write,
        // so the timeline is deterministic — never the LLM's responsibility.
        fs::write(&ts_file, &ep.date).map_err(|e| anyhow::anyhow!("write ts_file: {e}"))?;
        fs::write(&author_file, format!("{} <{}>", ep.author, ep.email))
            .map_err(|e| anyhow::anyhow!("write author_file: {e}"))?;

        let prompt = episode_prompt(ep, &diff, &repo_name, args.skill_path.as_deref());

        let success = match args.agent {
            Agent::Claude => spawn_claude(
                &prompt,
                mcp_config_path.as_ref().expect("mcp_config set for claude"),
                skill_dir.as_deref(),
                &model,
            ),
            Agent::Codex => spawn_codex(
                &prompt,
                &args.data_dir,
                &args.ontology_dir,
                skill_dir.as_deref(),
                &args.repo,
                &model,
            ),
        };

        if success {
            applied += 1;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&applied_log)
                .map_err(|e| anyhow::anyhow!("open applied log: {e}"))?;
            writeln!(f, "{}", ep.sha).map_err(|e| anyhow::anyhow!("write applied log: {e}"))?;
            println!("[{idx}/{n}] DONE {}", &ep.sha[..8]);
        } else {
            println!(
                "[{idx}/{n}] FAIL {} (agent returned non-zero or timed out)",
                &ep.sha[..8]
            );
        }

        if args.milestone_every > 0 && applied > 0 && applied.is_multiple_of(args.milestone_every) {
            println!("    milestone: {applied} applied so far");
        }
    }

    println!("\nsummary: sent={sent} applied={applied} skipped={skipped} (of {n})");
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ep(subject: &str, body: &str, is_merge: bool) -> Episode {
        Episode {
            sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            author: "Test Author".to_string(),
            email: "test@example.com".to_string(),
            date: "2024-01-01T10:00:00+00:00".to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            is_merge,
        }
    }

    // ── enumerate parsing ─────────────────────────────────────────────────────

    fn parse_fixture(fixture: &str) -> Vec<Episode> {
        // Reimplements the parsing logic from `enumerate` for unit testing
        // without needing a real git repo.
        fixture
            .split(RS)
            .filter_map(|rec| {
                let rec = rec.trim_matches('\n');
                if rec.is_empty() {
                    return None;
                }
                let parts: Vec<&str> = rec.splitn(7, US).collect();
                if parts.len() < 7 {
                    return None;
                }
                let is_merge = parts[4].split_whitespace().count() > 1;
                Some(Episode {
                    sha: parts[0].trim().to_string(),
                    author: parts[1].to_string(),
                    email: parts[2].to_string(),
                    date: parts[3].trim().to_string(),
                    subject: parts[5].to_string(),
                    body: parts[6].to_string(),
                    is_merge,
                })
            })
            .collect()
    }

    #[test]
    fn enumerate_parses_fixture_correctly() {
        let fixture = format!(
            "aaaa1111{US}Alice Smith{US}alice@example.com{US}2024-03-01T09:00:00+00:00\
             {US}prev1111{US}Add feature X{US}Implements the X widget{RS}\
             bbbb2222{US}Bob Jones{US}bob@example.com{US}2024-03-02T11:00:00+00:00\
             {US}prev2222 prev3333{US}Merge branch feature/y{US}{RS}"
        );
        let eps = parse_fixture(&fixture);
        assert_eq!(eps.len(), 2, "should parse exactly 2 episodes");

        assert_eq!(eps[0].sha, "aaaa1111");
        assert_eq!(eps[0].author, "Alice Smith");
        assert_eq!(eps[0].email, "alice@example.com");
        assert_eq!(eps[0].date, "2024-03-01T09:00:00+00:00");
        assert_eq!(eps[0].subject, "Add feature X");
        assert_eq!(eps[0].body, "Implements the X widget");
        assert!(!eps[0].is_merge);

        assert_eq!(eps[1].sha, "bbbb2222");
        assert!(eps[1].is_merge, "two parents → is_merge");
    }

    #[test]
    fn enumerate_skips_empty_records() {
        let fixture = format!("\n{RS}abcd1234{US}A{US}a@b.com{US}2024-01-01T00:00:00+00:00{US}p1{US}Fix bug{US}body{RS}\n");
        let eps = parse_fixture(&fixture);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].sha, "abcd1234");
    }

    #[test]
    fn enumerate_body_with_newlines_stays_intact() {
        // Multi-line body is the 7th field — splitn(7) keeps it whole.
        let fixture = format!(
            "sha1{US}Au{US}au@e.com{US}2024-01-01T00:00:00+00:00{US}p1{US}Subject{US}Line one\nLine two\nLine three{RS}"
        );
        let eps = parse_fixture(&fixture);
        assert_eq!(eps.len(), 1);
        assert!(
            eps[0].body.contains("Line two"),
            "multi-line body preserved"
        );
    }

    // ── triage ────────────────────────────────────────────────────────────────

    fn src_files() -> Vec<String> {
        vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
    }

    fn big_diff() -> String {
        // 10 changed lines — above TB_MIN_LINES
        let mut d = "+++ b/src/lib.rs\n".to_string();
        for _ in 0..5 {
            d.push_str("+added line\n");
            d.push_str("-removed line\n");
        }
        d
    }

    fn small_diff() -> String {
        "+++ b/src/lib.rs\n+one\n-one\n+two\n-two\n".to_string()
    }

    #[test]
    fn triage_sends_decision_bearing_commit() {
        let ep = make_ep(
            "Introduce new auth strategy",
            "We chose JWT instead of sessions because sessions don't scale horizontally.",
            false,
        );
        assert!(
            triage(&ep, &big_diff(), &src_files()).is_none(),
            "decision-bearing commit should SEND"
        );
    }

    #[test]
    fn triage_skips_mechanical_cargo_fmt() {
        let ep = make_ep("cargo fmt", "", false);
        let reason = triage(&ep, &big_diff(), &src_files());
        assert!(reason.is_some(), "cargo fmt should be SKIP");
        assert!(reason.unwrap().starts_with("mechanical"));
    }

    #[test]
    fn triage_skips_empty_diff() {
        let ep = make_ep("Some commit", "", false);
        assert_eq!(triage(&ep, "", &[]).as_deref(), Some("empty-diff"));
    }

    #[test]
    fn triage_never_skips_merge() {
        let ep = make_ep("Merge branch 'cargo-fmt'", "", true);
        assert!(
            triage(&ep, &big_diff(), &src_files()).is_none(),
            "merge should never be skipped"
        );
    }

    #[test]
    fn triage_skips_all_noise_paths() {
        let ep = make_ep("Update lockfile", "", false);
        let diff = "+++ b/Cargo.lock\n+a = \"1.0\"\n-a = \"0.9\"\n+b\n-b\n+c\n-c\n".to_string();
        let files = vec!["Cargo.lock".to_string()];
        let reason = triage(&ep, &diff, &files);
        assert!(reason.is_some(), "lockfile-only commit should be SKIP");
    }

    #[test]
    fn triage_skips_tiny_no_why_cue() {
        let ep = make_ep("Minor cleanup", "", false);
        // Exactly TB_MIN_LINES (2 adds + 2 removes = 4 > 2) — wait, let me count:
        // small_diff has +one, -one, +two, -two = 4 changed lines > TB_MIN_LINES(2)
        // Let me use a diff with just 2 changed lines:
        let diff = "+++ b/src/lib.rs\n+one\n-one\n".to_string();
        assert_eq!(
            triage(&ep, &diff, &src_files()).as_deref(),
            Some("tiny-no-why-cue")
        );
    }

    #[test]
    fn triage_sends_tiny_with_why_cue() {
        let ep = make_ep(
            "Remove deprecated path",
            "We remove this because the old API is deprecated.",
            false,
        );
        // Only 2 lines — but has why-cue ("remove", "because", "deprecated")
        let diff = "+++ b/src/lib.rs\n+one\n-one\n".to_string();
        assert!(
            triage(&ep, &diff, &src_files()).is_none(),
            "tiny but has why-cue → SEND"
        );
    }

    #[test]
    fn triage_mechanical_but_why_cue_overrides() {
        let ep = make_ep(
            "cargo fmt",
            "We are reverting this because it broke the build.",
            false,
        );
        assert!(
            triage(&ep, &big_diff(), &src_files()).is_none(),
            "mechanical subject with why-cue in body should SEND"
        );
    }

    #[test]
    fn triage_small_diff_above_threshold_sends() {
        let ep = make_ep("Add helper function", "", false);
        // small_diff has 4 changed lines > TB_MIN_LINES(2) → no tiny-no-why-cue
        assert!(
            triage(&ep, &small_diff(), &src_files()).is_none(),
            "4 changed lines > 2 threshold → SEND even without why-cue"
        );
    }

    // ── detect_trunk override path ────────────────────────────────────────────

    #[test]
    fn detect_trunk_respects_override() {
        let repo = PathBuf::from("/nonexistent/repo");
        let result = detect_trunk(&repo, Some("main"));
        assert_eq!(result.unwrap(), "main");
    }

    // ── resume log skip logic ─────────────────────────────────────────────────

    #[test]
    fn resume_log_skip_excludes_done_shas() {
        let log = "abc123\ndef456\nghi789\n";
        let done: HashSet<String> = log.split_whitespace().map(String::from).collect();

        assert!(done.contains("abc123"), "should skip known SHA");
        assert!(done.contains("def456"), "should skip known SHA");
        assert!(!done.contains("new111"), "should not skip unknown SHA");
    }

    // ── is_skip_subject corner cases ─────────────────────────────────────────

    #[test]
    fn is_skip_subject_various() {
        assert!(is_skip_subject("cargo fmt"));
        assert!(is_skip_subject("Cargo Fmt")); // case-insensitive
        assert!(is_skip_subject("Bump 1.0"));
        assert!(is_skip_subject("WIP: half done"));
        assert!(is_skip_subject("release v1.2.3"));
        assert!(is_skip_subject("release 2.0"));
        assert!(is_skip_subject("fmt"));
        assert!(is_skip_subject("fmt:"));
        assert!(is_skip_subject("typo in README"));
        assert!(!is_skip_subject("Fix authentication bug"));
        // "wip" followed by alphanumeric → NOT a skip
        assert!(!is_skip_subject("wipcraft (not wip)"));
    }

    // ── noise path detection ─────────────────────────────────────────────────

    #[test]
    fn noise_path_detection() {
        assert!(is_noise_path("Cargo.lock"));
        assert!(is_noise_path("package-lock.json"));
        assert!(is_noise_path("go.sum"));
        assert!(is_noise_path(".github/workflows/ci.yml"));
        assert!(is_noise_path("vendor/some/lib.go"));
        assert!(is_noise_path("test.snap"));
        assert!(!is_noise_path("src/main.rs"));
        assert!(!is_noise_path("Cargo.toml"));
    }
}
