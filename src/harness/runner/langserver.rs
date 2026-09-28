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
use crate::code::substrate::lang::{language_servers, unresolved_names, LinterSpec, ServerSpec};
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
    /// For a name the error says is unresolved and the server located nowhere:
    /// declarations of that NAME in the task's files, "a `Terrain` is declared
    /// at file:line: <that line>". A lexical match, never presented as where
    /// the symbol is defined (Constraint 6bf5ef13).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<String>,
    /// Quick fixes the server offers for it, numbered for `apply_fix`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<OfferedFix>,
    /// Whether `fixes` is everything the server offered that it prefers: no
    /// cap, deadline or failed request cut the list, and no fix it prefers was
    /// dropped or can be applied only in part. Only then does "exactly one
    /// preferred fix" say something about the server's answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixes_complete: bool,
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
    /// The server marks it the fix to apply (LSP `isPreferred`; for rustc
    /// and clippy suggestions, machine-applicable ones).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preferred: bool,
}

/// What a server's code action became.
enum Offer {
    Fix(OfferedFix),
    /// A fix whose follow-up command the harness does not run: offered to
    /// the model as its edit alone, never as the server's preferred fix.
    /// `preferred` says the server preferred it.
    Partial {
        fix: OfferedFix,
        preferred: bool,
    },
    /// Not a fix the harness can apply (a bare command, another kind, an
    /// edit outside the plan). The list is complete without it unless the
    /// server preferred it: then the server's choice is not in the list.
    Filtered {
        preferred: bool,
    },
    /// The server failed to resolve it: the list may be missing a fix.
    Failed,
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
    /// The linters that ran, when any did, comma-separated (`clippy`,
    /// `clippy, ruff`).
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

    /// Why `id` names no offered fix, in words that lead somewhere: whether any
    /// fix is offered at all, and whether `id` is a finding's line number
    /// instead (badciv e3c533b4: a4b sent 131 and 135, a lint's lines, when no
    /// fix was offered).
    pub(super) fn unknown_fix(&self, id: usize) -> String {
        let offered = self
            .findings()
            .map(|finding| finding.fixes.len())
            .sum::<usize>();
        let mut reason = if offered == 0 {
            format!("no fix {id} is offered: no finding has a quick fix now; make the change with replace or write")
        } else {
            format!("no fix {id} is offered now; name a fix listed under an error or lint in the language server block")
        };
        if let Some(finding) = self.findings().find(|finding| finding.line as usize == id) {
            reason.push_str(&format!(
                "; {id} is the line of {}:{}: {}, not a fix number",
                finding.file,
                finding.line,
                finding.message.lines().next().unwrap_or_default()
            ));
        }
        reason
    }

    /// The fix the harness applies itself, if any (offloading change 2): the
    /// first error, then lint, whose complete list holds exactly one fix the
    /// server prefers, and that fix does not only delete. Warnings never:
    /// rustc's fixes for unused items delete scaffolding the model is about
    /// to use, or hide an omission (badciv edc914f6's `_key`).
    pub(super) fn auto_fix(&self) -> Option<(&Finding, &OfferedFix)> {
        if !self.settled {
            return None;
        }
        self.errors.iter().chain(&self.lints).find_map(|finding| {
            if !finding.fixes_complete {
                return None;
            }
            let mut preferred = finding.fixes.iter().filter(|fix| fix.preferred);
            let fix = preferred.next()?;
            let deletes_only = fix.edits.iter().all(|edit| edit.text.is_empty());
            (preferred.next().is_none() && !deletes_only).then_some((finding, fix))
        })
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
            let chosen = finding.declared.iter().fold(chosen, |out, declared| {
                format!("{out}  found by name: {declared}\n")
            });
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
    /// When diagnostics about the text last sent of each file arrived.
    published_at: HashMap<String, Instant>,
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
            let settings = spec.settings;
            std::thread::spawn(move || read_loop(stdout, outbox, seen, mirror, settings));
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
                // None: the sandbox denies the server a signal to the
                // harness, so a server that polls its parent with
                // `kill(pid, 0)` (pyright, every 3 s) would see it dead and
                // exit. Stopping the harness closes its stdin; the process
                // group goes on drop.
                "processId": Value::Null,
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

    /// Wait until the server has caught up with the edit of `file` sent at
    /// `since`: something arrived after it, nothing has arrived for a quiet
    /// period, no progress is open, and a server that reports its status
    /// says it is quiescent. False at the deadline.
    ///
    /// When this server checks `file`, a server that reports its status must
    /// also have shown work on this edit after `since` (a status report, or
    /// announced work ending): a quiescent flag left over from the previous
    /// edit says nothing about this one. A server that publishes for every
    /// version (pyright) must have published diagnostics about the text just
    /// sent of `file` when it has it open: it may say nothing while it
    /// analyzes, and its silence is not a clean result.
    pub(super) async fn settle(&mut self, file: &str, since: Instant, timeout: Duration) -> bool {
        const QUIET: Duration = Duration::from_millis(800);
        const FIRST_WORD: Duration = Duration::from_millis(1500);
        let timeout = if self.settled_once {
            timeout
        } else {
            timeout.max(Duration::from_secs(120))
        };
        let deadline = since + timeout;
        let needs_status = self.concerns(file);
        let awaits_report = self.spec.publishes_every_version && self.open.contains(file);
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
                    let reported =
                        !awaits_report || seen.published_at.get(file).is_some_and(|at| *at > since);
                    !seen.exited
                        && reported
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
    /// [`fix_cap`] of the diagnostics it stands for, with ids still to
    /// assign. Only fixes the harness can apply as one ordinary edit are
    /// kept: text edits to a single file among `editable` that change it.
    /// Any failure is no fixes.
    /// The fixes offered for `finding`, and whether that is the whole list:
    /// false when the cap, the deadline or a failed request cut it short.
    async fn fixes(
        &mut self,
        finding: &Finding,
        editable: &[String],
        timeout: Duration,
    ) -> (Vec<OfferedFix>, bool) {
        let deadline = Instant::now() + timeout;
        let mut fixes: Vec<OfferedFix> = Vec::new();
        let mut complete = true;
        let (targets, merged) = self.fix_targets(finding);
        let cap = fix_cap(merged);
        for (file, diagnostic) in targets {
            let Ok(uri) = uri(&self.mirror.join(&file)) else {
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(id) = self.request(
                "textDocument/codeAction",
                json!({"textDocument": {"uri": uri}, "range": diagnostic.range, "context": {"diagnostics": [diagnostic], "only": ["quickfix"]}}),
            ) else {
                return (fixes, false);
            };
            let actions = match self.response(id, left).await {
                Ok(Value::Array(actions)) => actions,
                Ok(Value::Null) => continue,
                _ => {
                    complete = false;
                    continue;
                }
            };
            for action in actions {
                if fixes.len() == cap || Instant::now() >= deadline {
                    return (fixes, false);
                }
                // The same fix arrives for the error and for its related
                // hint; distinct fixes may share a title.
                match self.offered(action, editable, deadline).await {
                    Offer::Fix(fix) => {
                        if !fixes
                            .iter()
                            .any(|known| known.file == fix.file && known.edits == fix.edits)
                        {
                            fixes.push(fix);
                        }
                    }
                    Offer::Partial { fix, preferred } => {
                        complete &= !preferred;
                        if !fixes
                            .iter()
                            .any(|known| known.file == fix.file && known.edits == fix.edits)
                        {
                            fixes.push(fix);
                        }
                    }
                    Offer::Filtered { preferred } => complete &= !preferred,
                    Offer::Failed => complete = false,
                }
            }
        }
        (fixes, complete)
    }

    /// One code action as a fix the harness can apply, resolving its edit
    /// when the server sends it without one.
    async fn offered(&mut self, action: Value, editable: &[String], deadline: Instant) -> Offer {
        // A bare Command (its `command` a string) is only runnable by the
        // server's own client. A CodeAction's follow-up command, when it has
        // one, is left unrun: its edit is offered alone (`Offer::Partial`).
        let listed_preferred = action["isPreferred"].as_bool() == Some(true);
        let Some(title) = action["title"].as_str().map(str::to_owned) else {
            return Offer::Filtered {
                preferred: listed_preferred,
            };
        };
        if action["command"].is_string()
            || action.get("disabled").is_some()
            || action["kind"]
                .as_str()
                .is_some_and(|kind| !kind.starts_with("quickfix"))
        {
            return Offer::Filtered {
                preferred: listed_preferred,
            };
        }
        // Read before a resolve request takes the action.
        let mut preferred = listed_preferred;
        let mut follow_up = action["command"].is_object();
        let edit = if action["edit"].is_object() {
            action["edit"].clone()
        } else if action.get("data").is_some() {
            let Ok(id) = self.request("codeAction/resolve", action) else {
                return Offer::Failed;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(resolved) = self.response(id, left).await else {
                return Offer::Failed;
            };
            preferred |= resolved["isPreferred"].as_bool() == Some(true);
            follow_up |= resolved["command"].is_object();
            resolved["edit"].clone()
        } else {
            return Offer::Filtered { preferred };
        };
        match self.fix_from_edit(&edit, title, preferred && !follow_up, editable) {
            Some(fix) if follow_up => Offer::Partial { fix, preferred },
            Some(fix) => Offer::Fix(fix),
            None => Offer::Filtered { preferred },
        }
    }

    /// A resolved WorkspaceEdit as a fix, when it is one text edit to a
    /// planned file of the current version that changes something.
    fn fix_from_edit(
        &self,
        edit: &Value,
        title: String,
        preferred: bool,
        editable: &[String],
    ) -> Option<OfferedFix> {
        let (target, version, edits) = single_file_edits(edit)?;
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
            preferred,
        })
    }

    /// The server's diagnostics to ask for fixes of `finding`, and how many
    /// diagnostics it stands for.
    fn fix_targets(&self, finding: &Finding) -> (Vec<(String, lsp_types::Diagnostic)>, usize) {
        let Ok(seen) = self.seen.lock() else {
            return (Vec::new(), 0);
        };
        fix_targets(&seen.diagnostics, finding, &self.mirror)
    }

    /// The name and diagnostic `source` of the linter this server runs: the
    /// one it was started with, or the server itself when it is one.
    pub(super) fn lint(&self) -> Option<(&'static str, &'static str)> {
        match (self.linter, self.spec.lint_source) {
            (Some(linter), _) => Some((linter.name, linter.source)),
            (None, Some(source)) => Some((self.spec.name, source)),
            (None, None) => None,
        }
    }

    /// Current errors, other warnings, and the linter's findings.
    pub(super) fn findings(&self) -> Findings {
        let Ok(seen) = self.seen.lock() else {
            return Findings::default();
        };
        let source = self.lint().map(|(_, source)| source);
        findings(&seen.diagnostics, source, &self.mirror)
    }
}

/// The diagnostics to ask for fixes of `finding`: every one it stands for,
/// then those at their related locations, and how many it stands for. A
/// compiler suggestion often sits away from the error (rustc's `mut` goes on
/// the `let`, not the borrow), and rust-analyzer publishes it there as a hint
/// carrying the fix. A finding stands for each diagnostic of its line and
/// message, whatever the column (the key [`findings`] merges them by):
/// rust-analyzer publishes "unresolved imports `a::B`, `a::C`" once per
/// name, each with that name's own fix.
fn fix_targets(
    diagnostics: &HashMap<String, Vec<lsp_types::Diagnostic>>,
    finding: &Finding,
    mirror: &Path,
) -> (Vec<(String, lsp_types::Diagnostic)>, usize) {
    let own: Vec<&lsp_types::Diagnostic> = diagnostics
        .get(&finding.file)
        .into_iter()
        .flatten()
        .filter(|diagnostic| {
            diagnostic.range.start.line + 1 == finding.line && diagnostic.message == finding.message
        })
        .collect();
    let merged = own.len();
    let mut targets: Vec<(String, lsp_types::Diagnostic)> = own
        .iter()
        .map(|diagnostic| (finding.file.clone(), (*diagnostic).clone()))
        .collect();
    for diagnostic in own {
        for related in diagnostic.related_information.iter().flatten().take(3) {
            let Some(file) = relative(related.location.uri.as_str(), mirror) else {
                continue;
            };
            let at = diagnostics
                .get(&file)
                .into_iter()
                .flatten()
                .filter(|diagnostic| diagnostic.range == related.location.range);
            for diagnostic in at.take(2) {
                let target = (file.clone(), diagnostic.clone());
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
    }
    (targets, merged)
}

/// Errors, other warnings, and the linter's findings of `diagnostics`, one
/// finding per file, line and message: rust-analyzer published badciv P5's
/// "unresolved imports `badciv_map::Terrain`, …" four times, once per name's
/// column, and the prompt listed it four times. The lowest column is kept;
/// [`fix_targets`] still asks for every column's fixes.
///
/// A warning is a lint when its `source` is `lint_source`, the linter's.
fn findings(
    diagnostics: &HashMap<String, Vec<lsp_types::Diagnostic>>,
    lint_source: Option<&str>,
    mirror: &Path,
) -> Findings {
    let mut found = Findings::default();
    let files: BTreeMap<&String, &Vec<lsp_types::Diagnostic>> = diagnostics.iter().collect();
    for (file, diagnostics) in files {
        for diagnostic in diagnostics {
            let finding = || Finding {
                file: file.clone(),
                line: diagnostic.range.start.line + 1,
                column: diagnostic.range.start.character + 1,
                message: diagnostic.message.clone(),
                detail: detail(diagnostic, mirror),
                definition: None,
                declared: Vec::new(),
                fixes: Vec::new(),
                fixes_complete: false,
            };
            let lint = lint_source.is_some() && diagnostic.source.as_deref() == lint_source;
            match diagnostic.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) | None => found.errors.push(finding()),
                Some(lsp_types::DiagnosticSeverity::WARNING) if lint => found.lints.push(finding()),
                Some(lsp_types::DiagnosticSeverity::WARNING) => found.warnings.push(finding()),
                _ => {}
            }
        }
    }
    for list in [&mut found.errors, &mut found.warnings, &mut found.lints] {
        list.sort_by(|a, b| {
            (&a.file, a.line, &a.message, a.column).cmp(&(&b.file, b.line, &b.message, b.column))
        });
        list.dedup_by(|later, kept| {
            later.file == kept.file && later.line == kept.line && later.message == kept.message
        });
        list.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    }
    found
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
    settings: fn(&str) -> Value,
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
                                state.published_at.insert(file.clone(), Instant::now());
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
                // Answer what the server asks: configuration from the
                // language's settings, by section (null for any it does not
                // name), and accept progress tokens and registrations.
                let result = match request.method.as_str() {
                    "workspace/configuration" => configuration(&request.params, settings),
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

/// The answer to a `workspace/configuration` request: one value per item,
/// by the item's section.
fn configuration(params: &Value, settings: fn(&str) -> Value) -> Value {
    params["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| item["section"].as_str().map_or(Value::Null, settings))
        .collect()
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
                notes.push(match spec.lint_source {
                    Some(_) => (
                        "language_linter_missing",
                        format!(
                            "No linter for {}: {} is not installed.",
                            spec.language, spec.name
                        ),
                    ),
                    None => (
                        "language_server",
                        format!("Language server {}: not installed.", spec.name),
                    ),
                });
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
            settled &= server.settle(file, since, timeout).await;
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
        let mut linters = Vec::new();
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
                (finding.fixes, finding.fixes_complete) =
                    server.fixes(finding, editable, left).await;
            }
            snapshot.errors.extend(found.errors);
            snapshot.lints.extend(found.lints);
            snapshot.warnings.extend(found.warnings);
            if let Some((linter, _)) = server.lint() {
                linters.push(linter);
            }
        }
        snapshot.linter = (!linters.is_empty()).then(|| linters.join(", "));
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
/// Declarations found by name listed per error at most.
const DECLARED_BY_NAME: usize = 2;
/// All definition lookups of one result share this deadline.
const DEFINITION_TIME: Duration = Duration::from_secs(4);
/// Errors, warnings and lints, each, per result whose quick fixes are asked
/// for.
const FIXED_FINDINGS: usize = 5;
/// Quick fixes kept per diagnostic a finding stands for.
const FIXES_PER_FINDING: usize = 3;
/// Quick fixes kept per finding at most, however many diagnostics it merged.
const FIXES_PER_MERGED_FINDING: usize = 8;

/// The quick fixes kept for a finding that stands for `merged` diagnostics:
/// each merged diagnostic (one per name of an unresolved-imports error) may
/// carry its own fix, so the cap grows with them, within a bound.
fn fix_cap(merged: usize) -> usize {
    (FIXES_PER_FINDING * merged.max(1)).min(FIXES_PER_MERGED_FINDING)
}
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
    /// True when the servers reported on this edit: a result that is about
    /// exactly the source now on disk.
    pub(super) async fn check_applied_edit(
        &mut self,
        file: &str,
        existed: bool,
        after: Option<&str>,
    ) -> bool {
        let Some(settings) = self.language_settings.filter(|settings| settings.enabled) else {
            return false;
        };
        match self
            .check_applied_edit_inner(file, existed, after, settings.settle_timeout)
            .await
        {
            // A result settles even for a file no server reads (a fixture, a
            // note); it is about this edit only when a server checks the file.
            Ok(fresh) => {
                fresh
                    && language_servers()
                        .any(|spec| spec.language_of(file).is_some() || spec.is_project_file(file))
            }
            Err(error) => {
                self.intent_event("language_server_error", &format!("{error:#}"));
                self.event(format!(
                    "Language server stopped: {error:#}. The task continues without it."
                ));
                self.language = LanguageState::Unavailable;
                // What the server last said is about a source that no longer
                // exists; neither the prompt nor the finish gate may use it.
                self.task.diagnostics = None;
                false
            }
        }
    }

    async fn check_applied_edit_inner(
        &mut self,
        file: &str,
        existed: bool,
        after: Option<&str>,
        timeout: Duration,
    ) -> Result<bool> {
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
                return Ok(false);
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
            return Ok(false);
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
            return Ok(false);
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
                    "{}: {} error(s), {} warning(s), {} lint(s), {} after {} ms, {} fix(es), {} preferred",
                    snapshot.servers.join(", "),
                    snapshot.errors.len(),
                    snapshot.warnings.len(),
                    snapshot.lints.len(),
                    if snapshot.settled {
                        "settled"
                    } else {
                        "NOT settled"
                    },
                    started.elapsed().as_millis(),
                    snapshot.findings().map(|finding| finding.fixes.len()).sum::<usize>(),
                    snapshot
                        .findings()
                        .flat_map(|finding| &finding.fixes)
                        .filter(|fix| fix.preferred)
                        .count()
                ),
            );
            let mut snapshot = snapshot;
            self.declare_unresolved_names(&mut snapshot);
            self.task.diagnostics = Some(snapshot);
            return Ok(true);
        }
        Ok(false)
    }

    /// For each of the first errors the server located nowhere whose message
    /// names unresolved names (badciv P5: "unresolved imports
    /// `badciv_map::Terrain`, …" while `Terrain` sat in `codes.rs`), the
    /// declarations of those names in the task's files: those read, edited or
    /// planned, at their current text. Found by name, so reported as such.
    fn declare_unresolved_names(&self, snapshot: &mut DiagnosticsSnapshot) {
        if !snapshot.settled {
            return;
        }
        let mut files: Vec<&str> = self
            .task
            .read_files
            .iter()
            .chain(self.task.edits.iter().map(|edit| &edit.file))
            .chain(self.task.plan.iter().flat_map(|plan| plan.files.iter()))
            .map(String::as_str)
            .collect();
        files.sort_unstable();
        files.dedup();
        let mut outlines: HashMap<&str, Vec<crate::code::substrate::OutlineEntry>> = HashMap::new();
        for error in snapshot.errors.iter_mut().take(DEFINED_ERRORS) {
            if error.definition.is_some() {
                continue;
            }
            let names = unresolved_names(&error.message);
            if names.is_empty() {
                continue;
            }
            'names: for name in &names {
                for file in &files {
                    let outline =
                        outlines
                            .entry(file)
                            .or_insert_with(|| match self.workspace.read(file) {
                                Ok(Some(text)) => {
                                    crate::code::substrate::outline(file, &text).unwrap_or_default()
                                }
                                _ => Vec::new(),
                            });
                    for entry in outline
                        .iter()
                        .filter(|entry| entry.name.as_deref() == Some(name.as_str()))
                    {
                        if error.declared.len() == DECLARED_BY_NAME {
                            break 'names;
                        }
                        error.declared.push(format!(
                            "a `{name}` is declared at {file}:{}: {}",
                            entry.line, entry.text
                        ));
                    }
                }
            }
        }
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
        // Machine-applicable: preferred, the list complete, and the fix the
        // harness applies itself (offloading change 2).
        let flags = |s: &DiagnosticsSnapshot| {
            s.findings()
                .map(|f| {
                    format!(
                        "{}:{} complete={} [{}]",
                        f.file,
                        f.line,
                        f.fixes_complete,
                        f.fixes
                            .iter()
                            .map(|x| format!("{} preferred={}", x.title, x.preferred))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                })
                .collect::<Vec<_>>()
        };
        eprintln!("clippy: {:?}", flags(&snapshot));
        let (_, chosen) = snapshot
            .auto_fix()
            .unwrap_or_else(|| panic!("clippy's fix is the one to apply: {:?}", flags(&snapshot)));
        assert!(chosen.title.contains("v.first()"), "{chosen:?}");
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
        eprintln!("mut: {:?}", flags(&snapshot));
        let (_, chosen) = snapshot.auto_fix().unwrap_or_else(|| {
            panic!(
                "the missing mut is the one to apply: {:?}",
                flags(&snapshot)
            )
        });
        assert!(chosen.apply(immutable).unwrap().contains("let mut v"));
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
        // A warning's fix is never applied by the harness, preferred or not.
        eprintln!("unused: {:?}", flags(&snapshot));
        assert!(snapshot.auto_fix().is_none(), "{:?}", flags(&snapshot));
        drop(servers);
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Real pyright (basedpyright when installed) and ruff under the macOS
    /// sandbox, over a mirror: a type error comes from the type checker, a
    /// lint from ruff with its fix, an undefined name once, and a clean edit
    /// settles clean.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires basedpyright or pyright, ruff and a functional OS sandbox; run explicitly"]
    async fn python_servers_settle_to_the_errors_of_each_edit() {
        let project = std::env::temp_dir().join(format!("moosedev-py-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(project.join("pkg")).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            "[project]\nname = \"py-probe\"\nversion = \"0.1.0\"\n\n[tool.ruff]\nextend-exclude = [\"pkg/gen.py\"]\n",
        )
        .unwrap();
        std::fs::write(project.join("pkg/__init__.py"), "").unwrap();
        let clean = "def one() -> int:\n    return 1\n";
        std::fs::write(project.join("pkg/mod.py"), clean).unwrap();
        let directory =
            std::env::temp_dir().join(format!("moosedev-py-lsp-{}", uuid::Uuid::new_v4()));
        let files = ["pyproject.toml", "pkg/__init__.py", "pkg/mod.py"].map(str::to_owned);
        let (mut servers, notes) = LanguageServers::start(&project, &directory, &[], &files)
            .await
            .unwrap();
        eprintln!("{notes:?}");
        let names: Vec<&str> = servers.servers.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["pyright", "ruff"], "{notes:?}");

        let editable = ["pkg/mod.py".to_owned()];
        async fn edit(
            servers: &mut LanguageServers,
            project: &Path,
            text: &str,
            editable: &[String],
        ) -> DiagnosticsSnapshot {
            std::fs::write(project.join("pkg/mod.py"), text).unwrap();
            let started = Instant::now();
            let snapshot = servers
                .after_edit(
                    "pkg/mod.py",
                    true,
                    Some(text),
                    Duration::from_secs(60),
                    editable,
                )
                .await
                .unwrap()
                .unwrap();
            eprintln!(
                "settled={} in {:?}:\n{}",
                snapshot.settled,
                started.elapsed(),
                snapshot.render(4_000)
            );
            assert!(snapshot.settled, "{snapshot:?}");
            snapshot
        }

        // A type error, from the type checker; both servers named, and ruff
        // the linter.
        let typed = "x: int = \"a\"\n";
        let snapshot = edit(&mut servers, &project, typed, &editable).await;
        assert_eq!(snapshot.servers, ["pyright", "ruff"]);
        assert_eq!(snapshot.linter.as_deref(), Some("ruff"));
        assert!(
            snapshot
                .errors
                .iter()
                .any(|e| e.line == 1 && e.message.contains("is not assignable")),
            "{snapshot:?}"
        );
        assert!(snapshot.lints.is_empty(), "{snapshot:?}");

        // An unused import: ruff's lint (F401) alone, its removal offered as
        // the fix ruff prefers. The harness does not apply it itself: a fix
        // that only deletes may take scaffolding the model is about to use.
        let unused = "import os\n\n\ndef one() -> int:\n    return 1\n";
        let snapshot = edit(&mut servers, &project, unused, &editable).await;
        assert!(
            snapshot.errors.is_empty() && snapshot.warnings.is_empty(),
            "the standard type check does not report it too: {snapshot:?}"
        );
        let [lint] = snapshot.lints.as_slice() else {
            panic!("one lint: {snapshot:?}");
        };
        assert!(
            lint.message.contains("`os` imported but unused"),
            "{lint:?}"
        );
        assert!(lint.fixes_complete, "{lint:?}");
        let preferred: Vec<&OfferedFix> = lint.fixes.iter().filter(|f| f.preferred).collect();
        let [removal] = preferred.as_slice() else {
            panic!("one preferred fix: {lint:?}");
        };
        assert!(
            removal.title.contains("Remove unused import"),
            "{removal:?}"
        );
        // No `# noqa` comment is offered: it silences, it does not fix.
        assert!(
            lint.fixes.iter().all(|f| !f.title.contains("Disable")),
            "{lint:?}"
        );
        assert_eq!(
            removal.apply(unused).unwrap(),
            "\n\ndef one() -> int:\n    return 1\n"
        );
        assert!(snapshot.auto_fix().is_none(), "{lint:?}");

        // A lint whose preferred fix replaces code (F632): the harness
        // applies that one itself, and it clears the lint.
        let literal = "def same(x: str) -> bool:\n    return x is \"a\"\n";
        let snapshot = edit(&mut servers, &project, literal, &editable).await;
        assert!(snapshot.errors.is_empty(), "{snapshot:?}");
        let (finding, chosen) = snapshot
            .auto_fix()
            .unwrap_or_else(|| panic!("ruff's fix is the one to apply: {snapshot:?}"));
        assert!(finding.message.contains("Use `==`"), "{finding:?}");
        let fixed = chosen.apply(literal).unwrap();
        assert!(fixed.contains("return x == \"a\""), "{fixed}");
        let snapshot = edit(&mut servers, &project, &fixed, &editable).await;
        assert!(snapshot.findings().next().is_none(), "{snapshot:?}");

        // An undefined name is reported once, by the type checker: ruff's
        // F821 is ignored.
        let undefined = "def one() -> int:\n    return missing\n";
        let snapshot = edit(&mut servers, &project, undefined, &editable).await;
        let named: Vec<&Finding> = snapshot
            .findings()
            .filter(|f| f.message.contains("missing"))
            .collect();
        assert_eq!(named.len(), 1, "{snapshot:?}");
        assert_eq!(named[0].message, "\"missing\" is not defined");
        assert_eq!(snapshot.errors.len(), 1, "{snapshot:?}");

        // A clean edit settles clean.
        let snapshot = edit(&mut servers, &project, clean, &editable).await;
        assert!(snapshot.findings().next().is_none(), "{snapshot:?}");
        assert!(
            snapshot
                .render(4_000)
                .contains("(pyright, ruff) after your last edit: no errors, no warnings, 0 lint(s) from ruff."),
            "{}",
            snapshot.render(4_000)
        );

        // A new file the project's ruff configuration excludes: ruff says
        // nothing about it, and the result still settles, with the type
        // checker's error and no lint.
        let generated = "import os\ny: str = 1\n";
        std::fs::write(project.join("pkg/gen.py"), generated).unwrap();
        let snapshot = servers
            .after_edit(
                "pkg/gen.py",
                false,
                Some(generated),
                Duration::from_secs(60),
                &["pkg/gen.py".to_owned()],
            )
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(snapshot.lints.is_empty(), "{snapshot:?}");
        assert!(
            snapshot
                .errors
                .iter()
                .any(|e| e.file == "pkg/gen.py" && e.line == 2),
            "{snapshot:?}"
        );
        drop(servers);
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn diagnostic(line: u32, column: u32, message: &str) -> lsp_types::Diagnostic {
        let at = lsp_types::Position::new(line - 1, column - 1);
        lsp_types::Diagnostic {
            range: lsp_types::Range::new(at, at),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: message.into(),
            ..Default::default()
        }
    }

    /// badciv P5: rust-analyzer published one unresolved-imports error four
    /// times, once per name's column. One finding stands for them, at the
    /// lowest column, and asks for every column's fixes.
    #[test]
    fn one_finding_per_line_and_message_asks_for_every_columns_fixes() {
        let message = "unresolved imports `badciv_map::Terrain`, `badciv_map::Climate`, `badciv_map::Resource`, `badciv_map::Faction`";
        let file = "badciv-map/tests/tiny_fixture.rs".to_string();
        let mirror = Path::new("/tmp/mirror");
        let published = vec![
            diagnostic(1, 60, message),
            diagnostic(1, 17, message),
            diagnostic(1, 40, message),
            diagnostic(1, 26, message),
            diagnostic(1, 5, "another error on the line"),
            diagnostic(3, 9, message),
        ];
        let diagnostics = HashMap::from([(file.clone(), published)]);
        let found = findings(&diagnostics, None, mirror);
        let listed: Vec<(u32, u32, &str)> = found
            .errors
            .iter()
            .map(|finding| (finding.line, finding.column, finding.message.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (1, 5, "another error on the line"),
                (1, 17, message),
                (3, 9, message)
            ]
        );
        let (targets, merged) = fix_targets(&diagnostics, &found.errors[1], mirror);
        assert_eq!(merged, 4, "the four columns merged into one finding");
        // Each merged diagnostic may bring its own fix: four names keep up
        // to eight fixes, not three; a lone diagnostic keeps three.
        assert_eq!(fix_cap(merged), 8);
        assert_eq!(fix_cap(2), 6);
        assert_eq!(fix_cap(1), FIXES_PER_FINDING);
        assert_eq!(fix_cap(0), FIXES_PER_FINDING);
        let mut columns: Vec<u32> = targets
            .iter()
            .map(|(target, diagnostic)| {
                assert_eq!(target, &file);
                diagnostic.range.start.character + 1
            })
            .collect();
        columns.sort_unstable();
        assert_eq!(columns, [17, 26, 40, 60], "every column, not line 3's");
        // A finding whose diagnostic is gone asks for nothing.
        let mut gone = found.errors[1].clone();
        gone.message = "changed".into();
        assert!(fix_targets(&diagnostics, &gone, mirror).0.is_empty());
    }

    /// An unresolved name the server located nowhere is pointed at its
    /// declarations in the task's files, found by name and said so.
    #[tokio::test]
    async fn an_unresolved_name_points_at_a_declaration_found_by_name() {
        use super::super::test_support::{context_router, serve, Project};
        let project = Project::new("declared-by-name");
        std::fs::create_dir_all(project.0.join("badciv-map/src")).unwrap();
        std::fs::write(
            project.0.join("badciv-map/src/codes.rs"),
            "#[derive(Debug)]\npub enum Terrain {\n    Plains,\n}\n\npub enum Climate {}\n",
        )
        .unwrap();
        std::fs::write(project.0.join("badciv-map/src/lib.rs"), "mod codes;\n").unwrap();
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = super::super::Runner::create(project.0.clone(), daemon, "Fix it".into())
            .await
            .unwrap();
        runner.task.read_files = vec![
            "badciv-map/src/lib.rs".into(),
            "badciv-map/src/codes.rs".into(),
        ];
        let error = |line, message: &str, definition: Option<&str>| Finding {
            file: "badciv-map/tests/tiny_fixture.rs".into(),
            line,
            column: 5,
            message: message.into(),
            detail: None,
            definition: definition.map(str::to_owned),
            declared: Vec::new(),
            fixes: vec![],
            fixes_complete: false,
        };
        let mut snapshot = DiagnosticsSnapshot {
            servers: vec!["rust-analyzer".into()],
            settled: true,
            errors: vec![
                error(
                    1,
                    "unresolved imports `badciv_map::Terrain`, `badciv_map::Climate`, `badciv_map::Faction`",
                    None,
                ),
                // The server located it: its answer stands alone.
                error(2, "cannot find type `Terrain` in this scope", Some("x.rs:1: y")),
                error(3, "mismatched types", None),
            ],
            ..Default::default()
        };
        runner.declare_unresolved_names(&mut snapshot);
        assert_eq!(
            snapshot.errors[0].declared,
            [
                "a `Terrain` is declared at badciv-map/src/codes.rs:2: pub enum Terrain {",
                "a `Climate` is declared at badciv-map/src/codes.rs:6: pub enum Climate {}",
            ]
        );
        assert!(snapshot.errors[1].declared.is_empty());
        assert!(snapshot.errors[2].declared.is_empty());
        let block = snapshot.render(3_000);
        assert!(
            block.contains("  found by name: a `Terrain` is declared at badciv-map/src/codes.rs:2: pub enum Terrain {\n"),
            "{block}"
        );
        assert!(
            !block.contains("defined at badciv-map/src/codes.rs"),
            "{block}"
        );
        // An unsettled result is not looked at, and an old journal loads.
        let mut unsettled = snapshot.clone();
        unsettled.settled = false;
        unsettled.errors[0].declared.clear();
        runner.declare_unresolved_names(&mut unsettled);
        assert!(unsettled.errors[0].declared.is_empty());
        let old: Finding = serde_json::from_value(json!({
            "file": "a.rs", "line": 1, "column": 1, "message": "m"
        }))
        .unwrap();
        assert!(old.declared.is_empty());
        server.abort();
    }

    /// A warning is a lint by its source: ruff, a server of its own, is the
    /// linter of Python beside pyright. Its errors stay errors; the other
    /// server's warnings stay warnings; information and hints are dropped.
    #[test]
    fn a_warning_is_a_lint_when_its_source_is_the_linters() {
        let with = |line, severity, source: &str| lsp_types::Diagnostic {
            severity: Some(severity),
            source: Some(source.into()),
            ..diagnostic(line, 1, &format!("{source} {line}"))
        };
        use lsp_types::DiagnosticSeverity as S;
        let published = vec![
            with(1, S::WARNING, "Ruff"),
            with(2, S::ERROR, "Ruff"),
            with(3, S::WARNING, "Pyright"),
            with(4, S::ERROR, "Pyright"),
            with(5, S::HINT, "Ruff"),
            with(6, S::INFORMATION, "Pyright"),
        ];
        let diagnostics = HashMap::from([("pkg/mod.py".to_string(), published)]);
        let lines = |findings: &[Finding]| findings.iter().map(|f| f.line).collect::<Vec<_>>();
        let found = findings(&diagnostics, Some("Ruff"), Path::new("/m"));
        assert_eq!(lines(&found.lints), [1]);
        assert_eq!(lines(&found.errors), [2, 4]);
        assert_eq!(lines(&found.warnings), [3]);
        // Without a linter every warning is a warning.
        let found = findings(&diagnostics, None, Path::new("/m"));
        assert!(found.lints.is_empty());
        assert_eq!(lines(&found.warnings), [1, 3]);
    }

    /// Each `workspace/configuration` item is answered from the language's
    /// settings by its section; an item without one, with null.
    #[test]
    fn configuration_is_answered_by_section() {
        fn settings(section: &str) -> Value {
            match section {
                "python" => json!({"analysis": {"typeCheckingMode": "standard"}}),
                _ => Value::Null,
            }
        }
        let params = json!({"items": [
            {"scopeUri": "file:///m", "section": "python"},
            {"section": "pyright"},
            {"scopeUri": "file:///m"},
        ]});
        assert_eq!(
            configuration(&params, settings),
            json!([{"analysis": {"typeCheckingMode": "standard"}}, null, null])
        );
        // rust-analyzer's answer is unchanged: null for every item.
        assert_eq!(
            configuration(
                &json!({"items": [{"section": "rust-analyzer"}]}),
                crate::code::substrate::lang::no_settings
            ),
            json!([null])
        );
        assert_eq!(configuration(&json!({}), settings), json!([]));
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
        assert!(any_checked(&files(&["pkg/labels.py"])));
        assert!(any_checked(&files(&["pyproject.toml"])));
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
            declared: Vec::new(),
            fixes: vec![],
            fixes_complete: false,
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
            declared: Vec::new(),
            fixes: vec![],
            fixes_complete: false,
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
                declared: Vec::new(),
                fixes: vec![],
                fixes_complete: false,
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
            preferred: false,
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
    fn auto_fix_takes_the_one_preferred_fix_of_a_complete_list() {
        let fix = |id, preferred: bool, text: &str| OfferedFix {
            id,
            title: format!("fix {id}"),
            file: "src/lib.rs".into(),
            base: String::new(),
            edits: vec![FixEdit {
                start: 0,
                end: 1,
                text: text.into(),
            }],
            preferred,
        };
        let finding = |fixes: Vec<OfferedFix>, complete: bool| Finding {
            file: "src/lib.rs".into(),
            line: 3,
            column: 1,
            message: "problem".into(),
            detail: None,
            definition: None,
            declared: Vec::new(),
            fixes,
            fixes_complete: complete,
        };
        let snapshot = |errors: Vec<Finding>, warnings: Vec<Finding>, lints: Vec<Finding>| {
            DiagnosticsSnapshot {
                settled: true,
                errors,
                warnings,
                lints,
                ..Default::default()
            }
        };
        let chosen = |s: &DiagnosticsSnapshot| s.auto_fix().map(|(_, fix)| fix.id);
        // One preferred among others: taken.
        let one = snapshot(
            vec![finding(vec![fix(1, false, "a"), fix(2, true, "b")], true)],
            vec![],
            vec![],
        );
        assert_eq!(chosen(&one), Some(2));
        // Two preferred, an incomplete list, a pure deletion: none.
        assert_eq!(
            chosen(&snapshot(
                vec![finding(vec![fix(1, true, "a"), fix(2, true, "b")], true)],
                vec![],
                vec![]
            )),
            None
        );
        assert_eq!(
            chosen(&snapshot(
                vec![finding(vec![fix(1, true, "a")], false)],
                vec![],
                vec![]
            )),
            None
        );
        assert_eq!(
            chosen(&snapshot(
                vec![finding(vec![fix(1, true, "")], true)],
                vec![],
                vec![]
            )),
            None
        );
        // Warnings never; a lint is taken; an error comes first.
        assert_eq!(
            chosen(&snapshot(
                vec![],
                vec![finding(vec![fix(1, true, "a")], true)],
                vec![]
            )),
            None
        );
        assert_eq!(
            chosen(&snapshot(
                vec![],
                vec![],
                vec![finding(vec![fix(4, true, "a")], true)]
            )),
            Some(4)
        );
        assert_eq!(
            chosen(&snapshot(
                vec![finding(vec![fix(1, true, "a")], true)],
                vec![],
                vec![finding(vec![fix(2, true, "b")], true)]
            )),
            Some(1)
        );
        // A disqualified error does not hide a good lint.
        assert_eq!(
            chosen(&snapshot(
                vec![finding(vec![fix(1, true, "a")], false)],
                vec![],
                vec![finding(vec![fix(2, true, "b")], true)]
            )),
            Some(2)
        );
        // Unsettled: nothing.
        let mut unsettled = one.clone();
        unsettled.settled = false;
        assert_eq!(chosen(&unsettled), None);
        // A journal from before these fields loads and selects nothing.
        let old: Finding = serde_json::from_value(serde_json::json!({
            "file": "src/lib.rs", "line": 3, "column": 1, "message": "problem",
            "fixes": [{"id": 1, "title": "t", "file": "src/lib.rs", "base": "", "edits": [{"start": 0, "end": 1, "text": "a"}]}]
        }))
        .unwrap();
        assert!(!old.fixes_complete && !old.fixes[0].preferred);
        assert_eq!(chosen(&snapshot(vec![old], vec![], vec![])), None);
    }

    #[test]
    fn an_unknown_fix_says_whether_any_fix_exists_and_names_a_line_number() {
        let lint = Finding {
            file: "badciv-map/src/codes.rs".into(),
            line: 135,
            column: 5,
            message: "method `from_str` can be confused for the standard trait method\nmore".into(),
            detail: None,
            definition: None,
            declared: Vec::new(),
            fixes: vec![],
            fixes_complete: false,
        };
        let snapshot = DiagnosticsSnapshot {
            settled: true,
            lints: vec![lint],
            ..Default::default()
        };
        let reason = snapshot.unknown_fix(135);
        assert!(
            reason.contains("no finding has a quick fix now"),
            "{reason}"
        );
        assert!(
            reason.contains("135 is the line of badciv-map/src/codes.rs:135: method `from_str` can be confused for the standard trait method, not a fix number"),
            "{reason}"
        );
        assert!(!snapshot.unknown_fix(4).contains("is the line of"));
    }

    #[test]
    fn the_block_numbers_each_offered_fix_under_its_finding() {
        let fix = |id, title: &str| OfferedFix {
            id,
            title: title.into(),
            file: "src/lib.rs".into(),
            base: String::new(),
            edits: vec![],
            preferred: false,
        };
        let finding = |line, fixes| Finding {
            file: "src/lib.rs".into(),
            line,
            column: 1,
            message: "problem".into(),
            detail: None,
            definition: None,
            declared: Vec::new(),
            fixes,
            fixes_complete: false,
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
