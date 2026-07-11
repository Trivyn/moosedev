//! Knowledge-LSP hover dossier integration tests.
//!
//! The harness combines a tiny synthetic SCIP substrate with a raw LSP socket
//! client, proving that hover serves the same dossier Markdown as the graph API.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{
    self, AppState, CodeSelector, DossierTarget, RecordInput, PROJECT_KG_GRAPH_IRI,
};
use moosedev::lsp;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixStream;

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
const PRIVATE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/private_helper().";
const LOCAL_SYMBOL: &str = "local 0";

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = Path::new("/private/tmp").join(format!(
        "mlh-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn bootstrap(name: &str) -> AppState {
    let dir = fresh_dir(name);
    AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state")
}

fn synthetic_repo_root(name: &str, runtime_line8: &str) -> PathBuf {
    let repo_root = fresh_dir(name);
    let src = repo_root.join("src");
    std::fs::create_dir_all(&src).expect("create synthetic src dir");
    std::fs::write(src.join("runtime.rs"), runtime_source(runtime_line8))
        .expect("write synthetic runtime.rs");
    repo_root
}

fn runtime_source(line8: &str) -> String {
    let mut lines = vec![
        "pub mod runtime {",
        "",
        "    pub struct Server;",
        "    impl Server {}",
        "",
        "    // setup",
        "    #[allow(dead_code)]",
        line8,
        "    let _after_public = 1;",
        "",
        "    #[allow(dead_code)]",
        "    private_helper();",
        "}",
        "",
        "// filler",
        "// filler",
        "// filler",
        "// filler",
        "// filler",
        "// filler",
        "        tmp",
    ];
    lines.push("");
    lines.join("\n")
}

fn state_with_substrate(name: &str, public_start: u32) -> AppState {
    let state = bootstrap(name);
    state.set_substrate(Arc::new(synthetic_substrate(public_start, false)));
    seed_component(&state, "runtime component", "src/");
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

fn seed_component(state: &AppState, title: &str, covers_path: &str) -> String {
    let iri = record(state, "SystemComponent", title);
    insert_literal(state, &iri, COVERS_PATH, covers_path);
    iri
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

fn link_public(state: &AppState, record_iri: &str, public_start: u32) {
    graph::link_code(
        state,
        record_iri,
        "concerns",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line: 8,
            col: public_start + 1,
        },
        "tester",
    )
    .expect("link public build_server");
}

fn synthetic_substrate(public_start: u32, stale: bool) -> Substrate {
    let mut index = Index::new();
    let mut document = doc("src/runtime.rs");
    document.symbols.push(info(
        MODULE_SYMBOL,
        "runtime",
        symbol_information::Kind::Module,
        "pub mod runtime",
    ));
    document
        .occurrences
        .push(occ(MODULE_SYMBOL, vec![0, 0, 30, 0], 1));
    document.symbols.push(info(
        PUBLIC_SYMBOL,
        "build_server",
        symbol_information::Kind::Function,
        "pub fn build_server()",
    ));
    document.occurrences.push(occ(
        PUBLIC_SYMBOL,
        vec![7, public_start as i32, public_start as i32 + 12],
        1,
    ));
    document.symbols.push(info(
        PRIVATE_SYMBOL,
        "private_helper",
        symbol_information::Kind::Function,
        "fn private_helper()",
    ));
    document
        .occurrences
        .push(occ(PRIVATE_SYMBOL, vec![11, 4, 18], 1));
    document.symbols.push(info(
        LOCAL_SYMBOL,
        "tmp",
        symbol_information::Kind::Variable,
        "let tmp",
    ));
    document
        .occurrences
        .push(occ(LOCAL_SYMBOL, vec![20, 8, 11], 0));
    index.documents.push(document);

    Substrate::from_index(index, meta(), stale).expect("synthetic substrate")
}

fn doc(relative_path: &str) -> Document {
    let mut document = Document::new();
    document.relative_path = relative_path.to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    document
}

fn occ(symbol: &str, range: Vec<i32>, symbol_roles: i32) -> Occurrence {
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.to_string();
    occurrence.range = range;
    occurrence.symbol_roles = symbol_roles;
    occurrence.enclosing_range = vec![0, 0, 30, 0];
    occurrence
}

fn info(
    symbol: &str,
    display_name: &str,
    kind: symbol_information::Kind,
    signature: &str,
) -> SymbolInformation {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = display_name.to_string();
    info.kind = EnumOrUnknown::new(kind);
    let mut signature_documentation = Signature::new();
    signature_documentation.text = signature.to_string();
    info.signature_documentation = MessageField::some(signature_documentation);
    info
}

fn meta() -> SubstrateMeta {
    SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        1,
        4,
    )
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
    std::fs::create_dir_all(data_dir).context("create listener data dir")?;
    let listener = lsp::spawn_lsp_listener_at(state, data_dir, repo_root.to_path_buf())
        .await
        .expect("LSP listener should start");
    wait_for_socket(listener.socket()).await;
    Ok(listener.socket().to_path_buf())
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

    async fn hover(
        &mut self,
        id: i32,
        uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> anyhow::Result<Value> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri.into() },
                "position": { "line": line, "character": character }
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

async fn direct_client(
    socket: &Path,
) -> anyhow::Result<RawLspClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>>
{
    let stream = UnixStream::connect(socket).await?;
    let (reader, writer) = stream.into_split();
    Ok(RawLspClient::new(reader, writer))
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn assert_null_hover(response: &Value) {
    assert!(response.get("error").is_none(), "hover error: {response}");
    assert!(
        response["result"].is_null(),
        "hover should be silent null: {response}"
    );
}

fn hover_markdown(response: &Value) -> &str {
    assert!(response.get("error").is_none(), "hover error: {response}");
    assert_eq!(response["result"]["contents"]["kind"], json!("markdown"));
    response["result"]["contents"]["value"]
        .as_str()
        .expect("markdown value")
}

#[tokio::test]
async fn hover_serves_linked_dossier_utf8() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("utf8-root", "    build_server();");
    let data_dir = fresh_dir("utf8-data");
    let state = Arc::new(state_with_substrate("utf8-state", 4));
    std::fs::write(state.data_dir.join("http.addr"), "127.0.0.1:7474\n")?;
    let decision = record(
        &state,
        "ArchitecturalDecision",
        "Runtime builder hover decision",
    );
    let constraint = record(&state, "Constraint", "Runtime builder hover constraint");
    link_public(&state, &decision, 4);
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;

    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    let response = client
        .hover(2, file_uri(&repo_root.join("src/runtime.rs")), 7, 4)
        .await?;
    let expected_dossier = graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line: 8,
            col: 5,
        },
    )?
    .expect("expected dossier");
    let expected = graph::render_markdown(&expected_dossier);

    assert_eq!(hover_markdown(&response), expected);
    assert!(hover_markdown(&response).contains("Runtime builder hover decision"));
    assert!(hover_markdown(&response).contains("](http://127.0.0.1:7474/#/adrs/"));
    assert!(hover_markdown(&response).contains("Runtime builder hover constraint"));
    assert!(hover_markdown(&response).contains("](http://127.0.0.1:7474/#/constraints/"));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn hover_is_silent_on_unlinked_and_tokenless() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("silent-root", "    build_server();");
    let data_dir = fresh_dir("silent-data");
    let state = Arc::new(state_with_substrate("silent-state", 4));
    let decision = record(
        &state,
        "ArchitecturalDecision",
        "Runtime builder linked only",
    );
    link_public(&state, &decision, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;

    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));
    assert_null_hover(&client.hover(2, &uri, 11, 4).await?);
    assert_null_hover(&client.hover(3, &uri, 1, 0).await?);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn hover_is_silent_for_uncovered_file() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("uncovered-root", "    build_server();");
    let ui = repo_root.join("ui/src");
    std::fs::create_dir_all(&ui)?;
    std::fs::write(ui.join("App.tsx"), "export function App() {}\n")?;
    let data_dir = fresh_dir("uncovered-data");
    let state = Arc::new(state_with_substrate("uncovered-state", 4));
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;

    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    assert_null_hover(&client.hover(2, file_uri(&ui.join("App.tsx")), 0, 0).await?);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn hover_utf16_conversion() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("utf16-root", "é   build_server();");
    let data_dir = fresh_dir("utf16-data");
    let state = Arc::new(state_with_substrate("utf16-state", 5));
    let decision = record(
        &state,
        "ArchitecturalDecision",
        "Runtime builder utf16 hover",
    );
    link_public(&state, &decision, 5);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;

    let mut client = direct_client(&socket).await?;
    client.initialize(false).await?;
    let response = client
        .hover(2, file_uri(&repo_root.join("src/runtime.rs")), 7, 4)
        .await?;

    assert!(hover_markdown(&response).contains("Runtime builder utf16 hover"));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn hover_outside_root_is_silent() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("outside-root", "    build_server();");
    let outside_root = synthetic_repo_root("outside-other", "    build_server();");
    let data_dir = fresh_dir("outside-data");
    let state = Arc::new(state_with_substrate("outside-state", 4));
    let decision = record(&state, "ArchitecturalDecision", "Runtime builder outside");
    link_public(&state, &decision, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;

    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    assert_null_hover(
        &client
            .hover(2, file_uri(&outside_root.join("src/runtime.rs")), 7, 4)
            .await?,
    );
    assert_null_hover(&client.hover(3, "untitled:///src/runtime.rs", 7, 4).await?);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    let _ = std::fs::remove_dir_all(&outside_root);
    Ok(())
}

#[tokio::test]
async fn repo_substrate_hover_when_index_present() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let substrate_data_dir = repo_root.join(".moosedev");
    let meta_path = substrate_data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping LSP hover substrate integration: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }

    let substrate = Substrate::load(&substrate_data_dir, repo_root)?;
    let state = Arc::new(bootstrap("repo-substrate-hover"));
    state.set_substrate(Arc::new(substrate));
    seed_component(&state, "source component", "src/");
    let scratch = record(
        &state,
        "ArchitecturalDecision",
        "Scratch build_server LSP hover",
    );
    let runtime = std::fs::read_to_string(repo_root.join("src/runtime.rs"))?;
    let (line, col) = runtime
        .lines()
        .enumerate()
        .find_map(|(idx, line)| {
            line.find("fn build_server")
                .map(|offset| (idx as u32 + 1, offset as u32 + 4))
        })
        .expect("build_server definition");
    graph::link_code(
        &state,
        &scratch,
        "concerns",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line,
            col,
        },
        "tester",
    )?;

    let data_dir = fresh_dir("repo-substrate-hover-data");
    let socket = spawn_listener(state, &data_dir, repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(true).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));
    let linked = client.hover(2, &uri, line - 1, col - 1).await?;
    assert!(hover_markdown(&linked).contains("Scratch build_server LSP hover"));

    let (comment_line, comment_char) = runtime
        .lines()
        .enumerate()
        .find_map(|(idx, line)| line.find("//").map(|offset| (idx as u32, offset as u32)))
        .context("comment line")?;
    assert_null_hover(&client.hover(3, &uri, comment_line, comment_char).await?);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}
