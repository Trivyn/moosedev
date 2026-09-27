//! A language server as a deterministic checker. The harness, not the model,
//! talks to it: it keeps a source mirror in step with applied edits, waits for
//! the server to settle after each one, and keeps the resulting errors as
//! current task state that every later prompt shows and that gates finish.
//!
//! Settling is the point. A diagnostic read before the server has caught up
//! is a stale one, and a small model that is shown stale errors learns to
//! ignore them (OpenCode on badciv: qwen called its rust-analyzer errors
//! stale and ran `cargo build` after about one edit in three). A result the
//! harness could not settle is shown as unknown, never as clean.
//!
//! What the server can do beyond reporting is offered as choices, not tools:
//! its quick fixes are numbered under their findings, and `apply_fix` turns
//! one into an ordinary edit.
use crate::code::substrate::lang::{language_servers, LinterSpec, ServerSpec};
use crate::harness::digest::sha256_hex;
use crate::harness::executor::{resolve_program, ServerDirectory};
use anyhow::{Context, Result};
use lsp_server::{Message, Notification, Request, RequestId, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Whether a language server checks any of `files`, or they shape its
/// project.
pub(super) fn any_checked(files: &[String]) -> bool {
    language_servers().any(|spec| {
        files
            .iter()
            .any(|file| spec.language_of(file).is_some() || spec.is_project_file(file))
    })
}

impl ServerSpec {
    fn language_of(&self, file: &str) -> Option<&'static str> {
        let extension = Path::new(file).extension()?.to_str()?;
        self.languages
            .iter()
            .find(|(known, _)| *known == extension)
            .map(|(_, id)| *id)
    }

    fn is_project_file(&self, file: &str) -> bool {
        let name = Path::new(file).file_name().and_then(|name| name.to_str());
        name.is_some_and(|name| self.project_files.contains(&name))
    }

    /// The first candidate command whose program is installed.
    fn argv(&self) -> Option<Vec<String>> {
        self.commands.iter().find_map(|command| {
            let program = resolve_program(command[0])?;
            Some(
                std::iter::once(program.to_string_lossy().into_owned())
                    .chain(command[1..].iter().map(|arg| arg.to_string()))
                    .collect(),
            )
        })
    }
}

/// One diagnostic as the harness keeps and shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub file: String,
    /// 1-based.
    pub line: u32,
    /// 1-based, in the server's position units.
    pub column: u32,
    pub message: String,
    /// The compiler's full text when the server passes it on (rust-analyzer's
    /// `data.rendered`: the source excerpt, `note:` and `help:` lines with a
    /// suggested fix), else the related spans as `note:` lines; bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where the symbol at the error is defined, when that is in the
    /// project: "file:line: <that line>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// Quick fixes the server offers for it, numbered for `apply_fix`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<OfferedFix>,
}

/// A quick fix a server offered, ready to apply: text edits to one file,
/// valid only for the exact text they were computed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedFix {
    /// Unique within one snapshot; what `apply_fix` names.
    pub id: usize,
    pub title: String,
    pub file: String,
    /// SHA-256 of the text the edits apply to.
    pub base: String,
    pub edits: Vec<FixEdit>,
}

/// One replacement, in byte offsets into the fix's base text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl OfferedFix {
    /// `text` with the fix applied; None when `text` is not the text the fix
    /// was offered for, or the edits do not fit it.
    pub(super) fn apply(&self, text: &str) -> Option<String> {
        if sha256_hex(text) != self.base {
            return None;
        }
        splice(text, &self.edits)
    }
}

/// `text` with `edits` (sorted, non-overlapping byte ranges) applied.
fn splice(text: &str, edits: &[FixEdit]) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for edit in edits {
        if edit.start < at
            || edit.end < edit.start
            || !text.is_char_boundary(edit.start)
            || !text.is_char_boundary(edit.end)
        {
            return None;
        }
        out.push_str(&text[at..edit.start]);
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// The byte offset of an LSP position (UTF-16 units, the default encoding)
/// in `text`; None when it lies outside the text. Strict: a fix whose
/// positions do not fit is dropped, never clamped into a different edit.
fn byte_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (index, ch) in text[start..end].char_indices() {
        if units == character {
            return Some(start + index);
        }
        units += ch.len_utf16() as u32;
        if units > character {
            return None;
        }
    }
    (units == character).then_some(end)
}

/// The byte edits of LSP `TextEdit`s over `text`, sorted by position with
/// equal positions kept in their given order; None when any does not fit.
fn fix_edits(text: &str, edits: &[Value]) -> Option<Vec<FixEdit>> {
    let mut out = Vec::new();
    for edit in edits {
        let range = &edit["range"];
        let offset = |end: &str| {
            byte_offset(
                text,
                range[end]["line"].as_u64()?.try_into().ok()?,
                range[end]["character"].as_u64()?.try_into().ok()?,
            )
        };
        out.push(FixEdit {
            start: offset("start")?,
            end: offset("end")?,
            text: edit["newText"].as_str()?.to_owned(),
        });
    }
    out.sort_by_key(|edit| edit.start);
    splice(text, &out)?;
    Some(out)
}

impl Finding {
    fn line(&self, kind: &str) -> String {
        format!(
            "{}:{}:{} {kind}: {}\n",
            self.file,
            self.line,
            self.column,
            self.message.lines().next().unwrap_or_default()
        )
    }

    /// One line per offered fix, for under the finding.
    fn fix_lines(&self) -> String {
        self.fixes
            .iter()
            .map(|fix| format!("  fix {}: {}\n", fix.id, fix.title))
            .collect()
    }
}

/// What the language servers said after the last applied edit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub servers: Vec<String>,
    /// Every server settled before its deadline; otherwise the errors below
    /// may be incomplete or stale and nothing may be concluded from them.
    pub settled: bool,
    pub errors: Vec<Finding>,
    /// The linter's findings (clippy through rust-analyzer), apart from the
    /// other warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lints: Vec<Finding>,
    /// The linter that ran, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linter: Option<String>,
    /// Warnings that are not the linter's: the compiler's own (unused
    /// imports and variables).
    #[serde(
        default,
        deserialize_with = "findings_or_count",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub warnings: Vec<Finding>,
    /// The model was already sent back from finish for exactly these errors.
    #[serde(default)]
    pub finish_refused: bool,
}

/// Journals written before warnings were listed kept only their count. That
/// result is replaced by the next check, so the count is dropped.
fn findings_or_count<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Finding>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Listed(Vec<Finding>),
        Counted(#[allow(dead_code)] usize),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Listed(findings) => findings,
        Stored::Counted(_) => Vec::new(),
    })
}

impl DiagnosticsSnapshot {
    /// Every finding, in the order the block lists them: errors, warnings,
    /// lints.
    fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.errors.iter().chain(&self.warnings).chain(&self.lints)
    }

    /// The offered fix numbered `id`.
    pub(super) fn fix(&self, id: usize) -> Option<&OfferedFix> {
        self.findings()
            .flat_map(|finding| &finding.fixes)
            .find(|fix| fix.id == id)
    }

    /// Whether finish is sent back: settled, with any finding, and not
    /// already sent back for this result.
    pub(super) fn blocks_finish(&self) -> bool {
        self.settled && self.findings().next().is_some() && !self.finish_refused
    }

    /// The prompt block, within `budget` bytes: errors first, each with the
    /// compiler's full text while it fits and one line after that, then the
    /// warnings and the linter's findings, one line each; every finding with
    /// its numbered fixes.
    ///
    /// `full_budget` must leave room for the one-line heading: at least
    /// [`MIN_BLOCK_BYTES`].
    pub(super) fn render(&self, full_budget: usize) -> String {
        debug_assert!(full_budget >= MIN_BLOCK_BYTES);
        // Room held back for the omission counts and the list headings, so
        // the block never passes `full_budget`.
        const FOOTERS: usize = 220;
        let budget = full_budget.saturating_sub(FOOTERS);
        let servers = self.servers.join(", ");
        if !self.settled {
            return format!(
                "Language server ({servers}) after your last edit: not settled in time; its errors are unknown, so rely on commands and required checks.\n"
            );
        }
        let linter = self
            .linter
            .as_deref()
            .map(|linter| format!(", {} lint(s) from {linter}", self.lints.len()))
            .unwrap_or_else(|| ", no linter".to_owned());
        if self.findings().next().is_none() {
            return format!(
                "Language server ({servers}) after your last edit: no errors, no warnings{linter}.\n"
            );
        }
        let mut out = format!(
            "Language server ({servers}) after your last edit: {} error(s), {} warning(s){linter}. These are current; fix them before finish.\n",
            self.errors.len(),
            self.warnings.len()
        );
        let mut shown = 0;
        for finding in &self.errors {
            let detailed = finding
                .detail
                .as_deref()
                .map(|detail| format!("{}\n", detail.trim_end()));
            let line = finding.line("error");
            let chosen = match detailed {
                // Leave room for a one-line form of what follows.
                Some(detail) if out.len() + detail.len() + 400 <= budget => detail,
                _ => line,
            };
            let chosen = match &finding.definition {
                Some(definition) => format!("{chosen}  defined at {definition}\n"),
                None => chosen,
            };
            let chosen = chosen + &finding.fix_lines();
            if out.len() + chosen.len() > budget {
                break;
            }
            out.push_str(&chosen);
            shown += 1;
        }
        let rest = format!(
            "[{} more error(s) not listed; a build command prints them all]\n",
            self.errors.len() - shown
        );
        if shown < self.errors.len() && out.len() + rest.len() <= full_budget {
            out.push_str(&rest);
        }
        let budgets = (budget, full_budget);
        list(&mut out, "Warnings:\n", "warning", &self.warnings, budgets);
        if let Some(linter) = &self.linter {
            list(
                &mut out,
                &format!("Lints ({linter}):\n"),
                "lint",
                &self.lints,
                budgets,
            );
        }
        out
    }
}

/// `findings` under `heading`, one line each with its first `help:` line
/// (the suggestion is the useful part of a warning) and its fixes: at most
/// ten within `budget`, the heading only above a listed line, and a count of
/// the rest when it fits `full_budget`.
fn list(
    out: &mut String,
    heading: &str,
    kind: &str,
    findings: &[Finding],
    (budget, full_budget): (usize, usize),
) {
    const LISTED: usize = 10;
    let mut listed = 0;
    for finding in findings.iter().take(LISTED) {
        let mut line = finding.line(kind);
        if let Some(help) = finding
            .detail
            .as_deref()
            .and_then(|detail| detail.lines().find(|l| l.contains("help:")))
        {
            line.push_str(&format!("  {}\n", &help[help.find("help:").unwrap_or(0)..]));
        }
        line.push_str(&finding.fix_lines());
        let heading = if listed == 0 { heading } else { "" };
        if out.len() + heading.len() + line.len() > budget {
            break;
        }
        out.push_str(heading);
        out.push_str(&line);
        listed += 1;
    }
    let rest = format!("[{} more {kind}(s) not listed]\n", findings.len() - listed);
    if listed < findings.len() && out.len() + rest.len() <= full_budget {
        out.push_str(&rest);
    }
}

/// The smallest budget [`DiagnosticsSnapshot::render`] is given.
pub(super) const MIN_BLOCK_BYTES: usize = 400;

/// What the reader thread has seen from the server.
#[derive(Default)]
struct Seen {
    /// Latest diagnostics per mirror-relative file.
    diagnostics: HashMap<String, Vec<lsp_types::Diagnostic>>,
    /// The version of each open document the runner last sent; diagnostics
    /// for an older version arrive late and are dropped.
    sent_versions: HashMap<String, i32>,
    last_activity: Option<Instant>,
    /// When the last `experimental/serverStatus` arrived.
    status_at: Option<Instant>,
    /// When the last piece of work the server announced (`$/progress`)
    /// ended: for rust-analyzer, its `cargo check`.
    progress_ended_at: Option<Instant>,
    progress: HashSet<String>,
    quiescent: Option<bool>,
    responses: HashMap<RequestId, Response>,
    exited: bool,
}

/// One running server.
pub(super) struct LanguageServer {
    spec: ServerSpec,
    child: Child,
    /// Messages to the server, written by their own thread: a server that
    /// stops reading must never block the runner.
    outbox: Sender<Message>,
    seen: Arc<Mutex<Seen>>,
    mirror: PathBuf,
    next_id: i32,
    /// The linter this server runs, when one is installed.
    linter: Option<LinterSpec>,
    /// The last version sent per document, kept after a close.
    versions: HashMap<String, i32>,
    open: HashSet<String>,
    settled_once: bool,
}

impl LanguageServer {
    /// Spawn `command`, run `initialize` and open nothing yet.
    pub(super) async fn start(
        spec: ServerSpec,
        linter: Option<LinterSpec>,
        mut command: std::process::Command,
        mirror: &Path,
    ) -> Result<Self> {
        let mut child = command
            .spawn()
            .with_context(|| format!("start {}", spec.name))?;
        let stdin = child.stdin.take().context("server stdin")?;
        let stdout = child.stdout.take().context("server stdout")?;
        let (outbox, inbox) = channel::<Message>();
        std::thread::spawn(move || write_loop(stdin, inbox));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mirror = mirror.to_path_buf();
        {
            let outbox = outbox.clone();
            let seen = seen.clone();
            let mirror = mirror.clone();
            std::thread::spawn(move || read_loop(stdout, outbox, seen, mirror));
        }
        let mut server = Self {
            spec,
            child,
            outbox,
            seen,
            mirror,
            next_id: 1,
            linter,
            versions: HashMap::new(),
            open: HashSet::new(),
            settled_once: false,
        };
        let root = uri(&server.mirror)?;
        let id = server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root,
                "workspaceFolders": [{"uri": root, "name": "project"}],
                "initializationOptions": linter.map_or(spec.options, |linter| linter.options)(),
                "capabilities": {
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "publishDiagnostics": {"versionSupport": true, "relatedInformation": true},
                        // Literal actions, so a quick fix arrives with its
                        // edit (or resolves to one) rather than as a command.
                        "codeAction": {
                            "codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix"]}},
                            "resolveSupport": {"properties": ["edit"]},
                            "dataSupport": true,
                        },
                    },
                    "window": {"workDoneProgress": true},
                    "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": false}, "configuration": true, "workspaceFolders": true},
                    "experimental": {"serverStatusNotification": true},
                },
            }),
        )?;
        server
            .response(id, Duration::from_secs(60))
            .await
            .with_context(|| format!("{} did not initialize", spec.name))?;
        server.notify("initialized", json!({}))?;
        Ok(server)
    }

    pub(super) fn name(&self) -> &'static str {
        self.spec.name
    }

    fn send(&self, message: Message) -> Result<()> {
        self.outbox
            .send(message)
            .map_err(|_| anyhow::anyhow!("{} stopped reading", self.spec.name))
    }

    fn record_sent(&self, file: &str, version: i32) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.sent_versions.insert(file.to_owned(), version);
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<RequestId> {
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        self.send(Message::Request(Request::new(
            id.clone(),
            method.into(),
            params,
        )))?;
        Ok(id)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(Message::Notification(Notification::new(
            method.into(),
            params,
        )))
    }

    async fn response(&self, id: RequestId, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut seen = self
                    .seen
                    .lock()
                    .map_err(|_| anyhow::anyhow!("server state poisoned"))?;
                if let Some(response) = seen.responses.remove(&id) {
                    if let Some(error) = response.error {
                        anyhow::bail!("{}: {}", error.code, error.message);
                    }
                    return Ok(response.result.unwrap_or(Value::Null));
                }
                anyhow::ensure!(!seen.exited, "the server exited");
            }
            anyhow::ensure!(Instant::now() < deadline, "no response within {timeout:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Whether this server checks `file`, or `file` shapes its project.
    pub(super) fn concerns(&self, file: &str) -> bool {
        self.spec.language_of(file).is_some() || self.spec.is_project_file(file)
    }

    /// Tell the server about one mirrored edit: open or change the document
    /// with its full text and save it, or close it when deleted. Every change
    /// is also reported as a watched-file event, which is how servers notice
    /// manifests they do not open.
    pub(super) fn sync(&mut self, file: &str, existed: bool, text: Option<&str>) -> Result<()> {
        let uri = uri(&self.mirror.join(file))?;
        if let Some(language) = self.spec.language_of(file) {
            // Versions only grow, across a delete and a re-create too, so a
            // late report about an earlier text is always recognisable.
            let version = self.versions.get(file).copied().unwrap_or(0) + 1;
            match (text, self.open.contains(file)) {
                (Some(text), false) => {
                    self.versions.insert(file.to_owned(), version);
                    self.open.insert(file.to_owned());
                    self.record_sent(file, version);
                    self.notify(
                        "textDocument/didOpen",
                        json!({"textDocument": {"uri": uri, "languageId": language, "version": version, "text": text}}),
                    )?;
                    self.notify(
                        "textDocument/didSave",
                        json!({"textDocument": {"uri": uri}}),
                    )?;
                }
                (Some(text), true) => {
                    self.versions.insert(file.to_owned(), version);
                    self.record_sent(file, version);
                    self.notify(
                        "textDocument/didChange",
                        json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}),
                    )?;
                    self.notify(
                        "textDocument/didSave",
                        json!({"textDocument": {"uri": uri}}),
                    )?;
                }
                (None, true) => {
                    // Past every version sent, so a late report about the
                    // deleted text is dropped as stale.
                    self.versions.insert(file.to_owned(), version);
                    self.record_sent(file, version);
                    self.open.remove(file);
                    self.notify(
                        "textDocument/didClose",
                        json!({"textDocument": {"uri": uri}}),
                    )?;
                }
                (None, false) => {}
            }
        }
        if text.is_none() {
            // A deleted file has no diagnostics, whatever the server last
            // said about it.
            if let Ok(mut seen) = self.seen.lock() {
                seen.diagnostics.remove(file);
            }
        }
        // 1 created, 2 changed, 3 deleted: from what the edit did, not from
        // what this server has seen.
        let kind = match (text, existed) {
            (None, _) => 3,
            (Some(_), false) => 1,
            (Some(_), true) => 2,
        };
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{"uri": uri, "type": kind}]}),
        )
    }

    /// Wait until the server has caught up with everything sent before
    /// `since`: something arrived after it, nothing has arrived for a quiet
    /// period, no progress is open, and a server that reports its status
    /// says it is quiescent. False at the deadline.
    ///
    /// With `needs_status` (an edit this server checks), a server that reports
    /// its status must also have shown work on this edit after `since` (a
    /// status report, or announced work ending): a quiescent flag left over
    /// from the previous edit says nothing about this one.
    pub(super) async fn settle(
        &mut self,
        since: Instant,
        timeout: Duration,
        needs_status: bool,
    ) -> bool {
        const QUIET: Duration = Duration::from_millis(800);
        const FIRST_WORD: Duration = Duration::from_millis(1500);
        let timeout = if self.settled_once {
            timeout
        } else {
            timeout.max(Duration::from_secs(120))
        };
        let deadline = since + timeout;
        loop {
            let now = Instant::now();
            let settled = match self.seen.lock() {
                Ok(seen) => {
                    let heard = seen.last_activity.is_some_and(|at| at > since);
                    let quiet = seen
                        .last_activity
                        .is_none_or(|at| now.duration_since(at) >= QUIET);
                    // Evidence the server worked on this edit: a status report
                    // or the end of announced work after it. A quiescent flag
                    // left from the previous edit alone proves nothing.
                    let worked = seen.status_at.is_some_and(|at| at > since)
                        || seen.progress_ended_at.is_some_and(|at| at > since);
                    let status = if self.spec.server_status && needs_status {
                        seen.quiescent == Some(true) && worked
                    } else {
                        seen.quiescent != Some(false)
                    };
                    !seen.exited
                        && (heard || now.duration_since(since) >= FIRST_WORD)
                        && quiet
                        && seen.progress.is_empty()
                        && status
                }
                Err(_) => false,
            };
            if settled {
                self.settled_once = true;
                return true;
            }
            if now >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Where the symbol at `line`/`column` (1-based, server units) of `file`
    /// is defined, when that lies in the mirror: "file:line: <that line>".
    /// Bounded; any failure is no answer.
    async fn definition(
        &mut self,
        file: &str,
        line: u32,
        column: u32,
        timeout: Duration,
    ) -> Option<String> {
        let uri = uri(&self.mirror.join(file)).ok()?;
        let id = self
            .request(
                "textDocument/definition",
                json!({"textDocument": {"uri": uri}, "position": {"line": line - 1, "character": column - 1}}),
            )
            .ok()?;
        let result = self.response(id, timeout).await.ok()?;
        let items = match result {
            Value::Array(items) => items,
            Value::Null => return None,
            single => vec![single],
        };
        // Shorthand like `E { section }` names both a binding and a field;
        // show each distinct target, at most two.
        let mut shown: Vec<String> = Vec::new();
        for item in items.iter().take(4) {
            // A Location has `uri`/`range`; a LocationLink `targetUri` and
            // `targetSelectionRange`.
            let Some(target) = item["uri"].as_str().or(item["targetUri"].as_str()) else {
                continue;
            };
            let range = if item["range"].is_object() {
                &item["range"]
            } else {
                &item["targetSelectionRange"]
            };
            let Some(target_line) = range["start"]["line"].as_u64().map(|l| l as usize) else {
                continue;
            };
            let Some(target_file) = relative(target, &self.mirror) else {
                continue;
            };
            if target_file == file && target_line + 1 == line as usize {
                continue;
            }
            let Some(text) = read_in_mirror(&self.mirror, &target_file) else {
                continue;
            };
            let Some(source) = text.lines().nth(target_line) else {
                continue;
            };
            let source: String = source.trim().chars().take(160).collect();
            let entry = format!("{target_file}:{}: {source}", target_line + 1);
            if !shown.contains(&entry) && shown.len() < 2 {
                shown.push(entry);
            }
        }
        (!shown.is_empty()).then(|| shown.join("; "))
    }

    /// The quick fixes the server offers for `finding`, at most
    /// [`FIXES_PER_FINDING`], with ids still to assign. Only fixes the harness
    /// can apply as one ordinary edit are kept: text edits to a single file
    /// among `editable` that change it. Any failure is no fixes.
    async fn fixes(
        &mut self,
        finding: &Finding,
        editable: &[String],
        timeout: Duration,
    ) -> Vec<OfferedFix> {
        let deadline = Instant::now() + timeout;
        let mut fixes: Vec<OfferedFix> = Vec::new();
        for (file, diagnostic) in self.fix_targets(finding) {
            let Ok(uri) = uri(&self.mirror.join(&file)) else {
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(id) = self.request(
                "textDocument/codeAction",
                json!({"textDocument": {"uri": uri}, "range": diagnostic.range, "context": {"diagnostics": [diagnostic], "only": ["quickfix"]}}),
            ) else {
                return fixes;
            };
            let Ok(Value::Array(actions)) = self.response(id, left).await else {
                continue;
            };
            for action in actions {
                if fixes.len() == FIXES_PER_FINDING || Instant::now() >= deadline {
                    return fixes;
                }
                // The same fix arrives for the error and for its related
                // hint; distinct fixes may share a title.
                if let Some(fix) = self.offered(action, editable, deadline).await {
                    if !fixes
                        .iter()
                        .any(|known| known.file == fix.file && known.edits == fix.edits)
                    {
                        fixes.push(fix);
                    }
                }
            }
        }
        fixes
    }

    /// One code action as a fix the harness can apply, resolving its edit
    /// when the server sends it without one.
    async fn offered(
        &mut self,
        action: Value,
        editable: &[String],
        deadline: Instant,
    ) -> Option<OfferedFix> {
        // A bare Command (its `command` a string) is only runnable by the
        // server's own client. A CodeAction's follow-up command, when it has
        // one, is left unrun: the edit is the fix.
        let title = action["title"].as_str()?.to_owned();
        if action["command"].is_string()
            || action.get("disabled").is_some()
            || action["kind"]
                .as_str()
                .is_some_and(|kind| !kind.starts_with("quickfix"))
        {
            return None;
        }
        let edit = if action["edit"].is_object() {
            action["edit"].clone()
        } else if action.get("data").is_some() {
            let id = self.request("codeAction/resolve", action).ok()?;
            let left = deadline.saturating_duration_since(Instant::now());
            self.response(id, left).await.ok()?["edit"].clone()
        } else {
            return None;
        };
        let (target, version, edits) = single_file_edits(&edit)?;
        let file = relative(&target, &self.mirror).filter(|file| editable.contains(file))?;
        // An edit for another version of the document than the one sent last
        // would land on the wrong bytes of this one.
        if version.is_some_and(|version| self.versions.get(&file) != Some(&version)) {
            return None;
        }
        let text = read_in_mirror(&self.mirror, &file)?;
        let edits = fix_edits(&text, &edits)?;
        if splice(&text, &edits)? == text {
            return None;
        }
        Some(OfferedFix {
            id: 0,
            title,
            file,
            base: sha256_hex(&text),
            edits,
        })
    }

    /// The server's diagnostics to ask for fixes of `finding`: its own, then
    /// those at its related locations. A compiler suggestion often sits
    /// away from the error (rustc's `mut` goes on the `let`, not the borrow),
    /// and rust-analyzer publishes it there as a hint carrying the fix.
    fn fix_targets(&self, finding: &Finding) -> Vec<(String, lsp_types::Diagnostic)> {
        let Ok(seen) = self.seen.lock() else {
            return Vec::new();
        };
        let Some(own) = seen.diagnostics.get(&finding.file).and_then(|diagnostics| {
            diagnostics.iter().find(|diagnostic| {
                diagnostic.range.start.line + 1 == finding.line
                    && diagnostic.range.start.character + 1 == finding.column
                    && diagnostic.message == finding.message
            })
        }) else {
            return Vec::new();
        };
        let mut targets = vec![(finding.file.clone(), own.clone())];
        for related in own.related_information.iter().flatten().take(3) {
            let Some(file) = relative(related.location.uri.as_str(), &self.mirror) else {
                continue;
            };
            let at = seen
                .diagnostics
                .get(&file)
                .into_iter()
                .flatten()
                .filter(|diagnostic| diagnostic.range == related.location.range);
            for diagnostic in at.take(2) {
                targets.push((file.clone(), diagnostic.clone()));
            }
        }
        targets
    }

    /// Current errors, other warnings, and the linter's findings.
    pub(super) fn findings(&self) -> Findings {
        let mut found = Findings::default();
        let Ok(seen) = self.seen.lock() else {
            return found;
        };
        let files: BTreeMap<&String, &Vec<lsp_types::Diagnostic>> =
            seen.diagnostics.iter().collect();
        for (file, diagnostics) in files {
            for diagnostic in diagnostics {
                let finding = || Finding {
                    file: file.clone(),
                    line: diagnostic.range.start.line + 1,
                    column: diagnostic.range.start.character + 1,
                    message: diagnostic.message.clone(),
                    detail: detail(diagnostic, &self.mirror),
                    definition: None,
                    fixes: Vec::new(),
                };
                let lint = self
                    .linter
                    .is_some_and(|linter| diagnostic.source.as_deref() == Some(linter.source));
                match diagnostic.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) | None => {
                        found.errors.push(finding())
                    }
                    Some(lsp_types::DiagnosticSeverity::WARNING) if lint => {
                        found.lints.push(finding())
                    }
                    Some(lsp_types::DiagnosticSeverity::WARNING) => found.warnings.push(finding()),
                    _ => {}
                }
            }
        }
        for list in [&mut found.errors, &mut found.warnings, &mut found.lints] {
            list.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
            list.dedup();
        }
        found
    }
}

#[derive(Default)]
pub(super) struct Findings {
    pub errors: Vec<Finding>,
    pub warnings: Vec<Finding>,
    pub lints: Vec<Finding>,
}

/// The one file a `WorkspaceEdit` changes, the document version it was
/// computed for when it says, and its text edits; None when it changes
/// several files or creates, renames or deletes one.
fn single_file_edits(edit: &Value) -> Option<(String, Option<i32>, Vec<Value>)> {
    let mut files: Vec<(String, Option<i32>, Vec<Value>)> = Vec::new();
    if let Some(changes) = edit["documentChanges"].as_array() {
        for change in changes {
            // A resource operation (create, rename, delete) has a `kind`.
            if change.get("kind").is_some() {
                return None;
            }
            let document = &change["textDocument"];
            files.push((
                document["uri"].as_str()?.to_owned(),
                document["version"]
                    .as_i64()
                    .and_then(|version| i32::try_from(version).ok()),
                change["edits"].as_array()?.clone(),
            ));
        }
    } else {
        for (uri, edits) in edit["changes"].as_object()? {
            files.push((uri.clone(), None, edits.as_array()?.clone()));
        }
    }
    match <[_; 1]>::try_from(files) {
        Ok([only]) => Some(only),
        Err(_) => None,
    }
}

/// The compiler's full text of a diagnostic, bounded; else its related spans.
fn detail(diagnostic: &lsp_types::Diagnostic, mirror: &Path) -> Option<String> {
    if let Some(rendered) = diagnostic
        .data
        .as_ref()
        .and_then(|data| data["rendered"].as_str())
        .filter(|rendered| !rendered.trim().is_empty())
    {
        return Some(bounded_detail(rendered));
    }
    let notes: Vec<String> = diagnostic
        .related_information
        .iter()
        .flatten()
        .take(3)
        .map(|related| {
            let file = relative(related.location.uri.as_str(), mirror)
                .unwrap_or_else(|| "(outside the project)".to_owned());
            format!(
                "  note: {file}:{}: {}",
                related.location.range.start.line + 1,
                related.message
            )
        })
        .collect();
    (!notes.is_empty()).then(|| {
        bounded_detail(&format!(
            "{}\n{}",
            diagnostic.message.lines().next().unwrap_or_default(),
            notes.join("\n")
        ))
    })
}

/// Bytes of compiler text one finding keeps.
const DETAIL_BYTES: usize = 800;

/// `text` within [`DETAIL_BYTES`], cut at a character with a marker.
fn bounded_detail(text: &str) -> String {
    let text = text.trim_end();
    if text.len() <= DETAIL_BYTES {
        return text.to_owned();
    }
    let mut end = DETAIL_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} […]", &text[..end])
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        // Best effort and never blocking: an exit queued for the writer, then
        // the whole process group goes, so a proc-macro server or a cargo
        // check it started never outlives it.
        let _ = self.notify("exit", Value::Null);
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn uri(path: &Path) -> Result<String> {
    let path = path.to_str().context("server path must be UTF-8")?;
    Ok(format!("file://{}", percent_encode(path)))
}

fn percent_encode(path: &str) -> String {
    path.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mirror-relative path of a `file://` URI, if it lies in the mirror.
fn relative(uri: &str, mirror: &Path) -> Option<String> {
    let path = percent_decode(uri.strip_prefix("file://")?);
    let path = Path::new(&path);
    let mirror = mirror
        .canonicalize()
        .unwrap_or_else(|_| mirror.to_path_buf());
    let relative = path.strip_prefix(&mirror).ok()?;
    // Only plain names: a `..` or `.` in what the server sent must not lead
    // the harness, which reads these files unconfined, out of the mirror.
    relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
        .then(|| relative.to_str().map(str::to_owned))
        .flatten()
}

/// The text of a mirror file the server named, read only when it resolves
/// inside the mirror (no symlink out of it).
fn read_in_mirror(mirror: &Path, file: &str) -> Option<String> {
    let root = mirror.canonicalize().ok()?;
    let path = root.join(file).canonicalize().ok()?;
    path.starts_with(&root)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

fn write_loop(stdin: ChildStdin, inbox: std::sync::mpsc::Receiver<Message>) {
    let mut writer = BufWriter::new(stdin);
    while let Ok(message) = inbox.recv() {
        if message.write(&mut writer).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

fn read_loop(
    stdout: std::process::ChildStdout,
    outbox: Sender<Message>,
    seen: Arc<Mutex<Seen>>,
    mirror: PathBuf,
) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(message)) = Message::read(&mut reader) {
        let Ok(mut state) = seen.lock() else { break };
        state.last_activity = Some(Instant::now());
        match message {
            Message::Notification(notification) => match notification.method.as_str() {
                "textDocument/publishDiagnostics" => {
                    if let Ok(params) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(
                        notification.params,
                    ) {
                        if let Some(file) = relative(params.uri.as_str(), &mirror) {
                            let stale = params.version.is_some_and(|version| {
                                state
                                    .sent_versions
                                    .get(&file)
                                    .is_some_and(|sent| version < *sent)
                            });
                            if !stale {
                                state.diagnostics.insert(file, params.diagnostics);
                            }
                        }
                    }
                }
                "experimental/serverStatus" => {
                    state.quiescent = notification.params["quiescent"].as_bool();
                    state.status_at = state.last_activity;
                }
                "$/progress" => {
                    let token = notification.params["token"].to_string();
                    match notification.params["value"]["kind"].as_str() {
                        Some("begin") => {
                            state.progress.insert(token);
                        }
                        Some("end") => {
                            state.progress.remove(&token);
                            state.progress_ended_at = state.last_activity;
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            Message::Response(response) => {
                state.responses.insert(response.id.clone(), response);
            }
            Message::Request(request) => {
                drop(state);
                // Answer what the server asks with defaults: no configuration
                // beyond the initialization options, and accept progress
                // tokens and registrations.
                let result = match request.method.as_str() {
                    "workspace/configuration" => {
                        let items = request.params["items"].as_array().map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; items])
                    }
                    _ => Value::Null,
                };
                let _ = outbox.send(Message::Response(Response::new_ok(request.id, result)));
                continue;
            }
        }
    }
    if let Ok(mut state) = seen.lock() {
        state.exited = true;
    }
}

/// Whether `linter` is installed: its probe succeeds under the server's
/// sandbox, where it will run.
async fn linter_installed(
    directory: &ServerDirectory,
    linter: LinterSpec,
    read_paths: &[PathBuf],
) -> bool {
    let Some(program) = resolve_program(linter.probe[0]) else {
        return false;
    };
    let argv: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
        .chain(linter.probe[1..].iter().map(|arg| arg.to_string()))
        .collect();
    let Ok(mut command) = directory.command(&argv, read_paths) else {
        return false;
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => {
                // A hung probe takes its whole process group with it.
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The servers of one task, over one mirror.
pub(super) struct LanguageServers {
    pub directory: ServerDirectory,
    pub servers: Vec<LanguageServer>,
}

impl LanguageServers {
    /// Prepare the mirror and start a server for every language with a
    /// source or project file in the project and an installed server.
    /// Returns what was started and what was missing, for the journal.
    pub(super) async fn start(
        project: &Path,
        directory: &Path,
        read_paths: &[PathBuf],
        files: &[String],
    ) -> Result<(Self, Vec<(&'static str, String)>)> {
        let directory = ServerDirectory::prepare(project, directory)?;
        let mut servers = Vec::new();
        let mut notes = Vec::new();
        for spec in language_servers() {
            if !files
                .iter()
                .any(|file| spec.language_of(file).is_some() || spec.is_project_file(file))
            {
                continue;
            }
            let Some(argv) = spec.argv() else {
                notes.push((
                    "language_server",
                    format!("Language server {}: not installed.", spec.name),
                ));
                continue;
            };
            let linter = match spec.linter {
                Some(linter) if linter_installed(&directory, linter, read_paths).await => {
                    Some(linter)
                }
                Some(linter) => {
                    notes.push((
                        "language_linter_missing",
                        format!(
                            "No linter for {}: {} is not installed ({}); {} checks without it.",
                            spec.language, linter.name, linter.install_hint, spec.name
                        ),
                    ));
                    None
                }
                None => None,
            };
            let command = directory.command(&argv, read_paths)?;
            match LanguageServer::start(*spec, linter, command, &directory.mirror()).await {
                Ok(server) => {
                    let with = linter
                        .map(|l| format!(" with {}", l.name))
                        .unwrap_or_default();
                    notes.push((
                        "language_server",
                        format!(
                            "Language server {}: started{with} ({}).",
                            spec.name, argv[0]
                        ),
                    ));
                    servers.push(server);
                }
                Err(error) => notes.push((
                    "language_server",
                    format!("Language server {}: failed to start: {error:#}.", spec.name),
                )),
            }
        }
        Ok((Self { directory, servers }, notes))
    }

    /// Mirror one applied edit, tell each concerned server, wait for them to
    /// settle and return what they report. None when no server concerns it.
    ///
    /// Quick fixes are kept only for `editable` files, the ones an edit may
    /// change now.
    pub(super) async fn after_edit(
        &mut self,
        file: &str,
        existed: bool,
        text: Option<&str>,
        timeout: Duration,
        editable: &[String],
    ) -> Result<Option<DiagnosticsSnapshot>> {
        self.directory.mirror_edit(file, text)?;
        if self.servers.is_empty() {
            return Ok(None);
        }
        // Every server hears every edit: a file of no server's language can
        // still change a build (`include_str!`, a build script's input).
        let since = Instant::now();
        for server in &mut self.servers {
            server.sync(file, existed, text)?;
        }
        let mut settled = true;
        for server in &mut self.servers {
            let checks = server.concerns(file);
            settled &= server.settle(since, timeout, checks).await;
        }
        let mut snapshot = DiagnosticsSnapshot {
            servers: self
                .servers
                .iter()
                .map(|server| server.name().to_owned())
                .collect(),
            settled,
            ..Default::default()
        };
        for server in &mut self.servers {
            let mut found = server.findings();
            // The definition behind each of the first errors: what a type or
            // lifetime error is measured against, without a model step.
            // Only for a settled result, and within one deadline for all.
            let deadline = Instant::now() + DEFINITION_TIME;
            for error in found.errors.iter_mut().take(DEFINED_ERRORS) {
                if !settled || Instant::now() >= deadline {
                    break;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                error.definition = server
                    .definition(&error.file, error.line, error.column, left)
                    .await;
            }
            // The fixes the server offers for the first errors, warnings
            // and lints, within their own deadline.
            let deadline = Instant::now() + FIX_TIME;
            for finding in found
                .errors
                .iter_mut()
                .take(FIXED_FINDINGS)
                .chain(found.warnings.iter_mut().take(FIXED_FINDINGS))
                .chain(found.lints.iter_mut().take(FIXED_FINDINGS))
            {
                if !settled || Instant::now() >= deadline {
                    break;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                finding.fixes = server.fixes(finding, editable, left).await;
            }
            snapshot.errors.extend(found.errors);
            snapshot.lints.extend(found.lints);
            snapshot.warnings.extend(found.warnings);
            if let Some(linter) = server.linter {
                snapshot
                    .linter
                    .get_or_insert_with(|| linter.name.to_owned());
            }
        }
        // Numbered in the order the block lists them.
        let offered = snapshot
            .errors
            .iter_mut()
            .chain(snapshot.warnings.iter_mut())
            .chain(snapshot.lints.iter_mut())
            .flat_map(|finding| finding.fixes.iter_mut());
        for (id, fix) in offered.enumerate() {
            fix.id = id + 1;
        }
        Ok(Some(snapshot))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// Errors per result that get their definition looked up.
const DEFINED_ERRORS: usize = 5;
/// All definition lookups of one result share this deadline.
const DEFINITION_TIME: Duration = Duration::from_secs(4);
/// Errors, warnings and lints, each, per result whose quick fixes are asked
/// for.
const FIXED_FINDINGS: usize = 5;
/// Quick fixes kept per finding.
const FIXES_PER_FINDING: usize = 3;
/// All quick-fix requests of one result share this deadline.
const FIX_TIME: Duration = Duration::from_secs(4);

/// Whether this runner has language servers.
#[derive(Default)]
pub(super) enum LanguageState {
    /// None started yet; the next code edit tries.
    #[default]
    Idle,
    Running(LanguageServers),
    /// The project's languages have no installed or working server; not
    /// retried until the project's shape changes.
    Unavailable,
}

impl super::Runner {
    /// The directory of this task's language servers, beside the command
    /// scratch rather than in it (every command clears its scratch).
    fn language_directory(&self) -> PathBuf {
        self.task
            .root
            .join(".moosedev/harness/lsp")
            .join(&self.task.id)
    }

    /// After an applied edit: keep the servers' mirror in step, wait for them
    /// to settle, and keep what they report as the task's diagnostics. Never
    /// fails the step: a server problem is journaled and the harness carries
    /// on without one.
    pub(super) async fn check_applied_edit(
        &mut self,
        file: &str,
        existed: bool,
        after: Option<&str>,
    ) {
        let Some(settings) = self.language_settings.filter(|settings| settings.enabled) else {
            return;
        };
        if let Err(error) = self
            .check_applied_edit_inner(file, existed, after, settings.settle_timeout)
            .await
        {
            self.intent_event("language_server_error", &format!("{error:#}"));
            self.event(format!(
                "Language server stopped: {error:#}. The task continues without it."
            ));
            self.language = LanguageState::Unavailable;
            // What the server last said is about a source that no longer
            // exists; neither the prompt nor the finish gate may use it.
            self.task.diagnostics = None;
        }
    }

    async fn check_applied_edit_inner(
        &mut self,
        file: &str,
        existed: bool,
        after: Option<&str>,
        timeout: Duration,
    ) -> Result<()> {
        let reshaped =
            language_servers().any(|spec| spec.is_project_file(file)) && existed != after.is_some();
        if reshaped {
            // A manifest came or went: start again so the servers
            // rediscover the project, and retry an unavailable language.
            // Nothing said about the old shape may stand meanwhile.
            self.language = LanguageState::Idle;
            self.task.diagnostics = None;
        }
        if matches!(self.language, LanguageState::Idle) {
            let files = self.workspace.files()?;
            let concerned = language_servers().any(|spec| {
                files
                    .iter()
                    .any(|file| spec.language_of(file).is_some() || spec.is_project_file(file))
            });
            if !concerned {
                return Ok(());
            }
            self.show_status("Starting the language server…");
            let read_paths: Vec<PathBuf> = self
                .standing_read_paths()
                .iter()
                .map(PathBuf::from)
                .collect();
            let directory = self.language_directory();
            let root = self.workspace.root().to_path_buf();
            let (servers, notes) =
                LanguageServers::start(&root, &directory, &read_paths, &files).await?;
            for (kind, note) in notes {
                self.intent_event(kind, &note);
                self.event(note);
            }
            self.language = if servers.is_empty() {
                LanguageState::Unavailable
            } else {
                LanguageState::Running(servers)
            };
        }
        let LanguageState::Running(servers) = &mut self.language else {
            return Ok(());
        };
        let names = servers
            .servers
            .iter()
            .map(|server| server.name())
            .collect::<Vec<_>>()
            .join(", ");
        self.show_status(&format!("Checking {file} with {names}…"));
        let editable = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        let LanguageState::Running(servers) = &mut self.language else {
            return Ok(());
        };
        let started = Instant::now();
        if let Some(snapshot) = servers
            .after_edit(file, existed, after, timeout, &editable)
            .await?
        {
            let elapsed = started.elapsed().as_secs_f32();
            self.event(if snapshot.settled {
                let lints = snapshot
                    .linter
                    .as_deref()
                    .map(|linter| format!(", {} lint(s) from {linter}", snapshot.lints.len()))
                    .unwrap_or_default();
                format!(
                    "{}: {} error(s), {} warning(s){lints} after {file} (settled in {elapsed:.1} s).",
                    snapshot.servers.join(", "),
                    snapshot.errors.len(),
                    snapshot.warnings.len()
                )
            } else {
                format!(
                    "{}: not settled within {elapsed:.0} s after {file}; its errors are unknown.",
                    snapshot.servers.join(", ")
                )
            });
            self.intent_event(
                "language_server_diagnostics",
                &format!(
                    "{}: {} error(s), {} warning(s), {} lint(s), {} after {} ms",
                    snapshot.servers.join(", "),
                    snapshot.errors.len(),
                    snapshot.warnings.len(),
                    snapshot.lints.len(),
                    if snapshot.settled {
                        "settled"
                    } else {
                        "NOT settled"
                    },
                    started.elapsed().as_millis()
                ),
            );
            self.task.diagnostics = Some(snapshot);
        }
        Ok(())
    }

    fn show_status(&self, status: &str) {
        if let Some(progress) = &self.progress {
            let _ = progress.send(crate::harness::progress::Progress::Status(
                status.to_owned(),
            ));
        }
    }

    /// Stop this task's servers and remove their directory.
    pub(super) fn stop_language_servers(&mut self) {
        self.language = LanguageState::Idle;
        let directory = self.language_directory();
        if directory.exists() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real rust-analyzer under the macOS sandbox, over a mirror: an edit
    /// that breaks the build settles to its error, and the fix settles clean.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires rust-analyzer and a functional OS sandbox; run explicitly"]
    async fn rust_analyzer_settles_to_the_errors_of_each_edit() {
        let project = std::env::temp_dir().join(format!("moosedev-ra-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"ra-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
        let directory =
            std::env::temp_dir().join(format!("moosedev-ra-lsp-{}", uuid::Uuid::new_v4()));
        let files = vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()];
        // A run grants the standing sandbox reads; here, the path overrides
        // the user's Cargo configuration names, which Cargo must read.
        let overrides: Vec<PathBuf> = std::env::var_os("HOME")
            .and_then(|home| {
                std::fs::read_to_string(Path::new(&home).join(".cargo/config.toml")).ok()
            })
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| {
                table
                    .get("paths")
                    .and_then(|paths| paths.as_array())
                    .cloned()
            })
            .unwrap_or_default()
            .iter()
            .filter_map(|path| path.as_str().map(PathBuf::from))
            .collect();
        let (mut servers, notes) = LanguageServers::start(&project, &directory, &overrides, &files)
            .await
            .unwrap();
        assert!(!servers.is_empty(), "{notes:?}");

        let editable = ["src/lib.rs".to_owned()];
        let broken = "pub fn one() -> u32 { \"one\" }\n";
        std::fs::write(project.join("src/lib.rs"), broken).unwrap();
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(broken),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(
            snapshot
                .errors
                .iter()
                .any(|e| e.file == "src/lib.rs" && e.line == 1),
            "{snapshot:?}"
        );

        // A lifetime error comes only from `cargo check` (badciv 839ebeec's
        // loop): settling must wait for the check, not only the analysis.
        let lifetime = "pub struct E {\n    pub section: &'static str,\n}\npub fn make(section: &str) -> E {\n    E { section }\n}\npub fn one() -> u32 { 1 }\n";
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(lifetime),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(
            snapshot
                .errors
                .iter()
                .any(|e| e.line == 5 && e.message.contains("lifetime")),
            "{snapshot:?}"
        );

        // The compiler's full text rides along: the related line qwen
        // ran `cargo build` to see in badciv 4ab49399.
        assert!(
            snapshot.errors.iter().any(|e| e
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("let's call the lifetime"))),
            "{snapshot:?}"
        );

        // The definitions behind it: the `&str` binding and the field that
        // needs `&'static str`.
        assert!(
            snapshot.errors.iter().any(|e| e
                .definition
                .as_deref()
                .is_some_and(|d| d.contains("pub section: &'static str"))),
            "{snapshot:?}"
        );

        // Clippy runs as the check: a lint arrives with its suggestion.
        let linted =
            "pub fn first(v: &[u8]) -> Option<&u8> { v.get(0) }\npub fn one() -> u32 { 1 }\n";
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(linted),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert_eq!(snapshot.linter.as_deref(), Some("clippy"), "{snapshot:?}");
        assert!(
            snapshot
                .lints
                .iter()
                .any(|l| l.detail.as_deref().is_some_and(|d| d.contains("v.first()"))),
            "{snapshot:?}"
        );

        // Its suggestion is offered as a fix, and applying it clears the lint.
        let fix = snapshot.fix(1).expect("clippy's suggestion is offered");
        assert!(fix.title.contains("v.first()"), "{fix:?}");
        let fixed = fix.apply(linted).unwrap();
        assert!(
            fix.apply(&fixed).is_none(),
            "a fix applies only to its text"
        );
        std::fs::write(project.join("src/lib.rs"), &fixed).unwrap();
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(&fixed),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(
            snapshot.errors.is_empty() && snapshot.lints.is_empty(),
            "{snapshot:?}"
        );

        // A compiler error whose suggestion is an edit: the missing `mut`.
        // (The lifetime error above has none: rustc explains it, but
        // suggests no replacement, so no fix is offered.)
        let immutable =
            "pub fn made() -> Vec<u32> {\n    let v = Vec::new();\n    v.push(1);\n    v\n}\n";
        std::fs::write(project.join("src/lib.rs"), immutable).unwrap();
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(immutable),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        let fix = snapshot
            .errors
            .iter()
            .flat_map(|error| &error.fixes)
            .next()
            .unwrap_or_else(|| panic!("the missing mut is offered as a fix: {snapshot:?}"));
        let fixed = fix.apply(immutable).unwrap();
        assert!(fixed.contains("let mut v"), "{fixed}");
        // Outside the editable files, nothing is offered.
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(immutable),
                Duration::from_secs(60),
                &[],
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            snapshot.errors.iter().all(|error| error.fixes.is_empty()),
            "{snapshot:?}"
        );
        std::fs::write(project.join("src/lib.rs"), &fixed).unwrap();
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(&fixed),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(snapshot.errors.is_empty(), "{snapshot:?}");

        // A compiler warning is listed, not only counted, with its fix: the
        // unused `key` badciv edc914f6 finished with.
        let unused =
            "pub fn parse_u32(key: &str, value: &str) -> u32 {\n    value.len() as u32\n}\n";
        std::fs::write(project.join("src/lib.rs"), unused).unwrap();
        let snapshot = servers
            .after_edit(
                "src/lib.rs",
                true,
                Some(unused),
                Duration::from_secs(60),
                &editable,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled && snapshot.blocks_finish(), "{snapshot:?}");
        let warning = snapshot
            .warnings
            .iter()
            .find(|warning| warning.message.contains("unused variable"))
            .unwrap_or_else(|| panic!("the unused variable is listed: {snapshot:?}"));
        let titles: Vec<&str> = warning.fixes.iter().map(|fix| fix.title.as_str()).collect();
        assert!(
            warning
                .fixes
                .iter()
                .filter_map(|fix| fix.apply(unused))
                .any(|fixed| fixed.contains("_key: &str")),
            "rustc's `_key` is among the fixes: {titles:?}"
        );
        drop(servers);
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_plan_is_checked_when_a_server_covers_one_of_its_files() {
        let files = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        assert!(any_checked(&files(&["README.md", "badciv-map/src/lib.rs"])));
        assert!(any_checked(&files(&["badciv-map/Cargo.toml"])));
        assert!(!any_checked(&files(&["code.txt", "notes.md"])));
    }

    #[test]
    fn a_uri_round_trips_to_a_mirror_relative_path() {
        let mirror = Path::new("/tmp/x y/source");
        let uri = uri(&mirror.join("src/a b.rs")).unwrap();
        assert_eq!(uri, "file:///tmp/x%20y/source/src/a%20b.rs");
        assert_eq!(relative(&uri, mirror).as_deref(), Some("src/a b.rs"));
        assert_eq!(relative("file:///elsewhere/a.rs", mirror), None);
        // What the server sends never leads out of the mirror.
        assert_eq!(
            relative("file:///tmp/x%20y/source/../../etc/passwd", mirror),
            None
        );
    }

    #[test]
    fn the_block_lists_errors_bounded_and_never_calls_unknown_clean() {
        let finding = |file: &str, line| Finding {
            file: file.into(),
            line,
            column: 1,
            message: "mismatched types\nexpected `u8`".into(),
            detail: None,
            definition: None,
            fixes: vec![],
        };
        let snapshot = DiagnosticsSnapshot {
            servers: vec!["rust-analyzer".into()],
            settled: true,
            errors: vec![finding("src/a.rs", 3), finding("src/b.rs", 9)],
            warnings: vec![],
            lints: vec![],
            linter: None,
            finish_refused: false,
        };
        let block = snapshot.render(3_000);
        assert!(
            block.contains("2 error(s), 0 warning(s), no linter."),
            "{block}"
        );
        assert!(
            block.contains("src/a.rs:3:1 error: mismatched types\n"),
            "{block}"
        );
        let crowded = DiagnosticsSnapshot {
            errors: (0..20).map(|n| finding("src/c.rs", n)).collect(),
            ..snapshot.clone()
        };
        let tight = crowded.render(MIN_BLOCK_BYTES);
        assert!(tight.contains("more error(s) not listed"), "{tight}");
        // The compiler's full text replaces the one line while it fits, and a
        // lint keeps its suggestion.
        let mut rich = snapshot.clone();
        rich.errors[0].detail =
            Some("error[E0308]: mismatched types\n --> src/a.rs:3:1\n  = help: try `1u8`".into());
        rich.errors[1].definition = Some("src/b.rs:2: pub section: &'static str,".into());
        rich.linter = Some("clippy".into());
        rich.lints = vec![Finding {
            file: "src/b.rs".into(),
            line: 1,
            column: 41,
            message: "accessing first element with `v.get(0)`\nmore".into(),
            detail: Some(
                "warning: accessing first element\n  |     ^^^^ help: try: `v.first()`".into(),
            ),
            definition: None,
            fixes: vec![],
        }];
        let block = rich.render(3_000);
        assert!(
            block.contains("2 error(s), 0 warning(s), 1 lint(s) from clippy"),
            "{block}"
        );
        assert!(block.contains("= help: try `1u8`"), "{block}");
        assert!(
            block.contains(
                "error: mismatched types\n  defined at src/b.rs:2: pub section: &'static str,"
            ),
            "{block}"
        );
        assert!(
            block.contains("Lints (clippy):\nsrc/b.rs:1:41 lint: accessing first element with `v.get(0)`\n  help: try: `v.first()`"),
            "{block}"
        );
        assert!(rich.blocks_finish());
        let lints_only = DiagnosticsSnapshot {
            errors: vec![],
            ..rich.clone()
        };
        assert!(
            lints_only.blocks_finish(),
            "lints alone send finish back once"
        );
        // The compiler's warnings are listed with their suggestion, before
        // the lints, and send finish back once too.
        let warned = DiagnosticsSnapshot {
            errors: vec![],
            lints: vec![],
            warnings: vec![Finding {
                file: "src/c.rs".into(),
                line: 3,
                column: 29,
                message: "unused import: `Terrain`".into(),
                detail: Some(
                    "warning: unused import: `Terrain`\nhelp: remove the unused import".into(),
                ),
                definition: None,
                fixes: vec![],
            }],
            ..rich.clone()
        };
        let block = warned.render(3_000);
        assert!(
            block.contains("0 error(s), 1 warning(s), 0 lint(s) from clippy"),
            "{block}"
        );
        assert!(
            block.contains("Warnings:\nsrc/c.rs:3:29 warning: unused import: `Terrain`\n  help: remove the unused import\n"),
            "{block}"
        );
        assert!(warned.blocks_finish(), "warnings send finish back once");
        let clean = DiagnosticsSnapshot {
            warnings: vec![],
            ..warned
        };
        assert!(!clean.blocks_finish());
        assert!(
            clean
                .render(3_000)
                .contains("no errors, no warnings, 0 lint(s)"),
            "{}",
            clean.render(3_000)
        );
        let unknown = DiagnosticsSnapshot {
            settled: false,
            ..snapshot
        };
        assert!(unknown.render(3_000).contains("not settled in time"));
        assert!(!unknown.blocks_finish());

        // Any mix of errors and lints stays within the budget.
        let many = DiagnosticsSnapshot {
            errors: (0..40).map(|n| finding("src/c.rs", n)).collect(),
            warnings: (0..40).map(|n| finding("src/e.rs", n)).collect(),
            lints: (0..40).map(|n| finding("src/d.rs", n)).collect(),
            ..rich.clone()
        };
        for budget in [400, 1_000, 4_000] {
            let block = many.render(budget);
            assert!(block.len() <= budget, "{budget}: {}", block.len());
        }
        // A long heading line leaves no room for a list: still within budget.
        let crowded_heading = DiagnosticsSnapshot {
            servers: vec!["s".repeat(250)],
            ..many
        };
        let block = crowded_heading.render(MIN_BLOCK_BYTES);
        assert!(block.len() <= MIN_BLOCK_BYTES, "{}", block.len());
    }

    #[test]
    fn a_journal_that_kept_only_the_warning_count_still_loads() {
        let stored = json!({"servers": ["rust-analyzer"], "settled": true, "errors": [], "warnings": 6, "finish_refused": false});
        let snapshot: DiagnosticsSnapshot = serde_json::from_value(stored).unwrap();
        assert!(snapshot.warnings.is_empty());
        let round = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_value::<DiagnosticsSnapshot>(round).unwrap(),
            snapshot
        );
    }

    #[test]
    fn positions_are_utf16_and_strict() {
        let text = "a😀b\nsecond\n";
        assert_eq!(byte_offset(text, 0, 0), Some(0));
        // The emoji is two UTF-16 units and four bytes.
        assert_eq!(byte_offset(text, 0, 3), Some(5));
        assert_eq!(byte_offset(text, 0, 2), None, "inside a surrogate pair");
        assert_eq!(byte_offset(text, 0, 4), Some(6), "the line's end");
        assert_eq!(byte_offset(text, 0, 5), None, "past the line's end");
        assert_eq!(byte_offset(text, 1, 6), Some(13));
        assert_eq!(byte_offset(text, 2, 0), Some(text.len()));
        assert_eq!(byte_offset(text, 3, 0), None);
    }

    #[test]
    fn a_fix_applies_only_to_the_text_it_was_offered_for() {
        let edit = |line, from, to, text: &str| json!({"range": {"start": {"line": line, "character": from}, "end": {"line": line, "character": to}}, "newText": text});
        let text = "let v = x.get(0);\nlet w = 1;\n";
        // Out of order, with two inserts at one point kept in their order.
        let edits = fix_edits(
            text,
            &[
                edit(1, 4, 4, "mut "),
                edit(0, 8, 16, "x.first()"),
                edit(0, 0, 0, "// a\n"),
                edit(0, 0, 0, "// b\n"),
            ],
        )
        .unwrap();
        let fix = OfferedFix {
            id: 1,
            title: "try".into(),
            file: "src/lib.rs".into(),
            base: sha256_hex(text),
            edits,
        };
        let after = fix.apply(text).unwrap();
        assert_eq!(after, "// a\n// b\nlet v = x.first();\nlet mut w = 1;\n");
        assert_eq!(fix.apply(&after), None, "stale: the text has changed");
        // Overlapping edits and positions past the text are never offered.
        assert!(fix_edits(text, &[edit(0, 0, 5, "a"), edit(0, 3, 6, "b")]).is_none());
        assert!(fix_edits(text, &[edit(0, 0, 40, "a")]).is_none());
    }

    #[test]
    fn only_a_single_file_text_edit_is_a_fix() {
        let edits = json!([{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "x"}]);
        let (uri, version, found) =
            single_file_edits(&json!({"changes": {"file:///m/src/lib.rs": edits}})).unwrap();
        assert_eq!(
            (uri.as_str(), version, found.len()),
            ("file:///m/src/lib.rs", None, 1)
        );
        let (_, version, _) = single_file_edits(&json!({"documentChanges": [{"textDocument": {"uri": "file:///m/a.rs", "version": 4}, "edits": edits}]})).unwrap();
        assert_eq!(version, Some(4), "checked against the version sent last");
        assert!(single_file_edits(
            &json!({"changes": {"file:///m/a.rs": edits, "file:///m/b.rs": edits}})
        )
        .is_none());
        assert!(single_file_edits(
            &json!({"documentChanges": [{"kind": "create", "uri": "file:///m/new.rs"}]})
        )
        .is_none());
        assert!(single_file_edits(&json!({})).is_none());
    }

    #[test]
    fn the_block_numbers_each_offered_fix_under_its_finding() {
        let fix = |id, title: &str| OfferedFix {
            id,
            title: title.into(),
            file: "src/lib.rs".into(),
            base: String::new(),
            edits: vec![],
        };
        let finding = |line, fixes| Finding {
            file: "src/lib.rs".into(),
            line,
            column: 1,
            message: "problem".into(),
            detail: None,
            definition: None,
            fixes,
        };
        let snapshot = DiagnosticsSnapshot {
            servers: vec!["rust-analyzer".into()],
            settled: true,
            errors: vec![finding(
                3,
                vec![fix(1, "consider changing this to be mutable")],
            )],
            lints: vec![finding(7, vec![fix(2, "try: `v.first()`")])],
            linter: Some("clippy".into()),
            ..Default::default()
        };
        let block = snapshot.render(4_000);
        assert!(
            block.contains(
                "src/lib.rs:3:1 error: problem\n  fix 1: consider changing this to be mutable\n"
            ),
            "{block}"
        );
        assert!(
            block.contains("src/lib.rs:7:1 lint: problem\n  fix 2: try: `v.first()`\n"),
            "{block}"
        );
        assert_eq!(snapshot.fix(2).unwrap().title, "try: `v.first()`");
        assert!(snapshot.fix(3).is_none());
    }
}
