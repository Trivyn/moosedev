//! Architecture-shape validation over the durable project KG.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation::{self, ViolationKind};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};

#[test]
fn normal_capture_conforms_to_required_information_record_fields() {
    let dir = std::env::temp_dir().join(format!("moosedev-validation-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Adopt validation".to_string(),
                ),
                (state.capture.title.clone(), "Adopt validation".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        report.conforms(),
        "normal capture should conform:\n{}",
        validation::format_report(&report)
    );
    assert!(
        report.skipped > 0,
        "the report should disclose unsupported SHACL constraints from the shipped shapes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_raw_instance_reports_missing_required_fields() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-validation-missing-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    insert_raw_decision(&state, &class_iri, "https://moosedev.dev/kg/test/missing");

    let report = validation::validate_project(&state).expect("validate project");
    assert!(!report.conforms());
    assert!(report
        .violations
        .iter()
        .any(|v| { v.kind == ViolationKind::MissingRequired && v.path == state.capture.author }));
    assert!(report.violations.iter().any(|v| {
        v.kind == ViolationKind::MissingRequired && v.path == state.capture.timestamp
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_raw_instance_reports_timestamp_datatype_mismatch() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-validation-datatype-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let subject = insert_raw_decision(&state, &class_iri, "https://moosedev.dev/kg/test/bad-date");
    insert_literal(&state, &subject, &state.capture.author, "test-agent");
    insert_literal(&state, &subject, &state.capture.status, "proposed");
    insert_literal(&state, &subject, &state.capture.timestamp, "not-a-datetime");

    let report = validation::validate_project(&state).expect("validate project");
    assert!(!report.conforms());
    assert!(report.violations.iter().any(|v| {
        v.kind == ViolationKind::DatatypeMismatch && v.path == state.capture.timestamp
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn under_linked_record_is_advised_without_breaking_conformance() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-validation-advisory-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    // A well-formed decision (record_instance fills author/status/timestamp) with
    // no isMotivatedBy link — fully conformant, but under-linked.
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let ad = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Adopt a local cache".to_string(),
                ),
                (state.capture.title.clone(), "Adopt a local cache".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        report.conforms() && report.violations.is_empty(),
        "advisories must be non-blocking:\n{}",
        validation::format_report(&report)
    );
    assert!(
        report
            .advisories
            .iter()
            .any(|a| a.node == ad && a.missing_predicate == "isMotivatedBy"),
        "an AD with no isMotivatedBy should be advised:\n{}",
        validation::format_report(&report)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Insert a deliberately incomplete instance directly into the project graph,
/// bypassing `record_instance`, so validation can prove malformed data is caught.
fn insert_raw_decision(state: &AppState, class_iri: &str, iri: &str) -> String {
    let subject = NamedNode::new(iri).unwrap();
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap());
    state
        .store
        .insert(&Quad::new(
            subject.clone(),
            NamedNode::new(moose::RDF_TYPE).unwrap(),
            NamedNode::new(class_iri).unwrap(),
            graph.clone(),
        ))
        .unwrap();
    state
        .store
        .insert(&Quad::new(
            subject.clone(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Literal::new_simple_literal("Malformed decision"),
            graph,
        ))
        .unwrap();
    subject.as_str().to_string()
}

/// Add an untyped literal to a raw fixture instance.
fn insert_literal(state: &AppState, subject_iri: &str, predicate_iri: &str, value: &str) {
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(subject_iri).unwrap(),
            NamedNode::new(predicate_iri).unwrap(),
            Literal::new_simple_literal(value),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}
