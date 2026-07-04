//! Relationship write-path validation against SHACL shape constraints.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!("moosedev-relate-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record item")
}

fn has_edge(state: &AppState, subject: &str, predicate: &str, object: &str) -> bool {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            Some(NamedNodeRef::new(object).unwrap().into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .next()
        .is_some()
}

fn literal_values(state: &AppState, subject: &str, predicate: &str) -> Vec<String> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn relate_accepts_ir_to_ir_edge_from_shacl_or() {
    let state = bootstrap("ir-ir");
    let decision = record(&state, "ArchitecturalDecision", "Adopt local cache");
    let requirement = record(&state, "Requirement", "Avoid repeated graph scans");
    let predicate = state.resolve_object_property("isMotivatedBy").unwrap();

    graph::relate(&state, &decision, "isMotivatedBy", &requirement).expect("relate");

    assert!(has_edge(&state, &decision, &predicate, &requirement));
}

#[test]
fn relate_accepts_component_edges_from_direct_and_or_shapes() {
    let state = bootstrap("component");
    let decision = record(&state, "ArchitecturalDecision", "Extract graph writer");
    let constraint = record(&state, "Constraint", "Graph writes stay local");
    let component = record(&state, "SystemComponent", "graph writer module");

    let concerns = state.resolve_object_property("concerns").unwrap();
    let constrains = state.resolve_object_property("constrains").unwrap();

    graph::relate(&state, &decision, "concerns", &component).expect("decision concerns component");
    graph::relate(&state, &constraint, "constrains", &component)
        .expect("constraint constrains component");

    assert!(has_edge(&state, &decision, &concerns, &component));
    assert!(has_edge(&state, &constraint, &constrains, &component));
}

#[test]
fn lesson_concerns_component_enriches_inverse_and_validates() {
    let state = bootstrap("lesson-concerns-component");
    let lesson = record(&state, "Lesson", "Inline concerns can target components");
    let component = record(&state, "SystemComponent", "graph/store layer");

    let concerns = state.resolve_object_property("concerns").unwrap();
    let inverse = state.resolve_object_property("isConcernedBy").unwrap();
    graph::relate(&state, &lesson, "concerns", &component).expect("lesson concerns component");

    assert!(has_edge(&state, &lesson, &concerns, &component));
    state.ensure_enriched();
    assert!(
        has_edge(&state, &component, &inverse, &lesson),
        "GROWL should materialize isConcernedBy inverse for InformationRecord subjects"
    );
    let report = validation::validate_project(&state).expect("validate project");
    assert!(report.conforms(), "{}", validation::format_report(&report));
}

#[test]
fn relate_rejects_out_of_range_edge_and_writes_nothing() {
    let state = bootstrap("reject");
    let anti_pattern = record(&state, "AntiPattern", "Hardcoded namespace");
    let requirement = record(&state, "Requirement", "Resolve terms by local name");
    let violates = state.resolve_object_property("violates").unwrap();

    let err = match graph::relate(&state, &anti_pattern, "violates", &requirement) {
        Ok(_) => panic!("Requirement is outside violates range"),
        Err(err) => err,
    };
    let msg = err.to_string();
    assert!(msg.contains("object"), "error names object endpoint: {msg}");
    assert!(
        msg.contains("Requirement") && msg.contains("Constraint"),
        "error names actual and expected classes: {msg}"
    );
    assert!(
        !has_edge(&state, &anti_pattern, &violates, &requirement),
        "invalid relate must not write the edge"
    );
}

#[test]
fn system_component_capture_uses_declared_label_property_and_validates() {
    let state = bootstrap("component-label");
    let component = record(&state, "SystemComponent", "extraction module");
    let component_name = state
        .arch_vocab
        .datatype_properties
        .iter()
        .find(|entry| entry.local_name == "hasComponentName")
        .map(|entry| entry.iri.clone())
        .expect("hasComponentName datatype property");

    assert_eq!(
        literal_values(&state, &component, moose::RDFS_LABEL),
        vec!["extraction module"]
    );
    assert_eq!(
        literal_values(&state, &component, &component_name),
        vec!["extraction module"]
    );
    assert!(
        literal_values(&state, &component, &state.capture.title).is_empty(),
        "SystemComponent should mirror rdfs:label into hasComponentName instead of hasTitle"
    );

    let report = validation::validate_project(&state).expect("validate project");
    assert!(
        report.conforms(),
        "SystemComponent capture should validate:\n{}",
        validation::format_report(&report)
    );
}
