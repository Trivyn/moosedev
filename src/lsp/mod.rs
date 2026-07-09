//! Knowledge-LSP transport skeleton.
//!
//! Editors spawn `moosedev lsp`, which only relays stdio to this daemon-owned
//! Unix socket. The daemon session shares the same [`AppState`] as MCP/HTTP, so
//! editor hover can serve the same dossiers as the MCP tools.

use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use lsp_server::{Connection, ErrorCode, Message, Response};
use lsp_types::{
    ClientCapabilities, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, MarkupContent, MarkupKind, PositionEncodingKind,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
};
use tokio::net::{UnixListener, UnixStream};

use crate::graph::{get_entity_dossier, render_markdown, AppState, DossierTarget};

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
                    "textDocument/didOpen"
                    | "textDocument/didSave"
                    | "textDocument/didClose"
                    | "textDocument/didChange" => {}
                    "exit" => break,
                    _ => {}
                },
                Message::Response(_) => {}
            }
        }
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
    // Do not canonicalize or resolve symlinks here: the substrate stores the
    // literal repo-relative paths produced during indexing.
    Some(
        relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    )
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
}
