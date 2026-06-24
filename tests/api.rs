//! Human-facing HTTP API tests.
//!
//! These exercise the router as the UI sees it: JSON in, JSON out. The goal is
//! to lock down the product boundaries that are easy to accidentally loosen,
//! especially "SPARQL defaults to the project graph" for the first web UI.

use std::sync::Arc;

use axum::http::header::CONTENT_DISPOSITION;
use axum_test::TestServer;
use chrono::Utc;
use moosedev::api::routes::build_routes;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::llm::LlmConfig;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use serde_json::{json, Value};

const PROVENANCE_GRAPH_IRI: &str = "https://moosedev.dev/kg/provenance";

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-api-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn test_server(state: AppState) -> TestServer {
    TestServer::new(build_routes(Arc::new(state))).expect("build test server")
}

fn unconfigured_llm() -> LlmConfig {
    LlmConfig {
        base_url: "http://localhost:1234/v1".to_string(),
        api_key: "test".to_string(),
        model: "fake-model".to_string(),
        configured: false,
    }
}

#[tokio::test]
async fn health_reports_project_graph_and_data_dir() {
    let dir = temp_dir("health");
    let state = AppState::bootstrap_with_llm_config(&dir, &ontology_dir(), unconfigured_llm())
        .expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/api/v1/health").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["project_graph"], PROJECT_KG_GRAPH_IRI);
    assert_eq!(body["data_dir"], dir.to_string_lossy().as_ref());
    assert_eq!(
        body["project_name"],
        dir.file_name().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(
        body["project_root"],
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(body["llm_configured"], false);
    assert_eq!(body["llm_assist_level"], "PureSymbolic");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn health_reports_project_root_for_conventional_data_dir() {
    let project = temp_dir("health-project-root");
    let dir = project.join(".moosedev");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/api/v1/health").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(
        body["project_name"],
        project.file_name().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(
        body["project_root"],
        std::fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(body["data_dir"], dir.to_string_lossy().as_ref());

    let _ = std::fs::remove_dir_all(&project);
}

#[tokio::test]
async fn sparql_defaults_to_project_graph_only() {
    let dir = temp_dir("sparql-scope");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let project_iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Project visible".to_string()),
                (state.capture.title.clone(), "Project visible".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project decision");

    let provenance_subject = NamedNode::new("https://example.test/provenance-only").unwrap();
    state
        .store
        .insert(&Quad::new(
            provenance_subject.clone(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Term::Literal(Literal::new_simple_literal("Provenance only")),
            GraphName::NamedNode(NamedNode::new(PROVENANCE_GRAPH_IRI).unwrap()),
        ))
        .expect("insert provenance-only triple");

    let server = test_server(state);
    let response = server
        .post("/api/v1/sparql/query")
        .json(&json!({ "query": "SELECT ?s WHERE { ?s ?p ?o }" }))
        .await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    let values: Vec<&str> = body["results"]["bindings"]
        .as_array()
        .expect("bindings array")
        .iter()
        .filter_map(|row| row["s"]["value"].as_str())
        .collect();

    assert!(
        values.contains(&project_iri.as_str()),
        "project graph record should be visible: {body}"
    );
    assert!(
        !values.contains(&provenance_subject.as_str()),
        "default UI query must not read provenance graph: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_rejects_empty_query() {
    let dir = temp_dir("sparql-empty");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server
        .post("/api/v1/sparql/query")
        .json(&json!({ "query": "   " }))
        .await;

    response.assert_status_bad_request();
    assert!(
        response.text().contains("query must not be empty"),
        "empty query error should be explicit"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn graph_export_downloads_project_nquads_only() {
    let dir = temp_dir("graph-export");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let project_iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Export visible".to_string()),
                (state.capture.title.clone(), "Export visible".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project decision");

    let provenance_subject = NamedNode::new("https://example.test/export-provenance-only").unwrap();
    state
        .store
        .insert(&Quad::new(
            provenance_subject.clone(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Term::Literal(Literal::new_simple_literal("Export provenance only")),
            GraphName::NamedNode(NamedNode::new(PROVENANCE_GRAPH_IRI).unwrap()),
        ))
        .expect("insert provenance-only triple");

    let server = test_server(state);
    let response = server
        .get("/api/v1/graph/export?format=nq&graph=project")
        .await;

    response.assert_status_ok();
    assert_eq!(response.content_type(), "application/n-quads");
    assert!(response
        .header(CONTENT_DISPOSITION)
        .to_str()
        .expect("content-disposition is text")
        .contains("attachment; filename=\"moosedev-project.nq\""));
    let body = response.text();
    assert!(body.contains(&project_iri));
    assert!(!body.contains(provenance_subject.as_str()));
    assert!(!body.contains(PROVENANCE_GRAPH_IRI));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn graph_export_rejects_unknown_format() {
    let dir = temp_dir("graph-export-bad-format");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/api/v1/graph/export?format=bogus").await;

    response.assert_status_bad_request();
    assert!(response.text().contains("unknown export format"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chat_session_routes_report_unavailable_without_session_db() {
    let dir = temp_dir("chat-unavailable");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/api/v1/chat/sessions").await;

    response.assert_status_service_unavailable();
    assert!(
        response
            .text()
            .contains("MOOSE chat sessions are not enabled"),
        "chat routes should explain missing session setup"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chat_reports_unavailable_without_llm_provider() {
    let dir = temp_dir("chat-no-llm");
    let state = AppState::bootstrap_with_llm_config(&dir, &ontology_dir(), unconfigured_llm())
        .expect("bootstrap app state");
    let server = test_server(state);

    let response = server
        .post("/api/v1/chat")
        .json(&json!({
            "messages": [{ "role": "user", "content": "What is recorded?" }]
        }))
        .await;

    response.assert_status_service_unavailable();
    assert!(
        response
            .text()
            .contains("MOOSE chat requires an explicit LLM provider"),
        "chat should explain how to enable the LLM-backed surface"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(any(not(feature = "embedded-frontend"), feature = "headless"))]
async fn static_fallback_explains_missing_embedded_frontend() {
    let dir = temp_dir("static-fallback");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/").await;

    response.assert_status_not_found();
    assert!(
        response.text().contains("UI is not embedded"),
        "non-embedded builds should have an explicit fallback message"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
