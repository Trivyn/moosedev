//! NLQ query path: record a couple of decisions, then query the project KG in
//! pure-symbolic mode (no LLM) and confirm MOOSE's graph-walk pipeline runs
//! end-to-end and returns an execution trace. Forcing `PureSymbolic` keeps the
//! test hermetic — it never reaches out to an LLM endpoint.

use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use moose::traits::LlmClient;
use moose::types::{EngineError, LlmAssistLevel, LlmParams};
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::llm::LlmConfig;

struct MetaIntentLlm;

#[async_trait]
impl LlmClient for MetaIntentLlm {
    async fn chat_completion(
        &self,
        _model: &str,
        prompt: &str,
        _params: Option<&LlmParams>,
    ) -> Result<String, EngineError> {
        if prompt.contains("Respond with ONLY a JSON object")
            && prompt.contains("\"intent\"")
            && prompt.contains("\"object\"")
        {
            return Ok(
                r#"{"intent":"meta","object":"architectural decisions","modifiers":[]}"#
                    .to_string(),
            );
        }

        Ok(r#"{"summary":"synthetic answer","relevant_iris":[],"confidence":"high","unanswered_aspects":[]}"#.to_string())
    }
}

#[tokio::test]
async fn query_runs_pure_symbolic_over_recorded_decisions() {
    let dir = std::env::temp_dir().join(format!("moosedev-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    let state = AppState::bootstrap_with_llm_config(
        &dir,
        &ontology_dir,
        LlmConfig {
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: "test".to_string(),
            model: "fake-model".to_string(),
            configured: false,
        },
    )
    .expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    for title in [
        "Adopt rmcp for the MCP transport",
        "Use the oxigraph on-disk store for the durable KG",
    ] {
        graph::record_instance(
            &state,
            &RecordInput {
                class_iri: class_iri.clone(),
                class_local: "ArchitecturalDecision".to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), title.to_string()),
                    (state.capture.status.clone(), "accepted".to_string()),
                ],
            },
            "test-agent",
            Utc::now(),
        )
        .expect("record decision");
    }

    let result = graph::query(&state, "list the architectural decisions")
        .await
        .expect("query should run without error in pure-symbolic mode");

    // The pipeline ran symbolically (no LLM sensors fired) and produced a trace.
    assert!(
        result.trace.contains("pure symbolic"),
        "trace should show no LLM sensors fired; got:\n{}",
        result.trace
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn query_does_not_surface_schema_spec_internal_error_for_project_records() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-query-schema-regression-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    let mut state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    state.engine_config.llm_assist_level = LlmAssistLevel::Sensor;

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Keep project knowledge in the durable graph".to_string(),
                ),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    let result = graph::query_with_llm_client(
        &state,
        &MetaIntentLlm,
        "fake-model",
        "What are the current architectural decisions for this project?",
    )
    .await
    .expect("query should run");

    assert!(
        !result
            .answer
            .contains("Schema query intent set but no SchemaQuerySpec"),
        "internal schema contract violation leaked to query answer:\n{}",
        result.answer
    );
    assert!(
        result.trace.contains("assist level: PureSymbolic"),
        "schema-contract fallback should retry symbolically; trace:\n{}",
        result.trace
    );

    let _ = std::fs::remove_dir_all(&dir);
}
