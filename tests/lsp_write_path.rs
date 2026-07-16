//! v2.3 acceptance (spec §7): the editor write path.
//!
//! A code-action proposal round-trips editor → ratification queue → workbench
//! ratify → visible in the next hover; executeCommand validates its arguments
//! and is idempotent; and a source scan proves no graph-write call path
//! originates in `src/lsp/` except queue submission.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, ProposalKind, RecordInput, PROJECT_KG_GRAPH_IRI};
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

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let short_tag = tag.chars().take(8).collect::<String>();
    let dir = Path::new("/private/tmp").join(format!(
        "mlwp-{}-{}-{}",
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

    async fn initialize(&mut self, client_name: Option<&str>) -> anyhow::Result<()> {
        let mut params = json!({
            "processId": null,
            "rootUri": null,
            "capabilities": { "general": { "positionEncodings": ["utf-8"] } },
            "initializationOptions": null
        });
        if let Some(name) = client_name {
            params["clientInfo"] = json!({ "name": name, "version": "0.11.0" });
        }
        let response = self.request(1, "initialize", params).await?;
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
    ) -> anyhow::Result<Vec<Value>> {
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
                    "context": { "diagnostics": [] }
                }),
            )
            .await?;
        Ok(response["result"].as_array().cloned().unwrap_or_default())
    }

    async fn execute_command(&mut self, command: &str, argument: Value) -> anyhow::Result<Value> {
        self.request(
            66,
            "workspace/executeCommand",
            json!({ "command": command, "arguments": [argument] }),
        )
        .await
    }

    async fn hover_markdown(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> anyhow::Result<Option<String>> {
        let response = self
            .request(
                44,
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character }
                }),
            )
            .await?;
        Ok(response["result"]["contents"]["value"]
            .as_str()
            .map(str::to_string))
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

#[tokio::test]
async fn code_action_proposal_round_trips_editor_to_hover() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("rt-root");
    let data_dir = fresh_dir("rt-data");
    let state = Arc::new(state_with_substrate("rt-state"));
    let candidate = record(&state, "Constraint", "build_server timeout contract");
    mint_public_entities(&state);
    graph::ensure_taxonomy_individuals(&state)?;

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let uri = file_uri(&repo_root.join("src/runtime.rs"));
    let mut client = direct_client(&socket).await?;
    client.initialize(Some("Neovim")).await?;

    // 1. The lightbulb offers the candidate; take its command verbatim.
    let actions = client.code_action(&uri, 7, 5).await?;
    let link = actions
        .iter()
        .find(|a| {
            a["command"]["command"] == json!("moosedev.proposeLink")
                && a["command"]["arguments"][0]["recordIri"] == json!(candidate.as_str())
        })
        .expect("candidate link action offered")
        .clone();
    assert_eq!(
        link["command"]["arguments"][0]["targetSymbol"],
        json!(NORMALIZED_SYMBOL),
        "the editor command carries stable proposal identity"
    );

    // 2. executeCommand files the proposal — and only a proposal: no edge yet,
    //    hover unchanged (advisory-until-ratified).
    let response = client
        .execute_command(
            "moosedev.proposeLink",
            link["command"]["arguments"][0].clone(),
        )
        .await?;
    let proposal_iri = response["result"]["proposalIri"]
        .as_str()
        .expect("proposalIri returned")
        .to_string();
    let pending = graph::list_proposals(&state, Some("proposed"))?;
    let entry = pending
        .iter()
        .find(|p| p.iri == proposal_iri)
        .expect("proposal sits in the queue");
    assert_eq!(entry.kind, ProposalKind::Link);
    assert_eq!(entry.subject_iri, candidate);
    assert_eq!(entry.target_symbol, NORMALIZED_SYMBOL);
    let hover = client.hover_markdown(&uri, 7, 5).await?;
    assert!(
        !hover.unwrap_or_default().contains("timeout contract"),
        "proposed links stay invisible to hover"
    );

    // 3. Repeating the command is idempotent — the pending twin is returned.
    let repeat = client
        .execute_command(
            "moosedev.proposeLink",
            link["command"]["arguments"][0].clone(),
        )
        .await?;
    assert_eq!(
        repeat["result"]["proposalIri"],
        json!(proposal_iri.as_str())
    );

    // 4. Workbench ratifies (simulated via the same graph call it uses) →
    //    the edge materializes and the very next hover shows the record.
    graph::accept_proposal(&state, &proposal_iri, "james")?;
    let hover = client
        .hover_markdown(&uri, 7, 5)
        .await?
        .expect("hover speaks after ratification");
    assert!(
        hover.contains("build_server timeout contract"),
        "ratified link visible in next hover: {hover}"
    );

    // 5. The judgment path round-trips the same way, with editor authorship.
    let entity = public_entity_iri(&state);
    let judged = client
        .execute_command(
            "moosedev.proposeJudgment",
            json!({ "entityIri": entity, "predicate": "playsRole", "targetLocal": "boundary" }),
        )
        .await?;
    let judgment_iri = judged["result"]["proposalIri"]
        .as_str()
        .expect("judgment proposalIri")
        .to_string();
    graph::accept_proposal(&state, &judgment_iri, "james")?;
    let judgments = graph::judgments_for_entity(&state, &entity)?;
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].target_local, "boundary");
    assert_eq!(
        judgments[0].author, "editor:neovim",
        "authorship names the editor surface"
    );
    let hover = client.hover_markdown(&uri, 7, 5).await?.unwrap_or_default();
    assert!(
        hover.contains("role: boundary"),
        "ratified judgment visible in next hover: {hover}"
    );

    // Editor proposals never nudge as judgments; the link was ratified, so
    // nothing is pending at all.
    assert_eq!(graph::pending_count(&state)?, 0);

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

#[tokio::test]
async fn execute_command_refuses_malformed_and_side_door_requests() -> anyhow::Result<()> {
    let _guard = ENV_LOCK.lock().await;
    let _restore = EnvRestore::remove("MOOSEDEV_NO_LSP");
    let repo_root = synthetic_repo_root("err-root");
    let data_dir = fresh_dir("err-data");
    let state = Arc::new(state_with_substrate("err-state"));
    let candidate = record(&state, "Constraint", "build_server timeout contract");
    mint_public_entities(&state);
    let entity = public_entity_iri(&state);

    let socket = spawn_listener(state.clone(), &data_dir, &repo_root).await?;
    let mut client = direct_client(&socket).await?;
    client.initialize(None).await?;

    let invalid_params = json!(-32602);
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "moosedev.proposeLink",
            json!({ "recordIri": "not an iri", "predicate": "concerns",
                    "targetSymbol": PUBLIC_SYMBOL, "targetPath": "src/runtime.rs" }),
            "malformed record IRI",
        ),
        (
            "moosedev.proposeLink",
            json!({ "recordIri": entity.as_str(), "predicate": "concerns",
                    "targetSymbol": PUBLIC_SYMBOL, "targetPath": "src/runtime.rs" }),
            "a code entity is not an InformationRecord subject",
        ),
        (
            "moosedev.proposeLink",
            json!({ "recordIri": candidate.as_str(), "predicate": "constrains",
                    "targetSymbol": PUBLIC_SYMBOL, "targetPath": "src/runtime.rs" }),
            "off-whitelist predicate",
        ),
        (
            "moosedev.proposeLink",
            json!({ "recordIri": candidate.as_str(), "predicate": "concerns",
                    "targetSymbol": "rust-analyzer cargo moosedev 0.6.3 missing/nowhere().",
                    "targetPath": "src/runtime.rs" }),
            "symbol without a workspace definition would queue an unratifiable proposal",
        ),
        (
            "moosedev.proposeLink",
            json!({ "recordIri": candidate.as_str(), "predicate": "concerns",
                    "targetSymbol": PUBLIC_SYMBOL, "targetPath": "src/elsewhere.rs" }),
            "path contradicting the symbol's defining file",
        ),
        (
            "moosedev.proposeJudgment",
            json!({ "entityIri": entity.as_str(), "predicate": "playsRole",
                    "targetLocal": "supreme-leader" }),
            "off-taxonomy role",
        ),
        (
            "moosedev.proposeJudgment",
            json!({ "entityIri": entity.as_str(), "predicate": "hasCriticality",
                    "targetLocal": "standard" }),
            "implicit default is not proposable",
        ),
        (
            "moosedev.proposeJudgment",
            json!({ "entityIri": "https://moosedev.dev/kg/CodeEntity/nonexistent",
                    "predicate": "playsRole", "targetLocal": "boundary" }),
            "unminted entity",
        ),
        (
            "moosedev.proposeJudgment",
            json!({ "entityIri": entity.as_str(), "predicate": "concerns",
                    "targetLocal": "boundary" }),
            "judgment predicates only",
        ),
        ("moosedev.acceptProposal", json!({}), "unknown command"),
    ];
    for (command, argument, label) in cases {
        let response = client.execute_command(command, argument).await?;
        assert_eq!(
            response["error"]["code"], invalid_params,
            "{label}: expected InvalidParams, got {response}"
        );
    }

    // Nothing above reached the queue.
    assert!(graph::list_proposals(&state, Some("proposed"))?.is_empty());

    client.shutdown_and_exit().await?;
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&repo_root);
    Ok(())
}

/// Spec §7 grep-proof, executable: no graph-write call path originates in
/// `src/lsp/` except queue submission (`propose_link` / `propose_judgment`).
/// A future legitimate write helper must be added to the allowlist here — that
/// friction is the point.
#[test]
fn lsp_source_has_no_graph_write_call_path_except_queue_submission() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lsp/mod.rs"))
            .expect("read src/lsp/mod.rs");

    let denylist = [
        "link_code(",
        "relate(",
        "accept_proposal",
        "reject_proposal",
        "recategorize_judgment",
        "record_instance",
        "capture_decision_point",
        "ensure_entity",
        "ensure_taxonomy_individuals",
        "mint_instance_iri",
        "apply_mint",
        "set_status",
        "supersede",
        "retract_decision",
        "start_transaction",
        "txn.insert",
        "store.insert",
        "remove_quad",
    ];
    for token in denylist {
        assert!(
            !source.contains(token),
            "graph-write token {token:?} found in src/lsp/mod.rs — the LSP write \
             path may only submit queue proposals (Constraint 2ba76439)"
        );
    }
    for required in ["propose_link(", "propose_judgment("] {
        assert!(
            source.contains(required),
            "queue submission {required:?} missing from src/lsp/mod.rs"
        );
    }
}
