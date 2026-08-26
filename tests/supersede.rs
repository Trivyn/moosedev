//! Supersede-with-rationale lifecycle + generic object-property capture.
//!
//! Proves: a decision change persists both new -supersedes-> old and old
//! -isSupersededBy-> new, captures the WHY as a linked `Rationale`, and flips the
//! old decision to `superseded` while preserving it (and all its other triples)
//! as history; the precondition rejects bad targets without writing; the read
//! path hides retired records by default and surfaces the chain; and
//! `record_instance_with_relations` writes relations. Relation-aware supersede
//! accepts explicit replacement links atomically and reports, but never copies,
//! prior semantic links that were not reasserted.

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

fn record_item(state: &AppState, kind: &str, title: &str) -> String {
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

fn record_code_entity(state: &AppState, name: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: state.resolve_code_class("CodeEntity").unwrap(),
            class_local: "CodeEntity".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), name.to_string()),
                (
                    state
                        .resolve_code_datatype_property("hasSubstrateSymbol")
                        .unwrap(),
                    format!("test crate 0.1.0 src/lib.rs/{name}()."),
                ),
                (
                    state
                        .resolve_code_datatype_property("hasEntityKind")
                        .unwrap(),
                    "function".to_string(),
                ),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record code entity")
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
    let is_superseded_by = state.resolve_object_property("isSupersededBy").unwrap();
    let has_rationale = state.resolve_object_property("hasRationale").unwrap();
    let rationale_class = state.resolve_class("Rationale").unwrap();

    // Both lifecycle directions are asserted by the write itself; readers must
    // not depend on an OWL reasoner having materialized the inverse first.
    assert!(has_edge(&state, &out.new_iri, &supersedes, &old));
    assert!(has_edge(&state, &old, &is_superseded_by, &out.new_iri));
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
fn legacy_supersede_does_not_trigger_semantic_enrichment() {
    let state = bootstrap("legacy-no-enrichment");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, &dc, "Original lightweight lifecycle write");
    let constraint = record_item(&state, "Constraint", "No whole-graph read on legacy writes");
    let constrains = state.resolve_object_property("constrains").unwrap();
    let is_constrained_by = state.resolve_object_property("isConstrainedBy").unwrap();

    graph::relate(&state, &constraint, "constrains", &old).expect("constraint edge");
    assert!(has_edge(&state, &constraint, &constrains, &old));
    assert!(
        !has_edge(&state, &old, &is_constrained_by, &constraint),
        "the inverse should remain unmaterialized before the legacy write"
    );

    graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Replacement lightweight lifecycle write"),
            rationale: "The compatibility API must remain lifecycle-only.".into(),
        },
        "tester",
        Utc::now(),
    )
    .expect("legacy supersede");

    assert!(
        !has_edge(&state, &old, &is_constrained_by, &constraint),
        "legacy supersede must not run GROWL solely to build a discarded report"
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
            .any(|p| p.predicate == "supersedes" && p.value == old),
        "new item shows what it supersedes; got {:?}",
        new_item.properties
    );
    assert!(
        new_item
            .properties
            .iter()
            .any(|p| p.predicate == "rationale" && p.value.contains("remote")),
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
            .any(|p| p.predicate == "supersededBy" && p.value == out.new_iri),
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

#[test]
fn relation_aware_supersede_applies_explicit_links_and_reports_the_rest() {
    let state = bootstrap("relation-aware");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let requirement = record_item(&state, "Requirement", "Keep remote recall reliable");
    let constraint = record_item(&state, "Constraint", "Keep writes auditable");
    let component = record_item(&state, "SystemComponent", "graph runtime");
    let code_entity = record_code_entity(&state, "supersede_impl");

    let old = graph::record_instance_with_relation_args(
        &state,
        &decision_input(&state, &dc, "Original graph write"),
        &[
            (
                "isMotivatedBy".to_string(),
                "Keep remote recall reliable".to_string(),
            ),
            ("concerns".to_string(), component.clone()),
            ("concerns".to_string(), code_entity.clone()),
        ],
        "tester",
        Utc::now(),
    )
    .expect("record linked decision")
    .iri;

    let out = graph::supersede_decision_with_relation_args(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Replacement graph write"),
            rationale: "The replacement makes relation authorship explicit.".to_string(),
        },
        &[
            (
                "isMotivatedBy".to_string(),
                "Keep remote recall reliable".to_string(),
            ),
            // Duplicate input is deliberately ignored by the shared planner.
            ("isMotivatedBy".to_string(), requirement.clone()),
            ("isMotivatedBy".to_string(), constraint.clone()),
            ("concerns".to_string(), code_entity.clone()),
        ],
        "tester",
        Utc::now(),
    )
    .expect("relation-aware supersede");

    let replacement = &out.lifecycle.new_iri;
    let motivated_by = state.resolve_object_property("isMotivatedBy").unwrap();
    let concerns = state.resolve_object_property("concerns").unwrap();
    assert!(has_edge(&state, replacement, &motivated_by, &requirement));
    assert!(has_edge(&state, replacement, &motivated_by, &constraint));
    assert!(has_edge(&state, replacement, &concerns, &code_entity));
    assert!(
        !has_edge(&state, replacement, &concerns, &component),
        "omitted prior relation must not be copied"
    );
    assert!(
        has_edge(&state, &old, &concerns, &component),
        "retired record keeps its relation history"
    );
    assert_eq!(out.applied_edges.len(), 3, "duplicate edge is deduped");
    assert_eq!(
        out.not_carried_edges
            .iter()
            .map(|edge| (edge.predicate_local.as_str(), edge.object_iri.as_str()))
            .collect::<Vec<_>>(),
        vec![("concerns", component.as_str())]
    );
    assert!(
        validation::validate_project(&state).unwrap().conforms(),
        "relation-aware replacement must conform"
    );
}

#[test]
fn omission_report_includes_growl_inverse_but_not_lifecycle_relations() {
    let state = bootstrap("inverse-report");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let base = record_decision(&state, &dc, "Original constrained choice");
    let old = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: base,
            new: decision_input(&state, &dc, "Intermediate constrained choice"),
            rationale: "The first replacement establishes lifecycle relations.".into(),
        },
        "tester",
        Utc::now(),
    )
    .expect("first supersede")
    .new_iri;
    let constraint = record_item(&state, "Constraint", "Runtime remains local");

    graph::relate(&state, &constraint, "constrains", &old).expect("constraint edge");
    state.mark_inferred_stale();

    let out = graph::supersede_decision_with_relation_args(
        &state,
        &SupersedeInput {
            superseded_iri: old,
            new: decision_input(&state, &dc, "Replacement constrained choice"),
            rationale: "The implementation changed, but the constraint may still apply.".into(),
        },
        &[],
        "tester",
        Utc::now(),
    )
    .expect("supersede with inverse snapshot");

    assert!(out.not_carried_edges.iter().any(|edge| {
        edge.predicate_local == "isConstrainedBy" && edge.object_iri == constraint
    }));
    for lifecycle in [
        "supersedes",
        "isSupersededBy",
        "hasRationale",
        "isRationaleFor",
    ] {
        assert!(
            out.not_carried_edges
                .iter()
                .all(|edge| edge.predicate_local != lifecycle),
            "{lifecycle} is lifecycle bookkeeping, not a carry suggestion"
        );
    }
}

#[test]
fn invalid_replacement_relation_aborts_the_authoritative_supersede_write() {
    let state = bootstrap("relation-atomicity");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, &dc, "Stable original decision");
    let decisions_before = count_class(&state, "ArchitecturalDecision");
    let rationales_before = count_class(&state, "Rationale");

    let error = graph::supersede_decision_with_relation_args(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Must not be recorded"),
            rationale: "This write must remain atomic.".into(),
        },
        &[(
            "isMotivatedBy".to_string(),
            "missing requirement".to_string(),
        )],
        "tester",
        Utc::now(),
    )
    .expect_err("unknown relation target must abort");

    assert!(
        error.to_string().contains("matches no typed target"),
        "{error}"
    );
    assert_eq!(
        count_class(&state, "ArchitecturalDecision"),
        decisions_before
    );
    assert_eq!(count_class(&state, "Rationale"), rationales_before);
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["accepted"],
        "old status remains authoritative"
    );
    let is_superseded_by = state.resolve_object_property("isSupersededBy").unwrap();
    let old_node = NamedNodeRef::new(&old).unwrap();
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    assert!(
        state
            .store
            .quads_for_pattern(
                Some(old_node.into()),
                Some(NamedNodeRef::new(&is_superseded_by).unwrap()),
                None,
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
            .next()
            .is_none(),
        "no successor backlink is written"
    );
}

#[test]
fn code_domain_inverse_is_reported_replayable_and_reenriched() {
    let state = bootstrap("code-inverse");
    let requirement_class = state.resolve_class("Requirement").unwrap();
    let old = record_item(&state, "Requirement", "Runtime intent requirement");
    let retained_code = record_code_entity(&state, "retained_impl");
    let omitted_code = record_code_entity(&state, "omitted_impl");
    graph::relate(&state, &retained_code, "satisfies", &old).expect("retained code intent");
    graph::relate(&state, &omitted_code, "satisfies", &old).expect("omitted code intent");
    state.mark_inferred_stale();

    let out = graph::supersede_decision_with_relation_args(
        &state,
        &SupersedeInput {
            superseded_iri: old,
            new: RecordInput {
                class_iri: requirement_class,
                class_local: "Requirement".to_string(),
                properties: vec![
                    (
                        moose::RDFS_LABEL.to_string(),
                        "Replacement runtime intent requirement".to_string(),
                    ),
                    (
                        state.capture.title.clone(),
                        "Replacement runtime intent requirement".to_string(),
                    ),
                ],
            },
            rationale: "Only one implementation still satisfies the requirement.".into(),
        },
        &[("isSatisfiedBy".to_string(), retained_code.clone())],
        "tester",
        Utc::now(),
    )
    .expect("reassert inverse code intent");

    assert!(out.applied_edges.iter().any(|edge| {
        edge.predicate_local == "isSatisfiedBy" && edge.object_iri == retained_code
    }));
    assert_eq!(
        out.not_carried_edges
            .iter()
            .filter(|edge| edge.predicate_local == "isSatisfiedBy")
            .map(|edge| edge.object_iri.as_str())
            .collect::<Vec<_>>(),
        vec![omitted_code.as_str()]
    );

    state.ensure_enriched();
    let satisfies = state.resolve_object_property("satisfies").unwrap();
    assert!(has_edge(
        &state,
        &retained_code,
        &satisfies,
        &out.lifecycle.new_iri
    ));
    assert!(
        !has_edge(&state, &omitted_code, &satisfies, &out.lifecycle.new_iri),
        "an omitted code inverse must not be manufactured"
    );
}

#[test]
fn managed_lifecycle_relation_is_rejected_without_a_supersede_write() {
    let state = bootstrap("managed-relation");
    let dc = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, &dc, "Original lifecycle owner");
    let unrelated = record_decision(&state, &dc, "Unrelated lifecycle target");
    let decisions_before = count_class(&state, "ArchitecturalDecision");
    let rationales_before = count_class(&state, "Rationale");

    let error = graph::supersede_decision_with_relation_args(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: decision_input(&state, &dc, "Rejected lifecycle injection"),
            rationale: "The caller must not create a second lifecycle chain.".into(),
        },
        &[("supersedes".to_string(), unrelated)],
        "tester",
        Utc::now(),
    )
    .expect_err("managed lifecycle relation must fail");

    assert!(error.to_string().contains("managed by supersede_decision"));
    assert_eq!(
        count_class(&state, "ArchitecturalDecision"),
        decisions_before
    );
    assert_eq!(count_class(&state, "Rationale"), rationales_before);
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["accepted"]
    );
}

#[test]
fn high_stakes_supersession_stays_proposed_until_atomic_acceptance() {
    let state = bootstrap("ratified-constraint");
    let constraint_class = state.resolve_class("Constraint").unwrap();
    let old = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: constraint_class.clone(),
            class_local: "Constraint".into(),
            properties: vec![
                (state.capture.title.clone(), "Bounded reads".into()),
                (
                    state.capture.description.clone(),
                    "Reads stay within the selected window.\nNo whole-file allocation.".into(),
                ),
                (state.capture.status.clone(), "accepted".into()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .unwrap();
    let component = record_item(&state, "SystemComponent", "source reader");

    let out = graph::supersede_decision_with_options(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: RecordInput {
                class_iri: constraint_class,
                class_local: "Constraint".into(),
                properties: vec![
                    (state.capture.title.clone(), "Bounded source reads".into()),
                    (
                        state.capture.description.clone(),
                        "Reads normally stay within the selected window.\nLarge files may use a bounded retained buffer.".into(),
                    ),
                ],
            },
            rationale: "The supported source path changed.".into(),
        },
        &[("concerns".into(), component.clone())],
        Some(graph::SupersessionReason::CodeDiverged),
        "tester",
        Utc::now(),
    )
    .unwrap();

    assert_eq!(out.lifecycle.status, "proposed");
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["accepted"]
    );
    assert_eq!(
        literals(&state, &out.lifecycle.new_iri, &state.capture.status),
        vec!["proposed"]
    );
    for (predicate, object) in [
        (
            state.resolve_object_property("supersedes").unwrap(),
            old.clone(),
        ),
        (
            state.resolve_object_property("hasRationale").unwrap(),
            out.lifecycle.rationale_iri.clone(),
        ),
        (
            state.resolve_object_property("concerns").unwrap(),
            component.clone(),
        ),
    ] {
        assert!(
            has_edge(&state, &out.lifecycle.new_iri, &predicate, &object),
            "proposed records retain their semantic relation {predicate}"
        );
    }
    state.ensure_enriched();
    assert!(
        has_edge(
            &state,
            &old,
            &state.resolve_object_property("isSupersededBy").unwrap(),
            &out.lifecycle.new_iri
        ),
        "the lifecycle inverse remains available while status controls authority"
    );

    let pending = graph::list_proposals(&state, Some("proposed")).unwrap();
    assert_eq!(graph::pending_count(&state).unwrap(), 1);
    let proposal = pending
        .iter()
        .find(|proposal| proposal.iri == out.lifecycle.new_iri)
        .unwrap();
    assert_eq!(proposal.predecessor_iri.as_deref(), Some(old.as_str()));
    assert_eq!(
        proposal.supersession_reason.as_deref(),
        Some("code-diverged")
    );
    assert!(proposal
        .claim_diff
        .as_ref()
        .unwrap()
        .text
        .contains("- Reads stay"));

    graph::accept_proposal(&state, &out.lifecycle.new_iri, "ratifier").unwrap();
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["superseded"]
    );
    assert_eq!(
        literals(&state, &out.lifecycle.new_iri, &state.capture.status),
        vec!["accepted"]
    );
    assert_eq!(
        literals(&state, &out.lifecycle.rationale_iri, &state.capture.status),
        vec!["accepted"]
    );
    assert!(has_edge(
        &state,
        &out.lifecycle.new_iri,
        &state.resolve_object_property("supersedes").unwrap(),
        &old
    ));
    assert!(has_edge(
        &state,
        &out.lifecycle.new_iri,
        &state.resolve_object_property("concerns").unwrap(),
        &component
    ));
    assert!(has_edge(
        &state,
        &component,
        &state.resolve_object_property("isConcernedBy").unwrap(),
        &out.lifecycle.new_iri
    ));
    let report = validation::validate_project(&state).unwrap();
    assert!(report.conforms(), "{}", validation::format_report(&report));
}

#[test]
fn proposed_supersession_rejects_duplicates_and_rejection_leaves_old_authoritative() {
    let state = bootstrap("ratified-reject");
    let old = record_item(&state, "AntiPattern", "Implicit global mutation");
    let input = SupersedeInput {
        superseded_iri: old.clone(),
        new: RecordInput {
            class_iri: String::new(),
            class_local: String::new(),
            properties: vec![(state.capture.title.clone(), "Scoped mutation".into())],
        },
        rationale: "The scope was narrowed.".into(),
    };
    let first = graph::supersede_decision_with_options(
        &state,
        &input,
        &[],
        Some(graph::SupersessionReason::ScopeNarrowed),
        "tester",
        Utc::now(),
    )
    .unwrap();
    let duplicate =
        graph::supersede_decision_with_options(&state, &input, &[], None, "tester", Utc::now())
            .unwrap_err();
    assert!(duplicate
        .to_string()
        .contains("already has a pending supersession"));

    graph::reject_proposal(&state, &first.lifecycle.new_iri, "ratifier").unwrap();
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["accepted"]
    );
    assert_eq!(
        literals(&state, &first.lifecycle.new_iri, &state.capture.status),
        vec!["rejected"]
    );
    assert_eq!(
        literals(
            &state,
            &first.lifecycle.rationale_iri,
            &state.capture.status
        ),
        vec!["rejected"]
    );
    assert_eq!(graph::pending_count(&state).unwrap(), 0);
}

#[test]
fn explicit_accepted_status_bypasses_the_high_stakes_default() {
    let state = bootstrap("ratified-explicit-status");
    let old = record_item(&state, "Constraint", "Original invariant");
    let immediate = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: RecordInput {
                class_iri: String::new(),
                class_local: String::new(),
                properties: vec![
                    (state.capture.title.clone(), "Accepted invariant".into()),
                    (state.capture.status.clone(), "accepted".into()),
                ],
            },
            rationale: "Explicit Rust override.".into(),
        },
        "tester",
        Utc::now(),
    )
    .unwrap();
    assert_eq!(immediate.status, "accepted");
    assert_eq!(
        literals(&state, &old, &state.capture.status),
        vec!["superseded"]
    );
    let error = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old,
            new: RecordInput {
                class_iri: String::new(),
                class_local: String::new(),
                properties: vec![(state.capture.title.clone(), "Too late".into())],
            },
            rationale: "Must not mint.".into(),
        },
        "tester",
        Utc::now(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("predecessor is superseded"));
}

#[test]
fn supersession_diff_is_bounded() {
    let state = bootstrap("ratified-diff-reason");
    let constraint = state.resolve_class("Constraint").unwrap();
    let old_description = (0..120)
        .map(|index| format!("old clause {index}: {}", "é".repeat(100)))
        .collect::<Vec<_>>()
        .join("\n");
    let new_description = (0..120)
        .map(|index| format!("new clause {index}: {}", "ø".repeat(100)))
        .collect::<Vec<_>>()
        .join("\n");
    let old = graph::record_instance(
        &state,
        &RecordInput {
            class_iri: constraint,
            class_local: "Constraint".into(),
            properties: vec![
                (state.capture.title.clone(), "Large old claim".into()),
                (state.capture.description.clone(), old_description),
                (state.capture.status.clone(), "accepted".into()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .unwrap();
    let replacement = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old,
            new: RecordInput {
                class_iri: String::new(),
                class_local: String::new(),
                properties: vec![
                    (state.capture.title.clone(), "Large new claim".into()),
                    (state.capture.description.clone(), new_description),
                ],
            },
            rationale: "Large claim replacement.".into(),
        },
        "tester",
        Utc::now(),
    )
    .unwrap();
    let proposal = graph::list_proposals(&state, Some("proposed"))
        .unwrap()
        .into_iter()
        .find(|proposal| proposal.iri == replacement.new_iri)
        .unwrap();
    let diff = proposal.claim_diff.unwrap();
    assert!(diff.truncated);
    assert!(diff.text.len() <= 8 * 1024);
    assert!(diff.text.lines().count() <= 60);
    assert!(diff.text.ends_with("… diff truncated …"));
}
