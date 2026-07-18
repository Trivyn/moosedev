//! Knowledge-LSP transport skeleton.
//!
//! Editors spawn `moosedev lsp`, which only relays stdio to this daemon-owned
//! Unix socket. The daemon session shares the same [`AppState`] as MCP/HTTP, so
//! editor hover can serve the same dossiers as the MCP tools.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, FixedOffset};
use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::{
    ClientCapabilities, CodeAction, CodeActionKind, CodeActionParams, CodeActionProviderCapability,
    CodeDescription, CodeLens, CodeLensOptions, Diagnostic, DiagnosticOptions,
    DiagnosticServerCapabilities, DiagnosticSeverity, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, ExecuteCommandOptions,
    ExecuteCommandParams, FullDocumentDiagnosticReport, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, MarkupContent, MarkupKind,
    MessageType, NumberOrString, Position, PositionEncodingKind, PublishDiagnosticsParams, Range,
    RelatedFullDocumentDiagnosticReport, ServerCapabilities, ShowMessageParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Uri,
};
use serde::Deserialize;
use tokio::net::{UnixListener, UnixStream};

use crate::code::substrate::symbols::{logical_path, normalize_symbol};
use crate::code::substrate::{
    DefinitionEntry, Position as SubstratePosition, SourceRange, Substrate,
};
use crate::graph::{
    direct_records_for_entity, entities_by_symbol, get_entity_dossier, is_debt_surface,
    pending_count, render_markdown, AppState, CodeTerms, DossierTarget, ProposalKind,
    RecordSummary, CRITICALITY_LOCALS, ROLE_LOCALS,
};

const LSP_SOCKET_FILE_NAME: &str = "moosedev-lsp.sock";

/// Internal notification injected into a session's message loop at daemon
/// shutdown; never sent by editors (a client sending it merely retracts its
/// own diagnostics and ends its session).
const DAEMON_SHUTDOWN_METHOD: &str = "moosedev/daemonShutdown";

/// Upper bound on waiting for busy sessions to queue their retractions.
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// After every session has queued its retractions, the per-session writer
/// threads still have to drain them onto the sockets; local Unix-socket writes
/// take microseconds, so this is pure margin.
const WRITER_DRAIN_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEncoding {
    Utf8,
    Utf16,
}

#[derive(Debug)]
struct OpenDocument {
    rel_path: String,
    /// Saved source text whose positions the loaded substrate is assumed to use.
    base_text: String,
    /// Exact session substrate whose coordinates `base_text` represents.
    /// `None` means the current disk contents could not be proven to match an
    /// immutable index generation, so every position-bearing surface must stay
    /// silent for this document.
    base_substrate: Option<usize>,
    /// Current editor buffer, including unsaved changes.
    text: String,
    lines: LineMap,
}

impl OpenDocument {
    fn new(
        rel_path: String,
        base_text: String,
        text: String,
        base_substrate: Option<usize>,
    ) -> Self {
        let lines = LineMap::between(&base_text, &text);
        Self {
            rel_path,
            base_text,
            base_substrate,
            text,
            lines,
        }
    }

    fn update_text(&mut self, text: String) {
        self.lines = LineMap::between(&self.base_text, &text);
        self.text = text;
    }

    fn update_base(&mut self, base_text: String, substrate: &Arc<Substrate>) {
        self.lines = LineMap::between(&base_text, &self.text);
        self.base_text = base_text;
        self.base_substrate = Some(substrate_identity(substrate));
    }

    fn invalidate_base(&mut self) {
        self.base_substrate = None;
    }

    fn is_aligned_with(&self, substrate: &Arc<Substrate>) -> bool {
        self.base_substrate == Some(substrate_identity(substrate))
    }
}

fn substrate_identity(substrate: &Arc<Substrate>) -> usize {
    Arc::as_ptr(substrate) as usize
}

fn indexed_base_text(
    repo_root: &Path,
    substrate: &Arc<Substrate>,
    rel_path: &str,
) -> Option<String> {
    if substrate.repo_root().is_none() {
        // Synthetic substrates are test-only and have no generation artifacts;
        // their caller owns the promise that the fixture text matches the index.
        std::fs::read_to_string(repo_root.join(rel_path)).ok()
    } else {
        substrate.read_indexed_source(rel_path)
    }
}

#[derive(Debug)]
struct LineMap {
    base_to_buffer: Vec<Option<u32>>,
    buffer_to_base: Vec<Option<u32>>,
}

impl LineMap {
    const MAX_DIFF_CELLS: usize = 4_000_000;

    /// Align only byte-identical lines. Inserted lines shift anchors exactly;
    /// changed/deleted lines stay unmapped so editor surfaces remain silent
    /// instead of guessing at an entity's new position.
    fn between(base: &str, buffer: &str) -> Self {
        let base_lines = base.split('\n').collect::<Vec<_>>();
        let buffer_lines = buffer.split('\n').collect::<Vec<_>>();
        let base_len = base_lines.len();
        let buffer_len = buffer_lines.len();
        let mut map = Self {
            base_to_buffer: vec![None; base_len],
            buffer_to_base: vec![None; buffer_len],
        };
        if base == buffer {
            for line in 0..base_len {
                map.base_to_buffer[line] = u32::try_from(line).ok();
                map.buffer_to_base[line] = u32::try_from(line).ok();
            }
            return map;
        }

        let leading_equal = base_lines
            .iter()
            .zip(&buffer_lines)
            .take_while(|(base, buffer)| base == buffer)
            .count();
        let trailing_equal = base_lines[leading_equal..]
            .iter()
            .rev()
            .zip(buffer_lines[leading_equal..].iter().rev())
            .take_while(|(base, buffer)| base == buffer)
            .count();
        let changed_base = base_len - leading_equal - trailing_equal;
        let changed_buffer = buffer_len - leading_equal - trailing_equal;
        if changed_base.saturating_mul(changed_buffer) > Self::MAX_DIFF_CELLS {
            return map;
        }

        let mut base_line = 0usize;
        let mut buffer_line = 0usize;
        for part in diff::slice(&base_lines, &buffer_lines) {
            match part {
                diff::Result::Left(_) => base_line += 1,
                diff::Result::Right(_) => buffer_line += 1,
                diff::Result::Both(_, _) => {
                    if let (Ok(base_u32), Ok(buffer_u32)) =
                        (u32::try_from(base_line), u32::try_from(buffer_line))
                    {
                        map.base_to_buffer[base_line] = Some(buffer_u32);
                        map.buffer_to_base[buffer_line] = Some(base_u32);
                    }
                    base_line += 1;
                    buffer_line += 1;
                }
            }
        }
        map
    }

    fn to_buffer(&self, line: u32) -> Option<u32> {
        self.base_to_buffer.get(line as usize).copied().flatten()
    }

    fn to_base(&self, line: u32) -> Option<u32> {
        self.buffer_to_base.get(line as usize).copied().flatten()
    }
}

struct LspSession {
    state: Arc<AppState>,
    connection: Connection,
    encoding: SessionEncoding,
    repo_root: PathBuf,
    diagnostics: DiagnosticsConfig,
    code_lens_enabled: bool,
    nudge_enabled: bool,
    /// Whether the pending-ratifications nudge has fired this session (once only).
    nudged: bool,
    /// Author identity for editor-originated proposals, derived from the
    /// client's `clientInfo.name` at initialize (e.g. `editor:neovim`).
    author: String,
    open_docs: HashMap<String, OpenDocument>,
    /// Substrate generation this session serves from. Advanced ONLY by
    /// `refresh_if_state_changed`, together with every open document's
    /// `base_text`, so served positions and the index never mix generations.
    substrate: Option<Arc<Substrate>>,
    /// Per-file `git log -1` instants for the stale-rationale banner. A
    /// didChange cannot change a file's last commit, so publishes triggered by
    /// typing reuse the memo; cleared per file on save/open and wholesale on a
    /// substrate-generation change.
    commit_instants: HashMap<String, Option<DateTime<FixedOffset>>>,
    last_diagnostics_generation: Option<(usize, u64)>,
    retracted: Arc<AtomicBool>,
}

/// Handle to a running Knowledge-LSP listener: the rendezvous socket path plus
/// the live-session registry the daemon uses to retract published diagnostics
/// before it exits.
pub struct LspListener {
    socket: PathBuf,
    sessions: LspSessions,
}

impl LspListener {
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Retract every session's published diagnostics and wait (bounded) for
    /// the retractions to reach the editors. `publishDiagnostics` is sticky
    /// client-side: a daemon that exits without retracting leaves squiggles
    /// (and their hover message) alive in the editor with no server behind
    /// them to answer hovers.
    pub async fn shutdown_sessions(&self) {
        self.sessions.retract_and_flush().await;
    }
}

/// Registry of live sessions, shared by the accept loop and the shutdown path.
#[derive(Clone, Default)]
struct LspSessions(Arc<SessionsInner>);

#[derive(Default)]
struct SessionsInner {
    next_id: AtomicU64,
    hooks: Mutex<HashMap<u64, SessionHook>>,
}

struct SessionHook {
    /// Injects the shutdown notification into the session's message loop;
    /// returns false when the session's receiver is already gone.
    notify: Box<dyn Fn() -> bool + Send + Sync>,
    /// Set by the session once its retractions are queued to the writer.
    retracted: Arc<AtomicBool>,
}

impl LspSessions {
    fn register(
        &self,
        notify: Box<dyn Fn() -> bool + Send + Sync>,
        retracted: Arc<AtomicBool>,
    ) -> SessionRegistration {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock_hooks()
            .insert(id, SessionHook { notify, retracted });
        SessionRegistration {
            sessions: self.clone(),
            id,
        }
    }

    fn lock_hooks(&self) -> MutexGuard<'_, HashMap<u64, SessionHook>> {
        self.0
            .hooks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Notify every live session, wait until each has queued its retractions
    /// (or ended), then leave a short grace for the writer threads to drain.
    /// A session stuck in a long lookup past the timeout is abandoned — its
    /// editor keeps stale squiggles, exactly the pre-retraction status quo.
    async fn retract_and_flush(&self) {
        let notified: Vec<(u64, Arc<AtomicBool>)> = self
            .lock_hooks()
            .iter()
            .filter(|(_, hook)| (hook.notify)())
            .map(|(id, hook)| (*id, hook.retracted.clone()))
            .collect();
        if notified.is_empty() {
            return;
        }

        let deadline = tokio::time::Instant::now() + SHUTDOWN_FLUSH_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let pending = {
                let hooks = self.lock_hooks();
                notified.iter().any(|(id, retracted)| {
                    !retracted.load(Ordering::Relaxed) && hooks.contains_key(id)
                })
            };
            if !pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(WRITER_DRAIN_GRACE).await;
    }
}

/// RAII deregistration so sessions that end on their own drop out of the
/// shutdown fan-out.
struct SessionRegistration {
    sessions: LspSessions,
    id: u64,
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        self.sessions.lock_hooks().remove(&self.id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct DiagnosticsConfig {
    #[serde(default = "true_bool")]
    constraints: bool,
    #[serde(default = "true_bool", rename = "staleRationale")]
    stale_rationale: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            constraints: true,
            stale_rationale: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct InitializationOptions {
    #[serde(default)]
    diagnostics: DiagnosticsConfig,
    #[serde(default = "true_bool", rename = "codeLens")]
    code_lens: bool,
    #[serde(default = "true_bool")]
    nudge: bool,
}

impl Default for InitializationOptions {
    fn default() -> Self {
        Self {
            diagnostics: DiagnosticsConfig::default(),
            code_lens: true,
            nudge: true,
        }
    }
}

fn true_bool() -> bool {
    true
}

/// Derive the Knowledge-LSP rendezvous socket path for a data dir.
pub fn lsp_socket_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(LSP_SOCKET_FILE_NAME)
}

/// Start the daemon-owned Knowledge-LSP listener unless disabled.
///
/// Infallible from the caller's perspective: LSP bind/accept failures are logged
/// and never take down the MCP backend.
pub async fn spawn_lsp_listener(state: Arc<AppState>, data_dir: &Path) -> Option<LspListener> {
    if lsp_disabled() {
        tracing::info!("MOOSEDev Knowledge-LSP disabled by MOOSEDEV_NO_LSP");
        return None;
    }

    let repo_root = std::env::current_dir()
        .map_err(|e| {
            tracing::warn!(
                "Knowledge-LSP unavailable: resolve daemon repo root from current_dir: {e}; MCP backend continues"
            )
        })
        .ok()?;

    spawn_lsp_listener_at(state, data_dir, repo_root).await
}

#[doc(hidden)]
pub async fn spawn_lsp_listener_at(
    state: Arc<AppState>,
    data_dir: &Path,
    repo_root: PathBuf,
) -> Option<LspListener> {
    if lsp_disabled() {
        tracing::info!("MOOSEDev Knowledge-LSP disabled by MOOSEDEV_NO_LSP");
        return None;
    }

    let path = lsp_socket_path_for(data_dir);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                "Knowledge-LSP unavailable: remove stale socket {}: {e}; MCP backend continues",
                path.display()
            );
            return None;
        }
    }

    let listener = UnixListener::bind(&path)
        .map_err(|e| {
            tracing::warn!(
                "Knowledge-LSP unavailable: bind {}: {e}; MCP backend continues",
                path.display()
            )
        })
        .ok()?;

    tracing::info!("MOOSEDev Knowledge-LSP serving on {}", path.display());
    let sessions = LspSessions::default();
    tokio::spawn(accept_lsp_sessions(
        listener,
        state,
        repo_root,
        sessions.clone(),
    ));

    Some(LspListener {
        socket: path,
        sessions,
    })
}

fn lsp_disabled() -> bool {
    std::env::var_os("MOOSEDEV_NO_LSP")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

/// Accept LSP connections forever; per-session failures are logged and isolated
/// from the daemon listener.
async fn accept_lsp_sessions(
    listener: UnixListener,
    state: Arc<AppState>,
    repo_root: PathBuf,
    sessions: LspSessions,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(e) => {
                tracing::warn!("Knowledge-LSP accept failed: {e}");
                continue;
            }
        };
        spawn_lsp_session_thread(state.clone(), repo_root.clone(), stream, sessions.clone());
    }
}

/// Convert one accepted tokio stream to blocking std I/O and hand it to a plain
/// thread, matching lsp-server's synchronous transport model.
fn spawn_lsp_session_thread(
    state: Arc<AppState>,
    repo_root: PathBuf,
    stream: UnixStream,
    sessions: LspSessions,
) {
    let Some(stream) = blocking_lsp_stream(stream) else {
        return;
    };

    if let Err(e) = thread::Builder::new()
        .name("MooseDevLspSession".to_string())
        .spawn(move || {
            if let Err(e) = run_session(state, repo_root, stream, sessions) {
                tracing::warn!("Knowledge-LSP session ended with error: {e}");
            }
        })
    {
        tracing::warn!("Knowledge-LSP session spawn failed: {e}");
    }
}

/// `lsp-server` reads synchronously, so every accepted socket must be converted
/// out of tokio and put back into blocking mode before the session thread starts.
fn blocking_lsp_stream(stream: UnixStream) -> Option<StdUnixStream> {
    let stream = stream
        .into_std()
        .map_err(|e| tracing::warn!("Knowledge-LSP accept failed converting stream: {e}"))
        .ok()?;
    if let Err(e) = stream.set_nonblocking(false) {
        tracing::warn!("Knowledge-LSP session unavailable: set blocking mode: {e}");
        return None;
    }
    Some(stream)
}

/// Own one editor session from LSP handshake through shutdown.
fn run_session(
    state: Arc<AppState>,
    repo_root: PathBuf,
    stream: StdUnixStream,
    sessions: LspSessions,
) -> anyhow::Result<()> {
    let (connection, inject, io_threads) = socket_connection(stream)?;
    let retracted = Arc::new(AtomicBool::new(false));
    let notify = Box::new(move || {
        inject(Message::Notification(Notification::new(
            DAEMON_SHUTDOWN_METHOD.to_string(),
            serde_json::Value::Null,
        )))
    });
    let _registration = sessions.register(notify, retracted.clone());
    let mut session = LspSession {
        state,
        connection,
        encoding: SessionEncoding::Utf16,
        repo_root,
        diagnostics: DiagnosticsConfig::default(),
        code_lens_enabled: true,
        nudge_enabled: true,
        nudged: false,
        author: "editor".to_string(),
        open_docs: HashMap::new(),
        substrate: None,
        commit_instants: HashMap::new(),
        last_diagnostics_generation: None,
        retracted,
    };
    session.initialize()?;
    session.run()?;
    drop(session);
    io_threads.join()?;
    Ok(())
}

/// Injects a message into a session's incoming channel, as if the editor had
/// sent it; returns false once the session's receiver is gone.
type SessionInjector = Box<dyn Fn(Message) -> bool + Send + Sync>;

/// Build the same channel-based transport shape as `Connection::stdio`, but on a
/// blocking UnixStream accepted by the daemon listener.
fn socket_connection(
    stream: StdUnixStream,
) -> io::Result<(Connection, SessionInjector, SocketIoThreads)> {
    let read_stream = stream.try_clone()?;
    let write_stream = stream;
    let (connection, io_connection) = Connection::memory();

    let inject: SessionInjector = {
        let sender = io_connection.sender.clone();
        Box::new(move |msg| sender.send(msg).is_ok())
    };
    let reader_sender = io_connection.sender.clone();
    let reader = thread::Builder::new()
        .name("MooseDevLspReader".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(read_stream);
            while let Some(msg) = Message::read(&mut reader)? {
                let is_exit = matches!(&msg, Message::Notification(n) if n.method == "exit");
                if let Err(e) = reader_sender.send(msg) {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, e));
                }
                if is_exit {
                    break;
                }
            }
            Ok(())
        })
        .map_err(io::Error::other)?;

    let writer = thread::Builder::new()
        .name("MooseDevLspWriter".to_string())
        .spawn(move || {
            let mut writer = BufWriter::new(write_stream);
            for msg in io_connection.receiver {
                msg.write(&mut writer)?;
            }
            Ok(())
        })
        .map_err(io::Error::other)?;

    Ok((connection, inject, SocketIoThreads { reader, writer }))
}

struct SocketIoThreads {
    reader: thread::JoinHandle<io::Result<()>>,
    writer: thread::JoinHandle<io::Result<()>>,
}

impl SocketIoThreads {
    /// Wait for the reader/writer threads after the session has dropped its
    /// connection senders, allowing the writer loop to drain and exit.
    fn join(self) -> io::Result<()> {
        match self.reader.join() {
            Ok(result) => result?,
            Err(err) => std::panic::panic_any(err),
        }
        match self.writer.join() {
            Ok(result) => result,
            Err(err) => std::panic::panic_any(err),
        }
    }
}

impl LspSession {
    fn initialize(&mut self) -> anyhow::Result<()> {
        let (id, params) = self.connection.initialize_start()?;
        let params: InitializeParams = serde_json::from_value(params)?;
        self.encoding = negotiate_session_encoding(&params.capabilities);
        self.author = proposal_author(params.client_info.as_ref());
        let options = parse_init_options(params.initialization_options);
        self.diagnostics = options.diagnostics;
        self.code_lens_enabled = options.code_lens;
        self.nudge_enabled = options.nudge;

        let result = InitializeResult {
            capabilities: server_capabilities(self.encoding),
            server_info: None,
        };
        self.connection
            .initialize_finish(id, serde_json::to_value(result)?)?;
        Ok(())
    }

    fn run(&mut self) -> anyhow::Result<()> {
        loop {
            let msg = match self
                .connection
                .receiver
                .recv_timeout(Duration::from_secs(2))
            {
                Ok(msg) => msg,
                Err(error) if error.is_timeout() => {
                    self.refresh_if_state_changed()?;
                    continue;
                }
                Err(_) => break,
            };
            // Serve every message from a repaired state: the substrate pin,
            // document base_text, and published diagnostics advance together
            // before any dispatch, so no request can map old-baseline
            // positions against a newer lazily-loaded index.
            self.refresh_if_state_changed()?;
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req)? {
                        break;
                    }
                    if req.method == "textDocument/hover" {
                        self.handle_hover(req.id, req.params)?;
                    } else if req.method == "textDocument/codeLens" {
                        self.handle_code_lens(req.id, req.params)?;
                    } else if req.method == "textDocument/diagnostic" {
                        self.handle_pull_diagnostics(req.id, req.params)?;
                    } else if req.method == "textDocument/codeAction" {
                        self.handle_code_action(req.id, req.params)?;
                    } else if req.method == "workspace/executeCommand" {
                        self.handle_execute_command(req.id, req.params)?;
                    } else {
                        self.connection.sender.send(
                            Response::new_err(
                                req.id,
                                ErrorCode::MethodNotFound as i32,
                                format!("method not found: {}", req.method),
                            )
                            .into(),
                        )?;
                    }
                }
                Message::Notification(notification) => match notification.method.as_str() {
                    "textDocument/didOpen" => self.handle_did_open(notification.params)?,
                    "textDocument/didSave" => self.handle_did_save(notification.params)?,
                    "textDocument/didClose" => self.handle_did_close(notification.params),
                    "textDocument/didChange" => self.handle_did_change(notification.params)?,
                    DAEMON_SHUTDOWN_METHOD => {
                        self.retract_published_diagnostics();
                        break;
                    }
                    "exit" => break,
                    _ => {}
                },
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn handle_did_open(&mut self, params: serde_json::Value) -> anyhow::Result<()> {
        self.maybe_nudge();
        let Some(uri) = notification_text_document_uri(&params) else {
            return Ok(());
        };
        let Some(rel_path) = repo_relative_path(&self.repo_root, &uri) else {
            return Ok(());
        };
        let disk_text = std::fs::read_to_string(self.repo_root.join(&rel_path)).unwrap_or_default();
        let text = params
            .pointer("/textDocument/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&disk_text)
            .to_string();
        let (base_text, base_substrate) = self
            .substrate
            .as_ref()
            .and_then(|substrate| {
                indexed_base_text(&self.repo_root, substrate, &rel_path)
                    .map(|base_text| (base_text, Some(substrate_identity(substrate))))
            })
            .unwrap_or_default();
        self.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(rel_path.clone(), base_text, text, base_substrate),
        );
        self.commit_instants.remove(&rel_path);
        self.publish_current_diagnostics(uri, &rel_path)
    }

    fn handle_did_save(&mut self, params: serde_json::Value) -> anyhow::Result<()> {
        self.maybe_nudge();
        let Some(uri) = notification_text_document_uri(&params) else {
            return Ok(());
        };
        let Some(rel_path) = repo_relative_path(&self.repo_root, &uri) else {
            return Ok(());
        };
        let saved_text = params
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| std::fs::read_to_string(self.repo_root.join(&rel_path)).ok())
            .unwrap_or_default();
        match self.open_docs.get_mut(uri.as_str()) {
            Some(document) => document.update_text(saved_text),
            None => {
                let (base_text, base_substrate) = self
                    .substrate
                    .as_ref()
                    .and_then(|substrate| {
                        indexed_base_text(&self.repo_root, substrate, &rel_path)
                            .map(|base_text| (base_text, Some(substrate_identity(substrate))))
                    })
                    .unwrap_or_default();
                self.open_docs.insert(
                    uri.to_string(),
                    OpenDocument::new(rel_path.clone(), base_text, saved_text, base_substrate),
                );
            }
        }
        // Every save nudges, unconditionally: suppressing saves byte-identical
        // to base_text races the async re-base after a rebuild publishes (a
        // fast revert-save matches the STALE baseline and the reconciliation
        // it needs is exactly what gets suppressed). Editors already skip
        // didSave on clean buffers, and the scheduler debounces the rest.
        self.state.nudge_reindex(&rel_path);
        self.commit_instants.remove(&rel_path);
        self.publish_current_diagnostics(uri, &rel_path)
    }

    fn handle_did_change(&mut self, params: serde_json::Value) -> anyhow::Result<()> {
        let Some(uri) = notification_text_document_uri(&params) else {
            return Ok(());
        };
        let Some(text) = params
            .get("contentChanges")
            .and_then(serde_json::Value::as_array)
            .and_then(|changes| changes.last())
            .and_then(|change| change.get("text"))
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(());
        };
        let Some(document) = self.open_docs.get_mut(uri.as_str()) else {
            return Ok(());
        };
        document.update_text(text.to_string());
        // Push surfaces must re-publish when the state they render changes —
        // the buffer's line alignment is part of that state.
        let rel_path = document.rel_path.clone();
        self.publish_current_diagnostics(uri, &rel_path)
    }

    fn publish_current_diagnostics(&mut self, uri: Uri, rel_path: &str) -> anyhow::Result<()> {
        // Generation bookkeeping lives in `refresh_if_state_changed`, which
        // runs before every dispatched message.
        let diagnostics = self.file_diagnostics(rel_path);
        self.publish(uri, diagnostics)
    }

    fn handle_did_close(&mut self, params: serde_json::Value) {
        if let Some(uri) = notification_text_document_uri(&params) {
            self.open_docs.remove(uri.as_str());
        }
    }

    fn handle_code_lens(
        &self,
        id: lsp_server::RequestId,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let lenses = self.code_lenses(params).unwrap_or_default();
        self.connection
            .sender
            .send(Response::new_ok(id, lenses).into())?;
        Ok(())
    }

    /// Badges above declarations: role/criticality judgment badges (ratified
    /// plain, proposed suffixed `?`; criticality badged only when high — Zed
    /// lens length discipline), per-kind record counts where knowledge exists,
    /// and a hotspot flag on an undocumented public entity — upgraded to
    /// "core entity" wording when a ratified judgment marks it core/critical
    /// (spec §5.5). Driven off the same substrate + records the dossier uses
    /// (`is_debt_surface` shared with the why-coverage metric), so the lens
    /// and the metric never disagree.
    fn code_lenses(&self, params: serde_json::Value) -> Option<Vec<CodeLens>> {
        if !self.code_lens_enabled {
            return Some(Vec::new());
        }
        let uri = code_lens_uri(&params)?;
        let rel_path = repo_relative_path(&self.repo_root, &uri)?;
        let substrate = self.substrate.clone()?;
        if !self.document_matches_substrate(&rel_path, &substrate) {
            return Some(Vec::new());
        }
        let terms = CodeTerms::resolve(&self.state).ok()?;
        let entities = entities_by_symbol(&self.state, &terms).ok()?;
        let judgments = crate::graph::judgments_by_subject(&self.state).unwrap_or_default();

        let mut lenses = Vec::new();
        for definition in substrate.definitions_in_file(&rel_path) {
            let entity_iri = entities.get(&definition.entry.normalized_symbol);
            let records = entity_iri
                .and_then(|iri| direct_records_for_entity(&self.state, iri).ok())
                .unwrap_or_default();
            let entity_judgments = entity_iri
                .and_then(|iri| judgments.get(iri))
                .map(Vec::as_slice)
                .unwrap_or_default();

            let mut parts = judgment_badges(entity_judgments);
            let ratified_core = entity_judgments.iter().any(|j| {
                j.status == "accepted"
                    && matches!(
                        j.target_local.as_str(),
                        "core-algorithm" | "domain-logic" | "high"
                    )
            });
            if !records.is_empty() {
                parts.push(lens_count_title(&records));
            } else if ratified_core {
                parts.push("⚠ core entity — no linked rationale".to_string());
            } else if is_debt_surface(&definition.entry) {
                parts.push("⚠ no linked rationale".to_string());
            }
            if parts.is_empty() {
                continue;
            }
            let title = parts.join(" · ");
            let Some(range) = self.diagnostic_range(&substrate, &rel_path, definition.range) else {
                continue;
            };
            lenses.push(CodeLens {
                range,
                command: Some(lsp_types::Command {
                    title,
                    command: "moosedev.openEntity".to_string(),
                    arguments: entity_iri.map(|iri| vec![serde_json::Value::String(iri.clone())]),
                }),
                data: None,
            });
        }
        Some(lenses)
    }

    /// Fire the pending-ratifications nudge once per session (an info message),
    /// when the proposal queue is non-empty. Guarded so it never repeats.
    fn maybe_nudge(&mut self) {
        if self.nudged || !self.nudge_enabled {
            return;
        }
        self.nudged = true;
        let count = pending_count(&self.state).unwrap_or(0);
        if count == 0 {
            return;
        }
        let plural = if count == 1 { "" } else { "s" };
        let message = format!(
            "MOOSEDev: {count} pending proposal{plural} awaiting ratification in the workbench inbox"
        );
        let _ = self.connection.sender.send(
            Notification::new(
                "window/showMessage".to_string(),
                ShowMessageParams {
                    typ: MessageType::INFO,
                    message,
                },
            )
            .into(),
        );
    }

    /// The daemon is exiting: publish an empty set for every open doc so the
    /// editor does not keep this session's squiggles alive with no server
    /// behind them. Best-effort — the process is going down either way.
    fn retract_published_diagnostics(&self) {
        for uri in self.open_docs.keys() {
            let Ok(uri) = Uri::from_str(uri) else {
                continue;
            };
            let _ = self.publish(uri, Vec::new());
        }
        self.retracted.store(true, Ordering::Relaxed);
    }

    fn refresh_if_state_changed(&mut self) -> anyhow::Result<()> {
        // The ONLY call site of the lazily-reloading `state.substrate()` in
        // this session: the pin, every document's base_text, and the published
        // diagnostics advance together here, never independently.
        let live = self.state.substrate();
        let generation = (
            live.as_ref()
                .map_or(0, |substrate| Arc::as_ptr(substrate) as usize),
            self.state.project_write_generation(),
        );
        if self.last_diagnostics_generation == Some(generation) {
            return Ok(());
        }

        let substrate_changed = self
            .last_diagnostics_generation
            .is_some_and(|previous| previous.0 != generation.0);
        self.substrate = live;
        if substrate_changed {
            self.commit_instants.clear();
            for document in self.open_docs.values_mut() {
                let Some(substrate) = self.substrate.as_ref() else {
                    document.invalidate_base();
                    continue;
                };
                if let Some(base_text) =
                    indexed_base_text(&self.repo_root, substrate, &document.rel_path)
                {
                    document.update_base(base_text, substrate);
                } else {
                    // A save may have landed while this generation was being
                    // built. Without an exact indexed baseline, silence is the
                    // only honest coordinate response.
                    document.invalidate_base();
                }
            }
        }

        let open_docs: Vec<_> = self
            .open_docs
            .iter()
            .map(|(uri, document)| (uri.clone(), document.rel_path.clone()))
            .collect();
        for (uri, rel_path) in open_docs {
            let diagnostics = self.file_diagnostics(&rel_path);
            self.publish(Uri::from_str(&uri)?, diagnostics)?;
        }
        self.last_diagnostics_generation = Some(generation);
        Ok(())
    }

    fn file_diagnostics(&mut self, rel_path: &str) -> Vec<Diagnostic> {
        let Some(substrate) = self.substrate.clone() else {
            return Vec::new();
        };
        if !self.document_matches_substrate(rel_path, &substrate) {
            return Vec::new();
        }

        let terms = match CodeTerms::resolve(&self.state) {
            Ok(terms) => terms,
            Err(e) => {
                tracing::warn!("Knowledge-LSP diagnostics term lookup failed: {e}");
                return Vec::new();
            }
        };
        let entities = match entities_by_symbol(&self.state, &terms) {
            Ok(entities) => entities,
            Err(e) => {
                tracing::warn!("Knowledge-LSP diagnostics entity lookup failed: {e}");
                return Vec::new();
            }
        };
        let file_commit = self
            .diagnostics
            .stale_rationale
            .then(|| self.file_last_commit_instant_memo(rel_path))
            .flatten();

        // Message shaping and severity discipline are host-independent policy
        // (spec §5.4); this session only attaches ranges and encoding.
        let config = crate::policy::WarnConfig {
            constraints: self.diagnostics.constraints,
            stale_rationale: self.diagnostics.stale_rationale,
        };
        let mut diagnostics = Vec::new();
        for definition in substrate.definitions_in_file(rel_path) {
            let Some(entity_iri) = entities.get(&definition.entry.normalized_symbol) else {
                continue;
            };
            let records = match direct_records_for_entity(&self.state, entity_iri) {
                Ok(records) => records,
                Err(e) => {
                    tracing::warn!("Knowledge-LSP diagnostics record lookup failed: {e}");
                    continue;
                }
            };
            if records.is_empty() {
                continue;
            }
            let Some(range) = self.diagnostic_range(&substrate, rel_path, definition.range) else {
                continue;
            };

            for shaped in crate::policy::entity_diagnostics(
                &records,
                file_commit,
                &config,
                &definition.entry.normalized_symbol,
            ) {
                let severity = match shaped.severity.as_str() {
                    "information" => DiagnosticSeverity::INFORMATION,
                    "hint" => DiagnosticSeverity::HINT,
                    // Policy never exceeds the ceiling; skip defensively if it did.
                    _ => continue,
                };
                let mut diagnostic = Diagnostic::new(
                    range,
                    Some(severity),
                    None,
                    Some("moosedev".to_string()),
                    shaped.message,
                    None,
                    None,
                );
                add_record_code(&mut diagnostic, &shaped.record);
                diagnostics.push(diagnostic);
            }
        }

        diagnostics.sort_by(|a, b| {
            a.range
                .start
                .line
                .cmp(&b.range.start.line)
                .then_with(|| a.range.start.character.cmp(&b.range.start.character))
                .then_with(|| diagnostic_severity_rank(a).cmp(&diagnostic_severity_rank(b)))
        });
        diagnostics
    }

    fn document_matches_substrate(&self, rel_path: &str, substrate: &Arc<Substrate>) -> bool {
        self.open_docs
            .values()
            .find(|document| document.rel_path == rel_path)
            .is_none_or(|document| document.is_aligned_with(substrate))
    }

    fn substrate_position(
        &self,
        substrate: &Arc<Substrate>,
        rel_path: &str,
        position: Position,
    ) -> Option<SubstratePosition> {
        let document = self
            .open_docs
            .values()
            .find(|document| document.rel_path == rel_path);
        let buffer_line = match document {
            Some(document) => {
                if !document.is_aligned_with(substrate) {
                    return None;
                }
                document.text.lines().nth(position.line as usize)?
            }
            None => {
                return Some(SubstratePosition {
                    line: position.line,
                    col: utf8_col(
                        &self.repo_root,
                        rel_path,
                        position.line,
                        position.character,
                        self.encoding,
                    )?,
                })
            }
        };
        let line = document?.lines.to_base(position.line)?;
        let col = match self.encoding {
            SessionEncoding::Utf8 => position.character,
            SessionEncoding::Utf16 => utf16_character_to_utf8_col(buffer_line, position.character),
        };
        Some(SubstratePosition { line, col })
    }

    fn diagnostic_range(
        &self,
        substrate: &Arc<Substrate>,
        rel_path: &str,
        range: SourceRange,
    ) -> Option<Range> {
        let document = self
            .open_docs
            .values()
            .find(|document| document.rel_path == rel_path);
        let range = match document {
            Some(document) => {
                if !document.is_aligned_with(substrate) {
                    return None;
                }
                SourceRange {
                    start: SubstratePosition {
                        line: document.lines.to_buffer(range.start.line)?,
                        col: range.start.col,
                    },
                    end: SubstratePosition {
                        line: document.lines.to_buffer(range.end.line)?,
                        col: range.end.col,
                    },
                }
            }
            None => range,
        };
        match self.encoding {
            SessionEncoding::Utf8 => Some(Range::new(
                Position::new(range.start.line, range.start.col),
                Position::new(range.end.line, range.end.col),
            )),
            SessionEncoding::Utf16 => {
                let file = match document {
                    Some(document) => Cow::Borrowed(document.text.as_str()),
                    None => {
                        Cow::Owned(std::fs::read_to_string(self.repo_root.join(rel_path)).ok()?)
                    }
                };
                let start_line = file.lines().nth(range.start.line as usize)?;
                let end_line = file.lines().nth(range.end.line as usize)?;
                Some(Range::new(
                    Position::new(
                        range.start.line,
                        utf8_col_to_utf16_character(start_line, range.start.col),
                    ),
                    Position::new(
                        range.end.line,
                        utf8_col_to_utf16_character(end_line, range.end.col),
                    ),
                ))
            }
        }
    }

    /// Memoized per open file: publishes triggered by typing must not spawn a
    /// `git log` subprocess per keystroke. Invalidated on open/save and on a
    /// substrate-generation change (the post-commit reindex path).
    fn file_last_commit_instant_memo(&mut self, rel_path: &str) -> Option<DateTime<FixedOffset>> {
        if let Some(cached) = self.commit_instants.get(rel_path) {
            return *cached;
        }
        let instant = self.file_last_commit_instant(rel_path);
        self.commit_instants.insert(rel_path.to_string(), instant);
        instant
    }

    fn file_last_commit_instant(&self, rel_path: &str) -> Option<DateTime<FixedOffset>> {
        let output = Command::new("git")
            .arg("log")
            .arg("-1")
            .arg("--format=%cI")
            .arg("--")
            .arg(rel_path)
            .current_dir(&self.repo_root)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let timestamp = text.trim();
        if timestamp.is_empty() {
            return None;
        }
        DateTime::parse_from_rfc3339(timestamp).ok()
    }

    fn publish(&self, uri: Uri, mut diagnostics: Vec<Diagnostic>) -> anyhow::Result<()> {
        // Load-bearing severity ceiling: every diagnostics publish flows through
        // this gate so v2.0 cannot emit Warning or Error. Debug builds trip
        // loudly; release builds drop the offender and warn rather than ship it,
        // so the shipped binary enforces the ceiling and does not merely assert it.
        debug_assert!(diagnostics.iter().all(is_allowed_diagnostic_severity));
        diagnostics.retain(|diagnostic| {
            let allowed = is_allowed_diagnostic_severity(diagnostic);
            if !allowed {
                tracing::warn!(
                    severity = ?diagnostic.severity,
                    message = %diagnostic.message,
                    "dropping diagnostic above the v2.0 Information ceiling"
                );
            }
            allowed
        });
        self.connection.sender.send(
            Notification::new(
                "textDocument/publishDiagnostics".to_string(),
                PublishDiagnosticsParams {
                    uri,
                    diagnostics,
                    version: None,
                },
            )
            .into(),
        )?;
        Ok(())
    }

    /// Pull diagnostics (LSP 3.17, `textDocument/diagnostic`): the same
    /// per-file diagnostics the push path publishes, computed on demand.
    /// Unresolvable URIs get an honest empty full report, never an error.
    fn handle_pull_diagnostics(
        &mut self,
        id: lsp_server::RequestId,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let rel_path = serde_json::from_value::<DocumentDiagnosticParams>(params)
            .ok()
            .and_then(|params| repo_relative_path(&self.repo_root, &params.text_document.uri));
        let mut items = match rel_path {
            Some(rel_path) => self.file_diagnostics(&rel_path),
            None => Vec::new(),
        };
        // The same severity ceiling `publish` enforces guards this path.
        items.retain(is_allowed_diagnostic_severity);
        let report = DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            },
        ));
        self.connection
            .sender
            .send(Response::new_ok(id, report).into())?;
        Ok(())
    }

    fn handle_code_action(
        &self,
        id: lsp_server::RequestId,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let actions = self.code_actions(params).unwrap_or_default();
        self.connection
            .sender
            .send(Response::new_ok(id, actions).into())?;
        Ok(())
    }

    /// The lightbulb menu IS the picker (spec §5.6): every choice is a
    /// fully-formed quickfix action carrying a `moosedev.propose*` command, so
    /// no server→client prompt is ever needed. Selecting one files a proposal
    /// into the ratification queue via `workspace/executeCommand` — the only
    /// write path this session has (Constraint 2ba76439: no side doors).
    /// Silence discipline mirrors hover: no resolution, a local symbol, or a
    /// symbol without a workspace definition → no actions.
    fn code_actions(&self, params: serde_json::Value) -> Option<Vec<CodeAction>> {
        let params: CodeActionParams = serde_json::from_value(params).ok()?;
        if !quickfix_requested(params.context.only.as_deref()) {
            return Some(Vec::new());
        }
        let rel_path = repo_relative_path(&self.repo_root, &params.text_document.uri)?;
        let substrate = self.substrate.clone()?;
        let position = self.substrate_position(&substrate, &rel_path, params.range.start)?;
        let resolution = substrate.resolve(&rel_path, position)?;
        if resolution.is_local {
            return Some(Vec::new());
        }
        // Link actions anchor to the workspace definition (accept re-resolves
        // the symbol at HEAD and mints the entity lazily), so a definition is
        // the floor for every action.
        let definition = substrate.definition_for_symbol(&resolution.symbol)?;
        let entity_name = definition
            .display_name
            .clone()
            .unwrap_or_else(|| logical_path(&definition.symbol).unwrap_or_default());

        let terms = CodeTerms::resolve(&self.state).ok()?;
        let entity_iri = entities_by_symbol(&self.state, &terms)
            .ok()?
            .get(&definition.normalized_symbol)
            .cloned();

        let mut actions = self.link_actions(&definition, &entity_name, entity_iri.as_deref());
        if let Some(entity_iri) = &entity_iri {
            actions.extend(self.judgment_actions(entity_iri, &entity_name));
        }
        Some(actions)
    }

    /// "Link decision to this entity…" candidates: the top records the hybrid
    /// seed relates to this entity's name + logical path, minus records already
    /// linked to it and records with a pending link proposal for this symbol.
    fn link_actions(
        &self,
        definition: &DefinitionEntry,
        entity_name: &str,
        entity_iri: Option<&str>,
    ) -> Vec<CodeAction> {
        const CANDIDATE_CAP: usize = 4;

        let seed = format!(
            "{} {}",
            entity_name,
            logical_path(&definition.symbol).unwrap_or_default()
        );
        let already_linked = entity_iri
            .and_then(|iri| direct_records_for_entity(&self.state, iri).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|record| record.iri)
            .collect::<std::collections::HashSet<_>>();
        let pending = crate::graph::list_proposals(&self.state, Some("proposed"))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                p.kind == ProposalKind::Link
                    && normalize_symbol(&p.target_symbol).as_deref()
                        == Some(definition.normalized_symbol.as_str())
            })
            .map(|p| p.subject_iri)
            .collect::<std::collections::HashSet<_>>();
        let excluded = already_linked.union(&pending).cloned().collect();

        let candidates = match crate::graph::link_candidates_excluding(
            &self.state,
            &seed,
            CANDIDATE_CAP,
            &excluded,
        ) {
            Ok(candidates) => candidates,
            Err(e) => {
                tracing::warn!("Knowledge-LSP link-candidate search failed: {e}");
                return Vec::new();
            }
        };

        candidates
            .into_iter()
            .map(|candidate| {
                let title = format!(
                    "Link {} \"{}\" to {}",
                    candidate.kind,
                    truncate_title(&candidate.title, 60),
                    entity_name
                );
                propose_action(
                    title,
                    "moosedev.proposeLink",
                    serde_json::json!({
                        "recordIri": candidate.iri,
                        "predicate": "concerns",
                        "targetSymbol": definition.normalized_symbol,
                        "targetPath": definition.file,
                        "entityName": entity_name,
                    }),
                )
            })
            .collect()
    }

    /// "Propose role/criticality" actions for a minted entity. An axis with
    /// any live judgment (proposed or ratified) is suppressed entirely —
    /// `maxCount 1` makes a second edge unratifiable, and a pending twin
    /// belongs in the inbox, not the lightbulb. `standard` criticality is the
    /// implicit default and never offered.
    fn judgment_actions(&self, entity_iri: &str, entity_name: &str) -> Vec<CodeAction> {
        let judgments = crate::graph::judgments_by_subject(&self.state).unwrap_or_default();
        let entity_judgments = judgments.get(entity_iri).map(Vec::as_slice).unwrap_or(&[]);
        let axis_taken = |predicate: &str| {
            entity_judgments
                .iter()
                .any(|j| j.predicate_local == predicate)
        };

        let mut actions = Vec::new();
        if !axis_taken("playsRole") {
            for role in ROLE_LOCALS {
                actions.push(propose_action(
                    format!("Propose role: {role} ({entity_name})"),
                    "moosedev.proposeJudgment",
                    serde_json::json!({
                        "entityIri": entity_iri,
                        "predicate": "playsRole",
                        "targetLocal": role,
                    }),
                ));
            }
        }
        if !axis_taken("hasCriticality") {
            for criticality in CRITICALITY_LOCALS.iter().filter(|c| **c != "standard") {
                actions.push(propose_action(
                    format!("Propose criticality: {criticality} ({entity_name})"),
                    "moosedev.proposeJudgment",
                    serde_json::json!({
                        "entityIri": entity_iri,
                        "predicate": "hasCriticality",
                        "targetLocal": criticality,
                    }),
                ));
            }
        }
        actions
    }

    /// The session's ONLY write path (spec §7 grep-proof): every command files
    /// a proposal into the ratification queue via `propose_link` /
    /// `propose_judgment`; nothing here materializes an edge or accepts
    /// anything. Argument violations are `InvalidParams` errors, never silent.
    fn handle_execute_command(
        &self,
        id: lsp_server::RequestId,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let outcome = serde_json::from_value::<ExecuteCommandParams>(params)
            .map_err(|e| format!("malformed executeCommand params: {e}"))
            .and_then(|params| self.execute_command(&params));
        match outcome {
            Ok(proposal_iri) => {
                self.connection.sender.send(
                    Response::new_ok(id, serde_json::json!({ "proposalIri": proposal_iri })).into(),
                )?;
                let _ = self.connection.sender.send(
                    Notification::new(
                        "window/showMessage".to_string(),
                        ShowMessageParams {
                            typ: MessageType::INFO,
                            message:
                                "MOOSEDev: filed for ratification — review in the workbench inbox"
                                    .to_string(),
                        },
                    )
                    .into(),
                );
            }
            Err(message) => {
                self.connection
                    .sender
                    .send(Response::new_err(id, ErrorCode::InvalidParams as i32, message).into())?;
            }
        }
        Ok(())
    }

    fn execute_command(&self, params: &ExecuteCommandParams) -> Result<String, String> {
        let argument = params
            .arguments
            .first()
            .ok_or_else(|| format!("{} requires arguments[0]", params.command))?;
        match params.command.as_str() {
            "moosedev.proposeLink" => self.propose_link_command(argument),
            "moosedev.proposeJudgment" => self.propose_judgment_command(argument),
            other => Err(format!("unknown command: {other}")),
        }
    }

    fn propose_link_command(&self, argument: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LinkArgs {
            record_iri: String,
            predicate: String,
            target_symbol: String,
            target_path: String,
        }
        let args: LinkArgs = serde_json::from_value(argument.clone())
            .map_err(|e| format!("malformed proposeLink arguments: {e}"))?;
        // v2.3 offers `concerns` only — the general anchoring predicate the
        // capture path uses; intent predicates need record-side context the
        // editor menu does not have.
        if args.predicate != "concerns" {
            return Err(format!(
                "proposeLink predicate must be \"concerns\", got {:?}",
                args.predicate
            ));
        }
        let record = oxigraph::model::NamedNode::new(&args.record_iri)
            .map_err(|_| format!("recordIri is not a valid IRI: {}", args.record_iri))?;
        crate::graph::require_information_record(&self.state, &record)
            .map_err(|e| e.to_string())?;
        // The target must resolve to a real workspace definition NOW — a bogus
        // symbol (or a path contradicting its definition) would queue a
        // proposal that can never be accepted.
        let definition = self
            .substrate
            .as_deref()
            .and_then(|substrate| substrate.definition_for_symbol(&args.target_symbol))
            .ok_or_else(|| {
                format!(
                    "targetSymbol has no workspace definition in the substrate: {}",
                    args.target_symbol
                )
            })?;
        if definition.file != args.target_path {
            return Err(format!(
                "targetPath {:?} does not match the symbol's defining file {:?}",
                args.target_path, definition.file
            ));
        }
        crate::graph::propose_link(
            &self.state,
            &args.record_iri,
            &args.predicate,
            &definition.normalized_symbol,
            &args.target_path,
            &format!("proposed from an editor code action ({})", args.target_path),
            &self.author,
            chrono::Utc::now(),
        )
        .map_err(|e| e.to_string())
    }

    fn propose_judgment_command(&self, argument: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct JudgmentArgs {
            entity_iri: String,
            predicate: String,
            target_local: String,
        }
        let args: JudgmentArgs = serde_json::from_value(argument.clone())
            .map_err(|e| format!("malformed proposeJudgment arguments: {e}"))?;
        let target_iri = match args.predicate.as_str() {
            "playsRole" if ROLE_LOCALS.contains(&args.target_local.as_str()) => {
                crate::graph::role_iri(&args.target_local)
            }
            "hasCriticality"
                if CRITICALITY_LOCALS.contains(&args.target_local.as_str())
                    && args.target_local != "standard" =>
            {
                crate::graph::criticality_iri(&args.target_local)
            }
            "playsRole" | "hasCriticality" => {
                return Err(format!(
                    "{:?} is not a proposable {} target",
                    args.target_local, args.predicate
                ));
            }
            other => {
                return Err(format!(
                    "proposeJudgment predicate must be playsRole or hasCriticality, got {other:?}"
                ));
            }
        };
        oxigraph::model::NamedNode::new(&args.entity_iri)
            .map_err(|_| format!("entityIri is not a valid IRI: {}", args.entity_iri))?;
        let terms = CodeTerms::resolve(&self.state).map_err(|e| e.to_string())?;
        let minted = entities_by_symbol(&self.state, &terms)
            .map_err(|e| e.to_string())?
            .into_values()
            .any(|iri| iri == args.entity_iri);
        if !minted {
            return Err(format!(
                "entityIri is not a minted code entity: {}",
                args.entity_iri
            ));
        }
        // A human choosing from the menu is an assertion, not a guess:
        // confidence 1.0, escalated for inbox prominence — but still a
        // proposal the workbench ratifies (no auto-accept from the editor).
        crate::graph::propose_judgment(
            &self.state,
            &args.entity_iri,
            &args.predicate,
            &target_iri,
            1.0,
            crate::graph::ESCALATED,
            "asserted by a human from the editor",
            &self.author,
            chrono::Utc::now(),
        )
        .map_err(|e| e.to_string())
    }

    fn handle_hover(
        &self,
        id: lsp_server::RequestId,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let response = match self.hover_response(params) {
            Some(hover) => Response::new_ok(id, hover),
            None => Response::new_ok(id, serde_json::Value::Null),
        };
        self.connection.sender.send(response.into())?;
        Ok(())
    }

    fn hover_response(&self, params: serde_json::Value) -> Option<Hover> {
        let params: HoverParams = serde_json::from_value(params).ok()?;
        let rel_path = repo_relative_path(
            &self.repo_root,
            &params.text_document_position_params.text_document.uri,
        )?;
        let substrate = self.substrate.clone()?;
        let position = self.substrate_position(
            &substrate,
            &rel_path,
            params.text_document_position_params.position,
        )?;
        // Resolve the position against the session-pinned substrate so hover
        // and the rebased documents never mix generations; the dossier itself
        // is fetched by symbol, whose identity is generation-independent.
        let resolution = substrate.resolve(&rel_path, position)?;
        if resolution.is_local {
            return None;
        }
        let dossier =
            match get_entity_dossier(&self.state, &DossierTarget::Symbol(resolution.symbol)) {
                Ok(dossier) => dossier?,
                Err(e) => {
                    tracing::warn!("Knowledge-LSP hover dossier lookup failed: {e}");
                    return None;
                }
            };

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: render_markdown(&dossier),
            }),
            range: None,
        })
    }
}

fn repo_relative_path(repo_root: &Path, uri: &lsp_types::Uri) -> Option<String> {
    if uri.scheme()?.as_str() != "file" {
        return None;
    }

    let decoded = percent_decode_utf8(uri.path().as_str())?;
    let absolute_path = PathBuf::from(decoded);
    let relative = absolute_path.strip_prefix(repo_root).ok()?;
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    // Do not canonicalize or resolve symlinks here: the substrate stores the
    // literal repo-relative paths produced during indexing.
    Some(
        relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    )
}

fn parse_init_options(options: Option<serde_json::Value>) -> InitializationOptions {
    options
        .and_then(|options| serde_json::from_value::<InitializationOptions>(options).ok())
        .unwrap_or_default()
}

fn code_lens_uri(params: &serde_json::Value) -> Option<Uri> {
    let uri = params.get("textDocument")?.get("uri")?.as_str()?;
    Uri::from_str(uri).ok()
}

/// Render a code-lens badge like "3 decisions · 1 constraint" from linked records.
/// Short judgment badges for the lens title: role always (ratified plain,
/// proposed `?`-suffixed), criticality only when high — lens space is scarce.
fn judgment_badges(judgments: &[crate::graph::JudgmentSummary]) -> Vec<String> {
    let mut badges = Vec::new();
    for judgment in judgments {
        let provisional = judgment.status != "accepted";
        match judgment.predicate_local.as_str() {
            "playsRole" => {
                badges.push(if provisional {
                    format!("{}?", judgment.target_local)
                } else {
                    judgment.target_local.clone()
                });
            }
            "hasCriticality" if judgment.target_local == "high" => {
                badges.push(if provisional {
                    "critical?".to_string()
                } else {
                    "critical".to_string()
                });
            }
            _ => {}
        }
    }
    badges
}

fn lens_count_title(records: &[RecordSummary]) -> String {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for record in records {
        *counts.entry(record.kind.as_str()).or_default() += 1;
    }
    const ORDER: &[&str] = &[
        "ArchitecturalDecision",
        "Constraint",
        "Lesson",
        "Requirement",
    ];
    let mut parts = Vec::new();
    for kind in ORDER {
        if let Some(n) = counts.remove(*kind) {
            parts.push(format!("{n} {}", count_word(kind, n)));
        }
    }
    for (kind, n) in counts {
        parts.push(format!("{n} {}", count_word(kind, n)));
    }
    parts.join(" · ")
}

fn count_word(kind: &str, n: usize) -> String {
    let pick = |singular: &str, plural: &str| {
        if n == 1 {
            singular.to_string()
        } else {
            plural.to_string()
        }
    };
    match kind {
        "ArchitecturalDecision" => pick("decision", "decisions"),
        "Constraint" => pick("constraint", "constraints"),
        "Lesson" => pick("lesson", "lessons"),
        "Requirement" => pick("requirement", "requirements"),
        other => other.to_string(),
    }
}

fn notification_text_document_uri(params: &serde_json::Value) -> Option<Uri> {
    params
        .get("textDocument")?
        .get("uri")?
        .as_str()
        .and_then(|uri| Uri::from_str(uri).ok())
}

fn percent_decode_utf8(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = bytes.get(index + 1).copied().and_then(hex_value)?;
            let lo = bytes.get(index + 2).copied().and_then(hex_value)?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn utf8_col(
    repo_root: &Path,
    rel_path: &str,
    line0: u32,
    character: u32,
    encoding: SessionEncoding,
) -> Option<u32> {
    match encoding {
        SessionEncoding::Utf8 => Some(character),
        SessionEncoding::Utf16 => {
            let file = std::fs::read_to_string(repo_root.join(rel_path)).ok()?;
            let line = file.lines().nth(line0 as usize)?;
            Some(utf16_character_to_utf8_col(line, character))
        }
    }
}

fn utf16_character_to_utf8_col(line: &str, character: u32) -> u32 {
    let mut utf16_units = 0;
    let mut utf8_bytes = 0;

    for ch in line.chars() {
        if utf16_units >= character {
            return utf8_bytes;
        }

        let next_utf16_units = utf16_units + ch.len_utf16() as u32;
        if next_utf16_units > character {
            return utf8_bytes;
        }

        utf8_bytes += ch.len_utf8() as u32;
        utf16_units = next_utf16_units;

        if utf16_units == character {
            return utf8_bytes;
        }
    }

    line.len() as u32
}

fn utf8_col_to_utf16_character(line: &str, byte_col: u32) -> u32 {
    // SCIP stores UTF-8 byte columns; LSP diagnostics must be reported in the
    // session encoding negotiated with the editor.
    let mut utf16_units = 0;
    let mut utf8_bytes = 0;

    for ch in line.chars() {
        if utf8_bytes >= byte_col {
            return utf16_units;
        }

        let next_utf8_bytes = utf8_bytes + ch.len_utf8() as u32;
        if next_utf8_bytes > byte_col {
            return utf16_units;
        }

        utf8_bytes = next_utf8_bytes;
        utf16_units += ch.len_utf16() as u32;

        if utf8_bytes == byte_col {
            return utf16_units;
        }
    }

    line.encode_utf16().count() as u32
}

fn add_record_code(diagnostic: &mut Diagnostic, record: &crate::policy::RecordRef) {
    let Some(url) = &record.workbench_url else {
        return;
    };
    let Ok(href) = Uri::from_str(url) else {
        return;
    };
    diagnostic.code = Some(NumberOrString::String(crate::policy::short_record_id(
        &record.iri,
    )));
    diagnostic.code_description = Some(CodeDescription { href });
}

/// Build one quickfix action whose command files a queue proposal. Quickfix —
/// not a custom kind — so clients that filter with `context.only` keep it.
fn propose_action(title: String, command: &str, arguments: serde_json::Value) -> CodeAction {
    CodeAction {
        title: title.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(lsp_types::Command {
            title,
            command: command.to_string(),
            arguments: Some(vec![arguments]),
        }),
        ..Default::default()
    }
}

/// LSP kind filtering: a `context.only` list admits quickfix actions when any
/// requested kind is `quickfix` itself, a hierarchical prefix of it (`""`),
/// or a sub-kind request we satisfy exactly. Absent `only` admits everything.
fn quickfix_requested(only: Option<&[CodeActionKind]>) -> bool {
    let Some(kinds) = only else {
        return true;
    };
    kinds.iter().any(|kind| {
        let kind = kind.as_str();
        kind.is_empty() || kind == CodeActionKind::QUICKFIX.as_str()
    })
}

fn truncate_title(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        return title.to_string();
    }
    let mut out: String = title.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Author string for editor-originated proposals: `editor:<client>` from the
/// initialize `clientInfo`, slugified; bare `editor` when the client is
/// anonymous. Provenance names the surface, never impersonates a human.
fn proposal_author(client_info: Option<&lsp_types::ClientInfo>) -> String {
    let Some(name) = client_info
        .map(|info| info.name.trim())
        .filter(|n| !n.is_empty())
    else {
        return "editor".to_string();
    };
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    format!("editor:{}", slug.trim_matches('-'))
}

fn is_allowed_diagnostic_severity(diagnostic: &Diagnostic) -> bool {
    matches!(
        diagnostic.severity,
        Some(DiagnosticSeverity::INFORMATION | DiagnosticSeverity::HINT)
    )
}

fn diagnostic_severity_rank(diagnostic: &Diagnostic) -> u8 {
    match diagnostic.severity {
        Some(DiagnosticSeverity::INFORMATION) => 3,
        Some(DiagnosticSeverity::HINT) => 4,
        _ => 255,
    }
}

fn server_capabilities(encoding: SessionEncoding) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: (encoding == SessionEncoding::Utf8)
            .then_some(PositionEncodingKind::UTF8),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                "moosedev.proposeLink".to_string(),
                "moosedev.proposeJudgment".to_string(),
            ],
            work_done_progress_options: Default::default(),
        }),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("moosedev".to_string()),
            inter_file_dependencies: false,
            workspace_diagnostics: false,
            work_done_progress_options: Default::default(),
        })),
        ..Default::default()
    }
}

/// Prefer UTF-8 positions when the client offers them; otherwise keep the LSP
/// default UTF-16 encoding for backward compatibility.
fn negotiate_session_encoding(capabilities: &ClientCapabilities) -> SessionEncoding {
    let offers_utf8 = capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref())
        .is_some_and(|encodings| {
            encodings
                .iter()
                .any(|encoding| encoding.as_str() == "utf-8")
        });
    if offers_utf8 {
        SessionEncoding::Utf8
    } else {
        SessionEncoding::Utf16
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lsp_types::GeneralClientCapabilities;

    use super::*;

    #[test]
    fn proposal_author_slugifies_client_info() {
        let info = |name: &str| lsp_types::ClientInfo {
            name: name.to_string(),
            version: None,
        };
        assert_eq!(proposal_author(None), "editor");
        assert_eq!(proposal_author(Some(&info(""))), "editor");
        assert_eq!(proposal_author(Some(&info("Neovim"))), "editor:neovim");
        assert_eq!(
            proposal_author(Some(&info("Visual Studio Code"))),
            "editor:visual-studio-code"
        );
        assert_eq!(proposal_author(Some(&info("  Zed  "))), "editor:zed");
    }

    #[test]
    fn quickfix_kind_filtering_follows_only_semantics() {
        let kinds = |names: &[&str]| -> Vec<CodeActionKind> {
            names
                .iter()
                .map(|n| CodeActionKind::from(n.to_string()))
                .collect()
        };
        assert!(quickfix_requested(None));
        assert!(quickfix_requested(Some(&kinds(&["quickfix"]))));
        assert!(quickfix_requested(Some(&kinds(&[""]))));
        assert!(quickfix_requested(Some(&kinds(&["refactor", "quickfix"]))));
        assert!(!quickfix_requested(Some(&kinds(&["refactor"]))));
        assert!(!quickfix_requested(Some(&kinds(&[
            "source.organizeImports"
        ]))));
    }

    #[test]
    fn menu_titles_truncate_on_char_boundaries() {
        assert_eq!(truncate_title("short", 60), "short");
        let long = "x".repeat(80);
        let truncated = truncate_title(&long, 60);
        assert_eq!(truncated.chars().count(), 60);
        assert!(truncated.ends_with('…'));
        // Multi-byte input never splits a char.
        let unicode = "é".repeat(80);
        assert!(truncate_title(&unicode, 60).ends_with('…'));
    }

    #[test]
    fn negotiation_selects_utf8_when_client_offers_utf8() {
        let capabilities = ClientCapabilities {
            general: Some(GeneralClientCapabilities {
                position_encodings: Some(vec![
                    PositionEncodingKind::UTF16,
                    PositionEncodingKind::UTF8,
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(
            negotiate_session_encoding(&capabilities),
            SessionEncoding::Utf8
        );
    }

    #[test]
    fn negotiation_defaults_to_utf16_when_client_offers_nothing() {
        assert_eq!(
            negotiate_session_encoding(&ClientCapabilities::default()),
            SessionEncoding::Utf16
        );
    }

    #[test]
    fn line_map_tracks_insertions_and_rejects_changed_lines() {
        let base = "alpha\nbeta\ngamma\n";
        let inserted = "zero\nalpha\nbeta\ngamma\n";
        let map = LineMap::between(base, inserted);
        assert_eq!(map.to_buffer(0), Some(1));
        assert_eq!(map.to_buffer(2), Some(3));
        assert_eq!(map.to_base(0), None);
        assert_eq!(map.to_base(2), Some(1));

        let changed = LineMap::between(base, "alpha\nBETA\ngamma\n");
        assert_eq!(changed.to_buffer(1), None);
        assert_eq!(changed.to_base(1), None);
    }

    #[test]
    fn line_map_bounds_only_the_changed_middle() {
        let base = (0..3_000)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buffer = base.replacen("line 1500", "inserted\nline 1500", 1);
        let map = LineMap::between(&base, &buffer);

        assert_eq!(map.to_buffer(2_999), Some(3_000));
    }

    #[test]
    fn utf16_character_to_utf8_col_handles_multibyte_and_clamps() {
        let line = "létter 😀 αβ fn";

        assert_eq!(utf16_character_to_utf8_col(line, 0), 0);
        assert_eq!(utf16_character_to_utf8_col(line, 1), 1);
        assert_eq!(utf16_character_to_utf8_col(line, 2), "lé".len() as u32);

        let emoji_utf16 = "létter ".encode_utf16().count() as u32;
        let emoji_utf8 = "létter ".len() as u32;
        assert_eq!(utf16_character_to_utf8_col(line, emoji_utf16), emoji_utf8);
        assert_eq!(
            utf16_character_to_utf8_col(line, emoji_utf16 + 2),
            (emoji_utf8 + "😀".len() as u32)
        );

        let beta_utf16 = "létter 😀 αβ".encode_utf16().count() as u32;
        assert_eq!(
            utf16_character_to_utf8_col(line, beta_utf16),
            "létter 😀 αβ".len() as u32
        );
        assert_eq!(utf16_character_to_utf8_col(line, 10_000), line.len() as u32);
    }

    #[test]
    fn utf8_col_to_utf16_character_handles_multibyte_and_clamps() {
        let line = "létter 😀 αβ fn";

        assert_eq!(utf8_col_to_utf16_character(line, 0), 0);
        assert_eq!(utf8_col_to_utf16_character(line, 1), 1);
        assert_eq!(utf8_col_to_utf16_character(line, "lé".len() as u32), 2);

        let emoji_utf8 = "létter ".len() as u32;
        let emoji_utf16 = "létter ".encode_utf16().count() as u32;
        assert_eq!(utf8_col_to_utf16_character(line, emoji_utf8), emoji_utf16);
        assert_eq!(
            utf8_col_to_utf16_character(line, emoji_utf8 + "😀".len() as u32),
            emoji_utf16 + 2
        );

        assert_eq!(
            utf8_col_to_utf16_character(line, "létter 😀 αβ".len() as u32),
            "létter 😀 αβ".encode_utf16().count() as u32
        );
        assert_eq!(
            utf8_col_to_utf16_character(line, 10_000),
            line.encode_utf16().count() as u32
        );
    }

    #[test]
    fn diagnostics_config_defaults_on_absent_or_malformed_options() {
        assert_eq!(
            parse_init_options(None).diagnostics,
            DiagnosticsConfig::default()
        );
        assert!(parse_init_options(None).code_lens);
        assert!(parse_init_options(None).nudge);
        assert_eq!(
            parse_init_options(Some(serde_json::json!("not an object"))).diagnostics,
            DiagnosticsConfig::default()
        );
    }

    #[test]
    fn diagnostics_config_honors_false_and_ignores_unknown_keys() {
        let config = parse_init_options(Some(serde_json::json!({
            "diagnostics": {
                "constraints": false,
                "staleRationale": false,
                "ignored": "value"
            },
            "other": true
        })))
        .diagnostics;

        assert_eq!(
            config,
            DiagnosticsConfig {
                constraints: false,
                stale_rationale: false,
            }
        );
    }

    #[test]
    fn repo_relative_path_accepts_file_uri_under_root() {
        let root = Path::new("/tmp/moosedev root");
        let uri = lsp_types::Uri::from_str("file:///tmp/moosedev%20root/src/runtime.rs").unwrap();

        assert_eq!(
            repo_relative_path(root, &uri).as_deref(),
            Some("src/runtime.rs")
        );
    }

    #[test]
    fn repo_relative_path_rejects_outside_root_and_non_file_scheme() {
        let root = Path::new("/tmp/moosedev-root");
        let outside = lsp_types::Uri::from_str("file:///tmp/other/src/runtime.rs").unwrap();
        let untitled =
            lsp_types::Uri::from_str("untitled:///tmp/moosedev-root/src/runtime.rs").unwrap();

        assert!(repo_relative_path(root, &outside).is_none());
        assert!(repo_relative_path(root, &untitled).is_none());
    }

    #[test]
    fn repo_relative_path_rejects_parent_dir_components() {
        let root = Path::new("/tmp/moosedev-root");
        let uri = lsp_types::Uri::from_str("file:///tmp/moosedev-root/src/../secret.rs").unwrap();

        assert!(repo_relative_path(root, &uri).is_none());
    }
}
