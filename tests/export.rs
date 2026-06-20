use std::collections::BTreeSet;

use chrono::Utc;
use moosedev::export::{export_graph, ExportFormat, ExportScope};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::provenance::PROVENANCE_GRAPH_IRI;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, Quad, Term};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-export-{name}-{}-{}",
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

fn project_quads(state: &AppState) -> BTreeSet<String> {
    let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .map(|quad| quad.expect("read project quad").to_string())
        .collect()
}

fn parse_nquads(text: &str) -> BTreeSet<String> {
    RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(text.as_bytes())
        .map(|quad| quad.expect("parse exported n-quads").to_string())
        .collect()
}

#[test]
fn project_nquads_round_trips_to_store_quads() {
    let dir = temp_dir("round-trip");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "Export round trip");

    let dump =
        export_graph(&state.store, ExportScope::Project, ExportFormat::NQuads).expect("export");

    assert_eq!(dump.quad_count, dump.text.lines().count());
    assert_eq!(parse_nquads(&dump.text), project_quads(&state));
    assert_eq!(
        dump.graphs,
        vec![PROJECT_KG_GRAPH_IRI.to_string()],
        "project export should name only the project graph"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nquads_export_is_deterministic_and_sorted() {
    let dir = temp_dir("determinism");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "B title");
    record_decision(&state, "A title");

    let first =
        export_graph(&state.store, ExportScope::Project, ExportFormat::NQuads).expect("export");
    let second =
        export_graph(&state.store, ExportScope::Project, ExportFormat::NQuads).expect("export");

    assert_eq!(first.text, second.text);
    let lines: Vec<&str> = first.text.lines().collect();
    assert!(
        lines.windows(2).all(|pair| pair[0] <= pair[1]),
        "project N-Quads lines should be lexically sorted:\n{}",
        first.text
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_scope_isolates_project_and_provenance_graphs() {
    let dir = temp_dir("scope");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let project_iri = record_decision(&state, "Project scoped");
    let provenance_subject = insert_provenance_only(&state, "provenance-scoped");

    let project =
        export_graph(&state.store, ExportScope::Project, ExportFormat::NQuads).expect("project");
    let provenance = export_graph(&state.store, ExportScope::Provenance, ExportFormat::NQuads)
        .expect("provenance");
    let all = export_graph(&state.store, ExportScope::All, ExportFormat::NQuads).expect("all");

    assert!(project.text.contains(&project_iri));
    assert!(!project.text.contains(provenance_subject.as_str()));
    assert!(!project.text.contains(PROVENANCE_GRAPH_IRI));

    assert!(!provenance.text.contains(&project_iri));
    assert!(provenance.text.contains(provenance_subject.as_str()));

    assert!(all.text.contains(&project_iri));
    assert!(all.text.contains(provenance_subject.as_str()));
    assert!(!all.text.contains("ontology/software-architecture/shapes"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_rejects_unknown_format_and_scope() {
    assert!(ExportFormat::parse("xml").is_err());
    assert!(ExportScope::parse("ontology").is_err());
}

#[test]
fn graph_formats_strip_named_graph_identity_intentionally() {
    let dir = temp_dir("triple-format");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_decision(&state, "Triple format");

    let ntriples =
        export_graph(&state.store, ExportScope::Project, ExportFormat::NTriples).expect("nt");
    let turtle =
        export_graph(&state.store, ExportScope::Project, ExportFormat::Turtle).expect("ttl");

    assert!(!ntriples.text.contains(PROJECT_KG_GRAPH_IRI));
    assert!(!turtle.text.contains(PROJECT_KG_GRAPH_IRI));
    assert_eq!(ntriples.quad_count, ntriples.text.lines().count());
    assert!(!turtle.text.trim().is_empty());

    let parsed: Vec<_> = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(ntriples.text.as_bytes())
        .map(|quad| quad.expect("parse n-triples export"))
        .collect();
    assert!(parsed
        .iter()
        .all(|quad| quad.graph_name == GraphName::DefaultGraph));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn graph_formats_deduplicate_after_named_graphs_are_stripped() {
    let dir = temp_dir("triple-dedup");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let subject = NamedNode::new("https://example.test/duplicate-triple").unwrap();
    let predicate = NamedNode::new(moose::RDFS_LABEL).unwrap();
    let object = Term::Literal(Literal::new_simple_literal("Same triple"));

    for graph_iri in [PROJECT_KG_GRAPH_IRI, PROVENANCE_GRAPH_IRI] {
        state
            .store
            .insert(&Quad::new(
                subject.clone(),
                predicate.clone(),
                object.clone(),
                GraphName::NamedNode(NamedNode::new(graph_iri).unwrap()),
            ))
            .expect("insert duplicate triple in named graph");
    }

    let ntriples =
        export_graph(&state.store, ExportScope::All, ExportFormat::NTriples).expect("nt");
    let duplicate_lines = ntriples
        .text
        .lines()
        .filter(|line| line.contains(subject.as_str()))
        .count();

    assert_eq!(
        duplicate_lines, 1,
        "graph formats should emit a true triple union after graph names are stripped:\n{}",
        ntriples.text
    );

    let _ = std::fs::remove_dir_all(&dir);
}
