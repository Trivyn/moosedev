//! NLQ query path: record a couple of decisions, then query the project KG in
//! pure-symbolic mode (no LLM) and confirm MOOSE's graph-walk pipeline runs
//! end-to-end and returns an execution trace. Forcing `PureSymbolic` keeps the
//! test hermetic — it never reaches out to an LLM endpoint.

use std::path::Path;

use moose::types::LlmAssistLevel;
use moosedev::graph::{self, AppState, RecordInput};

#[tokio::test]
async fn query_runs_pure_symbolic_over_recorded_decisions() {
    let dir = std::env::temp_dir().join(format!("moosedev-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    let mut state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    // Force pure-symbolic so the test never reaches out to an LLM endpoint.
    state.engine_config.llm_assist_level = LlmAssistLevel::PureSymbolic;

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
