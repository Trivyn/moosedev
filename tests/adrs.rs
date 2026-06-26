use chrono::{TimeZone, Utc};
use moosedev::adrs::{generate_adr_set, AdrGenerationOptions, INDEX_FILENAME};
use moosedev::graph::{self, AppState, RecordInput};

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

#[test]
fn adr_set_renders_stable_numbered_markdown_from_project_graph() {
    let dir = temp_dir("render");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let later = record_decision(&state, "Second Decision", "2026-06-26T00:00:00Z");
    let earlier = record_decision(&state, "First Decision", "2026-06-25T00:00:00Z");

    let set =
        generate_adr_set(&state.store, AdrGenerationOptions::default()).expect("generate ADRs");

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

    let set =
        generate_adr_set(&state.store, AdrGenerationOptions::default()).expect("generate ADRs");

    assert_eq!(set.adrs[0].filename, "0001-repeated.md");
    assert_eq!(set.adrs[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.missing_context, vec!["0001", "0002"]);
    assert!(set.warnings.missing_decision.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
