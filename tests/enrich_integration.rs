//! End-to-end check of GROWL enrichment through the real `AppState` flow: record two
//! linked records, drive the lazy trigger exactly as the MCP write+read paths do, and
//! verify the inferred inverse edge is co-located in the project graph (so the
//! kg/project-scoped retrieval reads can traverse it), that enrichment adds no SHACL
//! violations, and that re-running is idempotent (drop-and-rerun).

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation::validate_project;
use oxigraph::model::{GraphNameRef, NamedNode};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-enrich-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    let class_iri = state.resolve_class(kind).unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record")
}

/// Count `(subject) motivates (object)` edges in the project graph, matched by predicate
/// local-name so the test stays namespace-agnostic.
fn motivates_count(state: &AppState, subject_iri: &str, object_iri: &str) -> usize {
    let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap();
    let subj = format!("<{subject_iri}>");
    let obj = format!("<{object_iri}>");
    state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .flatten()
        .filter(|q| {
            q.predicate.as_str().rsplit(['/', '#']).next() == Some("motivates")
                && q.subject.to_string() == subj
                && q.object.to_string() == obj
        })
        .count()
}

#[test]
fn enrich_materializes_inverse_keeps_shacl_clean_and_is_idempotent() {
    let dir = temp_dir("inverse");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");

    // A decision motivated by a requirement — the asserted forward edge.
    let req = record(&state, "Requirement", "NLQ must answer structural queries");
    let ad = record(&state, "ArchitecturalDecision", "Route NLQ by query class");
    graph::relate(&state, &ad, "isMotivatedBy", &req).expect("relate isMotivatedBy");

    // Baseline: no inverse edge yet; capture a violation count to measure enrichment against.
    assert_eq!(
        motivates_count(&state, &req, &ad),
        0,
        "no inverse before enrich"
    );
    let base_violations = validate_project(&state).expect("validate").violations.len();

    // Drive the lazy trigger exactly as the MCP write + read paths do.
    state.mark_inferred_stale();
    state.ensure_enriched();

    // (B) the inverse `req motivates ad` is materialized and co-located in kg/project,
    // so the kg/project-scoped retrieval reads can traverse Requirement -> decision.
    assert_eq!(
        motivates_count(&state, &req, &ad),
        1,
        "inverse materialized + co-located after enrich"
    );

    // (A) co-located inferred edges add no new SHACL violations.
    let report = validate_project(&state).expect("validate");
    assert_eq!(
        report.violations.len(),
        base_violations,
        "enrichment must not add SHACL violations: {:?}",
        report.violations
    );

    // Idempotent: another write marks stale; re-enrich keeps exactly one inverse edge.
    state.mark_inferred_stale();
    state.ensure_enriched();
    assert_eq!(
        motivates_count(&state, &req, &ad),
        1,
        "no duplicate inverse after re-enrich"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
