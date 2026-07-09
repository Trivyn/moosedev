//! Knowledge-LSP transport skeleton.
//!
//! Editors spawn `moosedev lsp`, which only relays stdio to this daemon-owned
//! Unix socket. The daemon session shares the same [`AppState`] as MCP/HTTP, but
//! this phase deliberately returns only a fixed hover placeholder.

use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use lsp_server::{Connection, ErrorCode, Message, Response};
use lsp_types::{
    ClientCapabilities, Hover, HoverContents, HoverProviderCapability, InitializeParams,
    InitializeResult, MarkupContent, MarkupKind, PositionEncodingKind, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncOptions, TextDocumentSyncSaveOptions,
};
use tokio::net::{UnixListener, UnixStream};

use crate::graph::AppState;

const LSP_SOCKET_FILE_NAME: &str = "moosedev-lsp.sock";
const PLACEHOLDER_HOVER: &str =
    "MOOSEDev knowledge-LSP: transport OK (dossier wiring lands in phase 2)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEncoding {
    Utf8,
    Utf16,
}

struct LspSession {
    _state: Arc<AppState>,
    connection: Connection,
    encoding: SessionEncoding,
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
    tokio::spawn(accept_lsp_sessions(listener, state));

    Some(path)
}

fn lsp_disabled() -> bool {
    std::env::var_os("MOOSEDEV_NO_LSP")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

/// Accept LSP connections forever; per-session failures are logged and isolated
/// from the daemon listener.
async fn accept_lsp_sessions(listener: UnixListener, state: Arc<AppState>) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(e) => {
                tracing::warn!("Knowledge-LSP accept failed: {e}");
                continue;
            }
        };
        spawn_lsp_session_thread(state.clone(), stream);
    }
}

/// Convert one accepted tokio stream to blocking std I/O and hand it to a plain
/// thread, matching lsp-server's synchronous transport model.
fn spawn_lsp_session_thread(state: Arc<AppState>, stream: UnixStream) {
    let Some(stream) = blocking_lsp_stream(stream) else {
        return;
    };

    if let Err(e) = thread::Builder::new()
        .name("MooseDevLspSession".to_string())
        .spawn(move || {
            if let Err(e) = run_session(state, stream) {
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
fn run_session(state: Arc<AppState>, stream: StdUnixStream) -> anyhow::Result<()> {
    let (connection, io_threads) = socket_connection(stream)?;
    let mut session = LspSession {
        _state: state,
        connection,
        encoding: SessionEncoding::Utf16,
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
        for msg in &self.connection.receiver {
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req)? {
                        break;
                    }
                    if req.method == "textDocument/hover" {
                        let hover = Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: PLACEHOLDER_HOVER.to_string(),
                            }),
                            range: None,
                        };
                        self.connection
                            .sender
                            .send(Response::new_ok(req.id, hover).into())?;
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
}
