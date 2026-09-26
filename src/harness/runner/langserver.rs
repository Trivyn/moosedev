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

/// One language the harness can check, and the server that checks it.
#[derive(Debug, Clone, Copy)]
pub(super) struct ServerSpec {
    /// Shown in prompts and the journal.
    pub name: &'static str,
    /// Candidate commands, first found on the trusted PATH wins.
    pub commands: &'static [&'static [&'static str]],
    /// File extensions and the language id each is opened with.
    pub languages: &'static [(&'static str, &'static str)],
    /// Files whose creation or deletion changes the project's shape; the
    /// server restarts so it rediscovers the project.
    pub project_files: &'static [&'static str],
    /// The server reports `experimental/serverStatus` (rust-analyzer), whose
    /// `quiescent` flag says when indexing and checking are done.
    pub server_status: bool,
    /// Sent as `initializationOptions`.
    pub options: fn() -> Value,
}

pub(super) const SERVERS: &[ServerSpec] = &[ServerSpec {
    name: "rust-analyzer",
    commands: &[&["rust-analyzer"]],
    languages: &[("rs", "rust")],
    project_files: &["Cargo.toml"],
    server_status: true,
    // Check with `cargo check` on save, so borrow and lifetime errors arrive
    // too, not only rust-analyzer's own analysis.
    options: || json!({"checkOnSave": true, "check": {"command": "check"}}),
}];

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
}

/// What the language servers said after the last applied edit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub servers: Vec<String>,
    /// Every server settled before its deadline; otherwise the errors below
    /// may be incomplete or stale and nothing may be concluded from them.
    pub settled: bool,
    pub errors: Vec<Finding>,
    pub warnings: usize,
    /// The model was already sent back from finish for exactly these errors.
    #[serde(default)]
    pub finish_refused: bool,
}

impl DiagnosticsSnapshot {
    /// The prompt block, bounded: errors only, at most `PER_FILE` per file
    /// and `FILES` files, within `budget` bytes.
    pub(super) fn render(&self, budget: usize) -> String {
        const PER_FILE: usize = 20;
        const FILES: usize = 5;
        let servers = self.servers.join(", ");
        if !self.settled {
            return format!(
                "Language server ({servers}) after your last edit: not settled in time; its errors are unknown, so rely on commands and required checks.\n"
            );
        }
        if self.errors.is_empty() {
            return format!(
                "Language server ({servers}) after your last edit: no errors ({} warning(s)).\n",
                self.warnings
            );
        }
        let mut out = format!(
            "Language server ({servers}) after your last edit: {} error(s), {} warning(s). These are current; fix them before finish.\n",
            self.errors.len(),
            self.warnings
        );
        let mut files: Vec<&str> = Vec::new();
        for finding in &self.errors {
            if !files.contains(&finding.file.as_str()) {
                files.push(&finding.file);
            }
        }
        let mut shown = 0;
        for file in files.iter().take(FILES) {
            for finding in self
                .errors
                .iter()
                .filter(|f| f.file == *file)
                .take(PER_FILE)
            {
                let line = format!(
                    "{}:{}:{} error: {}\n",
                    finding.file,
                    finding.line,
                    finding.column,
                    finding.message.replace('\n', " ")
                );
                if out.len() + line.len() > budget {
                    break;
                }
                out.push_str(&line);
                shown += 1;
            }
        }
        if shown < self.errors.len() {
            out.push_str(&format!(
                "[{} more error(s) not listed; a build command prints them all]\n",
                self.errors.len() - shown
            ));
        }
        out
    }
}

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
    /// The last version sent per document, kept after a close.
    versions: HashMap<String, i32>,
    open: HashSet<String>,
    settled_once: bool,
}

impl LanguageServer {
    /// Spawn `command`, run `initialize` and open nothing yet.
    pub(super) async fn start(
        spec: ServerSpec,
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
                "initializationOptions": (spec.options)(),
                "capabilities": {
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "publishDiagnostics": {"versionSupport": true, "relatedInformation": true},
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

    /// Current errors and the warning count.
    pub(super) fn findings(&self) -> (Vec<Finding>, usize) {
        let Ok(seen) = self.seen.lock() else {
            return (Vec::new(), 0);
        };
        let mut errors = Vec::new();
        let mut warnings = 0;
        let files: BTreeMap<&String, &Vec<lsp_types::Diagnostic>> =
            seen.diagnostics.iter().collect();
        for (file, diagnostics) in files {
            for diagnostic in diagnostics {
                match diagnostic.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) | None => errors.push(Finding {
                        file: file.clone(),
                        line: diagnostic.range.start.line + 1,
                        column: diagnostic.range.start.character + 1,
                        message: diagnostic.message.clone(),
                    }),
                    Some(lsp_types::DiagnosticSeverity::WARNING) => warnings += 1,
                    _ => {}
                }
            }
        }
        errors.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
        errors.dedup();
        (errors, warnings)
    }
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
    path.strip_prefix(&mirror)
        .ok()
        .and_then(|relative| relative.to_str())
        .map(str::to_owned)
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
    ) -> Result<(Self, Vec<String>)> {
        let directory = ServerDirectory::prepare(project, directory)?;
        let mut servers = Vec::new();
        let mut notes = Vec::new();
        for spec in SERVERS {
            if !files
                .iter()
                .any(|file| spec.language_of(file).is_some() || spec.is_project_file(file))
            {
                continue;
            }
            let Some(argv) = spec.argv() else {
                notes.push(format!("{}: not installed", spec.name));
                continue;
            };
            let command = directory.command(&argv, read_paths)?;
            match LanguageServer::start(*spec, command, &directory.mirror()).await {
                Ok(server) => {
                    notes.push(format!("{}: started ({})", spec.name, argv[0]));
                    servers.push(server);
                }
                Err(error) => notes.push(format!("{}: failed to start: {error:#}", spec.name)),
            }
        }
        Ok((Self { directory, servers }, notes))
    }

    /// Mirror one applied edit, tell each concerned server, wait for them to
    /// settle and return what they report. None when no server concerns it.
    pub(super) async fn after_edit(
        &mut self,
        file: &str,
        existed: bool,
        text: Option<&str>,
        timeout: Duration,
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
        for server in &self.servers {
            let (errors, warnings) = server.findings();
            snapshot.errors.extend(errors);
            snapshot.warnings += warnings;
        }
        Ok(Some(snapshot))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

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
            SERVERS.iter().any(|spec| spec.is_project_file(file)) && existed != after.is_some();
        if reshaped {
            // A manifest came or went: start again so the servers
            // rediscover the project, and retry an unavailable language.
            // Nothing said about the old shape may stand meanwhile.
            self.language = LanguageState::Idle;
            self.task.diagnostics = None;
        }
        if matches!(self.language, LanguageState::Idle) {
            let files = self.workspace.files()?;
            let concerned = SERVERS.iter().any(|spec| {
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
            for note in notes {
                self.intent_event("language_server", &note);
                self.event(format!("Language server {note}."));
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
        let LanguageState::Running(servers) = &mut self.language else {
            return Ok(());
        };
        let started = Instant::now();
        if let Some(snapshot) = servers.after_edit(file, existed, after, timeout).await? {
            let elapsed = started.elapsed().as_secs_f32();
            self.event(if snapshot.settled {
                format!(
                    "{}: {} error(s), {} warning(s) after {file} (settled in {elapsed:.1} s).",
                    snapshot.servers.join(", "),
                    snapshot.errors.len(),
                    snapshot.warnings
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
                    "{}: {} error(s), {} warning(s), {} after {} ms",
                    snapshot.servers.join(", "),
                    snapshot.errors.len(),
                    snapshot.warnings,
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

        let broken = "pub fn one() -> u32 { \"one\" }\n";
        std::fs::write(project.join("src/lib.rs"), broken).unwrap();
        let snapshot = servers
            .after_edit("src/lib.rs", true, Some(broken), Duration::from_secs(60))
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
        let lifetime = "pub struct E { pub section: &'static str }\npub fn make(section: &str) -> E { E { section } }\npub fn one() -> u32 { 1 }\n";
        let snapshot = servers
            .after_edit("src/lib.rs", true, Some(lifetime), Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(
            snapshot
                .errors
                .iter()
                .any(|e| e.line == 2 && e.message.contains("lifetime")),
            "{snapshot:?}"
        );

        let fixed = "pub fn one() -> u32 { 1 }\n";
        let snapshot = servers
            .after_edit("src/lib.rs", true, Some(fixed), Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.settled, "{snapshot:?}");
        assert!(snapshot.errors.is_empty(), "{snapshot:?}");
        drop(servers);
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_uri_round_trips_to_a_mirror_relative_path() {
        let mirror = Path::new("/tmp/x y/source");
        let uri = uri(&mirror.join("src/a b.rs")).unwrap();
        assert_eq!(uri, "file:///tmp/x%20y/source/src/a%20b.rs");
        assert_eq!(relative(&uri, mirror).as_deref(), Some("src/a b.rs"));
        assert_eq!(relative("file:///elsewhere/a.rs", mirror), None);
    }

    #[test]
    fn the_block_lists_errors_bounded_and_never_calls_unknown_clean() {
        let finding = |file: &str, line| Finding {
            file: file.into(),
            line,
            column: 1,
            message: "mismatched types\nexpected `u8`".into(),
        };
        let snapshot = DiagnosticsSnapshot {
            servers: vec!["rust-analyzer".into()],
            settled: true,
            errors: vec![finding("src/a.rs", 3), finding("src/b.rs", 9)],
            warnings: 2,
            finish_refused: false,
        };
        let block = snapshot.render(3_000);
        assert!(block.contains("2 error(s), 2 warning(s)"), "{block}");
        assert!(
            block.contains("src/a.rs:3:1 error: mismatched types expected `u8`"),
            "{block}"
        );
        let tight = snapshot.render(150);
        assert!(tight.contains("more error(s) not listed"), "{tight}");
        let unknown = DiagnosticsSnapshot {
            settled: false,
            ..snapshot
        };
        assert!(unknown.render(3_000).contains("not settled in time"));
    }
}
