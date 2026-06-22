//! Inline relations on capture: record a typed item AND its forward links in one
//! atomic call, validated against the SHACL domain/range. An invalid relation must
//! fail the whole capture (no orphan record left behind), target resolution accepts
//! IRI or exact title, and bad targets/predicates surface distinct errors.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphNameRef, NamedNodeRef};

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-inline-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

fn input(state: &AppState, kind: &str, title: &str) -> RecordInput {
    RecordInput {
        class_iri: state.resolve_class(kind).expect("known class"),
        class_local: kind.to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), title.to_string()),
            (state.capture.title.clone(), title.to_string()),
            (state.capture.status.clone(), "accepted".to_string()),
        ],
    }
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    graph::record_instance(state, &input(state, kind, title), "tester", Utc::now())
        .expect("record item")
}

fn record_with_relations(
    state: &AppState,
    kind: &str,
    title: &str,
    relations: &[(&str, &str)],
) -> anyhow::Result<graph::RecordOutcome> {
    let rels: Vec<(String, String)> = relations
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    graph::record_instance_with_relation_args(
        state,
        &input(state, kind, title),
        &rels,
        "tester",
        Utc::now(),
    )
}

fn has_edge(state: &AppState, subject: &str, predicate_local: &str, object: &str) -> bool {
    let predicate = state.resolve_object_property(predicate_local).unwrap();
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(&predicate).unwrap()),
            Some(NamedNodeRef::new(object).unwrap().into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .next()
        .is_some()
}

fn count_class(state: &AppState, kind: &str) -> usize {
    let class = state.resolve_class(kind).unwrap();
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            None,
            Some(NamedNodeRef::new(moose::RDF_TYPE).unwrap()),
            Some(NamedNodeRef::new(&class).unwrap().into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .count()
}

#[test]
fn inline_relation_links_and_validates() {
    let state = bootstrap("happy");
    let req = record(&state, "Requirement", "Avoid repeated graph scans");
    let outcome = record_with_relations(
        &state,
        "ArchitecturalDecision",
        "Adopt a local cache",
        &[("isMotivatedBy", "Avoid repeated graph scans")],
    )
    .expect("record + relation");

    assert_eq!(outcome.applied_edges.len(), 1, "one edge applied");
    assert!(has_edge(&state, &outcome.iri, "isMotivatedBy", &req));
    let report = validation::validate_project(&state).expect("validate");
    assert!(report.conforms(), "{}", validation::format_report(&report));
}

#[test]
fn inline_relation_target_by_iri_also_works() {
    let state = bootstrap("by-iri");
    let req = record(&state, "Requirement", "Keep memory current");
    let outcome = record_with_relations(
        &state,
        "ArchitecturalDecision",
        "Supersede on change",
        &[("isMotivatedBy", req.as_str())],
    )
    .expect("record + relation by IRI");
    assert!(has_edge(&state, &outcome.iri, "isMotivatedBy", &req));
}

#[test]
fn illegal_relation_fails_capture_atomically() {
    let state = bootstrap("atomic");
    record(&state, "Requirement", "Resolve terms by local name");
    let before = count_class(&state, "AntiPattern");

    // `violates` ranges over Constraint, not Requirement — must reject.
    let err = record_with_relations(
        &state,
        "AntiPattern",
        "Hardcoded namespace",
        &[("violates", "Resolve terms by local name")],
    )
    .expect_err("illegal relation must fail the capture");
    let msg = err.to_string();
    assert!(
        msg.contains("Requirement") && msg.contains("Constraint"),
        "error names actual + expected classes: {msg}"
    );

    // Atomicity: no AntiPattern record was written.
    assert_eq!(
        count_class(&state, "AntiPattern"),
        before,
        "a failed inline relation must leave no orphan record"
    );
}

#[test]
fn distinct_errors_for_unknown_predicate_and_missing_and_ambiguous_targets() {
    let state = bootstrap("errors");
    record(&state, "Requirement", "Avoid graph scans");

    let e = record_with_relations(
        &state,
        "ArchitecturalDecision",
        "D1",
        &[("frobnicates", "Avoid graph scans")],
    )
    .expect_err("unknown predicate");
    assert!(e.to_string().contains("unknown relationship"), "{e}");

    let e = record_with_relations(
        &state,
        "ArchitecturalDecision",
        "D2",
        &[("isMotivatedBy", "No such requirement")],
    )
    .expect_err("missing target");
    assert!(e.to_string().contains("matches no recorded item"), "{e}");

    // Two requirements sharing a title make the target ambiguous.
    record(&state, "Requirement", "Shared title");
    record(&state, "Requirement", "Shared title");
    let e = record_with_relations(
        &state,
        "ArchitecturalDecision",
        "D3",
        &[("isMotivatedBy", "Shared title")],
    )
    .expect_err("ambiguous target");
    assert!(e.to_string().contains("ambiguous"), "{e}");
}
