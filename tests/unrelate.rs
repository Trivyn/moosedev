//! `unrelate`: removing one asserted edge, its stored inverse and (after
//! enrichment) its reasoner-inferred copy; refusals write nothing.

use std::path::Path;

use chrono::Utc;
use moosedev::export::export_canonical_project;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphNameRef, NamedNodeRef, Quad};

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!("moosedev-unrelate-{name}-{}", std::process::id()));
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

/// Insert a raw quad into the project graph, bypassing `relate`'s guards.
fn insert_raw(state: &AppState, subject: &str, predicate: &str, object: &str) {
    state
        .store
        .insert(&Quad::new(
            oxigraph::model::NamedNode::new(subject).unwrap(),
            oxigraph::model::NamedNode::new(predicate).unwrap(),
            oxigraph::model::NamedNode::new(object).unwrap(),
            oxigraph::model::GraphName::NamedNode(
                oxigraph::model::NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            ),
        ))
        .unwrap();
}

/// A decision motivated by a requirement: (decision, requirement, isMotivatedBy, motivates).
fn motivated_pair(state: &AppState) -> (String, String, String, String) {
    let requirement = record(state, "Requirement", "Avoid repeated graph scans");
    let decision = record(state, "ArchitecturalDecision", "Adopt local cache");
    let forward = state.resolve_object_property("isMotivatedBy").unwrap();
    let inverse = state.resolve_object_property("motivates").unwrap();
    (decision, requirement, forward, inverse)
}

#[test]
fn unrelate_removes_the_asserted_edge() {
    let state = bootstrap("removes");
    let (decision, requirement, forward, _) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    assert!(has_edge(&state, &decision, &forward, &requirement));

    let out = graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    assert!(out.removed);
    assert!(!out.inverse_removed);
    assert_eq!(out.predicate_iri, forward);
    assert!(!has_edge(&state, &decision, &forward, &requirement));
}

#[test]
fn unrelate_removes_an_explicitly_stored_inverse() {
    let state = bootstrap("stored-inverse");
    let (decision, requirement, forward, inverse) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    insert_raw(&state, &requirement, &inverse, &decision);

    let out = graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    assert!(out.removed && out.inverse_removed);
    assert!(!has_edge(&state, &decision, &forward, &requirement));
    assert!(!has_edge(&state, &requirement, &inverse, &decision));
}

#[test]
fn unrelate_from_the_inverse_side_removes_the_stored_forward_edge() {
    let state = bootstrap("inverse-side");
    let (decision, requirement, forward, inverse) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    let out = graph::unrelate(&state, &requirement, "motivates", &decision).unwrap();

    assert!(!out.removed);
    assert!(out.inverse_removed);
    assert!(!has_edge(&state, &decision, &forward, &requirement));
    assert!(!has_edge(&state, &requirement, &inverse, &decision));
}

#[test]
fn unrelate_drops_the_reasoner_inferred_inverse() {
    let state = bootstrap("inferred");
    let (decision, requirement, forward, inverse) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    state.ensure_enriched();
    assert!(
        has_edge(&state, &requirement, &inverse, &decision),
        "enrichment must materialize the inverse for this test to mean anything"
    );

    let out = graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    assert!(out.removed);
    state.ensure_enriched();

    assert!(!has_edge(&state, &decision, &forward, &requirement));
    assert!(!has_edge(&state, &requirement, &inverse, &decision));
}

#[test]
fn unrelate_is_idempotent() {
    let state = bootstrap("idempotent");
    let (decision, requirement, _, _) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    let again = graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    assert!(!again.removed);
    assert!(!again.inverse_removed);
}

#[test]
fn unrelate_refuses_lifecycle_judgment_and_realizes_edges() {
    let state = bootstrap("refused");
    let a = record(&state, "ArchitecturalDecision", "First");
    let b = record(&state, "ArchitecturalDecision", "Second");
    for (predicate, expected) in [
        ("supersedes", "supersede_decision"),
        ("isSupersededBy", "supersede_decision"),
        ("hasRationale", "supersede_decision"),
        ("isRationaleFor", "supersede_decision"),
        ("playsRole", "ratification-only"),
        ("hasCriticality", "ratification-only"),
        ("realizes", "link_code"),
        ("isRealizedBy", "link_code"),
    ] {
        let predicate_iri = state.resolve_object_property(predicate).unwrap();
        insert_raw(&state, &a, &predicate_iri, &b);

        let err = graph::unrelate(&state, &a, predicate, &b)
            .expect_err(predicate)
            .to_string();

        assert!(err.contains(expected), "{predicate}: {err}");
        assert!(has_edge(&state, &a, &predicate_iri, &b), "{predicate}");
    }
}

#[test]
fn unrelate_rejects_an_unknown_predicate() {
    let state = bootstrap("unknown");
    let (decision, requirement, _, _) = motivated_pair(&state);

    let err = graph::unrelate(&state, &decision, "noSuchRelation", &requirement)
        .unwrap_err()
        .to_string();

    assert!(err.contains("unknown relationship"), "{err}");
}

#[test]
fn canonical_export_no_longer_contains_the_removed_edge() {
    let state = bootstrap("export");
    let (decision, requirement, forward, _) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    let edge_line = |text: &str| {
        text.lines()
            .any(|line| line.contains(&format!("<{decision}> <{forward}> <{requirement}>")))
    };
    assert!(edge_line(
        &export_canonical_project(&state.store).unwrap().text
    ));

    graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();

    assert!(!edge_line(
        &export_canonical_project(&state.store).unwrap().text
    ));
}

#[test]
fn reasserting_an_unrelated_edge_is_not_treated_as_inferred() {
    let state = bootstrap("stale-provenance");
    let (decision, requirement, forward, inverse) = motivated_pair(&state);
    graph::relate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    state.ensure_enriched();
    assert!(has_edge(&state, &requirement, &inverse, &decision));

    graph::unrelate(&state, &decision, "isMotivatedBy", &requirement).unwrap();
    // Re-assert the formerly inferred orientation before the next enrichment.
    graph::relate(&state, &requirement, "motivates", &decision).unwrap();
    state.ensure_enriched();

    assert!(has_edge(&state, &requirement, &inverse, &decision));
    assert!(has_edge(&state, &decision, &forward, &requirement));
    let text = export_canonical_project(&state.store).unwrap().text;
    assert!(
        text.lines()
            .any(|line| line.contains(&format!("<{requirement}> <{inverse}> <{decision}>"))),
        "the re-asserted edge must be exported as asserted"
    );
}

#[test]
fn unrelate_treats_a_symmetric_property_as_its_own_inverse() {
    let state = bootstrap("symmetric");
    let a = record(&state, "ArchitecturalDecision", "First");
    let b = record(&state, "ArchitecturalDecision", "Second");
    let restates = state.resolve_object_property("restates").unwrap();
    insert_raw(&state, &b, &restates, &a);

    let out = graph::unrelate(&state, &a, "restates", &b).unwrap();
    state.ensure_enriched();

    assert!(!out.removed);
    assert!(out.inverse_removed);
    assert!(!has_edge(&state, &a, &restates, &b));
    assert!(!has_edge(&state, &b, &restates, &a));
}
