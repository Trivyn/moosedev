use chrono::{TimeZone, Utc};
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::requirements::{
    generate_requirement_set, RequirementGenerationOptions, REQUIREMENTS_INDEX_FILENAME,
};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-requirements-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record_item(state: &AppState, class_local: &str, title: &str, timestamp: &str) -> String {
    let class_iri = state.resolve_class(class_local).unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: class_local.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("{class_local} body for {title}"),
                ),
                (state.capture.timestamp.clone(), timestamp.to_string()),
            ],
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 6, 25, 12, 0, 0).unwrap(),
    )
    .expect("record item")
}

#[test]
fn requirement_set_renders_related_adrs_from_project_graph() {
    let dir = temp_dir("render");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let req = record_item(
        &state,
        "Requirement",
        "Trace decisions to needs",
        "2026-06-25T00:00:00Z",
    );
    let adr = record_item(
        &state,
        "ArchitecturalDecision",
        "ADR Requirement Link",
        "2026-06-26T00:00:00Z",
    );
    graph::relate(&state, &adr, "isMotivatedBy", &req).expect("relate decision to requirement");

    let set = generate_requirement_set(&state.store, RequirementGenerationOptions::default())
        .expect("generate requirements");

    assert_eq!(set.graph_requirements, 1);
    assert_eq!(set.requirement_files, 1);
    assert_eq!(set.index_filename, REQUIREMENTS_INDEX_FILENAME);
    assert_eq!(set.requirements[0].num, "0001");
    assert_eq!(set.requirements[0].iri, req);
    assert_eq!(
        set.requirements[0].filename,
        "0001-trace-decisions-to-needs.md"
    );
    assert_eq!(set.requirements[0].related_adrs, 1);
    assert!(set.requirements[0]
        .markdown
        .contains("Requirement body for Trace decisions to needs"));
    assert!(set.requirements[0].markdown.contains(&adr));
    assert!(set
        .index_markdown
        .contains("[Trace decisions to needs](0001-trace-decisions-to-needs.md)"));
    assert!(set.warnings.unlinked_requirements.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requirement_set_reports_duplicate_slugs_and_unlinked_requirements() {
    let dir = temp_dir("warnings");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_item(&state, "Requirement", "Repeated", "2026-06-25T00:00:00Z");
    record_item(&state, "Requirement", "Repeated", "2026-06-26T00:00:00Z");

    let set = generate_requirement_set(&state.store, RequirementGenerationOptions::default())
        .expect("generate requirements");

    assert_eq!(set.requirements[0].filename, "0001-repeated.md");
    assert_eq!(set.requirements[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.unlinked_requirements, vec!["0001", "0002"]);
    assert!(set.warnings.missing_description.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
