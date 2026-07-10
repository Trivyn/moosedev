//! SystemComponent path coverage declaration tests.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{symbols, Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const REALIZES: &str = "https://trivyn.io/ontologies/software/code#realizes";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 code/substrate/resolve().";

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-declare-component-paths-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

fn state_with_substrate(name: &str) -> AppState {
    let state = bootstrap(name);
    state.set_substrate(Arc::new(synthetic_substrate(false)));
    state
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
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

fn seed_component(state: &AppState, title: &str) -> String {
    record(state, "SystemComponent", title)
}

fn covers_path_count(state: &AppState) -> usize {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            None,
            Some(NamedNodeRef::new(COVERS_PATH).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .count()
}

fn literal_values(state: &AppState, subject: &str, predicate: &str) -> Vec<String> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    let mut values = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn has_edge(state: &AppState, subject: &str, predicate: &str, object: &str) -> bool {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            Some(NamedNodeRef::new(object).unwrap().into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .next()
        .is_some()
}

#[test]
fn declare_paths_by_name_and_component_lookup_uses_prefix() {
    let state = bootstrap("by-name");
    let component = seed_component(&state, "substrate component");

    let out = graph::declare_component_paths(
        &state,
        "substrate component",
        &["src/code/".to_string(), "src/lib.rs".to_string()],
    )
    .expect("declare paths");

    assert_eq!(out.component_iri, component);
    assert_eq!(out.component_name, "substrate component");
    assert_eq!(out.added, vec!["src/code/", "src/lib.rs"]);
    assert!(out.already_covered.is_empty());
    let components = graph::load_components(&state).unwrap();
    let entry = components
        .iter()
        .find(|entry| entry.iri.as_deref() == Some(component.as_str()))
        .unwrap();
    assert_eq!(
        entry.covers_paths,
        ["src/code/".to_string(), "src/lib.rs".to_string()]
            .into_iter()
            .collect()
    );
    assert_eq!(
        graph::best_component_for_path("src/code/substrate/resolver.rs", &components)
            .unwrap()
            .iri
            .as_deref(),
        Some(component.as_str())
    );
}

#[test]
fn redeclare_by_iri_is_idempotent() {
    let state = bootstrap("idempotent");
    let component = seed_component(&state, "substrate component");
    graph::declare_component_paths(
        &state,
        "substrate component",
        &["src/code/".to_string(), "src/lib.rs".to_string()],
    )
    .unwrap();
    let before = covers_path_count(&state);

    let out = graph::declare_component_paths(
        &state,
        &component,
        &["src/lib.rs".to_string(), "src/code/".to_string()],
    )
    .expect("redeclare");

    assert!(out.added.is_empty());
    assert_eq!(out.already_covered, vec!["src/code/", "src/lib.rs"]);
    assert_eq!(covers_path_count(&state), before);
}

#[test]
fn unknown_component_name_errors() {
    let state = bootstrap("unknown");
    seed_component(&state, "known component");

    let err = graph::declare_component_paths(&state, "missing component", &["src/".to_string()])
        .expect_err("unknown component");

    assert!(err.to_string().contains("missing component"));
}

#[test]
fn non_component_target_is_rejected() {
    let state = bootstrap("non-component");
    let decision = record(&state, "ArchitecturalDecision", "Not a component");

    let err = graph::declare_component_paths(&state, &decision, &["src/".to_string()])
        .expect_err("non-component");

    assert!(err.to_string().contains("SystemComponent"));
}

#[test]
fn invalid_paths_write_nothing() {
    let state = bootstrap("invalid");
    seed_component(&state, "substrate component");
    let before = covers_path_count(&state);

    let err = graph::declare_component_paths(
        &state,
        "substrate component",
        &[
            "src/code/".to_string(),
            "".to_string(),
            "/abs/path".to_string(),
            "a/../b".to_string(),
        ],
    )
    .expect_err("invalid paths");

    assert!(err.to_string().contains("path"));
    assert_eq!(covers_path_count(&state), before);
}

#[test]
fn declared_coverage_wires_through_to_mint_realizes() {
    let state = state_with_substrate("mint");
    let component = seed_component(&state, "substrate component");
    let substrate = state.substrate().expect("substrate");
    let definitions = substrate.definitions();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();

    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 1);
    assert!(plan.create[0].realizes.is_none());
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let entity_iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&normalize(PUBLIC_SYMBOL)].clone();
    assert!(!has_edge(&state, &entity_iri, REALIZES, &component));

    graph::declare_component_paths(&state, &component, &["src/code/".to_string()]).unwrap();
    let components = graph::load_components(&state).unwrap();
    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.update.len(), 1);
    assert_eq!(plan.update[0].realizes.as_deref(), Some(component.as_str()));

    graph::apply_mint(&state, &plan, &terms).unwrap();
    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 0);
    assert_eq!(plan.unchanged, 1);
    assert!(has_edge(&state, &entity_iri, REALIZES, &component));
}

#[test]
fn declared_literals_are_stored_on_the_component() {
    let state = bootstrap("stored");
    let component = seed_component(&state, "substrate component");

    graph::declare_component_paths(&state, &component, &["src/code/".to_string()]).unwrap();

    assert_eq!(
        literal_values(&state, &component, COVERS_PATH),
        vec!["src/code/"]
    );
}

fn synthetic_substrate(stale: bool) -> Substrate {
    let mut index = Index::new();
    let mut document = doc("src/code/substrate/resolver.rs");
    document.symbols.push(info(
        PUBLIC_SYMBOL,
        "resolve",
        symbol_information::Kind::Function,
        "pub fn resolve()",
    ));
    document
        .occurrences
        .push(occ(PUBLIC_SYMBOL, vec![10, 4, 11], 1));
    index.documents.push(document);

    Substrate::from_index(index, meta(), stale).expect("synthetic substrate")
}

fn normalize(symbol: &str) -> String {
    symbols::normalize_symbol(symbol).expect("normalizable symbol")
}

fn doc(relative_path: &str) -> Document {
    let mut document = Document::new();
    document.relative_path = relative_path.to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    document
}

fn occ(symbol: &str, range: Vec<i32>, symbol_roles: i32) -> Occurrence {
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.to_string();
    occurrence.range = range;
    occurrence.symbol_roles = symbol_roles;
    occurrence.enclosing_range = vec![0, 0, 30, 0];
    occurrence
}

fn info(
    symbol: &str,
    display_name: &str,
    kind: symbol_information::Kind,
    signature: &str,
) -> SymbolInformation {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = display_name.to_string();
    info.kind = EnumOrUnknown::new(kind);
    let mut signature_documentation = Signature::new();
    signature_documentation.text = signature.to_string();
    info.signature_documentation = MessageField::some(signature_documentation);
    info
}

fn meta() -> SubstrateMeta {
    SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        1,
        1,
    )
}
