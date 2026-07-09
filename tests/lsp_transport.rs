//! In-process transport coverage for the Knowledge-LSP socket plus one real
//! binary shim test. The client below speaks raw Content-Length LSP framing so
//! the shim remains a byte relay in the test as well as in production.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use moosedev::graph::AppState;
use moosedev::lsp;
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixStream;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_moosedev")
}

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("moosedev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

async fn spawn_listener(data_dir: &Path) -> PathBuf {
    let state = AppState::bootstrap(data_dir, &ontology_dir()).expect("bootstrap app state");
    let socket = lsp::spawn_lsp_listener(Arc::new(state), data_dir)
        .await
        .expect("LSP listener should start");
    wait_for_socket(&socket).await;
    socket
}

// Polling keeps the in-process listener tests independent of daemon lifecycle
// plumbing; a successful connect proves the accept loop is actually serving.
async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("LSP socket {} never became ready", socket.display());
}

struct RawLspClient<R, W> {
    reader: BufReader<R>,
    writer: W,
}

impl<R, W> RawLspClient<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    async fn send(&mut self, message: Value) -> anyhow::Result<()> {
        let body = serde_json::to_vec(&message)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;
        Ok(())
    }

    // Read one Content-Length framed JSON-RPC message. The timeout turns a hung
    // relay into a crisp test failure instead of blocking the suite indefinitely.
    async fn read(&mut self) -> anyhow::Result<Option<Value>> {
        read_lsp_message(&mut self.reader).await
    }

    async fn initialize(&mut self, utf8: bool) -> anyhow::Result<Value> {
        let capabilities = if utf8 {
            json!({ "general": { "positionEncodings": ["utf-8"] } })
        } else {
            json!({})
        };
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                "capabilities": capabilities
            }
        }))
        .await?;
        let response = self.read().await?.expect("initialize response");
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }))
        .await?;
        Ok(response)
    }

    async fn hover(&mut self, id: i32) -> anyhow::Result<Value> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///tmp/example.rs" },
                "position": { "line": 0, "character": 0 }
            }
        }))
        .await?;
        Ok(self.read().await?.expect("hover response"))
    }

    async fn shutdown_and_exit(&mut self) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "shutdown",
            "params": null
        }))
        .await?;
        let shutdown = self.read().await?.expect("shutdown response");
        assert_eq!(shutdown["id"], json!(99));
        assert!(shutdown.get("result").is_some(), "shutdown: {shutdown}");
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }))
        .await?;
        Ok(())
    }
}

async fn read_lsp_message<R>(reader: &mut R) -> anyhow::Result<Option<Value>>
where
    R: AsyncBufRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n =
            tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
        if n == 0 {
            return Ok(None);
        }
        if line == "\r\n" {
            break;
        }
        let Some(line) = line.strip_suffix("\r\n") else {
            anyhow::bail!("malformed LSP header line: {line:?}");
        };
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.parse::<usize>()?);
        }
    }

    let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
    let mut body = vec![0; len];
    tokio::time::timeout(Duration::from_secs(10), reader.read_exact(&mut body)).await??;
    Ok(Some(serde_json::from_slice(&body)?))
}

// Direct daemon-socket client for tests that should not involve a child daemon
// or shim process.
async fn direct_client(
    socket: &Path,
) -> anyhow::Result<RawLspClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>>
{
    let stream = UnixStream::connect(socket).await?;
    let (reader, writer) = stream.into_split();
    Ok(RawLspClient::new(reader, writer))
}

fn assert_hover_placeholder(response: &Value) {
    assert!(response.get("error").is_none(), "hover error: {response}");
    assert_eq!(
        response["result"]["contents"]["kind"],
        json!("markdown"),
        "hover should return markdown: {response}"
    );
    assert_eq!(
        response["result"]["contents"]["value"],
        json!("MOOSEDev knowledge-LSP: transport OK (dossier wiring lands in phase 2)")
    );
}

#[tokio::test]
async fn initialize_negotiates_and_declares_capabilities() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let data_dir = fresh_data_dir("lsp-negotiate");
    let socket = spawn_listener(&data_dir).await;

    let mut client = direct_client(&socket).await?;
    let init = client.initialize(true).await?;
    let capabilities = &init["result"]["capabilities"];
    assert_eq!(capabilities["hoverProvider"], json!(true));
    assert_eq!(capabilities["textDocumentSync"]["openClose"], json!(true));
    assert_eq!(capabilities["textDocumentSync"]["save"], json!(true));
    assert_eq!(capabilities["positionEncoding"], json!("utf-8"));

    assert_hover_placeholder(&client.hover(2).await?);
    client.shutdown_and_exit().await?;
    let closed = tokio::time::timeout(Duration::from_secs(10), client.read()).await??;
    assert!(closed.is_none(), "socket should close cleanly: {closed:?}");

    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}

#[tokio::test]
async fn utf16_default_when_client_offers_nothing() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let data_dir = fresh_data_dir("lsp-utf16");
    let socket = spawn_listener(&data_dir).await;

    let mut client = direct_client(&socket).await?;
    let init = client.initialize(false).await?;
    let capabilities = &init["result"]["capabilities"];
    assert_ne!(capabilities["positionEncoding"], json!("utf-8"));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}

#[tokio::test]
async fn shim_relays_end_to_end() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let data_dir = fresh_data_dir("lsp-shim");
    let socket = spawn_listener(&data_dir).await;

    let mut child = tokio::process::Command::new(binary())
        .arg("lsp")
        .env("MOOSEDEV_DATA_DIR", &data_dir)
        .env("MOOSEDEV_NO_AUTOSPAWN", "1")
        .env_remove("MOOSEDEV_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn moosedev lsp");

    let stdin = child.stdin.take().expect("shim stdin");
    let stdout = child.stdout.take().expect("shim stdout");
    let mut stderr = child.stderr.take().expect("shim stderr");
    let mut client = RawLspClient::new(stdout, stdin);

    let init = match client.initialize(true).await {
        Ok(init) => init,
        Err(e) => {
            let _ = child.kill().await;
            let mut stderr_text = String::new();
            let _ = stderr.read_to_string(&mut stderr_text).await;
            let status = child.try_wait().ok().flatten();
            anyhow::bail!("shim initialize failed: {e}; status={status:?}; stderr={stderr_text}");
        }
    };
    assert_eq!(
        init["result"]["capabilities"]["positionEncoding"],
        json!("utf-8")
    );
    assert_hover_placeholder(&client.hover(2).await.context("shim hover")?);
    client
        .shutdown_and_exit()
        .await
        .context("shim shutdown/exit")?;
    drop(client);

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .context("wait for shim exit timed out")??;
    let mut stderr_text = String::new();
    let _ = stderr.read_to_string(&mut stderr_text).await;
    assert!(
        status.success(),
        "shim exited with {status}; stderr={stderr_text}"
    );
    assert!(
        socket.exists(),
        "in-process daemon socket should remain owned by test"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}

#[tokio::test]
async fn unknown_request_gets_method_not_found() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let data_dir = fresh_data_dir("lsp-unknown");
    let socket = spawn_listener(&data_dir).await;

    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": "file:///tmp/example.rs" },
                "position": { "line": 0, "character": 0 }
            }
        }))
        .await?;
    let response = client.read().await?.expect("definition response");
    assert_eq!(response["id"], json!(7));
    assert_eq!(response["error"]["code"], json!(-32601));

    assert_hover_placeholder(&client.hover(8).await?);
    client.shutdown_and_exit().await?;

    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}
