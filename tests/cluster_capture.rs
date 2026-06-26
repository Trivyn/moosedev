//! Inline cluster capture: `alternatives_considered` / `consequences` mint the typed
//! Alternative/Consequence nodes and link them (weighs/resultsIn) in ONE call, and
//! empty fields mint nothing (no coercion).

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, ClusterSlot, RecordInput};
use moosedev::sparql;
use serde_json::Value;

fn ask(state: &AppState, query: &str) -> bool {
    let json: Value =
        serde_json::from_str(&sparql::run_query(&state.store, query).expect("run ASK"))
            .expect("SPARQL JSON");
    json["boolean"].as_bool().unwrap_or(false)
}

#[test]
fn cluster_mints_alternatives_and_consequences_and_links_them() {
    let dir = std::env::temp_dir().join(format!("moosedev-cluster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let input = RecordInput {
        class_iri,
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), "Adopt RocksDB".to_string()),
            (state.capture.title.clone(), "Adopt RocksDB".to_string()),
        ],
    };
    let alternatives = vec!["Postgres - needs a separate server".to_string()];
    let consequences = vec!["Larger static binary".to_string()];
    let cluster = [
        ClusterSlot {
            predicate_local: "weighs",
            range_class_local: "Alternative",
            labels: &alternatives,
        },
        ClusterSlot {
            predicate_local: "resultsIn",
            range_class_local: "Consequence",
            labels: &consequences,
        },
    ];
    let (outcome, minted) = graph::record_decision_with_cluster(
        &state,
        &input,
        &[],
        &cluster,
        "test-agent",
        Utc::now(),
    )
    .expect("record with cluster");
    assert_eq!(minted.len(), 2, "one Alternative + one Consequence minted");

    let weighs = state.resolve_object_property("weighs").unwrap();
    let results_in = state.resolve_object_property("resultsIn").unwrap();
    let alt_class = state.resolve_class("Alternative").unwrap();
    let cons_class = state.resolve_class("Consequence").unwrap();
    let label = moose::RDFS_LABEL;
    let g = graph::PROJECT_KG_GRAPH_IRI;
    let d = &outcome.iri;

    assert!(
        ask(
            &state,
            &format!(
                "ASK {{ GRAPH <{g}> {{ <{d}> <{weighs}> ?a . ?a a <{alt_class}> ; <{label}> ?l . FILTER(CONTAINS(STR(?l), \"Postgres\")) }} }}"
            )
        ),
        "decision should weigh an Alternative carrying the supplied label"
    );
    assert!(
        ask(
            &state,
            &format!(
                "ASK {{ GRAPH <{g}> {{ <{d}> <{results_in}> ?c . ?c a <{cons_class}> ; <{label}> ?l . FILTER(CONTAINS(STR(?l), \"Larger\")) }} }}"
            )
        ),
        "decision should result in a Consequence carrying the supplied label"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_cluster_fields_mint_nothing() {
    let dir = std::env::temp_dir().join(format!("moosedev-cluster-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let input = RecordInput {
        class_iri,
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), "No cluster here".to_string()),
            (state.capture.title.clone(), "No cluster here".to_string()),
        ],
    };
    // Empty (and whitespace-only) labels must not coerce a node into existence.
    let alternatives = vec!["   ".to_string()];
    let empty: Vec<String> = Vec::new();
    let cluster = [
        ClusterSlot {
            predicate_local: "weighs",
            range_class_local: "Alternative",
            labels: &alternatives,
        },
        ClusterSlot {
            predicate_local: "resultsIn",
            range_class_local: "Consequence",
            labels: &empty,
        },
    ];
    let (_outcome, minted) = graph::record_decision_with_cluster(
        &state,
        &input,
        &[],
        &cluster,
        "test-agent",
        Utc::now(),
    )
    .expect("record without cluster");
    assert!(
        minted.is_empty(),
        "no cluster nodes when fields are empty/blank"
    );

    let alt_class = state.resolve_class("Alternative").unwrap();
    let g = graph::PROJECT_KG_GRAPH_IRI;
    assert!(
        !ask(
            &state,
            &format!("ASK {{ GRAPH <{g}> {{ ?a a <{alt_class}> }} }}")
        ),
        "no Alternative node should exist (no coercion)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
