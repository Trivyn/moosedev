use chrono::{TimeZone, Utc};
use moosedev::constraints::{
    generate_constraint_set, ConstraintGenerationOptions, CONSTRAINTS_INDEX_FILENAME,
};
use moosedev::graph::{self, AppState, RecordInput, SupersedeInput};
use oxigraph::model::{GraphName, NamedNode, Quad};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-constraints-{name}-{}-{}",
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
        Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap(),
    )
    .expect("record item")
}

fn insert_relation(state: &AppState, subject: &str, predicate_local: &str, object: &str) {
    let predicate = state.resolve_object_property(predicate_local).unwrap();
    state
        .store
        .insert(
            Quad::new(
                NamedNode::new(subject).unwrap(),
                NamedNode::new(predicate).unwrap(),
                NamedNode::new(object).unwrap(),
                GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
            )
            .as_ref(),
        )
        .expect("insert relation");
}

fn insert_incomplete_constraint(state: &AppState, iri: &str) {
    let class = state.resolve_class("Constraint").unwrap();
    state
        .store
        .insert(
            Quad::new(
                NamedNode::new(iri).unwrap(),
                NamedNode::new(moose::RDF_TYPE).unwrap(),
                NamedNode::new(class).unwrap(),
                GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
            )
            .as_ref(),
        )
        .expect("insert incomplete constraint");
}

#[test]
fn constraint_set_renders_both_target_directions_without_duplicates() {
    let dir = temp_dir("targets");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let direct = record_item(
        &state,
        "Constraint",
        "Direct constraint",
        "2026-07-09T00:00:00Z",
    );
    let inverse = record_item(
        &state,
        "Constraint",
        "Inverse constraint",
        "2026-07-10T00:00:00Z",
    );
    let target = record_item(
        &state,
        "ArchitecturalDecision",
        "Governed decision",
        "2026-07-11T00:00:00Z",
    );

    insert_relation(&state, &direct, "constrains", &target);
    insert_relation(&state, &target, "isConstrainedBy", &direct);
    insert_relation(&state, &target, "isConstrainedBy", &inverse);

    let set = generate_constraint_set(&state, ConstraintGenerationOptions { batch_size: 1 })
        .expect("generate constraints");

    assert_eq!(set.graph_constraints, 2);
    assert_eq!(set.constraint_files, 2);
    assert_eq!(set.index_filename, CONSTRAINTS_INDEX_FILENAME);
    assert_eq!(set.constraints[0].num, "0001");
    assert_eq!(set.constraints[0].related_targets, 1);
    assert_eq!(set.constraints[1].related_targets, 1);
    assert!(set.constraints[0].markdown.contains("# CST-0001."));
    assert!(set.constraints[0].markdown.contains("Governed decision"));
    assert_eq!(set.constraints[0].markdown.matches(&target).count(), 1);
    assert_eq!(set.summaries()[0].search_text, set.constraints[0].markdown);
    assert!(set.index_markdown.contains("| CST-0001 |"));
    assert!(set.warnings.unlinked_constraints.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constraint_set_reports_duplicate_slugs_and_unlinked_constraints() {
    let dir = temp_dir("warnings");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_item(&state, "Constraint", "Repeated", "2026-07-09T00:00:00Z");
    record_item(&state, "Constraint", "Repeated", "2026-07-10T00:00:00Z");

    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())
        .expect("generate constraints");

    assert_eq!(set.constraints[0].filename, "0001-repeated.md");
    assert_eq!(set.constraints[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.unlinked_constraints, vec!["0001", "0002"]);
    assert!(set.warnings.missing_description.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constraint_set_renders_supersession_links() {
    let dir = temp_dir("supersession-links");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let old = record_item(
        &state,
        "Constraint",
        "Original constraint",
        "2026-07-09T00:00:00Z",
    );
    let replacement = graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: old.clone(),
            new: RecordInput {
                class_iri: state.resolve_class("Constraint").unwrap(),
                class_local: "Constraint".to_string(),
                properties: vec![
                    (
                        moose::RDFS_LABEL.to_string(),
                        "Replacement constraint".to_string(),
                    ),
                    (
                        state.capture.title.clone(),
                        "Replacement constraint".to_string(),
                    ),
                    (
                        state.capture.timestamp.clone(),
                        "2026-07-10T00:00:00Z".to_string(),
                    ),
                ],
            },
            rationale: "The constraint changed.".to_string(),
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap(),
    )
    .expect("supersede constraint");

    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())
        .expect("generate constraints");
    let old_doc = set
        .constraints
        .iter()
        .find(|constraint| constraint.iri == old)
        .expect("old constraint");
    let new_doc = set
        .constraints
        .iter()
        .find(|constraint| constraint.iri == replacement.new_iri)
        .expect("replacement constraint");

    assert_eq!(old_doc.status, "Superseded by CST-0002");
    assert!(old_doc
        .markdown
        .contains("- Status: Superseded by [CST-0002](0002-replacement-constraint.md)"));
    assert!(new_doc
        .markdown
        .contains("- Supersedes: [CST-0001](0001-original-constraint.md)"));
    assert!(set.index_markdown.contains("Superseded by CST-0002"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constraint_set_preserves_incomplete_records() {
    let dir = temp_dir("incomplete");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    insert_incomplete_constraint(&state, "https://moosedev.dev/kg/Constraint/incomplete");

    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())
        .expect("generate constraints");

    assert_eq!(set.graph_constraints, 1);
    assert_eq!(set.constraint_files, 1);
    assert_eq!(set.constraints[0].title, "");
    assert_eq!(set.constraints[0].status, "not recorded");
    assert_eq!(set.constraints[0].filename, "0001-constraint.md");
    assert_eq!(set.warnings.missing_description, vec!["0001"]);
    assert_eq!(set.warnings.unlinked_constraints, vec!["0001"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_constraint_set_and_archive_are_complete() {
    let dir = temp_dir("empty");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");

    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())
        .expect("generate constraints");
    assert_eq!(set.graph_constraints, 0);
    assert!(set.constraints.is_empty());
    assert_eq!(
        set.index_markdown,
        "# Constraints\n\nNo constraints recorded yet.\n"
    );

    let bytes = set.zip_archive().expect("create archive");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open archive");
    assert_eq!(archive.len(), 1);
    assert!(archive.by_name(CONSTRAINTS_INDEX_FILENAME).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constraint_generation_rejects_zero_batch_size() {
    let dir = temp_dir("zero-batch");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");

    let error = generate_constraint_set(&state, ConstraintGenerationOptions { batch_size: 0 })
        .expect_err("zero batch size must fail");
    assert!(error.to_string().contains("batch size must be >= 1"));

    let _ = std::fs::remove_dir_all(&dir);
}
