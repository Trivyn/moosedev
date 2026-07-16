//! v2.2 acceptance (spec §7): one policy drives both hosts.
//!
//! Against a live MCP backend with a constrained entity: the same
//! `evaluate_policy` call reported by `claude-code` and by `opencode` returns
//! the identical gate verdict, and each fires one `gate` line into
//! `fires.jsonl`; push injects the same dossier bytes the hover tool renders;
//! grounded capture produces only `proposed` records, never auto-accepted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::mcp::MooseDevServer;
use moosedev::policy::fires::fires_log_path_for;
use moosedev::runtime;
use oxigraph::model::{Literal, NamedNode, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::{Peer, RoleClient, ServiceExt};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};
use serde_json::{json, Value};
use tokio::net::UnixStream;

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/";
const ALPHA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/alpha().";
const ALPHA_NORM: &str = "rust-analyzer cargo testpkg . foo/alpha().";
const FILE: &str = "src/foo/a.rs";

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-both-hosts-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn doc(path: &str) -> Document {
    let mut d = Document::new();
    d.relative_path = path.to_string();
    d.position_encoding = EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    d
}

fn add_def(d: &mut Document, symbol: &str, name: &str, kind: symbol_information::Kind, line: i32) {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = name.to_string();
    info.kind = EnumOrUnknown::new(kind);
    let mut sig = Signature::new();
    sig.text = format!("pub {name}");
    info.signature_documentation = MessageField::some(sig);
    d.symbols.push(info);

    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.range = vec![line, 0, 10];
    occ.symbol_roles = 1;
    occ.enclosing_range = vec![line, 0, 10];
    d.occurrences.push(occ);
}

fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();
    let mut module_doc = doc(FILE);
    add_def(
        &mut module_doc,
        MODULE_RAW,
        "foo",
        symbol_information::Kind::Module,
        0,
    );
    add_def(
        &mut module_doc,
        ALPHA_RAW,
        "alpha",
        symbol_information::Kind::Function,
        1,
    );
    index.documents.push(module_doc);
    let occurrences = index
        .documents
        .iter()
        .map(|d| d.occurrences.len())
        .sum::<usize>();
    let meta = SubstrateMeta::single("rust-analyzer", "commit0", Utc::now(), 1, occurrences);
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
}

fn record(state: &AppState, kind: &str, title: &str, status: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), status.to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record instance")
}

/// Build a state with a minted, `constrains`-governed entity.
fn constrained_state(data_dir: &Path) -> AppState {
    let state = AppState::bootstrap(data_dir, &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(synthetic_substrate()));

    let component = record(&state, "SystemComponent", "foo", "accepted");
    let quad = Quad::new(
        NamedNode::new(&component).unwrap(),
        NamedNode::new(COVERS_PATH).unwrap(),
        Term::from(Literal::new_simple_literal("src/foo/")),
        oxigraph::model::GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();

    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();
    let defs = state.substrate().unwrap().definitions();
    let plan = graph::plan_mint(
        &state,
        &defs,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    let alpha = graph::entities_by_symbol(&state, &terms)
        .unwrap()
        .get(ALPHA_NORM)
        .expect("alpha minted")
        .clone();
    let constraint = record(&state, "Constraint", "alpha is contract-bound", "accepted");
    graph::relate(&state, &constraint, "constrains", &alpha).expect("constrains edge");
    state
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

fn fire_lines(data_dir: &Path) -> Vec<Value> {
    let path = fires_log_path_for(data_dir);
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("parseable fire line"))
        .collect()
}

#[tokio::test]
async fn one_policy_drives_both_hosts() {
    let data_dir = fresh_data_dir("gate");
    let state = constrained_state(&data_dir);
    let server = MooseDevServer::new(Arc::new(state));
    let socket = runtime::socket_path_for(&data_dir);
    let backend = tokio::spawn({
        let socket = socket.clone();
        async move {
            if let Err(error) = runtime::serve_unix(server, &socket).await {
                eprintln!("backend exited: {error}");
            }
        }
    });
    wait_for_socket(&socket).await;
    let client = connect_client(&socket).await;

    // The scripted scenario: each host reports the same proposed edit against
    // the constrained entity. One policy engine, two adapters.
    let mut verdicts = Vec::new();
    for host in ["claude-code", "opencode"] {
        let result = call_raw(
            &client,
            "evaluate_policy",
            json!({"host": host, "event": "edit_proposed", "file": FILE}),
        )
        .await;
        assert_ne!(result.is_error, Some(true));
        verdicts.push(response_text(&result).to_string());
    }
    assert_eq!(
        verdicts[0], verdicts[1],
        "both hosts receive the identical gate verdict"
    );
    let verdict: Value = serde_json::from_str(&verdicts[0]).unwrap();
    assert_eq!(verdict["decision"], "gate");
    assert_eq!(verdict["disposition"], "require_ratification");
    assert!(verdict["reason"]
        .as_str()
        .unwrap()
        .contains("alpha is contract-bound"));

    // PUSH: the injected dossier bytes equal what the hover tool renders.
    let push = call_raw(
        &client,
        "evaluate_policy",
        json!({"host": "opencode", "event": "entity_touched", "file": FILE, "line": 2, "col": 1}),
    )
    .await;
    let push_verdict: Value = serde_json::from_str(response_text(&push)).unwrap();
    assert_eq!(push_verdict["decision"], "inject");
    let hover = call_raw(
        &client,
        "get_entity_dossier",
        json!({"file": FILE, "line": 2, "col": 1}),
    )
    .await;
    assert_eq!(
        push_verdict["dossier_markdown"].as_str().unwrap(),
        response_text(&hover),
        "push injects the same dossier bytes hover shows"
    );

    // CAPTURE: proposed only, with provenance, never auto-accepted.
    let capture = call_raw(
        &client,
        "capture_decision_point",
        json!({
            "host": "claude-code",
            "files": [FILE],
            "summary": "scripted acceptance decision point"
        }),
    )
    .await;
    assert_ne!(capture.is_error, Some(true));
    let text = response_text(&capture);
    assert!(text.contains("Proposed ArchitecturalDecision"));

    let pending = call_raw(&client, "pending_ratifications", json!({})).await;
    let pending_text = response_text(&pending);
    assert!(
        pending_text
            .contains("[record] ArchitecturalDecision \"scripted acceptance decision point\""),
        "the captured record sits in the queue: {pending_text}"
    );

    // fires.jsonl: one gate line per host, one push, one capture — in order.
    let fires = fire_lines(&data_dir);
    let brief: Vec<(String, String, String)> = fires
        .iter()
        .map(|f| {
            (
                f["verb"].as_str().unwrap().to_string(),
                f["host"].as_str().unwrap().to_string(),
                f["decision"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        brief,
        vec![
            (
                "gate".to_string(),
                "claude-code".to_string(),
                "require_ratification".to_string()
            ),
            (
                "gate".to_string(),
                "opencode".to_string(),
                "require_ratification".to_string()
            ),
            (
                "push".to_string(),
                "opencode".to_string(),
                "inject".to_string()
            ),
            (
                "capture".to_string(),
                "claude-code".to_string(),
                "proposed".to_string()
            ),
        ],
        "every acted decision fired exactly once, attributed to its host"
    );

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}
