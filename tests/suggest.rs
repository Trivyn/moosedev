//! Link suggester: symbolic-first candidate generation (hybrid retrieval) gated by
//! SHACL legality, suggest-only. A suggestion's `confirm()` args create exactly the
//! edge via the validated `relate` path, and a confirmed pair is not re-suggested
//! (idempotency). `under_linked_records` is driven by SHACL Warning results.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-suggest-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

fn record(state: &AppState, kind: &str, title: &str, description: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.description.clone(), description.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record item")
}

#[tokio::test]
async fn suggests_legal_link_and_confirm_creates_edge_idempotently() {
    let state = bootstrap("suggest");
    // A decision and a lexically-related requirement, with NO edge between them.
    let req = record(
        &state,
        "Requirement",
        "Avoid repeated graph scans during retrieval",
        "Retrieval must not rescan the whole graph; cache hot lookups.",
    );
    let ad = record(
        &state,
        "ArchitecturalDecision",
        "Adopt a local cache for graph scans",
        "Cache repeated graph scans to avoid rescanning during retrieval.",
    );

    let suggestions = graph::suggest_links_for_record(&state, &ad, 5, None, None).await;
    let pick = suggestions
        .iter()
        .find(|s| s.predicate_local == "isMotivatedBy")
        .unwrap_or_else(|| {
            panic!("isMotivatedBy not suggested; got {suggestions:?}");
        });
    // Direction: decision --isMotivatedBy--> requirement.
    assert_eq!(pick.subject_iri, ad);
    assert_eq!(pick.object_iri, req);

    // Confirm via the validated relate path.
    let (subject, predicate, object) = pick.confirm();
    graph::relate(&state, &subject, &predicate, &object).expect("confirm suggestion");

    // Idempotency: the now-linked pair is not re-suggested.
    let again = graph::suggest_links_for_record(&state, &ad, 5, None, None).await;
    assert!(
        !again
            .iter()
            .any(|s| s.object_iri == req || s.subject_iri == req),
        "an already-linked pair must not be re-suggested"
    );
}

#[tokio::test]
async fn gap_targeting_prefers_the_missing_predicate() {
    let state = bootstrap("gaptarget");
    // Between an ArchitecturalDecision and a Constraint, BOTH `isMotivatedBy`
    // (AD -> Constraint) and `constrains` (Constraint -> AD) are legal. Shared
    // vocabulary makes the constraint a retrieval hit for the decision.
    let con = record(
        &state,
        "Constraint",
        "Graph writes must stay local to the project graph",
        "All graph writes are confined to kg/project so retrieval stays graph-scoped.",
    );
    let ad = record(
        &state,
        "ArchitecturalDecision",
        "Confine graph writes to the project graph",
        "Keep every graph write local to kg/project so retrieval stays graph-scoped.",
    );

    // No target: a legal predicate is chosen deterministically (not isMotivatedBy).
    let plain = graph::suggest_links_for_record(&state, &ad, 5, None, None).await;
    let plain_pick = plain
        .iter()
        .find(|s| s.subject_iri == con || s.object_iri == con)
        .unwrap_or_else(|| panic!("the constraint should be a candidate; got {plain:?}"));
    assert_ne!(
        plain_pick.predicate_local, "isMotivatedBy",
        "without gap-targeting, the alphabetically-first legal predicate (constrains) wins"
    );

    // Gap-targeting isMotivatedBy flips the choice to the gap-filling edge,
    // oriented AD -> Constraint.
    let targeted =
        graph::suggest_links_for_record(&state, &ad, 5, None, Some("isMotivatedBy")).await;
    let pick = targeted
        .iter()
        .find(|s| s.object_iri == con)
        .unwrap_or_else(|| panic!("isMotivatedBy -> constraint expected; got {targeted:?}"));
    assert_eq!(pick.predicate_local, "isMotivatedBy");
    assert_eq!(pick.subject_iri, ad);
    assert_eq!(pick.object_iri, con);
}

#[tokio::test]
async fn under_linked_flags_only_records_missing_the_should_have_link() {
    let state = bootstrap("underlinked");
    let req = record(&state, "Requirement", "Keep memory current", "");
    let linked = record(&state, "ArchitecturalDecision", "Supersede on change", "");
    let unlinked = record(&state, "ArchitecturalDecision", "Adopt local cache", "");
    graph::relate(&state, &linked, "isMotivatedBy", &req).expect("link the decision");

    let flagged = graph::under_linked_records(&state, 50);
    assert!(
        flagged
            .iter()
            .any(|u| u.iri == unlinked && u.missing_predicate == "isMotivatedBy"),
        "an AD with no isMotivatedBy should be flagged; got {flagged:?}"
    );
    assert!(
        !flagged.iter().any(|u| u.iri == linked),
        "an AD that has isMotivatedBy should not be flagged"
    );
}
