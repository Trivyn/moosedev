//! Human-facing HTTP API tests.
//!
//! These exercise the router as the UI sees it: JSON in, JSON out. The goal is
//! to lock down the product boundaries that are easy to accidentally loosen,
//! especially "SPARQL defaults to the project graph" for the first web UI.

use std::sync::Arc;

use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum_test::TestServer;
use chrono::Utc;
use moosedev::api::routes::build_routes;
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::llm::LlmConfig;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};
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

struct CleanupDir(std::path::PathBuf);

impl Drop for CleanupDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
        context_window_tokens: moosedev::llm::DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
        structured_output: moosedev::llm::StructuredOutputMode::Auto,
    }
}

fn record_api_decision(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("Decision description for {title}"),
                ),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project decision")
}

fn record_api_requirement(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("Requirement").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "Requirement".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("Requirement description for {title}"),
                ),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project requirement")
}

fn record_api_lesson(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("Lesson").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "Lesson".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("Lesson description for {title}"),
                ),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project lesson")
}

fn record_api_constraint(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("Constraint").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "Constraint".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("Constraint description for {title}"),
                ),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record project constraint")
}

#[tokio::test]
async fn records_detail_returns_record_metadata_and_edges() {
    let dir = temp_dir("record-detail");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let decision = record_api_decision(&state, "Record detail decision");
    let lesson = record_api_lesson(&state, "Record detail lesson");
    state
        .store
        .insert(&oxigraph::model::Quad::new(
            oxigraph::model::NamedNode::new(&lesson).unwrap(),
            oxigraph::model::NamedNode::new(moose::RDF_TYPE).unwrap(),
            oxigraph::model::NamedNode::new(state.resolve_class("SystemComponent").unwrap())
                .unwrap(),
            oxigraph::model::GraphName::NamedNode(
                oxigraph::model::NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            ),
        ))
        .unwrap();
    graph::relate(&state, &decision, "yieldsLesson", &lesson).expect("link outgoing lesson");
    graph::relate(&state, &decision, "concerns", &lesson).expect("link Story component");
    graph::relate(&state, &lesson, "learnedFrom", &decision).expect("link incoming lesson");
    let uuid = decision.rsplit('/').next().expect("record uuid");
    let server = test_server(state);

    let response = server.get(&format!("/api/v1/records/{uuid}")).await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["iri"], decision);
    assert_eq!(body["kind"], "ArchitecturalDecision");
    assert_eq!(body["title"], "Record detail decision");
    assert_eq!(
        body["description"],
        "Decision description for Record detail decision"
    );
    assert_eq!(body["story_component_iri"], lesson);
    let outgoing = body["outgoing"].as_array().expect("outgoing array");
    let yields = outgoing
        .iter()
        .find(|edge| edge["predicate"] == "yieldsLesson")
        .expect("yieldsLesson edge");
    assert_eq!(yields["target_iri"], lesson);
    assert_eq!(yields["target_kind"], "SystemComponent");
    assert!(yields.get("target_status").is_none());
    assert_eq!(body["incoming"][0]["predicate"], "learnedFrom");
    assert_eq!(body["incoming"][0]["source_iri"], lesson);
    assert!(body["incoming"][0].get("source_status").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn records_detail_returns_not_found_for_unknown_uuid() {
    let dir = temp_dir("record-detail-missing");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server.get("/api/v1/records/not-a-record").await;

    response.assert_status_not_found();

    let _ = std::fs::remove_dir_all(&dir);
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
    // Canonical: cross-process consumers use this field as backend identity.
    assert_eq!(
        body["data_dir"],
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
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
    assert_eq!(
        body["data_dir"],
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );

    let _ = std::fs::remove_dir_all(&project);
}

#[tokio::test]
async fn health_uses_configured_repository_root_as_canonical_identity() {
    let project = temp_dir("health-configured-project-root");
    let data_dir = temp_dir("health-configured-data");
    std::fs::create_dir_all(&project).unwrap();
    let _project_cleanup = CleanupDir(project.clone());
    let _data_cleanup = CleanupDir(data_dir.clone());
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    state.load_substrate(&project);
    let server = test_server(state);

    let body = server.get("/api/v1/health").await.json::<Value>();
    assert_eq!(
        body["project_root"],
        std::fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
}

fn record_story_component(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("SystemComponent").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "SystemComponent".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
            ],
        },
        "story-test",
        Utc::now(),
    )
    .expect("record Story component")
}

fn project_quads(state: &AppState) -> std::collections::BTreeSet<String> {
    state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(oxigraph::model::GraphNameRef::NamedNode(
                oxigraph::model::NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .flatten()
        .map(|quad| quad.to_string())
        .collect()
}

#[tokio::test]
async fn story_api_covers_recipe_lifecycle_generation_ambiguity_and_grading() {
    let project = temp_dir("stories");
    let _cleanup = CleanupDir(project.clone());
    let data_dir = project.join(".moosedev");
    let state = Arc::new(
        AppState::bootstrap_with_llm_config(&data_dir, &ontology_dir(), unconfigured_llm())
            .expect("bootstrap Story state"),
    );
    let graph_component = record_story_component(&state, "Graph Store");
    let api_component = record_story_component(&state, "Graph API");
    let requirement = record_api_requirement(&state, "Preserve durable project knowledge");
    graph::relate(&state, &graph_component, "isConcernedBy", &requirement)
        .expect("link inverse Story evidence");
    let distractor = record_api_requirement(&state, "Zeta cross-linked API knowledge");
    graph::relate(&state, &distractor, "concerns", &api_component).expect("link Story distractor");
    graph::relate(&state, &distractor, "concerns", &graph_component)
        .expect("also link overlapping record to target");
    let safe_distractor = record_api_requirement(&state, "Keep API clients thin");
    graph::relate(&state, &safe_distractor, "concerns", &api_component)
        .expect("link safe Story distractor");
    let before_generate = project_quads(&state);
    let server = TestServer::new(build_routes(state.clone())).expect("build Story test server");

    let subjects = server.get("/api/v1/stories/actions/subjects").await;
    subjects.assert_status_ok();
    let subject_body = subjects.json::<Value>();
    let subject_rows = subject_body["subjects"].as_array().unwrap();
    assert!(subject_rows.len() >= 5);
    assert_eq!(subject_rows[0]["label"], "Graph API");
    assert_eq!(subject_rows[1]["label"], "Graph Store");
    assert!(subject_rows
        .iter()
        .any(|subject| subject["iri"] == graph_component));
    assert!(subject_rows
        .iter()
        .any(|subject| subject["iri"] == api_component));
    assert!(subject_rows
        .iter()
        .any(|subject| subject["iri"] == requirement));

    let limited_subjects = server
        .get("/api/v1/stories/actions/subjects?q=Graph&limit=1")
        .await;
    limited_subjects.assert_status_ok();
    assert_eq!(
        limited_subjects.json::<Value>()["subjects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let searched_subjects = server
        .get("/api/v1/stories/actions/subjects?q=durable%20project&limit=12")
        .await;
    searched_subjects.assert_status_ok();
    assert!(searched_subjects.json::<Value>()["subjects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|subject| subject["iri"] == requirement && subject["kind"] == "Requirement"));
    assert_eq!(project_quads(&state), before_generate);

    let topic_story = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"topic":"durable project knowledge", "assist_level":0}))
        .await;
    topic_story.assert_status_ok();
    let topic_body = topic_story.json::<Value>();
    assert_eq!(topic_body["story"]["subject"]["type"], "topic");
    assert!(topic_body["story"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|evidence| evidence["iri"] == requirement));
    assert_eq!(project_quads(&state), before_generate);

    server
        .get("/api/v1/stories/bad!id")
        .await
        .assert_status_bad_request();
    server
        .post("/api/v1/stories/bad!id/publish")
        .json(&json!({"updated_at":"stale"}))
        .await
        .assert_status_bad_request();
    server
        .post("/api/v1/stories/does-not-exist/publish")
        .json(&json!({"updated_at":"missing"}))
        .await
        .assert_status_not_found();
    server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"recipe_id":"bad!id", "assist_level":0}))
        .await
        .assert_status_bad_request();

    let ambiguous = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"prompt":"graph", "assist_level":0}))
        .await;
    ambiguous.assert_status_ok();
    let ambiguous_body = ambiguous.json::<Value>();
    assert_eq!(ambiguous_body["outcome"], "ambiguous");
    assert_eq!(ambiguous_body["candidates"].as_array().unwrap().len(), 2);

    let generated = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"component_iri":graph_component, "assist_level":0}))
        .await;
    generated.assert_status_ok();
    let generated_body = generated.json::<Value>();
    assert_eq!(generated_body["outcome"], "story");
    assert_eq!(generated_body["story"]["narration_mode"], "symbolic");
    assert_eq!(generated_body["story"]["schema_version"], 3);
    assert!(!generated_body["story"]["narrative"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(project_quads(&state), before_generate);

    let presentation_only = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({
            "component_iri":graph_component,
            "assist_level":0,
            "include_checks":false
        }))
        .await;
    presentation_only.assert_status_ok();
    let presentation_body = presentation_only.json::<Value>();
    assert!(presentation_body["story"]["checks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        presentation_body["story"]["gaps"],
        generated_body["story"]["gaps"]
    );

    let check = &generated_body["story"]["checks"][0];
    assert!(uuid::Uuid::parse_str(check["id"].as_str().unwrap()).is_ok());
    assert!(check["options"]
        .as_array()
        .unwrap()
        .iter()
        .all(|option| uuid::Uuid::parse_str(option["id"].as_str().unwrap()).is_ok()));
    let correct_option = check["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["label"] == "Preserve durable project knowledge")
        .unwrap();
    let grade = server
        .post("/api/v1/stories/checks/grade")
        .json(&json!({
            "check_id": check["id"],
            "selected_option_ids": [correct_option["id"].as_str().unwrap()]
        }))
        .await;
    grade.assert_status_ok();
    assert_eq!(grade.json::<Value>()["correct"], true);
    let wrong_option = check["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] != correct_option["id"])
        .unwrap();
    let wrong_grade = server
        .post("/api/v1/stories/checks/grade")
        .json(&json!({
            "check_id": check["id"],
            "selected_option_ids": [wrong_option["id"].as_str().unwrap()]
        }))
        .await;
    wrong_grade.assert_status_ok();
    let wrong_grade_body = wrong_grade.json::<Value>();
    assert_eq!(wrong_grade_body["correct"], false);
    assert_eq!(wrong_grade_body["revisit_section_id"], "orientation");
    assert_eq!(wrong_grade_body["evidence_iris"], json!([requirement]));

    server
        .post("/api/v1/stories/checks/grade")
        .json(&json!({"check_id": check["id"], "selected_option_ids": [uuid::Uuid::new_v4().to_string()]}))
        .await
        .assert_status_bad_request();
    server
        .post("/api/v1/stories/checks/grade")
        .json(&json!({"check_id": "not-a-handle", "selected_option_ids": []}))
        .await
        .assert_status_bad_request();
    server
        .post("/api/v1/stories/checks/grade")
        .json(&json!({"check_id": uuid::Uuid::new_v4().to_string(), "selected_option_ids": []}))
        .await
        .assert_status_not_found();

    server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"component_iri":graph_component, "assist_level":2}))
        .await
        .assert_status_bad_request();

    let draft = json!({
        "id":"graph-store",
        "title":"The Graph Store",
        "schema_version":3,
        "subject":{"type":"entity", "iri":graph_component},
        "goal":"Understand durable storage",
        "audience":"reboarding",
        "focus":{
            "include_record_iris":["https://example.test/missing-record"],
            "exclude_record_iris":[], "include_code_symbols":[],
            "exclude_code_symbols":[], "emphasis":["orientation"]
        },
        "status":"draft",
        "curator":"maintainer"
    });
    let put = server.put("/api/v1/stories/graph-store").json(&draft).await;
    put.assert_status_ok();
    let put_body = put.json::<Value>();
    assert_eq!(put_body["recipe"]["id"], "graph-store");
    assert_eq!(put_body["recipe"]["schema_version"], 3);
    assert_eq!(put_body["recipe"]["subject"]["type"], "entity");
    assert!(put_body["recipe"].get("subject_component_iri").is_none());

    // Action routes live below an extra segment, so ordinary recipe IDs such
    // as `generate` remain addressable by every resource method.
    let mut generate_recipe = draft.clone();
    generate_recipe["id"] = json!("generate");
    generate_recipe["title"] = json!("ZZ Generate recipe");
    server
        .put("/api/v1/stories/generate")
        .json(&generate_recipe)
        .await
        .assert_status_ok();
    let generated_recipe = server.get("/api/v1/stories/generate").await;
    generated_recipe.assert_status_ok();
    assert_eq!(generated_recipe.json::<Value>()["recipe"]["id"], "generate");

    let recipe_ambiguity = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({
            "recipe_id":"graph-store",
            "component_iri":"graph",
            "assist_level":0
        }))
        .await;
    recipe_ambiguity.assert_status_bad_request();

    let get = server.get("/api/v1/stories/graph-store").await;
    get.assert_status_ok();
    assert_eq!(get.json::<Value>()["recipe"]["status"], "draft");
    let list = server.get("/api/v1/stories").await;
    list.assert_status_ok();
    let list_body = list.json::<Value>();
    assert_eq!(list_body["stories"][0]["subject_label"], "Graph Store");
    assert_eq!(list_body["stories"][0]["drifted"], true);

    let invalid_publish = server
        .post("/api/v1/stories/graph-store/publish")
        .json(&json!({"updated_at":put_body["recipe"]["updated_at"]}))
        .await;
    invalid_publish.assert_status_bad_request();

    let mut publishable = draft;
    publishable["updated_at"] = put_body["recipe"]["updated_at"].clone();
    publishable["focus"]["include_record_iris"] = json!([requirement]);
    publishable["focus"]["emphasis"] = json!(["orientation", "current_state"]);
    let updated = server
        .put("/api/v1/stories/graph-store")
        .json(&publishable)
        .await;
    updated.assert_status_ok();
    let updated_body = updated.json::<Value>();
    let stale_put = server
        .put("/api/v1/stories/graph-store")
        .json(&publishable)
        .await;
    assert_eq!(stale_put.status_code(), axum::http::StatusCode::CONFLICT);

    let stale_publish = server
        .post("/api/v1/stories/graph-store/publish")
        .json(&json!({"updated_at":put_body["recipe"]["updated_at"]}))
        .await;
    assert_eq!(
        stale_publish.status_code(),
        axum::http::StatusCode::CONFLICT
    );
    let published = server
        .post("/api/v1/stories/graph-store/publish")
        .json(&json!({"updated_at":updated_body["recipe"]["updated_at"]}))
        .await;
    published.assert_status_ok();
    assert_eq!(published.json::<Value>()["recipe"]["status"], "published");

    let preferred = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"component_iri":graph_component, "assist_level":0}))
        .await;
    preferred.assert_status_ok();
    let preferred_body = preferred.json::<Value>();
    assert_eq!(preferred_body["story"]["trust_state"], "published");
    assert_eq!(preferred_body["story"]["recipe_id"], "graph-store");

    let replayed = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({
            "recipe_id":"graph-store",
            "component_iri":api_component,
            "assist_level":0
        }))
        .await;
    replayed.assert_status_bad_request();

    let fresh = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({
            "component_iri":graph_component,
            "fresh":true,
            "assist_level":0
        }))
        .await;
    fresh.assert_status_ok();
    let fresh_body = fresh.json::<Value>();
    assert_eq!(fresh_body["story"]["trust_state"], "generated");
    assert!(fresh_body["story"].get("recipe_id").is_none());

    let drifted = json!({
        "id":"drifted",
        "title":"Drifted Story",
        "schema_version":3,
        "subject":{"type":"entity", "iri":"https://example.test/missing-component"},
        "goal":"Repair a stale route",
        "audience":"reboarding",
        "focus":{"include_record_iris":["https://example.test/missing-record"],
            "exclude_record_iris":[], "include_code_symbols":[], "exclude_code_symbols":[],
            "emphasis":[]},
        "status":"draft",
        "curator":"maintainer"
    });
    server
        .put("/api/v1/stories/drifted")
        .json(&drifted)
        .await
        .assert_status_ok();
    let drifted_run = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"recipe_id":"drifted", "assist_level":0}))
        .await;
    drifted_run.assert_status_ok();
    let drifted_body = drifted_run.json::<Value>();
    assert_eq!(
        drifted_body["story"]["subject"]["iri"],
        "https://example.test/missing-component"
    );
    assert_eq!(
        drifted_body["story"]["subject"]["label"],
        "https://example.test/missing-component"
    );
    assert!(drifted_body["story"]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|gap| gap["id"] == "subject-drift"));

    let ungrounded = json!({
        "id":"ungrounded",
        "title":"Ungrounded draft",
        "schema_version":3,
        "subject":{"type":"entity", "iri":graph_component},
        "goal":"Surface curator drift",
        "audience":"reboarding",
        "focus":{"include_record_iris":[safe_distractor], "exclude_record_iris":[],
            "include_code_symbols":[], "exclude_code_symbols":[], "emphasis":[]},
        "status":"draft",
        "curator":"maintainer"
    });
    let ungrounded_put = server
        .put("/api/v1/stories/ungrounded")
        .json(&ungrounded)
        .await;
    ungrounded_put.assert_status_ok();
    let mut ungrounded_published = ungrounded.clone();
    ungrounded_published["updated_at"] =
        ungrounded_put.json::<Value>()["recipe"]["updated_at"].clone();
    ungrounded_published["status"] = json!("published");
    server
        .put("/api/v1/stories/ungrounded")
        .json(&ungrounded_published)
        .await
        .assert_status_bad_request();
    let ungrounded_run = server
        .post("/api/v1/stories/actions/generate")
        .json(&json!({"recipe_id":"ungrounded", "assist_level":0}))
        .await;
    ungrounded_run.assert_status_ok();
    let ungrounded_body = ungrounded_run.json::<Value>();
    assert!(ungrounded_body["story"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["iri"] != safe_distractor));
    assert!(ungrounded_body["story"]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|gap| gap["id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("outside-focus-record-")));
    assert_eq!(
        project_quads(&state),
        before_generate,
        "Story reads, recipe lifecycle, and grading must not mutate the project graph"
    );
}

#[tokio::test]
async fn story_recipe_io_failures_are_server_errors() {
    let project = temp_dir("story-io-error");
    let _cleanup = CleanupDir(project.clone());
    let data_dir = project.join(".moosedev");
    let state = Arc::new(
        AppState::bootstrap_with_llm_config(&data_dir, &ontology_dir(), unconfigured_llm())
            .expect("bootstrap Story I/O state"),
    );
    std::fs::write(project.join("stories"), "not a directory")
        .expect("block the Story repository directory");
    let server = TestServer::new(build_routes(state)).expect("build Story I/O test server");
    let response = server
        .put("/api/v1/stories/io-failure")
        .json(&json!({
            "id":"io-failure",
            "title":"I/O failure",
            "subject_component_iri":"https://example.test/component",
            "goal":"Exercise error mapping",
            "audience":"reboarding",
            "beats":[],
            "status":"draft",
            "curator":"tester"
        }))
        .await;
    response.assert_status_internal_server_error();
}

#[tokio::test]
async fn corrupt_story_is_excluded_from_list_and_get_is_server_error() {
    let project = temp_dir("story-corrupt");
    let _cleanup = CleanupDir(project.clone());
    let data_dir = project.join(".moosedev");
    let state = Arc::new(
        AppState::bootstrap_with_llm_config(&data_dir, &ontology_dir(), unconfigured_llm())
            .expect("bootstrap corrupt Story state"),
    );
    std::fs::create_dir_all(project.join("stories")).unwrap();
    std::fs::write(project.join("stories/corrupt.json"), "{bad json").unwrap();
    let server = TestServer::new(build_routes(state)).expect("build Story test server");

    let list = server.get("/api/v1/stories").await;
    list.assert_status_ok();
    assert!(list.json::<Value>()["stories"]
        .as_array()
        .unwrap()
        .is_empty());
    server
        .get("/api/v1/stories/corrupt")
        .await
        .assert_status_internal_server_error();
    server
        .post("/api/v1/stories/corrupt/publish")
        .json(&json!({"updated_at":"corrupt"}))
        .await
        .assert_status_internal_server_error();
    server
        .put("/api/v1/stories/corrupt")
        .json(&json!({
            "id":"corrupt",
            "title":"Replacement",
            "subject_component_iri":"https://example.test/component",
            "goal":"Replace corrupt storage",
            "audience":"reboarding",
            "beats":[],
            "status":"draft",
            "curator":"tester",
            "updated_at":"corrupt"
        }))
        .await
        .assert_status_internal_server_error();
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
async fn adrs_list_renders_project_decisions() {
    let dir = temp_dir("adrs-list");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let iri = record_api_decision(&state, "API ADR visible");
    let requirement = record_api_requirement(&state, "Searchable ADR requirement");
    graph::relate(&state, &iri, "isMotivatedBy", &requirement).expect("link ADR requirement");
    let server = test_server(state);

    let response = server.get("/api/v1/adrs").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["graph_decisions"], 1);
    assert_eq!(body["adr_files"], 1);
    assert_eq!(body["adrs"][0]["num"], "0001");
    assert_eq!(body["adrs"][0]["title"], "API ADR visible");
    assert_eq!(body["adrs"][0]["filename"], "0001-api-adr-visible.md");
    assert_eq!(body["adrs"][0]["iri"], iri);
    assert!(body["adrs"][0]["search_text"]
        .as_str()
        .expect("search text")
        .contains(&requirement));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn adrs_detail_returns_generated_markdown() {
    let dir = temp_dir("adrs-detail");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_decision(&state, "Detailed ADR");
    let server = test_server(state);

    let response = server.get("/api/v1/adrs/0001").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["summary"]["filename"], "0001-detailed-adr.md");
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains("Decision description for Detailed ADR"));

    let missing = server.get("/api/v1/adrs/9999").await;
    missing.assert_status_not_found();

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn adrs_archive_downloads_zip_with_generated_files() {
    let dir = temp_dir("adrs-archive");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_decision(&state, "Archive ADR");
    let server = test_server(state);

    let response = server.get("/api/v1/adrs/archive.zip").await;

    response.assert_status_ok();
    assert_eq!(response.content_type(), "application/zip");
    assert!(response
        .header(CONTENT_DISPOSITION)
        .to_str()
        .expect("content-disposition is text")
        .contains("attachment; filename=\"moosedev-adrs.zip\""));
    assert_eq!(
        response
            .header(CONTENT_TYPE)
            .to_str()
            .expect("content-type is text"),
        "application/zip"
    );

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(response.as_bytes().as_ref())).expect("zip");
    assert!(archive.by_name("0000-index.md").is_ok());
    let mut adr = archive
        .by_name("0001-archive-adr.md")
        .expect("generated ADR file");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut adr, &mut text).expect("read ADR");
    assert!(text.contains("Decision description for Archive ADR"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn requirements_list_renders_project_requirements() {
    let dir = temp_dir("requirements-list");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let req = record_api_requirement(&state, "API Requirement visible");
    let ad = record_api_decision(&state, "Requirement ADR");
    graph::relate(&state, &ad, "isMotivatedBy", &req).expect("link requirement");
    let server = test_server(state);

    let response = server.get("/api/v1/requirements").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["graph_requirements"], 1);
    assert_eq!(body["requirement_files"], 1);
    assert_eq!(body["requirements"][0]["num"], "0001");
    assert_eq!(body["requirements"][0]["title"], "API Requirement visible");
    assert_eq!(
        body["requirements"][0]["filename"],
        "0001-api-requirement-visible.md"
    );
    assert_eq!(body["requirements"][0]["iri"], req);
    assert_eq!(body["requirements"][0]["related_adrs"], 1);
    assert!(body["requirements"][0]["search_text"]
        .as_str()
        .expect("search text")
        .contains(&ad));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn requirements_detail_returns_generated_markdown() {
    let dir = temp_dir("requirements-detail");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let req = record_api_requirement(&state, "Detailed Requirement");
    let ad = record_api_decision(&state, "Detailed Requirement ADR");
    graph::relate(&state, &ad, "isMotivatedBy", &req).expect("link requirement");
    let server = test_server(state);

    let response = server.get("/api/v1/requirements/0001").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["summary"]["filename"], "0001-detailed-requirement.md");
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains("Requirement description for Detailed Requirement"));
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains(&ad));

    let missing = server.get("/api/v1/requirements/9999").await;
    missing.assert_status_not_found();

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn requirements_archive_downloads_zip_with_generated_files() {
    let dir = temp_dir("requirements-archive");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_requirement(&state, "Archive Requirement");
    let server = test_server(state);

    let response = server.get("/api/v1/requirements/archive.zip").await;

    response.assert_status_ok();
    assert_eq!(response.content_type(), "application/zip");
    assert!(response
        .header(CONTENT_DISPOSITION)
        .to_str()
        .expect("content-disposition is text")
        .contains("attachment; filename=\"moosedev-requirements.zip\""));
    assert_eq!(
        response
            .header(CONTENT_TYPE)
            .to_str()
            .expect("content-type is text"),
        "application/zip"
    );

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(response.as_bytes().as_ref())).expect("zip");
    assert!(archive.by_name("0000-index.md").is_ok());
    let mut requirement = archive
        .by_name("0001-archive-requirement.md")
        .expect("generated requirement file");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut requirement, &mut text).expect("read requirement");
    assert!(text.contains("Requirement description for Archive Requirement"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lessons_list_renders_project_lessons() {
    let dir = temp_dir("lessons-list");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let lesson = record_api_lesson(&state, "API Lesson visible");
    let ad = record_api_decision(&state, "Lesson ADR");
    graph::relate(&state, &lesson, "learnedFrom", &ad).expect("link lesson");
    let server = test_server(state);

    let response = server.get("/api/v1/lessons").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["graph_lessons"], 1);
    assert_eq!(body["lesson_files"], 1);
    assert_eq!(body["lessons"][0]["num"], "0001");
    assert_eq!(body["lessons"][0]["title"], "API Lesson visible");
    assert_eq!(body["lessons"][0]["filename"], "0001-api-lesson-visible.md");
    assert_eq!(body["lessons"][0]["iri"], lesson);
    assert_eq!(body["lessons"][0]["related_sources"], 1);
    assert!(body["lessons"][0]["search_text"]
        .as_str()
        .expect("search text")
        .contains(&ad));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lessons_detail_returns_generated_markdown() {
    let dir = temp_dir("lessons-detail");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let lesson = record_api_lesson(&state, "Detailed Lesson");
    let ad = record_api_decision(&state, "Detailed Lesson ADR");
    graph::relate(&state, &ad, "yieldsLesson", &lesson).expect("link lesson");
    let server = test_server(state);

    let response = server.get("/api/v1/lessons/0001").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["summary"]["filename"], "0001-detailed-lesson.md");
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains("Lesson description for Detailed Lesson"));
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains(&ad));

    let missing = server.get("/api/v1/lessons/9999").await;
    missing.assert_status_not_found();

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lessons_archive_downloads_zip_with_generated_files() {
    let dir = temp_dir("lessons-archive");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_lesson(&state, "Archive Lesson");
    let server = test_server(state);

    let response = server.get("/api/v1/lessons/archive.zip").await;

    response.assert_status_ok();
    assert_eq!(response.content_type(), "application/zip");
    assert!(response
        .header(CONTENT_DISPOSITION)
        .to_str()
        .expect("content-disposition is text")
        .contains("attachment; filename=\"moosedev-lessons.zip\""));
    assert_eq!(
        response
            .header(CONTENT_TYPE)
            .to_str()
            .expect("content-type is text"),
        "application/zip"
    );

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(response.as_bytes().as_ref())).expect("zip");
    assert!(archive.by_name("0000-index.md").is_ok());
    let mut lesson = archive
        .by_name("0001-archive-lesson.md")
        .expect("generated lesson file");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut lesson, &mut text).expect("read lesson");
    assert!(text.contains("Lesson description for Archive Lesson"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn constraints_list_renders_project_constraints() {
    let dir = temp_dir("constraints-list");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let constraint = record_api_constraint(&state, "API Constraint visible");
    let decision = record_api_decision(&state, "Constrained ADR");
    graph::relate(&state, &constraint, "constrains", &decision).expect("link constraint");
    let server = test_server(state);

    let response = server.get("/api/v1/constraints").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["graph_constraints"], 1);
    assert_eq!(body["constraint_files"], 1);
    assert_eq!(body["constraints"][0]["num"], "0001");
    assert_eq!(body["constraints"][0]["title"], "API Constraint visible");
    assert_eq!(
        body["constraints"][0]["filename"],
        "0001-api-constraint-visible.md"
    );
    assert_eq!(body["constraints"][0]["iri"], constraint);
    assert_eq!(body["constraints"][0]["related_targets"], 1);
    assert!(body["constraints"][0]["search_text"]
        .as_str()
        .expect("search text")
        .contains(&decision));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn constraints_detail_returns_generated_markdown() {
    let dir = temp_dir("constraints-detail");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_constraint(&state, "Detailed Constraint");
    let server = test_server(state);

    let response = server.get("/api/v1/constraints/0001").await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["summary"]["filename"], "0001-detailed-constraint.md");
    assert!(body["markdown"]
        .as_str()
        .expect("markdown string")
        .contains("Constraint description for Detailed Constraint"));

    let missing = server.get("/api/v1/constraints/9999").await;
    missing.assert_status_not_found();

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn constraints_archive_downloads_zip_with_generated_files() {
    let dir = temp_dir("constraints-archive");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_api_constraint(&state, "Archive Constraint");
    let server = test_server(state);

    let response = server.get("/api/v1/constraints/archive.zip").await;

    response.assert_status_ok();
    assert_eq!(response.content_type(), "application/zip");
    assert!(response
        .header(CONTENT_DISPOSITION)
        .to_str()
        .expect("content-disposition is text")
        .contains("attachment; filename=\"moosedev-constraints.zip\""));
    assert_eq!(
        response
            .header(CONTENT_TYPE)
            .to_str()
            .expect("content-type is text"),
        "application/zip"
    );

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(response.as_bytes().as_ref())).expect("zip");
    assert!(archive.by_name("0000-index.md").is_ok());
    let mut constraint = archive
        .by_name("0001-archive-constraint.md")
        .expect("generated constraint file");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut constraint, &mut text).expect("read constraint");
    assert!(text.contains("Constraint description for Archive Constraint"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn graph_import_patches_project_turtle() {
    let dir = temp_dir("graph-import");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);
    let ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<https://example.test/api-imported> rdfs:label "API imported" .
"#;

    let response = server
        .post("/api/v1/graph/import?format=ttl&graph=project&mode=patch")
        .text(ttl)
        .await;

    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["inserted_quad_count"], 1);
    assert_eq!(body["skipped_existing_count"], 0);

    let query = server
        .post("/api/v1/sparql/query")
        .json(&json!({
            "query": "SELECT ?s WHERE { ?s <http://www.w3.org/2000/01/rdf-schema#label> \"API imported\" }"
        }))
        .await;
    query.assert_status_ok();
    assert!(
        query.text().contains("https://example.test/api-imported"),
        "imported project triple should be queryable"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn graph_import_rejects_out_of_scope_nquads() {
    let dir = temp_dir("graph-import-bad-scope");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);
    let nq = format!(
        "<https://example.test/s> <{}> \"bad\" <{}> .\n",
        moose::RDFS_LABEL,
        PROVENANCE_GRAPH_IRI
    );

    let response = server
        .post("/api/v1/graph/import?format=nq&graph=project&mode=patch")
        .text(nq)
        .await;

    response.assert_status_bad_request();
    assert!(response.text().contains("outside the selected scope"));

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

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const ALPHA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/alpha().";

fn code_doc(path: &str) -> Document {
    let mut d = Document::new();
    d.relative_path = path.to_string();
    d.position_encoding = EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    d
}

fn add_public_fn(d: &mut Document, symbol: &str, name: &str) {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = name.to_string();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut sig = Signature::new();
    sig.text = format!("pub fn {name}()");
    info.signature_documentation = MessageField::some(sig);
    d.symbols.push(info);
    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.range = vec![0, 0, 10];
    occ.symbol_roles = 1;
    occ.enclosing_range = vec![0, 0, 10];
    d.occurrences.push(occ);
}

fn foo_substrate() -> Substrate {
    let mut index = Index::new();
    let mut module_doc = code_doc("src/foo/a.rs");
    add_public_fn(&mut module_doc, ALPHA_RAW, "alpha");
    index.documents.push(module_doc);
    let meta = SubstrateMeta::single("rust-analyzer", "c0", Utc::now(), 1, 1);
    Substrate::from_index(index, meta, false).expect("substrate")
}

fn record_component(state: &AppState, name: &str, covers: &str) -> String {
    let class_iri = state.resolve_class("SystemComponent").unwrap();
    let iri = graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "SystemComponent".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), name.to_string()),
                (state.capture.title.clone(), name.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .unwrap();
    let quad = Quad::new(
        NamedNode::new(&iri).unwrap(),
        NamedNode::new(COVERS_PATH).unwrap(),
        Term::from(Literal::new_simple_literal(covers)),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
    iri
}

#[tokio::test]
async fn debt_and_proposals_endpoints() {
    let state = AppState::bootstrap(&temp_dir("debt"), &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(foo_substrate()));
    record_component(&state, "foo", "src/foo/");
    let subject = record_api_decision(&state, "cited decision");
    graph::propose_link(
        &state,
        &subject,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "cited in prose",
        "tester",
        Utc::now(),
    )
    .unwrap();

    let server = test_server(state);

    // Debt: foo owns 1 public entity, 0 documented (the proposal must not count).
    let debt = server.get("/api/v1/debt").await;
    debt.assert_status_ok();
    let body = debt.json::<Value>();
    let row = body["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "foo")
        .expect("foo component");
    assert_eq!(row["denominator"], 1);
    assert_eq!(row["numerator"], 0);
    assert_eq!(row["status"], "accepted");

    // Inbox lists the pending proposal.
    let list = server.get("/api/v1/proposals?status=proposed").await;
    list.assert_status_ok();
    let proposals = list.json::<Value>();
    let arr = proposals["proposals"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["predicate"], "concerns");
    let id = arr[0]["id"].as_str().unwrap().to_string();

    // Accept materializes the real link.
    let accept = server.post(&format!("/api/v1/proposals/{id}/accept")).await;
    accept.assert_status_ok();
    assert_eq!(accept.json::<Value>()["status"], "accepted");

    // Debt numerator now reflects the ratified link.
    let debt2 = server.get("/api/v1/debt").await;
    let body2 = debt2.json::<Value>();
    let row2 = body2["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "foo")
        .expect("foo component");
    assert_eq!(row2["numerator"], 1);

    // Unknown id → 404.
    server
        .post("/api/v1/proposals/nonexistent/accept")
        .await
        .assert_status_not_found();
}

const ALPHA_NORM: &str = "rust-analyzer cargo testpkg . foo/alpha().";

/// Like [`record_api_constraint`] but with an explicit `accepted` lifecycle
/// status, which the policy gate requires before a Constraint governs an edit.
fn record_accepted_constraint(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("Constraint").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "Constraint".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record constraint")
}

#[tokio::test]
async fn policy_endpoint_gates_pushes_and_fires() {
    let data_dir = temp_dir("policy");
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(foo_substrate()));
    record_component(&state, "foo", "src/foo/");

    // Mint alpha and link an accepted Constraint to it.
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
    let constraint = record_accepted_constraint(&state, "alpha must stay stable");
    graph::relate(&state, &constraint, "constrains", &alpha).expect("constrains edge");

    let server = test_server(state);

    // GATE: an edit against the constrained file escalates to ratification.
    let gate = server
        .post("/api/v1/policy")
        .json(&json!({
            "host": "test-http",
            "kind": "edit_proposed",
            "file": "src/foo/a.rs",
        }))
        .await;
    gate.assert_status_ok();
    let verdict = gate.json::<Value>();
    assert_eq!(verdict["decision"], "gate");
    assert_eq!(verdict["disposition"], "require_ratification");
    assert!(verdict["reason"]
        .as_str()
        .unwrap()
        .contains("alpha must stay stable"));
    assert_eq!(verdict["records"][0]["iri"], constraint.as_str());
    assert_eq!(verdict["entities"][0], alpha.as_str());

    // PUSH: touching the file injects the linked knowledge.
    let push = server
        .post("/api/v1/policy")
        .json(&json!({
            "host": "test-http",
            "kind": "entity_touched",
            "file": "src/foo/a.rs",
        }))
        .await;
    push.assert_status_ok();
    let injected = push.json::<Value>();
    assert_eq!(injected["decision"], "inject");
    assert!(injected["dossier_markdown"]
        .as_str()
        .unwrap()
        .contains("alpha must stay stable"));

    // Both acted decisions appended fire telemetry.
    let fires =
        std::fs::read_to_string(moosedev::policy::fires::fires_log_path_for(&data_dir)).unwrap();
    let lines: Vec<Value> = fires
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["verb"], "gate");
    assert_eq!(lines[1]["verb"], "push");
    assert_eq!(lines[0]["host"], "test-http");
}

#[tokio::test]
async fn automatic_capture_journals_without_touching_the_graph() {
    let dir = temp_dir("capture-journals");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let response = server
        .post("/api/v1/capture")
        .json(&json!({
            "host": "claude-code",
            "summary": "Waiting on the test suite — status, not a decision",
            "files": ["src/lib.rs", "tests/api.rs"],
            // Retired field from older deployed adapters: ignored, not an error.
            "since_unix_seconds": 12345
        }))
        .await;

    response.assert_status_ok();
    assert_eq!(response.json::<Value>()["outcome"], "journaled");

    // Nothing entered the graph or the ratification inbox.
    let proposals = server.get("/api/v1/proposals?status=proposed").await;
    proposals.assert_status_ok();
    assert!(proposals.json::<Value>()["proposals"]
        .as_array()
        .unwrap()
        .is_empty());

    // The checkpoint is one fire-telemetry journal line carrying the payload.
    let fires = std::fs::read_to_string(moosedev::policy::fires::fires_log_path_for(&dir))
        .expect("journal fire");
    let event: Value = serde_json::from_str(fires.lines().last().unwrap()).unwrap();
    assert_eq!(event["verb"], "capture");
    assert_eq!(event["decision"], "journaled");
    assert_eq!(event["host"], "claude-code");
    assert_eq!(
        event["summary"],
        "Waiting on the test suite — status, not a decision"
    );
    assert_eq!(event["files"][1], "tests/api.rs");
    assert!(event["records_cited"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn capture_with_nothing_to_journal_rejects() {
    let dir = temp_dir("capture-empty");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let server = test_server(state);

    let empty = server
        .post("/api/v1/capture")
        .json(&json!({ "host": "claude-code", "summary": "   " }))
        .await;
    empty.assert_status_bad_request();
    assert!(empty.text().contains("nothing to journal"));
    assert!(!moosedev::policy::fires::fires_log_path_for(&dir).exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn automatic_capture_reports_journal_write_failure() {
    let dir = temp_dir("capture-write-failure");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    std::fs::create_dir(moosedev::policy::fires::fires_log_path_for(&dir))
        .expect("make fire-log path unwritable as a file");
    let server = test_server(state);

    let response = server
        .post("/api/v1/capture")
        .json(&json!({
            "host": "opencode",
            "files": ["src/lib.rs"]
        }))
        .await;

    response.assert_status_internal_server_error();
    assert!(response.text().contains("failed to journal capture"));

    let _ = std::fs::remove_dir_all(&dir);
}
