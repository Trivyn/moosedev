//! Knowledge-LSP codeAction menu tests (v2.3 write path, read-only side):
//! the lightbulb menu IS the picker — link candidates + judgment proposals as
//! fully-formed quickfix actions — with hover's silence discipline and
//! `context.only` kind filtering.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let short_tag = tag.chars().take(8).collect::<String>();
    let dir = Path::new("/private/tmp").join(format!(
        "mlca-{}-{}-{}",
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

fn link_public(state: &AppState, record_iri: &str) {
    graph::link_code(
        state,
        record_iri,
        "concerns",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line: 8,
            col: 5,
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

fn public_entity_iri(state: &AppState) -> String {
    let terms = graph::CodeTerms::resolve(state).expect("code terms");
    let normalized = "rust-analyzer cargo moosedev . runtime/build_server().";
    graph::entities_by_symbol(state, &terms)
        .expect("entities by symbol")
        .get(normalized)
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

    async fn initialize(&mut self) -> anyhow::Result<()> {
        let response = self
            .request(
                1,
                "initialize",
                json!({
                    "processId": null,
                    "rootUri": null,
                    "capabilities": { "general": { "positionEncodings": ["utf-8"] } },
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

    async fn code_action(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        only: Option<Vec<&str>>,
    ) -> anyhow::Result<Vec<Value>> {
        let mut context = json!({ "diagnostics": [] });
        if let Some(only) = only {
            context["only"] = json!(only);
        }
        let response = self
            .request(
                55,
                "textDocument/codeAction",
                json!({
                    "textDocument": { "uri": uri },
                    "range": {
                        "start": { "line": line, "character": character },
                        "end": { "line": line, "character": character }
                    },
                    "context": context
                }),
            )
            .await?;
        assert!(
            response.get("error").is_none(),
            "codeAction error: {response}"
        );
        Ok(response["result"].as_array().cloned().unwrap_or_default())
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

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn action_titles(actions: &[Value]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| a["title"].as_str().map(str::to_string))
        .collect()
}

fn link_record_iris(actions: &[Value]) -> Vec<String> {
    actions
        .iter()
        .filter(|a| a["command"]["command"] == json!("moosedev.proposeLink"))
        .filter_map(|a| {
            a["command"]["arguments"][0]["recordIri"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn code_actions_offer_links_and_judgments_with_menu_discipline() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("ca-root");
    let data_dir = fresh_dir("ca-data");
    let state = Arc::new(state_with_substrate("ca-state"));

    // A lexically-related record → offered; one already linked to the entity
    // and one with a pending link proposal → filtered out.
    let candidate = record(&state, "Constraint", "build_server timeout contract");
    let linked = record(&state, "Constraint", "build_server allocation rule");
    link_public(&state, &linked); // also lazily mints the entity
    for index in 0..8 {
        let crowded = record(
            &state,
            "Constraint",
            &format!("build_server build_server build_server linked {index}"),
        );
        link_public(&state, &crowded);
    }
    let pending = record(&state, "Lesson", "build_server retry policy");
    graph::propose_link(
        &state,
        &pending,
        "concerns",
        PUBLIC_SYMBOL,
        "src/runtime.rs",
        "prior capture",
        "tester",
        Utc::now(),
    )?;
    mint_public_entities(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));
    let mut client = direct_client(&socket).await?;
    client.initialize().await?;

    // The lightbulb on build_server (line 8, inside its occurrence).
    let actions = client.code_action(&uri, 7, 5, None).await?;
    let titles = action_titles(&actions);
    let offered = link_record_iris(&actions);
    assert!(
        offered.contains(&candidate),
        "lexically-related record offered: {titles:?}"
    );
    assert!(
        !offered.contains(&linked),
        "already-linked record never re-offered: {titles:?}"
    );
    assert!(
        !offered.contains(&pending),
        "record with a pending link proposal not re-offered: {titles:?}"
    );
    for role in ["core-algorithm", "boundary", "glue"] {
        assert!(
            titles
                .iter()
                .any(|t| t == &format!("Propose role: {role} (build_server)")),
            "role menu offers {role}: {titles:?}"
        );
    }
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("Propose criticality: high")),
        "criticality menu offers high: {titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t.contains("criticality: standard")),
        "implicit default never offered: {titles:?}"
    );
    for action in &actions {
        assert_eq!(action["kind"], json!("quickfix"), "quickfix kind: {action}");
    }

    // A live judgment on an axis suppresses that whole axis.
    graph::ensure_taxonomy_individuals(&state)?;
    graph::propose_judgment(
        &state,
        &public_entity_iri(&state),
        "playsRole",
        &graph::role_iri("boundary"),
        0.75,
        graph::AUTO_HELD,
        "R2 boundary",
        "moosedev-classifier",
        Utc::now(),
    )?;
    let actions = client.code_action(&uri, 7, 5, None).await?;
    let titles = action_titles(&actions);
    assert!(
        !titles.iter().any(|t| t.starts_with("Propose role:")),
        "pending judgment suppresses the role axis: {titles:?}"
    );
    assert!(
        titles.iter().any(|t| t.starts_with("Propose criticality:")),
        "untouched axis still offered: {titles:?}"
    );

    // `context.only` kind filtering: quickfix admitted, others filtered.
    let filtered = client
        .code_action(&uri, 7, 5, Some(vec!["refactor"]))
        .await?;
    assert!(filtered.is_empty(), "non-quickfix `only` filter → empty");
    let quickfix = client
        .code_action(&uri, 7, 5, Some(vec!["quickfix"]))
        .await?;
    assert!(!quickfix.is_empty(), "quickfix `only` filter keeps actions");

    // Silence discipline: an unresolved position and a foreign file get no menu.
    let silent = client.code_action(&uri, 4, 0, None).await?;
    assert!(silent.is_empty(), "unresolved position → no actions");
    let foreign = client
        .code_action("file:///tmp/elsewhere.rs", 0, 0, None)
        .await?;
    assert!(foreign.is_empty(), "file outside the repo → no actions");

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn link_candidates_are_capped_at_four() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("cap-root");
    let data_dir = fresh_dir("cap-data");
    let state = Arc::new(state_with_substrate("cap-state"));
    for i in 0..6 {
        record(
            &state,
            "ArchitecturalDecision",
            &format!("build_server decision {i}"),
        );
    }

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));
    let mut client = direct_client(&socket).await?;
    client.initialize().await?;

    let actions = client.code_action(&uri, 7, 5, None).await?;
    let links = link_record_iris(&actions);
    assert!(
        !links.is_empty() && links.len() <= 4,
        "candidate menu is bounded (got {})",
        links.len()
    );

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}
