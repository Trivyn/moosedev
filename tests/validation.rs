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
fn mistyped_or_relationship_now_blocks_conformance() {
    let dir = std::env::temp_dir().join(format!("moosedev-validation-or-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let ad_class = state.resolve_class("ArchitecturalDecision").unwrap();
    let component_class = state.resolve_class("SystemComponent").unwrap();
    let decision = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: ad_class,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Adopt a cache".to_string()),
                (state.capture.title.clone(), "Adopt a cache".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision");
    let component = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: component_class,
            class_local: "SystemComponent".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), "Cache worker".to_string()),
                (state.capture.title.clone(), "Cache worker".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record component");

    insert_iri(
        &state,
        &decision,
        &state.resolve_object_property("isMotivatedBy").unwrap(),
        &component,
    );

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        !report.conforms(),
        "isMotivatedBy -> SystemComponent should violate the sh:or range:\n{}",
        validation::format_report(&report)
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| matches!(v.kind, ViolationKind::Other(_))),
        "expected a non-M3 SHACL Core violation; got {:?}",
        report.violations
    );

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
                (
                    state.capture.title.clone(),
                    "Adopt a local cache".to_string(),
                ),
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

fn insert_iri(state: &AppState, subject_iri: &str, predicate_iri: &str, object_iri: &str) {
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(subject_iri).unwrap(),
            NamedNode::new(predicate_iri).unwrap(),
            NamedNode::new(object_iri).unwrap(),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}

/// A `supersedes` edge whose target was never flipped to `superseded` leaves a
/// replaced record in the working set, so recall returns it as current. SHACL
/// cannot express this — snarl is SHACL Core, with no `sh:sparql` — so it is
/// checked in Rust until that support lands. Three live instances existed across
/// the stores when this was written, two unnoticed for a month.
#[test]
fn unflipped_supersession_breaks_conformance() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-validation-unflipped-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let replacement = record_accepted(&state, "The replacement");
    let predecessor = record_accepted(&state, "The predecessor");
    assert!(
        validation::validate_project(&state)
            .expect("validate")
            .conforms(),
        "two ordinary accepted records must conform before the drift is introduced"
    );

    // Raw insert: the write path now refuses this, which is the point of the guard —
    // only a pre-existing store (or a direct quad write) can still hold one.
    let supersedes = state.resolve_object_property("supersedes").unwrap();
    insert_iri(&state, &replacement, &supersedes, &predecessor);

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        !report.conforms(),
        "an unflipped supersession must break conformance:\n{}",
        validation::format_report(&report)
    );
    let drift = report
        .violations
        .iter()
        .find(|v| v.kind == ViolationKind::Other("UnflippedSupersession".to_string()))
        .expect("the unflipped supersession is reported");
    assert_eq!(drift.node, predecessor, "the PREDECESSOR is the drifted node");
    assert!(
        drift.detail.contains("repair_unflipped_supersessions"),
        "the violation names its repair: {}",
        drift.detail
    );

    // Flipping the predecessor is exactly what the two supported write paths do.
    insert_literal(&state, &predecessor, &state.capture.status, "superseded");
    remove_literal(&state, &predecessor, &state.capture.status, "accepted");
    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        report.conforms(),
        "a flipped supersession conforms again:\n{}",
        validation::format_report(&report)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A PROPOSED replacement carrying a `supersedes` edge is the deferred
/// supersession path, not drift: `accept_proposed_supersession` flips the
/// predecessor when a human ratifies it, and until then the predecessor is
/// correctly still current. `guard_supersession_edge` admits exactly this edge,
/// so a detector that flagged it would fire on the one workflow the guard exists
/// to preserve — which it did, on the first real proposal drafted after #49.
#[test]
fn pending_proposed_supersession_still_conforms() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-validation-pending-supersession-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let predecessor = record_accepted(&state, "The ratified decision");
    let replacement = record_with_status(&state, "The proposed correction", "proposed");

    // The write path ALLOWS this one — a proposed subject is the deferred path's input.
    let supersedes = state.resolve_object_property("supersedes").unwrap();
    insert_iri(&state, &replacement, &supersedes, &predecessor);

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        report.conforms(),
        "a pending proposed supersession is not drift:\n{}",
        validation::format_report(&report)
    );

    // Ratification is what puts the edge in force; only THEN must the predecessor
    // have been flipped, and the check goes back to noticing when it was not.
    insert_literal(&state, &replacement, &state.capture.status, "accepted");
    remove_literal(&state, &replacement, &state.capture.status, "proposed");
    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        !report.conforms(),
        "once the replacement is accepted the unflipped predecessor is drift again:\n{}",
        validation::format_report(&report)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn record_accepted(state: &AppState, title: &str) -> String {
    record_with_status(state, title, "accepted")
}

fn record_with_status(state: &AppState, title: &str, status: &str) -> String {
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), status.to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision")
}

fn remove_literal(state: &AppState, subject_iri: &str, predicate_iri: &str, value: &str) {
    state
        .store
        .remove(&Quad::new(
            NamedNode::new(subject_iri).unwrap(),
            NamedNode::new(predicate_iri).unwrap(),
            Literal::new_simple_literal(value),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}
