use std::collections::BTreeSet;

use chrono::Utc;
use moosedev::export::{export_graph, ExportFormat, ExportScope};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::graph_import::{import_graph, ImportFormat, ImportMode};
use moosedev::provenance::PROVENANCE_GRAPH_IRI;
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, Quad, Term};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-import-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record_decision(state: &AppState, title: &str) -> String {
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision")
}

fn insert_provenance_only(state: &AppState, label: &str) -> NamedNode {
    let subject = NamedNode::new(format!("https://example.test/{label}")).unwrap();
    state
        .store
        .insert(&Quad::new(
            subject.clone(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Term::Literal(Literal::new_simple_literal(label)),
            GraphName::NamedNode(NamedNode::new(PROVENANCE_GRAPH_IRI).unwrap()),
        ))
        .expect("insert provenance-only triple");
    subject
}

fn graph_quads(state: &AppState, graph_iri: &str) -> BTreeSet<String> {
    let graph = NamedNode::new(graph_iri).unwrap();
    state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .map(|quad| quad.expect("read quad").to_string())
        .collect()
}

#[test]
fn ttl_patch_inserts_missing_project_triples_and_is_idempotent() {
    let dir = temp_dir("ttl-patch");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<https://example.test/imported> rdfs:label "Imported" .
"#;

    let first = import_graph(
        &state.store,
        ExportScope::Project,
        ImportFormat::Turtle,
        ImportMode::Patch,
        ttl,
    )
    .expect("patch import");
    let second = import_graph(
        &state.store,
        ExportScope::Project,
        ImportFormat::Turtle,
        ImportMode::Patch,
        ttl,
    )
    .expect("patch import again");

    assert_eq!(first.inserted_quad_count, 1);
    assert_eq!(first.skipped_existing_count, 0);
    assert_eq!(second.inserted_quad_count, 0);
    assert_eq!(second.skipped_existing_count, 1);
    assert!(graph_quads(&state, PROJECT_KG_GRAPH_IRI)
        .iter()
        .any(|quad| quad.contains("https://example.test/imported")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ttl_replace_removes_prior_project_triples_atomically() {
    let dir = temp_dir("ttl-replace");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let old_iri = record_decision(&state, "Old record");
    let ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<https://example.test/replacement> rdfs:label "Replacement" .
"#;

    let outcome = import_graph(
        &state.store,
        ExportScope::Project,
        ImportFormat::Turtle,
        ImportMode::Replace,
        ttl,
    )
    .expect("replace import");
    let quads = graph_quads(&state, PROJECT_KG_GRAPH_IRI);

    assert!(outcome.removed_quad_count > 0);
    assert_eq!(outcome.inserted_quad_count, 1);
    assert!(!quads.iter().any(|quad| quad.contains(&old_iri)));
    assert!(quads
        .iter()
        .any(|quad| quad.contains("https://example.test/replacement")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nquads_all_restore_preserves_project_and_provenance_graphs() {
    let source_dir = temp_dir("nq-source");
    let source = AppState::bootstrap(&source_dir, &ontology_dir()).expect("bootstrap source");
    let project_iri = record_decision(&source, "Dataset project");
    let provenance_subject = insert_provenance_only(&source, "dataset-provenance");
    let dump = export_graph(&source.store, ExportScope::All, ExportFormat::NQuads).expect("export");

    let target_dir = temp_dir("nq-target");
    let target = AppState::bootstrap(&target_dir, &ontology_dir()).expect("bootstrap target");
    record_decision(&target, "Target removed");

    let outcome = import_graph(
        &target.store,
        ExportScope::All,
        ImportFormat::NQuads,
        ImportMode::Replace,
        &dump.text,
    )
    .expect("restore nquads");

    assert!(outcome.removed_quad_count > 0);
    assert!(graph_quads(&target, PROJECT_KG_GRAPH_IRI)
        .iter()
        .any(|quad| quad.contains(&project_iri)));
    assert!(graph_quads(&target, PROVENANCE_GRAPH_IRI)
        .iter()
        .any(|quad| quad.contains(provenance_subject.as_str())));

    let _ = std::fs::remove_dir_all(&source_dir);
    let _ = std::fs::remove_dir_all(&target_dir);
}

#[test]
fn nquads_rejects_graphs_outside_selected_scope_before_mutating() {
    let dir = temp_dir("nq-reject-scope");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let before = graph_quads(&state, PROJECT_KG_GRAPH_IRI);
    let nq = format!(
        "<https://example.test/s> <{}> \"bad\" <{}> .\n",
        moose::RDFS_LABEL,
        PROVENANCE_GRAPH_IRI
    );

    let err = import_graph(
        &state.store,
        ExportScope::Project,
        ImportFormat::NQuads,
        ImportMode::Patch,
        &nq,
    )
    .expect_err("provenance quad should be outside project scope");

    assert!(err.to_string().contains("outside the selected scope"));
    assert_eq!(graph_quads(&state, PROJECT_KG_GRAPH_IRI), before);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_rdf_leaves_store_unchanged() {
    let dir = temp_dir("invalid-rdf");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "Keep me");
    let before = graph_quads(&state, PROJECT_KG_GRAPH_IRI);

    let err = import_graph(
        &state.store,
        ExportScope::Project,
        ImportFormat::Turtle,
        ImportMode::Replace,
        "this is not turtle",
    )
    .expect_err("invalid turtle should fail");

    assert!(err.to_string().contains("error") || err.to_string().contains("parse"));
    assert_eq!(graph_quads(&state, PROJECT_KG_GRAPH_IRI), before);

    let _ = std::fs::remove_dir_all(&dir);
}
