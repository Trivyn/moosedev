//! `moosedev.openEntity` is server-handled (no per-editor glue): the lens
//! command resolves the entity's workbench URL and asks the client to open it
//! via `window/showDocument` (fire-and-forget), degrading to an info message
//! when the capability or the workbench address is absent. Invalid arguments
//! are `InvalidParams`, matching the write-path error discipline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::lsp;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
const NORMALIZED_SYMBOL: &str = "rust-analyzer cargo moosedev . runtime/build_server().";
const WORKBENCH_ADDR: &str = "127.0.0.1:7474";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let short_tag = tag.chars().take(8).collect::<String>();
    let dir = Path::new("/private/tmp").join(format!(
        "mloe-{}-{}-{}",
        short_tag,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn synthetic_repo_root(name: &str) -> PathBuf {
    let repo_root = fresh_dir(name);
    let src = repo_root.join("src");
    std::fs::create_dir_all(&src).expect("create synthetic src dir");
    let source = [
        "pub mod runtime {",
        "",
        "    pub struct Server;",
        "    impl Server {}",
        "",
        "    // setup",
        "    #[allow(dead_code)]",
        "    build_server();",
        "}",
        "",
    ]
    .join("\n");
    std::fs::write(src.join("runtime.rs"), source).expect("write synthetic runtime.rs");
    repo_root
}

fn state_with_substrate(name: &str) -> AppState {
    let dir = fresh_dir(name);
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    state.set_substrate(Arc::new(synthetic_substrate()));
    let component = record(&state, "SystemComponent", "runtime component");
    insert_literal(&state, &component, COVERS_PATH, "src/");
    state
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record item")
}

fn insert_literal(state: &AppState, subject: &str, predicate: &str, value: &str) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(predicate).unwrap(),
        Literal::new_simple_literal(value),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
}

fn mint_public_entities(state: &AppState) {
    let substrate = state.substrate().expect("substrate");
    let terms = graph::CodeTerms::resolve(state).expect("code terms");
    let components = graph::load_components(state).expect("components");
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(
        state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .expect("mint plan");
    graph::apply_mint(state, &plan, &terms).expect("apply mint");
}

fn public_entity_iri(state: &AppState) -> String {
    let terms = graph::CodeTerms::resolve(state).expect("code terms");
    graph::entities_by_symbol(state, &terms)
        .expect("entities by symbol")
        .get(NORMALIZED_SYMBOL)
        .cloned()
        .expect("build_server minted")
}

fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();
    let mut document = Document::new();
    document.relative_path = "src/runtime.rs".to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);

    let mut module = SymbolInformation::new();
    module.symbol = MODULE_SYMBOL.to_string();
    module.display_name = "runtime".to_string();
    module.kind = EnumOrUnknown::new(symbol_information::Kind::Module);
    document.symbols.push(module);
    let mut module_occ = Occurrence::new();
    module_occ.symbol = MODULE_SYMBOL.to_string();
    module_occ.range = vec![0, 0, 30, 0];
    module_occ.symbol_roles = 1;
    module_occ.enclosing_range = vec![0, 0, 30, 0];
    document.occurrences.push(module_occ);

    let mut public = SymbolInformation::new();
    public.symbol = PUBLIC_SYMBOL.to_string();
    public.display_name = "build_server".to_string();
    public.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut signature = Signature::new();
    signature.text = "pub fn build_server()".to_string();
    public.signature_documentation = MessageField::some(signature);
    document.symbols.push(public);
    let mut public_occ = Occurrence::new();
    public_occ.symbol = PUBLIC_SYMBOL.to_string();
    public_occ.range = vec![7, 4, 16];
    public_occ.symbol_roles = 1;
    public_occ.enclosing_range = vec![0, 0, 30, 0];
    document.occurrences.push(public_occ);
    index.documents.push(document);

    let meta = SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        1,
        2,
    );
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
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

async fn spawn_listener(
    state: Arc<AppState>,
    data_dir: &Path,
    repo_root: &Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(data_dir)?;
    let listener = lsp::spawn_lsp_listener_at(state, data_dir, repo_root.to_path_buf())
        .await
        .expect("LSP listener should start");
    let socket = listener.socket().to_path_buf();
    wait_for_socket(&socket).await;
    Ok(socket)
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("LSP socket {} never became ready", socket.display());
}

/// Publish the HTTP address in-process, as `spawn_http_if_enabled` would at
/// bind time — the only source the workbench-URL builder trusts (the on-disk
/// `http.addr` file can be stale after a crash).
fn publish_workbench_addr(state: &AppState) {
    state.publish_http_addr(WORKBENCH_ADDR.parse().expect("socket addr"));
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

    async fn read(&mut self) -> anyhow::Result<Option<Value>> {
        read_lsp_message(&mut self.reader).await
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> anyhow::Result<Value> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        for _ in 0..20 {
            let message = self.read().await?.expect("LSP response");
            if message["id"] == json!(id) {
                return Ok(message);
            }
        }
        anyhow::bail!("no response for {method}")
    }

    async fn initialize(&mut self, capabilities: Value) -> anyhow::Result<()> {
        let response = self
            .request(
                1,
                "initialize",
                json!({
                    "processId": null,
                    "rootUri": null,
                    "capabilities": capabilities,
                    "initializationOptions": null
                }),
            )
            .await?;
        assert!(
            response.get("error").is_none(),
            "initialize error: {response}"
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))
            .await
    }

    /// Send `moosedev.openEntity` without waiting for the response, so the
    /// test can assert on everything the server emits in between.
    async fn send_open_entity(&mut self, id: u64, arguments: Value) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "workspace/executeCommand",
            "params": { "command": "moosedev.openEntity", "arguments": arguments }
        }))
        .await
    }

    async fn shutdown_and_exit(&mut self) -> anyhow::Result<()> {
        let shutdown = self.request(99, "shutdown", Value::Null).await?;
        assert!(shutdown.get("result").is_some(), "shutdown: {shutdown}");
        self.send(json!({ "jsonrpc": "2.0", "method": "exit", "params": null }))
            .await
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

async fn direct_client(
    socket: &Path,
) -> anyhow::Result<RawLspClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>>
{
    let stream = UnixStream::connect(socket).await?;
    let (reader, writer) = stream.into_split();
    Ok(RawLspClient::new(reader, writer))
}

fn show_document_capabilities() -> Value {
    json!({ "window": { "showDocument": { "support": true } } })
}

fn expected_workbench_url(entity_iri: &str) -> String {
    let uuid = entity_iri.rsplit('/').next().expect("IRI local name");
    format!("http://{WORKBENCH_ADDR}/#/record/{uuid}")
}

#[tokio::test]
async fn open_entity_opens_workbench_via_show_document() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("oe-root");
    let data_dir = fresh_dir("oe-data");
    let state = Arc::new(state_with_substrate("oe-state"));
    mint_public_entities(&state);
    let entity = public_entity_iri(&state);
    publish_workbench_addr(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(show_document_capabilities()).await?;

    client.send_open_entity(66, json!([entity])).await?;

    // The session writer is single-threaded: the showDocument request must
    // precede the executeCommand response.
    let request = client.read().await?.expect("showDocument request");
    assert_eq!(
        request["method"],
        json!("window/showDocument"),
        "expected a showDocument request first: {request}"
    );
    assert_eq!(request["params"]["external"], json!(true));
    assert_eq!(
        request["params"]["uri"],
        json!(expected_workbench_url(&entity))
    );
    let request_id = request["id"].clone();
    assert!(!request_id.is_null(), "showDocument must be a request");

    // Reply as an editor would; the session drops the response (AD fb61ce61).
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": { "success": true }
        }))
        .await?;

    let response = client.read().await?.expect("executeCommand response");
    assert_eq!(response["id"], json!(66), "response follows: {response}");
    assert!(response["result"].is_null(), "openEntity returns null");
    assert!(response.get("error").is_none());

    // The dropped client response must not wedge the loop.
    let follow_up = client
        .request(
            44,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": format!("file://{}", repo_root.join("src/runtime.rs").display()) },
                "position": { "line": 7, "character": 5 }
            }),
        )
        .await?;
    assert!(follow_up.get("error").is_none(), "hover after showDocument");

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn open_entity_falls_back_to_message_without_show_document() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("oe-fb-root");
    let data_dir = fresh_dir("oe-fb-data");
    let state = Arc::new(state_with_substrate("oe-fb-state"));
    mint_public_entities(&state);
    let entity = public_entity_iri(&state);
    publish_workbench_addr(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(json!({})).await?;

    client.send_open_entity(66, json!([entity])).await?;

    let message = client.read().await?.expect("showMessage fallback");
    assert_eq!(message["method"], json!("window/showMessage"));
    let text = message["params"]["message"].as_str().unwrap_or_default();
    assert!(
        text.contains(&expected_workbench_url(&entity)),
        "fallback carries the URL: {text}"
    );

    let response = client.read().await?.expect("executeCommand response");
    assert_eq!(response["id"], json!(66));
    assert!(response["result"].is_null());

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn open_entity_rejects_invalid_arguments() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("oe-err-root");
    let data_dir = fresh_dir("oe-err-data");
    let state = Arc::new(state_with_substrate("oe-err-state"));
    mint_public_entities(&state);
    publish_workbench_addr(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(show_document_capabilities()).await?;

    let invalid_params = -32602;
    for (id, argument) in [
        (61_u64, json!("not an iri")),
        (
            62,
            json!("https://moosedev.dev/kg/CodeEntity/00000000-0000-0000-0000-000000000000"),
        ),
        (63, json!({ "entity": "wrong shape" })),
    ] {
        let response = client
            .request(
                id,
                "workspace/executeCommand",
                json!({ "command": "moosedev.openEntity", "arguments": [argument] }),
            )
            .await?;
        assert_eq!(
            response["error"]["code"],
            json!(invalid_params),
            "argument {argument} must be InvalidParams: {response}"
        );
    }

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn open_entity_reports_when_workbench_not_serving() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("oe-na-root");
    let data_dir = fresh_dir("oe-na-data");
    let state = Arc::new(state_with_substrate("oe-na-state"));
    mint_public_entities(&state);
    let entity = public_entity_iri(&state);
    // A server that stops after publishing must withdraw the in-process
    // address; stale workbench links are no better than a stale addr file.
    let retired_addr = WORKBENCH_ADDR.parse().expect("socket addr");
    state.publish_http_addr(retired_addr);
    state.clear_http_addr_if(retired_addr);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(show_document_capabilities()).await?;

    client.send_open_entity(66, json!([entity])).await?;

    let message = client.read().await?.expect("showMessage explanation");
    assert_eq!(
        message["method"],
        json!("window/showMessage"),
        "no showDocument without a workbench address: {message}"
    );
    assert!(message["params"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not serving"));

    let response = client.read().await?.expect("executeCommand response");
    assert_eq!(response["id"], json!(66));
    assert!(response["result"].is_null());

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn open_entity_without_arguments_explains_instead_of_erroring() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("oe-noarg-root");
    let data_dir = fresh_dir("oe-noarg-data");
    let state = Arc::new(state_with_substrate("oe-noarg-state"));
    mint_public_entities(&state);
    publish_workbench_addr(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(show_document_capabilities()).await?;

    // Lenses over unminted debt-surface entities carry no arguments; a click
    // there gets an explanation, not an error popup.
    client.send_open_entity(66, json!([])).await?;

    let message = client.read().await?.expect("showMessage explanation");
    assert_eq!(message["method"], json!("window/showMessage"));
    assert!(message["params"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no knowledge entity minted"));

    let response = client.read().await?.expect("executeCommand response");
    assert_eq!(response["id"], json!(66));
    assert!(response["result"].is_null());

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}
