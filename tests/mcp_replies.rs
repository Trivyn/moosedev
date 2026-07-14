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
    let state = AppState::bootstrap(data_dir, &ontology_dir()).expect("bootstrap app state");
    if with_substrate {
        state.set_substrate(Arc::new(synthetic_substrate()));
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

#[tokio::test]
async fn dossier_and_link_code_distinguish_substrate_coverage() {
    let data_dir = fresh_data_dir("indexed");
    let socket = runtime::socket_path_for(&data_dir);
    let backend = spawn_backend(build_server(&data_dir, true), socket.clone()).await;
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
