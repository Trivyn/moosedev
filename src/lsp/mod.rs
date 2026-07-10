//! Knowledge-LSP transport skeleton.
//!
//! Editors spawn `moosedev lsp`, which only relays stdio to this daemon-owned
//! Unix socket. The daemon session shares the same [`AppState`] as MCP/HTTP, so
//! editor hover can serve the same dossiers as the MCP tools.

use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;

use chrono::{DateTime, FixedOffset};
use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::{
    ClientCapabilities, CodeDescription, Diagnostic, DiagnosticSeverity, Hover, HoverContents,
    HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, MarkupContent,
    MarkupKind, NumberOrString, Position, PositionEncodingKind, PublishDiagnosticsParams, Range,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Uri,
};
use serde::Deserialize;
use tokio::net::{UnixListener, UnixStream};

use crate::code::substrate::SourceRange;
use crate::graph::{
    direct_records_for_entity, entities_by_symbol, get_entity_dossier, render_markdown, AppState,
    CodeTerms, DossierTarget,
};

const LSP_SOCKET_FILE_NAME: &str = "moosedev-lsp.sock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEncoding {
    Utf8,
    Utf16,
}

struct LspSession {
    state: Arc<AppState>,
    connection: Connection,
    encoding: SessionEncoding,
    repo_root: PathBuf,
    diagnostics: DiagnosticsConfig,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
struct InitializationOptions {
    #[serde(default)]
    diagnostics: DiagnosticsConfig,
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
pub async fn spawn_lsp_listener(state: Arc<AppState>, data_dir: &Path) -> Option<PathBuf> {
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
) -> Option<PathBuf> {
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
    tokio::spawn(accept_lsp_sessions(listener, state, repo_root));

    Some(path)
}

fn lsp_disabled() -> bool {
    std::env::var_os("MOOSEDEV_NO_LSP")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

/// Accept LSP connections forever; per-session failures are logged and isolated
/// from the daemon listener.
async fn accept_lsp_sessions(listener: UnixListener, state: Arc<AppState>, repo_root: PathBuf) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(e) => {
                tracing::warn!("Knowledge-LSP accept failed: {e}");
                continue;
            }
        };
        spawn_lsp_session_thread(state.clone(), repo_root.clone(), stream);
    }
}

/// Convert one accepted tokio stream to blocking std I/O and hand it to a plain
/// thread, matching lsp-server's synchronous transport model.
fn spawn_lsp_session_thread(state: Arc<AppState>, repo_root: PathBuf, stream: UnixStream) {
    let Some(stream) = blocking_lsp_stream(stream) else {
        return;
    };

    if let Err(e) = thread::Builder::new()
        .name("MooseDevLspSession".to_string())
        .spawn(move || {
            if let Err(e) = run_session(state, repo_root, stream) {
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
) -> anyhow::Result<()> {
    let (connection, io_threads) = socket_connection(stream)?;
    let mut session = LspSession {
        state,
        connection,
        encoding: SessionEncoding::Utf16,
        repo_root,
        diagnostics: DiagnosticsConfig::default(),
    };
    session.initialize()?;
    session.run()?;
    drop(session);
    io_threads.join()?;
    Ok(())
}

/// Build the same channel-based transport shape as `Connection::stdio`, but on a
/// blocking UnixStream accepted by the daemon listener.
fn socket_connection(stream: StdUnixStream) -> io::Result<(Connection, SocketIoThreads)> {
    let read_stream = stream.try_clone()?;
    let write_stream = stream;
    let (connection, io_connection) = Connection::memory();

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

    Ok((connection, SocketIoThreads { reader, writer }))
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
        self.diagnostics = diagnostics_config(params.initialization_options);

        let result = InitializeResult {
            capabilities: server_capabilities(self.encoding),
            server_info: None,
        };
        self.connection
            .initialize_finish(id, serde_json::to_value(result)?)?;
        Ok(())
    }

    fn run(&mut self) -> anyhow::Result<()> {
        while let Ok(msg) = self.connection.receiver.recv() {
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req)? {
                        break;
                    }
                    if req.method == "textDocument/hover" {
                        self.handle_hover(req.id, req.params)?;
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
                    "textDocument/didOpen" | "textDocument/didSave" => {
                        self.handle_diagnostics_event(notification.params)?;
                    }
                    "textDocument/didClose" | "textDocument/didChange" => {}
                    "exit" => break,
                    _ => {}
                },
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn handle_diagnostics_event(&self, params: serde_json::Value) -> anyhow::Result<()> {
        // Diagnostics are intentionally saved-state only: use the URI from the
        // event, but ignore any text payload the editor sends with didOpen/didSave.
        let Some(uri) = notification_text_document_uri(&params) else {
            return Ok(());
        };
        let Some(rel_path) = repo_relative_path(&self.repo_root, &uri) else {
            return Ok(());
        };
        let diagnostics = self.file_diagnostics(&rel_path);
        self.publish(uri, diagnostics)
    }

    fn file_diagnostics(&self, rel_path: &str) -> Vec<Diagnostic> {
        let Some(substrate) = self.state.substrate() else {
            return Vec::new();
        };

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
            .then(|| self.file_last_commit_instant(rel_path))
            .flatten();

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
            let Some(range) = self.diagnostic_range(rel_path, definition.range) else {
                continue;
            };

            if self.diagnostics.constraints {
                for record in records.iter().filter(|record| record.kind == "Constraint") {
                    let message = match &record.description {
                        Some(description) => format!(
                            "constrained by \"{}\": {}",
                            record.title,
                            description_claim(description)
                        ),
                        None => format!(
                            "constrained by \"{}\" ({})",
                            record.title,
                            short_record_id(&record.iri)
                        ),
                    };
                    let mut diagnostic = Diagnostic::new(
                        range,
                        Some(DiagnosticSeverity::INFORMATION),
                        None,
                        Some("moosedev".to_string()),
                        message,
                        None,
                        None,
                    );
                    add_record_code(&mut diagnostic, record);
                    diagnostics.push(diagnostic);
                }
            }

            if self.diagnostics.stale_rationale {
                if let Some(record) = stale_rationale_record(&records, file_commit) {
                    let mut diagnostic = Diagnostic::new(
                        range,
                        Some(DiagnosticSeverity::HINT),
                        None,
                        Some("moosedev".to_string()),
                        "rationale predates later changes to this file".to_string(),
                        None,
                        None,
                    );
                    add_record_code(&mut diagnostic, record);
                    diagnostics.push(diagnostic);
                }
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

    fn diagnostic_range(&self, rel_path: &str, range: SourceRange) -> Option<Range> {
        match self.encoding {
            SessionEncoding::Utf8 => Some(Range::new(
                Position::new(range.start.line, range.start.col),
                Position::new(range.end.line, range.end.col),
            )),
            SessionEncoding::Utf16 => {
                let file = std::fs::read_to_string(self.repo_root.join(rel_path)).ok()?;
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

    fn publish(&self, uri: Uri, diagnostics: Vec<Diagnostic>) -> anyhow::Result<()> {
        // Load-bearing severity ceiling: every diagnostics publish flows through
        // this gate so v2.0 cannot accidentally emit Warning or Error.
        debug_assert!(diagnostics.iter().all(is_allowed_diagnostic_severity));
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
        let position = params.text_document_position_params.position;
        let col0 = utf8_col(
            &self.repo_root,
            &rel_path,
            position.line,
            position.character,
            self.encoding,
        )?;
        let dossier = match get_entity_dossier(
            &self.state,
            &DossierTarget::Position {
                file: rel_path,
                line: position.line + 1,
                col: col0 + 1,
            },
        ) {
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

fn diagnostics_config(options: Option<serde_json::Value>) -> DiagnosticsConfig {
    options
        .and_then(|options| serde_json::from_value::<InitializationOptions>(options).ok())
        .map(|options| options.diagnostics)
        .unwrap_or_default()
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

fn newest_record(records: &[crate::graph::RecordSummary]) -> Option<&crate::graph::RecordSummary> {
    records
        .iter()
        .filter_map(|record| {
            DateTime::parse_from_rfc3339(&record.timestamp)
                .ok()
                .map(|instant| (instant, record))
        })
        .max_by_key(|(instant, _)| *instant)
        .map(|(_, record)| record)
}

fn stale_rationale_record(
    records: &[crate::graph::RecordSummary],
    file_commit: Option<DateTime<FixedOffset>>,
) -> Option<&crate::graph::RecordSummary> {
    let record = newest_record(records)?;
    let instant = DateTime::parse_from_rfc3339(&record.timestamp).ok()?;
    (instant < file_commit?).then_some(record)
}

fn description_claim(description: &str) -> String {
    // First sentence or 140 chars, whichever ends first — a late first period
    // must not produce a paragraph-length diagnostic message.
    let capped = description.chars().take(140).collect::<String>();
    match capped.find('.') {
        Some(end) => capped[..=end].trim().to_string(),
        None => capped.trim().to_string(),
    }
}

fn add_record_code(diagnostic: &mut Diagnostic, record: &crate::graph::RecordSummary) {
    let Some(url) = &record.workbench_url else {
        return;
    };
    let Ok(href) = Uri::from_str(url) else {
        return;
    };
    diagnostic.code = Some(NumberOrString::String(short_record_id(&record.iri)));
    diagnostic.code_description = Some(CodeDescription { href });
}

fn short_record_id(iri: &str) -> String {
    iri.rsplit(['/', '#'])
        .next()
        .unwrap_or(iri)
        .chars()
        .take(8)
        .collect()
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
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
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
        assert_eq!(diagnostics_config(None), DiagnosticsConfig::default());
        assert_eq!(
            diagnostics_config(Some(serde_json::json!("not an object"))),
            DiagnosticsConfig::default()
        );
    }

    #[test]
    fn diagnostics_config_honors_false_and_ignores_unknown_keys() {
        let config = diagnostics_config(Some(serde_json::json!({
            "diagnostics": {
                "constraints": false,
                "staleRationale": false,
                "ignored": "value"
            },
            "other": true
        })));

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
