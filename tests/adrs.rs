use chrono::{TimeZone, Utc};
use moosedev::adrs::{generate_adr_set, AdrGenerationOptions, INDEX_FILENAME};
use moosedev::graph::{self, AppState, RecordInput, SupersedeInput};
use oxigraph::model::{GraphName, NamedNode, Quad};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-adrs-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record_decision(state: &AppState, title: &str, timestamp: &str) -> String {
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("Decision body for {title}"),
                ),
                (state.capture.timestamp.clone(), timestamp.to_string()),
            ],
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 6, 25, 12, 0, 0).unwrap(),
    )
    .expect("record decision")
}

fn insert_incomplete_decision(state: &AppState, iri: &str) {
    let class = state.resolve_class("ArchitecturalDecision").unwrap();
    let quad = Quad::new(
        NamedNode::new(iri).unwrap(),
        NamedNode::new(moose::RDF_TYPE).unwrap(),
        NamedNode::new(class).unwrap(),
        GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    state
        .store
        .insert(quad.as_ref())
        .expect("insert incomplete decision");
}

#[test]
fn adr_set_renders_stable_numbered_markdown_from_project_graph() {
    let dir = temp_dir("render");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let later = record_decision(&state, "Second Decision", "2026-06-26T00:00:00Z");
    let earlier = record_decision(&state, "First Decision", "2026-06-25T00:00:00Z");

    let set = generate_adr_set(&state, AdrGenerationOptions::default()).expect("generate ADRs");

    assert_eq!(set.graph_decisions, 2);
    assert_eq!(set.adr_files, 2);
    assert_eq!(set.index_filename, INDEX_FILENAME);
    assert_eq!(set.adrs[0].num, "0001");
    assert_eq!(set.adrs[0].iri, earlier);
    assert_eq!(set.adrs[0].filename, "0001-first-decision.md");
    assert_eq!(set.adrs[1].num, "0002");
    assert_eq!(set.adrs[1].iri, later);
    assert!(set.adrs[0]
        .markdown
        .contains("Decision body for First Decision"));
    assert_eq!(set.summaries()[0].search_text, set.adrs[0].markdown);
    assert!(set
        .index_markdown
        .contains("[First Decision](0001-first-decision.md)"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adr_set_keeps_duplicate_slugs_distinct_and_reports_missing_fields() {
    let dir = temp_dir("warnings");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "Repeated", "2026-06-25T00:00:00Z");
    record_decision(&state, "Repeated", "2026-06-26T00:00:00Z");

    let set = generate_adr_set(&state, AdrGenerationOptions::default()).expect("generate ADRs");

    assert_eq!(set.adrs[0].filename, "0001-repeated.md");
    assert_eq!(set.adrs[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.missing_context, vec!["0001", "0002"]);
    assert!(set.warnings.missing_decision.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adr_set_renders_incomplete_records_instead_of_dropping_them() {
    let dir = temp_dir("incomplete");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    insert_incomplete_decision(
        &state,
        "https://moosedev.dev/kg/ArchitecturalDecision/incomplete",
    );

    let set = generate_adr_set(&state, AdrGenerationOptions::default()).expect("generate ADRs");

    assert_eq!(set.graph_decisions, 1);
    assert_eq!(
        set.adr_files, 1,
        "typed records must not be silently dropped"
    );
    assert_eq!(set.adrs[0].title, "");
    assert_eq!(set.adrs[0].status, "not recorded");
    assert_eq!(set.adrs[0].filename, "0001-decision.md");
    assert!(set.warnings.missing_decision.contains(&"0001".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adr_summary_status_for_superseded_record_is_plain_text() {
    let dir = temp_dir("superseded-status");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let old = record_decision(&state, "Old Decision", "2026-06-25T00:00:00Z");
    graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old,
            new: RecordInput {
                class_iri,
                class_local: "ArchitecturalDecision".to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), "New Decision".to_string()),
                    (state.capture.title.clone(), "New Decision".to_string()),
                    (
                        state.capture.timestamp.clone(),
                        "2026-06-26T00:00:00Z".to_string(),
                    ),
                ],
            },
            rationale: "New evidence changed the decision.".to_string(),
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 6, 26, 12, 0, 0).unwrap(),
    )
    .expect("supersede");
    let set = generate_adr_set(&state, AdrGenerationOptions::default()).expect("generate ADRs");
    assert_eq!(set.adrs[0].status, "Superseded by ADR-0002");
    assert!(!set.adrs[0].status.contains('['));
    assert!(set.adrs[0]
        .markdown
        .contains("- Status: Superseded by [ADR-0002](0002-new-decision.md)"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adr_set_memo_serves_warm_reads_and_invalidates_on_write() {
    let dir = temp_dir("memo");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "First decision", "2026-06-25T12:00:00Z");
    state.note_project_write();

    let first = moosedev::adrs::generate_adr_set_cached(&state, AdrGenerationOptions::default())
        .expect("cold generate");
    let warm = moosedev::adrs::generate_adr_set_cached(&state, AdrGenerationOptions::default())
        .expect("warm generate");
    assert!(
        std::sync::Arc::ptr_eq(&first, &warm),
        "unchanged generation serves the memoized set"
    );

    // A project-graph write bumps the generation → the next read regenerates
    // and the new decision is visible (no staleness window).
    record_decision(&state, "Second decision", "2026-06-26T12:00:00Z");
    state.note_project_write();
    let refreshed =
        moosedev::adrs::generate_adr_set_cached(&state, AdrGenerationOptions::default())
            .expect("regenerate after write");
    assert!(!std::sync::Arc::ptr_eq(&first, &refreshed));
    assert_eq!(refreshed.adrs.len(), 2, "the write is visible immediately");

    // Different options never reuse a memo built for other parameters.
    let other =
        moosedev::adrs::generate_adr_set_cached(&state, AdrGenerationOptions { batch_size: 5 })
            .expect("options variant");
    assert!(!std::sync::Arc::ptr_eq(&refreshed, &other));

    let _ = std::fs::remove_dir_all(&dir);
}
