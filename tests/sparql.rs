//! Raw SPARQL escape hatch: deterministic, read-only querying over the loaded
//! named graphs without the NLQ pipeline.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::sparql;
use serde_json::Value;

#[test]
fn sparql_selects_recorded_instances_and_serializes_ask() {
    let dir = std::env::temp_dir().join(format!("moosedev-sparql-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: class_iri.clone(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Adopt SPARQL".to_string()),
                (state.capture.title.clone(), "Adopt SPARQL".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    let select = format!("SELECT ?s WHERE {{ ?s a <{class_iri}> }}");
    let json: Value =
        serde_json::from_str(&sparql::run_query(&state.store, &select).expect("run SELECT"))
            .expect("SPARQL JSON");
    assert!(
        json["results"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|binding| binding["s"]["value"] == iri),
        "SELECT should return the recorded instance: {json}"
    );

    let ask = format!("ASK {{ <{iri}> a <{class_iri}> }}");
    let json: Value =
        serde_json::from_str(&sparql::run_query(&state.store, &ask).expect("run ASK"))
            .expect("SPARQL JSON");
    assert_eq!(json["boolean"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sparql_serializes_construct_as_ntriples() {
    let dir =
        std::env::temp_dir().join(format!("moosedev-sparql-construct-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: class_iri.clone(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Construct me".to_string()),
                (state.capture.title.clone(), "Construct me".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    let construct = format!("CONSTRUCT {{ <{iri}> ?p ?o }} WHERE {{ <{iri}> ?p ?o }}");
    let ntriples = sparql::run_query(&state.store, &construct).expect("run CONSTRUCT");
    assert!(
        ntriples.contains(&format!("<{iri}>")),
        "CONSTRUCT should serialize the subject as N-Triples:\n{ntriples}"
    );
    assert!(
        ntriples.contains(moose::RDF_TYPE),
        "CONSTRUCT should include the typed assertion:\n{ntriples}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sparql_rejects_update_syntax() {
    let dir = std::env::temp_dir().join(format!("moosedev-sparql-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let before = state.store.iter().count();

    let err = sparql::run_query(
        &state.store,
        "INSERT DATA { <https://example.test/s> <https://example.test/p> <https://example.test/o> }",
    )
    .expect_err("SPARQL UPDATE should be rejected by the read-only query parser");
    assert!(
        err.to_string().contains("parse query"),
        "expected parse-query rejection, got: {err}"
    );
    assert_eq!(
        state.store.iter().count(),
        before,
        "rejected UPDATE must not change the store"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
