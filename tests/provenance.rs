//! PROV-O edit provenance: recording an instance, then writing + reading back
//! its edit provenance (who asserted it, when, via which activity). Hermetic.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::provenance;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

const PROV_GENERATED_AT_TIME: &str = "http://www.w3.org/ns/prov#generatedAtTime";

#[test]
fn records_and_reads_edit_provenance() {
    let dir = std::env::temp_dir().join(format!("moosedev-prov-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![(moose::RDFS_LABEL.to_string(), "Adopt rmcp".to_string())],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    // Nothing recorded yet.
    assert!(provenance::read_provenance(&state.store, &iri)
        .unwrap()
        .is_none());

    let when = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
    provenance::record_provenance_at(&state.store, &iri, "test-agent", when)
        .expect("write provenance");

    let p = provenance::read_provenance(&state.store, &iri)
        .unwrap()
        .expect("provenance should be present");
    assert_eq!(p.agent, "test-agent");
    assert_eq!(DateTime::parse_from_rfc3339(&p.time).unwrap(), when);
    assert!(
        p.activity.starts_with("https://moosedev.dev/kg/Activity/"),
        "activity IRI: {}",
        p.activity
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn domain_timestamp_matches_provenance_generated_time() {
    let dir = std::env::temp_dir().join(format!("moosedev-prov-time-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let when = Utc.with_ymd_and_hms(2026, 6, 17, 12, 30, 0).unwrap();
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![(moose::RDFS_LABEL.to_string(), "One timestamp".to_string())],
        },
        "test-agent",
        when,
    )
    .expect("record decision");
    provenance::record_provenance_at(&state.store, &iri, "test-agent", when)
        .expect("write provenance");

    let domain_time = literal_value(&state, PROJECT_KG_GRAPH_IRI, &iri, &state.capture.timestamp)
        .expect("domain timestamp");
    let provenance_time = literal_value(
        &state,
        provenance::PROVENANCE_GRAPH_IRI,
        &iri,
        PROV_GENERATED_AT_TIME,
    )
    .expect("provenance generatedAtTime");

    assert_eq!(
        DateTime::parse_from_rfc3339(&domain_time).unwrap(),
        DateTime::parse_from_rfc3339(&provenance_time).unwrap(),
        "domain hasTimestamp and prov:generatedAtTime should be the same instant"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Read one literal value from a named graph for timestamp parity assertions.
fn literal_value(
    state: &AppState,
    graph_iri: &str,
    subject_iri: &str,
    predicate_iri: &str,
) -> Option<String> {
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject_iri).unwrap().into()),
            Some(NamedNodeRef::new(predicate_iri).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(graph_iri).unwrap(),
            )),
        )
        .flatten()
        .find_map(|q| match q.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
}
