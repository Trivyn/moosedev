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
