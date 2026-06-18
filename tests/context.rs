//! `get_relevant_context` retrieval: record a couple of decisions, then confirm
//! list-all returns them as structured items and a topic filters by label.
//! Symbolic — no LLM, fully hermetic.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};

fn record(state: &AppState, class_iri: &str, title: &str) {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: class_iri.to_string(),
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

#[test]
fn relevant_context_lists_all_and_filters_by_topic() {
    let dir = std::env::temp_dir().join(format!("moosedev-context-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    record(&state, &class_iri, "Adopt rmcp for the MCP transport");
    record(
        &state,
        &class_iri,
        "Use the oxigraph on-disk store for the durable KG",
    );

    // No topic → both items, with their type, label, and status property.
    let all = graph::relevant_context(&state, None, 10, false).expect("list all");
    assert_eq!(all.len(), 2, "should list both recorded decisions");
    assert!(all.iter().all(|i| i.kind == "ArchitecturalDecision"));
    assert!(all.iter().any(|i| i
        .properties
        .iter()
        .any(|(k, v)| k == "hasLifecycleStatus" && v == "accepted")));

    // Topic → BM25 lexical relevance over each record's label + description
    // (moose `search_records`). "rmcp" appears in the rmcp decision's label.
    let hits = graph::relevant_context(&state, Some("rmcp"), 10, false).expect("topic search");
    assert!(
        hits.iter().any(|i| i.label.contains("rmcp")),
        "topic 'rmcp' should match the rmcp decision; got {:?}",
        hits.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    // Honest empty-state (invariant #6): a topic that shares no term with any
    // record returns nothing rather than padded noise — `search_records` excludes
    // zero-match records, so relevance is never asserted where none exists.
    let none = graph::relevant_context(&state, Some("kubernetes helm rollout"), 10, false)
        .expect("no-match topic");
    assert!(
        none.is_empty(),
        "topic sharing no term with any record should return nothing; got {:?}",
        none.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
