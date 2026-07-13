//! Knowledge-LSP publishDiagnostics integration tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, CodeSelector, RecordInput, PROJECT_KG_GRAPH_IRI};
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
const SECOND_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/no_records().";

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let short_tag = tag.chars().take(8).collect::<String>();
    let dir = Path::new("/private/tmp").join(format!(
        "mld-{}-{}-{}",
        short_tag,
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
        "    no_records();",
        "}",
    ];
    lines.push("");
    lines.join("\n")
}

fn synthetic_repo_root(name: &str, runtime_line8: &str) -> PathBuf {
    let repo_root = fresh_dir(name);
    let src = repo_root.join("src");
    std::fs::create_dir_all(&src).expect("create synthetic src dir");
    std::fs::write(src.join("runtime.rs"), runtime_source(runtime_line8))
        .expect("write synthetic runtime.rs");
    repo_root
}

fn state_with_substrate(name: &str, public_start: u32) -> AppState {
    let state = bootstrap(name);
    state.set_substrate(Arc::new(synthetic_substrate(public_start)));
    seed_component(&state, "runtime component", "src/");
    state
}

fn record_at(state: &AppState, kind: &str, title: &str, when: DateTime<Utc>) -> String {
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
        when,
    )
    .expect("record item")
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    record_at(state, kind, title, Utc::now())
}

fn record_with_description(state: &AppState, kind: &str, title: &str, description: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.description.clone(), description.to_string()),
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
        "constrains",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line: 8,
            col: public_start + 1,
        },
        "tester",
    )
    .expect("link public build_server");
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

fn synthetic_substrate(public_start: u32) -> Substrate {
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
        SECOND_SYMBOL,
        "no_records",
        symbol_information::Kind::Function,
        "pub fn no_records()",
    ));
    document
        .occurrences
        .push(occ(SECOND_SYMBOL, vec![8, 4, 14], 1));
    index.documents.push(document);

    Substrate::from_index(index, meta(), false).expect("synthetic substrate")
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
        3,
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
    let listener = spawn_listener_handle(state, data_dir, repo_root).await?;
    Ok(listener.socket().to_path_buf())
}

async fn spawn_listener_handle(
    state: Arc<AppState>,
    data_dir: &Path,
    repo_root: &Path,
) -> anyhow::Result<lsp::LspListener> {
    std::fs::create_dir_all(data_dir).context("create listener data dir")?;
    let listener = lsp::spawn_lsp_listener_at(state, data_dir, repo_root.to_path_buf())
        .await
        .expect("LSP listener should start");
    wait_for_socket(listener.socket()).await;
    Ok(listener)
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

    async fn initialize(
        &mut self,
        utf8: bool,
        initialization_options: Value,
    ) -> anyhow::Result<()> {
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
                "capabilities": capabilities,
                "initializationOptions": initialization_options
            }
        }))
        .await?;
        let response = self.read().await?.expect("initialize response");
        assert!(
            response.get("error").is_none(),
            "initialize error: {response}"
        );
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }))
        .await?;
        Ok(())
    }

    async fn did_open(&mut self, uri: impl Into<String>) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri.into(),
                    "languageId": "rust",
                    "version": 1,
                    "text": "ignored unsaved text"
                }
            }
        }))
        .await
    }

    async fn did_save(&mut self, uri: impl Into<String>) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {
                "textDocument": { "uri": uri.into() },
                "text": "ignored saved notification text"
            }
        }))
        .await
    }

    async fn did_close(&mut self, uri: impl Into<String>) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {
                "textDocument": { "uri": uri.into() }
            }
        }))
        .await
    }

    async fn code_lens(&mut self, uri: &str) -> anyhow::Result<Vec<Value>> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 77,
            "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": uri } }
        }))
        .await?;
        for _ in 0..20 {
            let message = self.read().await?.expect("codeLens response");
            if message["id"] == json!(77) {
                return Ok(message["result"].as_array().cloned().unwrap_or_default());
            }
        }
        anyhow::bail!("no codeLens response for {uri}")
    }

    async fn read_until_diagnostics(&mut self, uri: &str) -> anyhow::Result<Value> {
        for _ in 0..20 {
            let message = self.read().await?.expect("LSP message");
            if message["method"] == json!("textDocument/publishDiagnostics")
                && message["params"]["uri"] == json!(uri)
            {
                return Ok(message);
            }
        }
        anyhow::bail!("no publishDiagnostics for {uri}")
    }

    async fn read_until_diagnostics_timeout(
        &mut self,
        uri: &str,
        timeout: Duration,
    ) -> anyhow::Result<Option<Value>> {
        match tokio::time::timeout(timeout, self.read_until_diagnostics(uri)).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None),
        }
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

    async fn exit(&mut self) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }))
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

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn diagnostics(message: &Value) -> Vec<Value> {
    message["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .clone()
}

fn assert_severity_ceiling(diagnostics: &[Value]) {
    for diagnostic in diagnostics {
        assert!(
            matches!(diagnostic["severity"].as_i64(), Some(3 | 4)),
            "unexpected severity: {diagnostic}"
        );
    }
}

#[tokio::test]
async fn diagnostics_republish_when_substrate_becomes_available() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-late-root", "    build_server();");
    let data_dir = fresh_dir("diag-late-data");
    let state = Arc::new(bootstrap("diag-late-state"));
    seed_component(&state, "runtime component", "src/");
    let constraint = record(&state, "Constraint", "Late substrate constraint");
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    assert_eq!(
        client.read_until_diagnostics(&uri).await?["params"]["diagnostics"],
        json!([])
    );

    state.set_substrate(Arc::new(synthetic_substrate(4)));
    link_public(&state, &constraint, 4);
    let republished = client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(6))
        .await?
        .expect("diagnostics republished after late substrate");
    assert!(diagnostics(&republished).iter().any(|diagnostic| {
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("Late substrate constraint"))
    }));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn diagnostics_republish_after_post_open_graph_write() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-write-root", "    build_server();");
    let data_dir = fresh_dir("diag-write-data");
    let state = Arc::new(state_with_substrate("diag-write-state", 4));
    let initial = record(&state, "Constraint", "Initial constraint");
    link_public(&state, &initial, 4);
    let added = record(&state, "Constraint", "Post-open constraint");
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    assert_eq!(
        diagnostics(&client.read_until_diagnostics(&uri).await?).len(),
        1
    );

    link_public(&state, &added, 4);
    // `link_public` calls the graph primitive directly; production MCP writes
    // invoke the AppState post-write hook after that primitive succeeds.
    state.note_project_write();
    let republished = client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(6))
        .await?
        .expect("diagnostics republished after graph write");
    let republished = diagnostics(&republished);
    assert_eq!(republished.len(), 2);
    assert!(republished.iter().any(|diagnostic| {
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("Post-open constraint"))
    }));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn diagnostics_republish_after_substrate_swap() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-swap-root", "    build_server();");
    let data_dir = fresh_dir("diag-swap-data");
    let state = Arc::new(state_with_substrate("diag-swap-state", 4));
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    client.read_until_diagnostics(&uri).await?;

    state.set_substrate(Arc::new(synthetic_substrate(4)));
    assert!(client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(6))
        .await?
        .is_some());

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn unchanged_knowledge_generation_does_not_republish() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-idle-root", "    build_server();");
    let data_dir = fresh_dir("diag-idle-data");
    let state = Arc::new(state_with_substrate("diag-idle-state", 4));
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    client.read_until_diagnostics(&uri).await?;
    state.set_substrate(Arc::new(synthetic_substrate(4)));
    client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(6))
        .await?
        .expect("diagnostics republished after generation change");

    assert!(client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(3))
        .await?
        .is_none());
    client.exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn closed_documents_are_pruned_from_diagnostics_refresh() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-close-root", "    build_server();");
    let data_dir = fresh_dir("diag-close-data");
    let state = Arc::new(state_with_substrate("diag-close-state", 4));
    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    client.read_until_diagnostics(&uri).await?;
    client.did_close(&uri).await?;
    state.set_substrate(Arc::new(synthetic_substrate(4)));

    assert!(client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(3))
        .await?
        .is_none());
    client.exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn daemon_shutdown_retracts_published_diagnostics() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-retract-root", "    build_server();");
    let data_dir = fresh_dir("diag-retract-data");
    let state = Arc::new(state_with_substrate("diag-retract-state", 4));
    let constraint = record(&state, "Constraint", "Retracted on shutdown");
    link_public(&state, &constraint, 4);
    let listener = spawn_listener_handle(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(listener.socket()).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    assert_eq!(
        diagnostics(&client.read_until_diagnostics(&uri).await?).len(),
        1
    );

    listener.shutdown_sessions().await;

    let retracted = client
        .read_until_diagnostics_timeout(&uri, Duration::from_secs(5))
        .await?
        .expect("shutdown should retract published diagnostics");
    assert_eq!(retracted["params"]["diagnostics"], json!([]));

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn constrained_entity_gets_information() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-info-root", "    build_server();");
    let data_dir = fresh_dir("diag-info-data");
    let state = Arc::new(state_with_substrate("diag-info-state", 4));
    let constraint = record(&state, "Constraint", "Runtime builder must stay local");
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;
    let diagnostics = diagnostics(&published);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], json!(3));
    assert_eq!(
        diagnostics[0]["range"]["start"],
        json!({ "line": 7, "character": 4 })
    );
    assert_eq!(
        diagnostics[0]["range"]["end"],
        json!({ "line": 7, "character": 16 })
    );
    assert!(diagnostics[0]["message"]
        .as_str()
        .unwrap()
        .contains("Runtime builder must stay local"));
    assert_eq!(
        diagnostics[0]["message"],
        json!(
            "constrained by \"Runtime builder must stay local\" (".to_string()
                + &constraint.rsplit('/').next().unwrap()[..8]
                + ")"
        )
    );
    assert!(diagnostics[0].get("codeDescription").is_none());
    assert_severity_ceiling(&diagnostics);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn constraint_diagnostic_links_to_workbench_with_description() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-link-root", "    build_server();");
    let data_dir = fresh_dir("diag-link-data");
    let state = Arc::new(state_with_substrate("diag-link-state", 4));
    std::fs::write(state.data_dir.join("http.addr"), "127.0.0.1:7474\n")?;
    let constraint = record_with_description(
        &state,
        "Constraint",
        "Linked constraint",
        "The public builder must remain local. This sentence is not shown.",
    );
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;
    let diagnostics = diagnostics(&published);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0]["message"],
        json!("constrained by \"Linked constraint\": The public builder must remain local.")
    );
    assert_eq!(
        diagnostics[0]["code"],
        json!(constraint.rsplit('/').next().unwrap()[..8].to_string())
    );
    assert!(diagnostics[0]["codeDescription"]["href"]
        .as_str()
        .expect("workbench href")
        .contains("/#/constraints/"));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn no_records_publishes_empty() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-empty-root", "    build_server();");
    let data_dir = fresh_dir("diag-empty-data");
    let state = Arc::new(state_with_substrate("diag-empty-state", 4));
    mint_public_entities(&state);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;

    assert_eq!(published["params"]["diagnostics"], json!([]));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn uncovered_file_publishes_empty() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-uncovered-root", "    build_server();");
    let ui = repo_root.join("ui/src");
    std::fs::create_dir_all(&ui)?;
    std::fs::write(ui.join("App.tsx"), "export function App() {}\n")?;
    let data_dir = fresh_dir("diag-uncovered-data");
    let state = Arc::new(state_with_substrate("diag-uncovered-state", 4));
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&ui.join("App.tsx"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;

    assert_eq!(published["params"]["diagnostics"], json!([]));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn disable_flags_respected() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-disable-root", "    build_server();");
    let data_dir = fresh_dir("diag-disable-data");
    let state = Arc::new(state_with_substrate("diag-disable-state", 4));
    let constraint = record(&state, "Constraint", "Disabled constraint");
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client
        .initialize(
            true,
            json!({ "diagnostics": { "constraints": false, "staleRationale": false } }),
        )
        .await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;

    assert_eq!(published["params"]["diagnostics"], json!([]));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn stale_rationale_hint_from_git() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");

    let old = DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let old_diagnostics = git_diagnostics_for_record("diag-stale-old", old).await?;
    assert_eq!(
        old_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["severity"] == json!(4))
            .count(),
        1
    );
    assert!(old_diagnostics.iter().any(|diagnostic| {
        diagnostic["message"] == json!("rationale predates later changes to this file")
    }));
    assert_severity_ceiling(&old_diagnostics);

    let future = DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let future_diagnostics = git_diagnostics_for_record("diag-stale-future", future).await?;
    assert_eq!(
        future_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["severity"] == json!(4))
            .count(),
        0
    );
    assert!(future_diagnostics
        .iter()
        .any(|diagnostic| diagnostic["severity"] == json!(3)));
    assert_severity_ceiling(&future_diagnostics);
    Ok(())
}

async fn git_diagnostics_for_record(tag: &str, when: DateTime<Utc>) -> anyhow::Result<Vec<Value>> {
    let repo_root = synthetic_repo_root(&format!("{tag}-root"), "    build_server();");
    init_git_repo(&repo_root)?;
    let data_dir = fresh_dir(&format!("{tag}-data"));
    let state = Arc::new(state_with_substrate(&format!("{tag}-state"), 4));
    let constraint = record_at(&state, "Constraint", "Git stale constraint", when);
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    client.did_save(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;
    let diagnostics = diagnostics(&published);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(diagnostics)
}

fn init_git_repo(repo_root: &Path) -> anyhow::Result<()> {
    run_git(repo_root, &["init"])?;
    run_git(repo_root, &["config", "user.email", "tester@example.com"])?;
    run_git(repo_root, &["config", "user.name", "Test User"])?;
    run_git(repo_root, &["add", "src/runtime.rs"])?;
    run_git(repo_root, &["commit", "-m", "initial"])?;
    Ok(())
}

fn run_git(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[tokio::test]
async fn utf16_range_conversion() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("diag-utf16-root", "é   build_server();");
    let data_dir = fresh_dir("diag-utf16-data");
    let state = Arc::new(state_with_substrate("diag-utf16-state", 5));
    let constraint = record(&state, "Constraint", "UTF16 constraint");
    link_public(&state, &constraint, 5);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(false, json!(null)).await?;
    client.did_open(&uri).await?;
    let published = client.read_until_diagnostics(&uri).await?;
    let diagnostics = diagnostics(&published);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0]["range"]["start"],
        json!({ "line": 7, "character": 4 })
    );
    assert_eq!(
        diagnostics[0]["range"]["end"],
        json!({ "line": 7, "character": 16 })
    );
    assert_severity_ceiling(&diagnostics);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn repo_substrate_diagnostics_when_index_present() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let substrate_data_dir = repo_root.join(".moosedev");
    let meta_path = substrate_data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping LSP diagnostics substrate integration: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }

    let substrate = Substrate::load(&substrate_data_dir, repo_root)?;
    let state = Arc::new(bootstrap("repo-substrate-diagnostics"));
    state.set_substrate(Arc::new(substrate));
    seed_component(&state, "source component", "src/");
    let scratch = record(&state, "Constraint", "Scratch build_server LSP diagnostic");
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
        "constrains",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line,
            col,
        },
        "tester",
    )?;

    let data_dir = fresh_dir("repo-substrate-diagnostics-data");
    let socket = spawn_listener(state, &data_dir, repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    let runtime_uri = file_uri(&repo_root.join("src/runtime.rs"));
    client.did_open(&runtime_uri).await?;
    let linked = diagnostics(&client.read_until_diagnostics(&runtime_uri).await?);
    assert!(linked.iter().any(|diagnostic| {
        diagnostic["severity"] == json!(3)
            && diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("Scratch build_server LSP diagnostic"))
    }));
    assert!(!linked
        .iter()
        .any(|diagnostic| diagnostic["severity"] == json!(4)
            && diagnostic["message"] == json!("rationale predates later changes to this file")));
    assert_severity_ceiling(&linked);

    let lib_uri = file_uri(&repo_root.join("src/lib.rs"));
    client.did_open(&lib_uri).await?;
    let empty = client.read_until_diagnostics(&lib_uri).await?;
    assert_eq!(empty["params"]["diagnostics"], json!([]));

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}

#[tokio::test]
async fn code_lens_shows_counts_and_hotspots() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("lens-root", "    build_server();");
    let data_dir = fresh_dir("lens-data");
    let state = Arc::new(state_with_substrate("lens-state", 4));
    // build_server (public) gets a linked constraint; no_records (public) stays bare.
    let constraint = record(&state, "Constraint", "Runtime builder must stay local");
    link_public(&state, &constraint, 4);
    let socket = spawn_listener(state, &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));

    let mut client = direct_client(&socket).await?;
    client.initialize(true, json!(null)).await?;
    let lenses = client.code_lens(&uri).await?;

    let titles: Vec<String> = lenses
        .iter()
        .filter_map(|lens| lens["command"]["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("constraint")),
        "documented public entity should carry a record-count badge: {titles:?}"
    );
    assert!(
        titles.iter().any(|t| t.contains("no linked rationale")),
        "undocumented public entity should carry a hotspot badge: {titles:?}"
    );

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}
