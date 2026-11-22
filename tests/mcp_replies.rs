//! MCP reply semantics for covered, uncovered, and unavailable code substrates.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::AppState;
use moosedev::mcp::MooseDevServer;
use moosedev::runtime;
use protobuf::{EnumOrUnknown, MessageField};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::{Peer, RoleClient, ServiceExt};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};
use serde_json::{json, Value};
use tokio::net::UnixStream;

const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
const PLAIN_REPLY: &str =
    "No recorded knowledge is linked to this code; attach records with `link_code`.";

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-mcp-replies-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn build_server(data_dir: &Path, with_substrate: bool) -> MooseDevServer {
    build_server_minted(data_dir, with_substrate, false)
}

/// `minted` mints the substrate's CodeEntities before serving. A store with a
/// substrate but nothing minted is a real and separate state (Requirement
/// a0581252) that answers every dossier with the mint hint, so a test about
/// anything else has to mint first or it only ever exercises that one reply.
fn build_server_minted(data_dir: &Path, with_substrate: bool, minted: bool) -> MooseDevServer {
    let state = AppState::bootstrap(data_dir, &ontology_dir()).expect("bootstrap app state");
    if with_substrate {
        state.set_substrate(Arc::new(synthetic_substrate()));
    }
    if minted {
        let substrate = state.substrate().expect("substrate required to mint");
        let terms = moosedev::graph::CodeTerms::resolve(&state).expect("code terms");
        let components = moosedev::graph::load_components(&state).expect("components");
        let plan = moosedev::graph::plan_mint(
            &state,
            &substrate.definitions(),
            &terms,
            &components,
            Some(&substrate),
        )
        .expect("plan mint");
        moosedev::graph::apply_mint(&state, &plan, &terms).expect("apply mint");
    }
    MooseDevServer::new(Arc::new(state))
}

async fn spawn_backend(server: MooseDevServer, socket: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = runtime::serve_unix(server, &socket).await {
            eprintln!("backend exited: {error}");
        }
    })
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("backend socket {} never became ready", socket.display());
}

async fn connect_client(socket: &Path) -> rmcp::service::RunningService<RoleClient, ()> {
    let stream = UnixStream::connect(socket)
        .await
        .expect("connect to backend socket");
    ().serve(stream).await.expect("client MCP handshake")
}

async fn call_raw(peer: &Peer<RoleClient>, name: &str, args: Value) -> CallToolResult {
    let arguments = args.as_object().cloned().unwrap_or_default();
    peer.call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("call_tool {name} failed: {error}"))
}

fn response_text(result: &CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .expect("text tool response")
}

fn recorded_iri(result: &CallToolResult) -> String {
    response_text(result)
        .split_once(" → ")
        .expect("record reply contains arrow")
        .1
        .split_whitespace()
        .next()
        .expect("record reply contains IRI")
        .to_string()
}

#[tokio::test]
async fn dossier_and_link_code_distinguish_substrate_coverage() {
    let data_dir = fresh_data_dir("indexed");
    let socket = runtime::socket_path_for(&data_dir);
    // Minted, so the store-wide unminted rung stays out of the way and each
    // reply below reflects substrate COVERAGE, which is what this test is about.
    let backend = spawn_backend(build_server_minted(&data_dir, true, true), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    let covered = call_raw(
        &client,
        "get_entity_dossier",
        json!({"file": "src/runtime.rs", "line": 8, "col": 5}),
    )
    .await;
    assert_ne!(covered.is_error, Some(true));
    assert_eq!(response_text(&covered), PLAIN_REPLY);

    let uncovered = call_raw(
        &client,
        "get_entity_dossier",
        json!({"file": "ui/src/App.tsx", "line": 1, "col": 1}),
    )
    .await;
    assert_ne!(uncovered.is_error, Some(true));
    assert!(response_text(&uncovered).contains("`ui/src/App.tsx` is not in the code substrate"));
    assert!(response_text(&uncovered).contains("rust-analyzer 1 docs"));

    let link = call_raw(
        &client,
        "link_code",
        json!({
            "record_iri": "urn:test:any-record",
            "file": "ui/src/App.tsx",
            "line": 1,
            "col": 1
        }),
    )
    .await;
    assert_eq!(link.is_error, Some(true));
    assert!(response_text(&link).contains("is not in the code substrate"));

    let symbol = call_raw(
        &client,
        "get_entity_dossier",
        json!({"symbol": "rust-analyzer cargo moosedev 0.6.3 missing/unknown()."}),
    )
    .await;
    assert_ne!(symbol.is_error, Some(true));
    assert_eq!(response_text(&symbol), PLAIN_REPLY);

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// The unminted-store rung sits between the substrate rungs and the plain reply
/// (Requirement a0581252): it must fire for every selector, but never ahead of
/// the coverage discrimination AD a8f95059 exists to provide.
#[tokio::test]
async fn unminted_store_reports_mint_without_masking_coverage() {
    let data_dir = fresh_data_dir("unminted");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, true), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    // Anchorable position, and by symbol: both reach the new rung.
    for args in [
        json!({"file": "src/runtime.rs", "line": 8, "col": 5}),
        json!({"symbol": PUBLIC_SYMBOL}),
    ] {
        let reply = call_raw(&client, "get_entity_dossier", args.clone()).await;
        assert_ne!(reply.is_error, Some(true));
        let text = response_text(&reply);
        assert!(text.contains("mint --apply"), "{args}: {text}");
        assert_ne!(text, PLAIN_REPLY, "{args}");
    }

    // An uncovered file still gets its coverage report, NOT the mint hint —
    // sending someone to `mint` when the file was never indexed is wrong advice.
    let uncovered = call_raw(
        &client,
        "get_entity_dossier",
        json!({"file": "ui/src/App.tsx", "line": 1, "col": 1}),
    )
    .await;
    let text = response_text(&uncovered);
    assert!(text.contains("is not in the code substrate"), "{text}");
    assert!(!text.contains("mint --apply"), "{text}");

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn evaluate_policy_tool_returns_verdict_json() {
    let data_dir = fresh_data_dir("policy");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, true), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    // Unconstrained edit → typed Allow verdict as JSON.
    let allow = call_raw(
        &client,
        "evaluate_policy",
        json!({
            "host": "test-mcp",
            "event": "edit_proposed",
            "file": "src/runtime.rs",
        }),
    )
    .await;
    assert_ne!(allow.is_error, Some(true));
    let verdict: Value = serde_json::from_str(response_text(&allow)).expect("verdict is JSON");
    assert_eq!(verdict["decision"], "allow");

    // Unknown event kind → honest tool error, not a crash.
    let bad = call_raw(
        &client,
        "evaluate_policy",
        json!({"event": "telepathy", "file": "src/runtime.rs"}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));
    assert!(response_text(&bad).contains("unknown event kind"));

    // Missing file for a gate event → honest tool error.
    let missing = call_raw(
        &client,
        "evaluate_policy",
        json!({"event": "edit_proposed"}),
    )
    .await;
    assert_eq!(missing.is_error, Some(true));
    assert!(response_text(&missing).contains("requires `file`"));

    // Judgment predicates cannot bypass the ratification queue via relate: a
    // bare edge would carry no provenance and be invisible to badges + gate.
    for predicate in ["playsRole", "hasCriticality"] {
        let bypass = call_raw(
            &client,
            "relate",
            json!({
                "subject_iri": "https://moosedev.dev/kg/CodeEntity/any",
                "predicate": predicate,
                "object_iri": "https://moosedev.dev/kg/CodeRole/boundary"
            }),
        )
        .await;
        assert_eq!(bypass.is_error, Some(true));
        assert!(response_text(&bypass).contains("ratification-only"));
    }

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn supersede_reasserts_selected_relations_and_reports_the_rest() {
    let data_dir = fresh_data_dir("supersede-relations");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, false), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    let retained_requirement = call_raw(
        &client,
        "record_important_decision",
        json!({
            "kind": "Requirement",
            "title": "Retained supersession test requirement",
            "description": "The replacement still satisfies this requirement."
        }),
    )
    .await;
    assert_ne!(retained_requirement.is_error, Some(true));
    let retained_iri = recorded_iri(&retained_requirement);

    let omitted_requirement = call_raw(
        &client,
        "record_important_decision",
        json!({
            "kind": "Requirement",
            "title": "Omitted supersession test requirement",
            "description": "The replacement deliberately does not reassert this requirement."
        }),
    )
    .await;
    assert_ne!(omitted_requirement.is_error, Some(true));
    let omitted_iri = recorded_iri(&omitted_requirement);

    let original = call_raw(
        &client,
        "record_important_decision",
        json!({
            "title": "Original supersession relation test decision",
            "description": "An original decision with two semantic links.",
            "relations": [
                {"predicate": "isMotivatedBy", "target": retained_iri},
                {"predicate": "isMotivatedBy", "target": omitted_iri}
            ]
        }),
    )
    .await;
    assert_ne!(original.is_error, Some(true));
    let original_iri = recorded_iri(&original);

    let replacement = call_raw(
        &client,
        "supersede_decision",
        json!({
            "superseded_iri": original_iri,
            "title": "Replacement supersession relation test decision",
            "description": "A replacement that explicitly reasserts only one link.",
            "rationale": "The second requirement no longer applies.",
            "relations": [
                {"predicate": "isMotivatedBy", "target": retained_iri}
            ]
        }),
    )
    .await;
    assert_ne!(replacement.is_error, Some(true));
    let text = response_text(&replacement);
    assert!(
        text.starts_with(&format!("Superseded {original_iri} → ")),
        "{text}"
    );
    assert!(
        text.contains(&format!("Linked: isMotivatedBy → {retained_iri}")),
        "{text}"
    );
    assert!(text.contains("Not carried:"), "{text}");
    assert!(
        text.contains(&format!(
            "{{\"predicate\":\"isMotivatedBy\",\"target\":\"{omitted_iri}\"}}"
        )),
        "{text}"
    );

    let constraint = call_raw(
        &client,
        "record_important_decision",
        json!({
            "kind": "Constraint",
            "title": "Original ratified supersession constraint",
            "description": "The original constraint stays authoritative during review."
        }),
    )
    .await;
    assert_ne!(constraint.is_error, Some(true));
    let constraint_iri = recorded_iri(&constraint);
    let proposed = call_raw(
        &client,
        "supersede_decision",
        json!({
            "superseded_iri": constraint_iri,
            "title": "Replacement ratified supersession constraint",
            "description": "The replacement applies only after ratification.",
            "rationale": "The implementation scope changed.",
            "reason": "scope-narrowed",
            "relations": [
                {"predicate": "constrains", "target": original_iri}
            ]
        }),
    )
    .await;
    assert_ne!(proposed.is_error, Some(true));
    let proposed_text = response_text(&proposed);
    assert!(
        proposed_text.starts_with("Proposed supersession"),
        "{proposed_text}"
    );
    assert!(
        proposed_text.contains(&format!("Linked: constrains → {original_iri}")),
        "{proposed_text}"
    );
    assert!(
        proposed_text.contains("Reason: scope-narrowed"),
        "{proposed_text}"
    );

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn dossier_position_reports_unavailable_substrate() {
    let data_dir = fresh_data_dir("unavailable");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, false), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    let result = call_raw(
        &client,
        "get_entity_dossier",
        json!({"file": "src/runtime.rs", "line": 8, "col": 5}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    assert!(response_text(&result).contains("code substrate unavailable"));

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();
    let mut document = Document::new();
    document.relative_path = "src/runtime.rs".to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);

    let mut info = SymbolInformation::new();
    info.symbol = PUBLIC_SYMBOL.to_string();
    info.display_name = "build_server".to_string();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut signature = Signature::new();
    signature.text = "pub fn build_server()".to_string();
    info.signature_documentation = MessageField::some(signature);
    document.symbols.push(info);

    let mut occurrence = Occurrence::new();
    occurrence.symbol = PUBLIC_SYMBOL.to_string();
    occurrence.range = vec![7, 4, 16];
    occurrence.symbol_roles = 1;
    occurrence.enclosing_range = vec![0, 0, 30, 0];
    document.occurrences.push(occurrence);
    index.documents.push(document);

    let meta = SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        1,
        1,
    );
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
}

#[tokio::test]
async fn sparql_reply_keeps_result_text_parseable_and_sends_the_note_separately() {
    let data_dir = fresh_data_dir("sparql");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, false), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    // moose-mcp's own text is a 20-row preview; moosedev's reply is the full
    // result, one content block, and never structuredContent.
    let rows = call_raw(
        &client,
        "sparql",
        json!({"query": "SELECT ?c WHERE { ?c a <http://www.w3.org/2002/07/owl#Class> }"}),
    )
    .await;
    assert_ne!(rows.is_error, Some(true));
    assert_eq!(rows.content.len(), 1);
    assert!(rows.structured_content.is_none());
    let json: Value = serde_json::from_str(response_text(&rows)).expect("SPARQL JSON");
    assert!(json["results"]["bindings"].as_array().unwrap().len() > 20);

    // An unknown term: the note is a second block, the first stays JSON.
    let empty = call_raw(
        &client,
        "sparql",
        json!({"query": "SELECT ?s WHERE { ?s <https://moosedev.dev/kg/ontology/software-architecture#hasNoSuchThing> ?o }"}),
    )
    .await;
    assert_ne!(empty.is_error, Some(true));
    assert_eq!(empty.content.len(), 2, "{:?}", empty.content);
    serde_json::from_str::<Value>(response_text(&empty)).expect("first block is SPARQL JSON");
    let note = empty.content[1].as_text().expect("note text");
    assert!(note.text.contains("hasNoSuchThing"), "{}", note.text);

    // A rejection is a tool error naming the alternative.
    let from = call_raw(
        &client,
        "sparql",
        json!({"query": "SELECT ?s FROM <https://moosedev.dev/kg/project> WHERE { ?s ?p ?o }"}),
    )
    .await;
    assert_eq!(from.is_error, Some(true));
    assert!(response_text(&from).contains("GRAPH"));

    client.cancel().await.ok();
    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn served_sparql_description_names_the_loaded_term_namespaces() {
    let data_dir = fresh_data_dir("sparql-namespaces");
    let socket = runtime::socket_path_for(&data_dir);
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    let decision = state.resolve_class("ArchitecturalDecision").unwrap();
    let expected = moosedev::sparql::term_namespaces(&state.store);
    let backend = spawn_backend(MooseDevServer::new(Arc::new(state)), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    let tools = client.list_all_tools().await.expect("list tools");
    let sparql = tools
        .iter()
        .find(|tool| tool.name == "sparql")
        .expect("sparql tool");
    let description = sparql.description.as_deref().unwrap_or_default();
    assert!(!expected.is_empty());
    for ns in &expected {
        assert!(
            description.contains(&format!("<{}>", ns.namespace)),
            "{description}"
        );
    }

    // A query written from the advertised namespace finds the class.
    let arch = expected
        .iter()
        .find(|ns| decision.starts_with(&ns.namespace))
        .unwrap();
    let reply = call_raw(
        &client,
        "sparql",
        json!({"query": format!(
            "PREFIX a: <{}> ASK {{ a:ArchitecturalDecision ?p ?o }}",
            arch.namespace
        )}),
    )
    .await;
    let json: Value = serde_json::from_str(response_text(&reply)).unwrap();
    assert_eq!(json["boolean"], true);

    client.cancel().await.ok();
    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

fn seed(state: &AppState, kind: &str, title: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    moosedev::graph::record_instance(
        state,
        &moosedev::graph::RecordInput {
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
    .expect("seed record")
}

/// Components `ingest files` (src/, shared/) and `pipeline` (lib/, shared/),
/// one Constraint and one Decision on the first, served over a socket.
struct LifecycleFixture {
    state: Arc<AppState>,
    a: String,
    b: String,
    constraint: String,
    decision: String,
    client: rmcp::service::RunningService<RoleClient, ()>,
    backend: tokio::task::JoinHandle<()>,
    data_dir: PathBuf,
}

async fn lifecycle_fixture(tag: &str) -> LifecycleFixture {
    let data_dir = fresh_data_dir(tag);
    let state = Arc::new(AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap"));
    let a = seed(&state, "SystemComponent", "ingest files");
    let b = seed(&state, "SystemComponent", "pipeline");
    moosedev::graph::declare_component_paths(
        &state,
        &a,
        &["src/".to_string(), "shared/".to_string()],
    )
    .unwrap();
    moosedev::graph::declare_component_paths(
        &state,
        &b,
        &["lib/".to_string(), "shared/".to_string()],
    )
    .unwrap();
    let constraint = seed(&state, "Constraint", "Ingest must stay streaming");
    moosedev::graph::relate(&state, &constraint, "constrains", &a).unwrap();
    let decision = seed(&state, "ArchitecturalDecision", "Ingest decision");
    moosedev::graph::relate(&state, &decision, "concerns", &a).unwrap();
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(MooseDevServer::new(state.clone()), socket.clone()).await;
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;
    LifecycleFixture {
        state,
        a,
        b,
        constraint,
        decision,
        client,
        backend,
        data_dir,
    }
}

impl LifecycleFixture {
    async fn finish(self) {
        self.client.cancel().await.ok();
        self.backend.abort();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

fn status_of(state: &AppState, iri: &str) -> Vec<String> {
    let status = state.capture.status.clone();
    let graph = oxigraph::model::NamedNodeRef::new(moosedev::graph::PROJECT_KG_GRAPH_IRI).unwrap();
    let subject = oxigraph::model::NamedNode::new(iri).unwrap();
    let predicate = oxigraph::model::NamedNode::new(status).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(predicate.as_ref()),
            None,
            Some(oxigraph::model::GraphNameRef::NamedNode(graph)),
        )
        .map(|q| match q.unwrap().object {
            oxigraph::model::Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        })
        .collect()
}

#[tokio::test]
async fn retire_component_dry_run_names_the_constraint_and_writes_nothing() {
    let f = lifecycle_fixture("retire-dry").await;
    let reply = call_raw(
        &f.client,
        "retire_component",
        json!({"component": "ingest files", "into": "pipeline",
               "rationale": "ingest is one stage of the pipeline", "dry_run": true}),
    )
    .await;
    assert_ne!(reply.is_error, Some(true), "{}", response_text(&reply));
    let text = response_text(&reply);
    println!("DRY RUN MERGE REPLY:\n{text}");
    assert!(
        text.starts_with("DRY RUN — nothing written: would merge"),
        "{text}"
    );
    assert!(
        text.contains("Constraints whose reach widens to \"pipeline\" (1):"),
        "{text}"
    );
    assert!(text.contains("\"Ingest must stay streaming\""), "{text}");
    assert!(text.contains(&f.constraint), "{text}");
    assert!(text.contains("Confirm this with the user"), "{text}");
    // Nothing was written: the component is still current.
    assert_eq!(status_of(&f.state, &f.a), vec!["accepted".to_string()]);
    let listing = moosedev::graph::load_components(&f.state).unwrap();
    assert!(
        listing
            .iter()
            .any(|c| c.iri.as_deref() == Some(f.a.as_str())),
        "ingest files left the working set"
    );
    f.finish().await;
}

/// A blank `into` is a malformed merge, never a plain retire.
#[tokio::test]
async fn retire_component_refuses_a_blank_into() {
    let f = lifecycle_fixture("retire-blank-into").await;
    for into in ["", "   "] {
        let reply = call_raw(
            &f.client,
            "retire_component",
            json!({"component": "ingest files", "into": into,
                   "rationale": "ingest is one stage of the pipeline"}),
        )
        .await;
        assert_eq!(reply.is_error, Some(true), "{}", response_text(&reply));
        let text = response_text(&reply);
        assert!(text.contains("`into` must name the component"), "{text}");
        assert!(text.contains("omit `into`"), "{text}");
    }
    assert_eq!(status_of(&f.state, &f.a), vec!["accepted".to_string()]);
    assert!(moosedev::graph::load_components(&f.state)
        .unwrap()
        .iter()
        .any(|c| c.iri.as_deref() == Some(f.a.as_str())));
    f.finish().await;
}

#[tokio::test]
async fn retire_component_merge_reply_reports_moved_links_and_paths() {
    let f = lifecycle_fixture("retire-merge").await;
    let reply = call_raw(
        &f.client,
        "retire_component",
        json!({"component": "ingest files", "into": "pipeline",
               "rationale": "ingest is one stage of the pipeline"}),
    )
    .await;
    assert_ne!(reply.is_error, Some(true), "{}", response_text(&reply));
    let text = response_text(&reply);
    println!("REAL MERGE REPLY:\n{text}");
    assert!(text.starts_with("Merged \"ingest files\""), "{text}");
    assert!(!text.contains("DRY RUN"), "{text}");
    assert!(text.contains("is now superseded"), "{text}");
    assert!(text.contains("Links moved (2):"), "{text}");
    assert!(text.contains("  constrains (1):"), "{text}");
    assert!(text.contains(&format!("    - {}", f.constraint)), "{text}");
    assert!(text.contains("  concerns (1):"), "{text}");
    assert!(text.contains(&format!("    - {}", f.decision)), "{text}");
    assert!(text.contains("Paths moved: [src/]"), "{text}");
    assert!(
        text.contains("Paths already on the successor: [shared/]"),
        "{text}"
    );
    assert!(!text.contains("Confirm this with the user"), "{text}");
    // The write happened: the retired component left the current set.
    let listing = moosedev::graph::load_components(&f.state).unwrap();
    assert!(
        !listing
            .iter()
            .any(|c| c.iri.as_deref() == Some(f.a.as_str())),
        "ingest files is still current"
    );
    assert!(listing
        .iter()
        .any(|c| c.iri.as_deref() == Some(f.b.as_str())));
    f.finish().await;
}

#[tokio::test]
async fn retire_component_plain_reply_lists_constraint_as_no_longer_delivered() {
    let f = lifecycle_fixture("retire-plain").await;
    let reply = call_raw(
        &f.client,
        "retire_component",
        json!({"component": "ingest files", "rationale": "the stage was deleted"}),
    )
    .await;
    assert_ne!(reply.is_error, Some(true), "{}", response_text(&reply));
    let text = response_text(&reply);
    println!("PLAIN RETIRE REPLY:\n{text}");
    assert!(text.starts_with("Retired \"ingest files\""), "{text}");
    assert!(
        text.contains("Constraints no longer delivered (1):"),
        "{text}"
    );
    assert!(text.contains("\"Ingest must stay streaming\""), "{text}");
    assert!(!text.contains("widens"), "{text}");
    assert!(
        text.contains("Remaining paths (kept as history): [src/, shared/]")
            || text.contains("Remaining paths (kept as history): [shared/, src/]"),
        "{text}"
    );
    assert!(text.contains("Remaining linked records"), "{text}");
    assert!(
        text.contains("ArchitecturalDecision \"Ingest decision\""),
        "{text}"
    );
    f.finish().await;
}

#[tokio::test]
async fn rename_component_reply_states_old_and_new_name() {
    let f = lifecycle_fixture("rename").await;
    let reply = call_raw(
        &f.client,
        "rename_component",
        json!({"component": "ingest files", "new_name": "ingest stage"}),
    )
    .await;
    assert_eq!(
        response_text(&reply),
        format!("Renamed \"ingest files\" -> \"ingest stage\" ({})", f.a)
    );
    let again = call_raw(
        &f.client,
        "rename_component",
        json!({"component": f.a, "new_name": "ingest stage"}),
    )
    .await;
    assert!(
        response_text(&again).contains("already has that name"),
        "{}",
        response_text(&again)
    );
    let clash = call_raw(
        &f.client,
        "rename_component",
        json!({"component": f.a, "new_name": "pipeline"}),
    )
    .await;
    assert_eq!(clash.is_error, Some(true));
    f.finish().await;
}
