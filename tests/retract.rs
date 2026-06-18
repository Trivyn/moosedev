//! Retract (in-place deprecate) lifecycle.
//!
//! Proves: retracting a recorded item flips its status to exactly `deprecated`,
//! captures the WHY as a linked `Rationale`, and preserves the record (and its
//! other triples) as history; the precondition rejects bad targets without
//! writing; and the read path hides retracted records by default while the
//! history view surfaces them with the retraction rationale.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!("moosedev-retract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

/// Record a base ArchitecturalDecision (status "accepted") and return its IRI.
fn record_decision(state: &AppState, class_iri: &str, title: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: class_iri.to_string(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record decision")
}

/// Literal object values of `(subject, predicate, *)` in the project graph.
fn literals(state: &AppState, subject: &str, predicate: &str) -> Vec<String> {
    let s = NamedNodeRef::new(subject).unwrap();
    let p = NamedNodeRef::new(predicate).unwrap();
    let g = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(s.into()),
            Some(p),
            None,
            Some(GraphNameRef::NamedNode(g)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .collect()
}

/// Whether the edge `(subject, predicate, object)` exists in the project graph.
fn has_edge(state: &AppState, subject: &str, predicate: &str, object: &str) -> bool {
    let s = NamedNodeRef::new(subject).unwrap();
    let p = NamedNodeRef::new(predicate).unwrap();
    let o = NamedNodeRef::new(object).unwrap();
    let g = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(s.into()),
            Some(p),
            Some(o.into()),
            Some(GraphNameRef::NamedNode(g)),
        )
        .flatten()
        .next()
        .is_some()
}

#[test]
fn retract_marks_deprecated_captures_why_and_preserves_record() {
    let state = bootstrap("marks");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let target = record_decision(&state, &dc, "A decision recorded in error");
    assert_eq!(
        literals(&state, &target, &state.capture.status),
        vec!["accepted"]
    );

    let out = graph::retract_decision(
        &state,
        &target,
        "Recorded in error — duplicates an existing decision.",
        "tester",
        Utc::now(),
    )
    .expect("retract");

    let has_rationale = state.resolve_object_property("hasRationale").unwrap();
    let rationale_class = state.resolve_class("Rationale").unwrap();

    // Status flipped to *exactly* one "deprecated" (prior value removed — no dup).
    assert_eq!(
        literals(&state, &target, &state.capture.status),
        vec!["deprecated"],
        "target flipped to exactly one 'deprecated' status"
    );

    // The record is preserved: still typed, title intact.
    assert!(
        has_edge(&state, &target, moose::RDF_TYPE, &dc),
        "retracted record still present"
    );
    assert_eq!(
        literals(&state, &target, &state.capture.title),
        vec!["A decision recorded in error"],
        "title left intact"
    );

    // The WHY: the record links to a typed Rationale carrying the reason text.
    assert!(has_edge(
        &state,
        &target,
        &has_rationale,
        &out.rationale_iri
    ));
    assert!(has_edge(
        &state,
        &out.rationale_iri,
        moose::RDF_TYPE,
        &rationale_class
    ));
    assert_eq!(
        literals(&state, &out.rationale_iri, &state.capture.description),
        vec!["Recorded in error — duplicates an existing decision."]
    );

    // Everything still conforms to the architecture shapes.
    assert!(
        validation::validate_project(&state).unwrap().conforms(),
        "retract output must conform to the SHACL shapes"
    );
}

#[test]
fn retract_rejects_unknown_target_and_writes_nothing() {
    let state = bootstrap("precond");
    let before = graph::relevant_context(&state, None, 50, true)
        .unwrap()
        .len();

    let result = graph::retract_decision(
        &state,
        "https://moosedev.dev/kg/ArchitecturalDecision/does-not-exist",
        "n/a",
        "tester",
        Utc::now(),
    );
    assert!(
        result.is_err(),
        "retracting a non-existent record must error"
    );

    // No partial write: no Rationale (or anything else) reaches the graph.
    let after = graph::relevant_context(&state, None, 50, true)
        .unwrap()
        .len();
    assert_eq!(
        before, after,
        "precondition failure must leave the graph untouched"
    );
}

#[test]
fn read_path_hides_retracted_by_default_and_history_surfaces_rationale() {
    let state = bootstrap("readpath");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let target = record_decision(&state, &dc, "Decision to be retracted");

    graph::retract_decision(
        &state,
        &target,
        "Superfluous — folded into another decision.",
        "tester",
        Utc::now(),
    )
    .expect("retract");

    // Default view = current working set: the retracted record is hidden.
    let current = graph::relevant_context(&state, None, 50, false).unwrap();
    assert!(
        !current.iter().any(|i| i.iri == target),
        "the retracted record is hidden by default"
    );

    // History view includes it, marked historical, with the rationale text surfaced.
    let history = graph::relevant_context(&state, None, 50, true).unwrap();
    let item = history
        .iter()
        .find(|i| i.iri == target)
        .expect("retracted record appears in history view");
    assert!(item.is_historical());
    assert!(
        item.properties
            .iter()
            .any(|(k, v)| k == "rationale" && v.contains("folded")),
        "history item surfaces the retraction rationale text; got {:?}",
        item.properties
    );
}
