//! Supersede-with-rationale lifecycle + generic object-property capture.
//!
//! Proves: a decision change links new -supersedes-> old, captures the WHY as a
//! linked `Rationale`, and flips the old decision to `superseded` while preserving
//! it (and all its other triples) as history; the precondition rejects bad
//! targets without writing; the read path hides retired records by default and
//! surfaces the chain; and `record_instance_with_relations` writes relations.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, SupersedeInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

fn bootstrap(name: &str) -> AppState {
    let dir =
        std::env::temp_dir().join(format!("moosedev-supersede-{name}-{}", std::process::id()));
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

fn decision_input(state: &AppState, class_iri: &str, title: &str) -> RecordInput {
    RecordInput {
        class_iri: class_iri.to_string(),
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), title.to_string()),
            (state.capture.title.clone(), title.to_string()),
        ],
    }
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
fn supersede_links_records_captures_why_and_preserves_old() {
    let state = bootstrap("links");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, &dc, "Use a Unix socket for the backend");
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["accepted"]
    );

    let out = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Use TCP for the backend"),
            rationale: "Unix sockets can't serve remote clients; TCP enables cross-host agents."
                .to_string(),
        },
        "tester",
        Utc::now(),
    )
    .expect("supersede");

    let supersedes = state.resolve_object_property("supersedes").unwrap();
    let has_rationale = state.resolve_object_property("hasRationale").unwrap();
    let rationale_class = state.resolve_class("Rationale").unwrap();

    // The links: new -supersedes-> old, new -hasRationale-> rationale node.
    assert!(has_edge(&state, &out.new_iri, &supersedes, &old));
    assert!(has_edge(
        &state,
        &out.new_iri,
        &has_rationale,
        &out.rationale_iri
    ));

    // The WHY: a typed Rationale carrying the reason text.
    assert!(has_edge(
        &state,
        &out.rationale_iri,
        moose::RDF_TYPE,
        &rationale_class
    ));
    assert_eq!(
        literals(&state, &out.rationale_iri, &state.capture.description),
        vec!["Unix sockets can't serve remote clients; TCP enables cross-host agents."]
    );

    // The new decision is current.
    assert_eq!(
        literals(&state, &out.new_iri, &state.capture.status),
        vec!["accepted"]
    );

    // The OLD decision is preserved as history: still typed, title intact, and its
    // status is *exactly* "superseded" (the flip removed the prior value — no dup).
    assert!(
        has_edge(&state, &old, moose::RDF_TYPE, &dc),
        "old decision still present"
    );
    assert_eq!(
        literals(&state, &old, &state.capture.title),
        vec!["Use a Unix socket for the backend"],
        "old title left intact"
    );
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["superseded"],
        "old flipped to exactly one 'superseded' status"
    );

    // Everything still conforms to the architecture shapes.
    assert!(
        validation::validate_project(&state).unwrap().conforms(),
        "supersede output must conform to the SHACL shapes"
    );
}

#[test]
fn supersede_preserves_type_for_non_decision_records() {
    let state = bootstrap("nonad");
    let req_class = state.resolve_class("Requirement").unwrap();
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();

    // A Requirement — NOT an ArchitecturalDecision — was unsupersedable before.
    let old = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: req_class.clone(),
            class_local: "Requirement".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Support remote clients".to_string(),
                ),
                (
                    state.capture.title.clone(),
                    "Support remote clients".to_string(),
                ),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record requirement");

    // Supersede it, deliberately passing a DIFFERENT caller kind (decision_input
    // builds an ArchitecturalDecision) to prove the replacement is minted as the
    // superseded record's type, not the caller's.
    let out = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Support remote and local clients"),
            rationale: "Scope widened to also cover local clients.".to_string(),
        },
        "tester",
        Utc::now(),
    )
    .expect("supersede requirement");

    // Type-preserving: the replacement is a Requirement, not the caller's kind.
    assert!(
        has_edge(&state, &out.new_iri, moose::RDF_TYPE, &req_class),
        "replacement must inherit the superseded record's class (Requirement)"
    );
    assert!(
        !has_edge(&state, &out.new_iri, moose::RDF_TYPE, &dc),
        "replacement must NOT be minted as the caller-supplied kind"
    );

    // The lifecycle still works for a non-decision: link + flip + conformance.
    let supersedes = state.resolve_object_property("supersedes").unwrap();
    assert!(has_edge(&state, &out.new_iri, &supersedes, &old));
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["superseded"],
        "old requirement flipped to exactly one 'superseded' status"
    );
    assert!(
        validation::validate_project(&state).unwrap().conforms(),
        "non-decision supersede must conform to the architecture shapes"
    );
}

#[test]
fn supersede_rejects_unknown_target_and_writes_nothing() {
    let state = bootstrap("precond");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();

    let result = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: "https://moosedev.dev/kg/ArchitecturalDecision/does-not-exist"
                .to_string(),
            new: decision_input(&state, &dc, "Should not be written"),
            rationale: "n/a".to_string(),
        },
        "tester",
        Utc::now(),
    );
    assert!(
        result.is_err(),
        "superseding a non-existent decision must error"
    );

    // No partial write: the replacement never reaches the graph.
    let all = graph::relevant_context(&state, None, 50, true).unwrap();
    assert!(
        !all.iter().any(|i| i.label == "Should not be written"),
        "precondition failure must leave the graph untouched"
    );
}

#[test]
fn read_path_hides_superseded_by_default_and_surfaces_chain() {
    let state = bootstrap("readpath");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, &dc, "Original decision alpha");

    let out = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Replacement decision beta"),
            rationale: "Switched because remote clients need it.".to_string(),
        },
        "tester",
        Utc::now(),
    )
    .expect("supersede");

    // Default view = current working set: new shown, old hidden.
    let current = graph::relevant_context(&state, None, 50, false).unwrap();
    assert!(
        current.iter().any(|i| i.iri == out.new_iri),
        "the new decision is current"
    );
    assert!(
        !current.iter().any(|i| i.iri == old),
        "the superseded decision is hidden by default"
    );

    // The current item surfaces the supersedes link and the rationale TEXT.
    let new_item = current.iter().find(|i| i.iri == out.new_iri).unwrap();
    assert!(
        new_item
            .properties
            .iter()
            .any(|(k, v)| k == "supersedes" && v == &old),
        "new item shows what it supersedes; got {:?}",
        new_item.properties
    );
    assert!(
        new_item
            .properties
            .iter()
            .any(|(k, v)| k == "rationale" && v.contains("remote")),
        "new item surfaces the rationale text; got {:?}",
        new_item.properties
    );

    // History view includes the old record, marked and back-linked.
    let history = graph::relevant_context(&state, None, 50, true).unwrap();
    let old_item = history
        .iter()
        .find(|i| i.iri == old)
        .expect("superseded decision appears in history view");
    assert!(old_item.is_historical());
    assert!(
        old_item
            .properties
            .iter()
            .any(|(k, v)| k == "supersededBy" && v == &out.new_iri),
        "history item back-links to its replacement; got {:?}",
        old_item.properties
    );
}

#[test]
fn record_instance_with_relations_writes_object_edges() {
    let state = bootstrap("relations");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let req_class = state.resolve_class("Requirement").unwrap();

    let requirement = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: req_class,
            class_local: "Requirement".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Must support remote clients".to_string(),
                ),
                (
                    state.capture.title.clone(),
                    "Must support remote clients".to_string(),
                ),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record requirement");

    let is_motivated_by = state.resolve_object_property("isMotivatedBy").unwrap();
    let decision = graph::record_instance_with_relations(
        &state,
        &decision_input(&state, &dc, "Adopt TCP transport"),
        &[(is_motivated_by.clone(), requirement.clone())],
        "tester",
        Utc::now(),
    )
    .expect("record decision with relation");

    assert!(
        has_edge(&state, &decision, &is_motivated_by, &requirement),
        "isMotivatedBy edge should be written by record_instance_with_relations"
    );
}
