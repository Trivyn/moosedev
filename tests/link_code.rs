//! CodeEntity link tool integration tests.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{symbols, Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, CodeSelector, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
const PRIVATE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/private_helper().";
const LOCAL_SYMBOL: &str = "local 0";

fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-link-code-{name}-{}-{}",
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
    seed_component(&state, "runtime component", "src/");
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

fn seed_component(state: &AppState, title: &str, covers_path: &str) -> String {
    let iri = record(state, "SystemComponent", title);
    insert_literal(state, &iri, COVERS_PATH, covers_path);
    iri
}

fn insert_literal(state: &AppState, subject: &str, predicate: &str, value: &str) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(predicate).unwrap(),
        Literal::new_simple_literal(value),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
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

fn has_type(state: &AppState, subject: &str, class_iri: &str) -> bool {
    has_edge(state, subject, moose::RDF_TYPE, class_iri)
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

fn project_quad_count(state: &AppState) -> usize {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph)))
        .flatten()
        .count()
}

fn pre_mint_public(state: &AppState) -> String {
    let substrate = state.substrate().expect("substrate");
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let components = graph::load_components(state).unwrap();
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(state, &definitions, &terms, &components).unwrap();
    graph::apply_mint(state, &plan, &terms).unwrap();
    graph::entities_by_symbol(state, &terms).unwrap()[&normalize(PUBLIC_SYMBOL)].clone()
}

fn normalize(symbol: &str) -> String {
    symbols::normalize_symbol(symbol).expect("normalizable symbol")
}

fn selector_public_position() -> CodeSelector {
    CodeSelector::Position {
        file: "src/runtime.rs".to_string(),
        line: 8,
        col: 5,
    }
}

fn selector_private_position() -> CodeSelector {
    CodeSelector::Position {
        file: "src/runtime.rs".to_string(),
        line: 12,
        col: 5,
    }
}

#[test]
fn link_existing_entity_by_position_without_duplicate_mint() {
    let state = state_with_substrate("existing");
    let decision = record(&state, "ArchitecturalDecision", "Runtime builder decision");
    let entity_iri = pre_mint_public(&state);
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let count_before = graph::entities_by_symbol(&state, &terms).unwrap().len();

    let out = graph::link_code(
        &state,
        &decision,
        "concerns",
        &selector_public_position(),
        "tester",
    )
    .expect("link code");

    assert!(!out.created);
    assert_eq!(out.entity_iri, entity_iri);
    assert_eq!(out.entity_name, "build_server");
    assert_eq!(out.subject_iri, decision);
    assert_eq!(out.object_iri, entity_iri);
    let predicate = state.resolve_object_property(&out.predicate_local).unwrap();
    assert!(has_edge(
        &state,
        &out.subject_iri,
        &predicate,
        &out.object_iri
    ));
    assert_eq!(
        graph::entities_by_symbol(&state, &terms).unwrap().len(),
        count_before
    );
}

#[test]
fn lazy_mint_private_function_and_link() {
    let state = state_with_substrate("private");
    let decision = record(&state, "ArchitecturalDecision", "Private helper decision");
    let terms = graph::CodeTerms::resolve(&state).unwrap();

    let out = graph::link_code(
        &state,
        &decision,
        "concerns",
        &selector_private_position(),
        "tester",
    )
    .expect("link code");

    assert!(out.created);
    assert!(has_type(&state, &out.entity_iri, &terms.code_entity_class));
    assert_eq!(
        literal_values(&state, &out.entity_iri, &terms.has_substrate_symbol),
        vec![normalize(PRIVATE_SYMBOL)]
    );
    let predicate = state.resolve_object_property(&out.predicate_local).unwrap();
    assert!(has_edge(
        &state,
        &out.subject_iri,
        &predicate,
        &out.object_iri
    ));
}

#[test]
fn whitespace_position_writes_nothing() {
    let state = state_with_substrate("whitespace");
    let decision = record(&state, "ArchitecturalDecision", "Whitespace miss");
    let count_before = project_quad_count(&state);
    let selector = CodeSelector::Position {
        file: "src/runtime.rs".to_string(),
        line: 2,
        col: 1,
    };

    let err = graph::link_code(&state, &decision, "concerns", &selector, "tester")
        .expect_err("whitespace should miss");

    assert!(err
        .to_string()
        .starts_with("no code entity at src/runtime.rs:2:1"));
    assert_eq!(project_quad_count(&state), count_before);
}

#[test]
fn local_symbol_position_writes_nothing() {
    let state = state_with_substrate("local");
    let decision = record(&state, "ArchitecturalDecision", "Local miss");
    let count_before = project_quad_count(&state);
    let selector = CodeSelector::Position {
        file: "src/runtime.rs".to_string(),
        line: 21,
        col: 9,
    };

    let err = graph::link_code(&state, &decision, "concerns", &selector, "tester")
        .expect_err("locals should be rejected");

    assert!(err.to_string().contains("local"));
    assert_eq!(project_quad_count(&state), count_before);
}

#[test]
fn inverse_orientation_for_requirement_satisfies() {
    let state = state_with_substrate("inverse");
    let requirement = record(&state, "Requirement", "Runtime builder requirement");
    let satisfies = state.resolve_object_property("satisfies").unwrap();

    let out = graph::link_code(
        &state,
        &requirement,
        "satisfies",
        &selector_public_position(),
        "tester",
    )
    .expect("link code");

    assert_eq!(out.subject_iri, out.entity_iri);
    assert_eq!(out.object_iri, requirement);
    assert!(has_edge(
        &state,
        &out.subject_iri,
        &satisfies,
        &out.object_iri
    ));
}

#[test]
fn normalized_symbol_selector_reuses_existing_entity() {
    let state = state_with_substrate("symbol");
    let decision = record(&state, "ArchitecturalDecision", "Runtime builder symbol");
    let entity_iri = pre_mint_public(&state);
    let selector = CodeSelector::Symbol(normalize(PUBLIC_SYMBOL));

    let out =
        graph::link_code(&state, &decision, "concerns", &selector, "tester").expect("link code");

    assert!(!out.created);
    assert_eq!(out.entity_iri, entity_iri);
}

#[test]
fn no_substrate_is_reported() {
    let state = bootstrap("no-substrate");
    let decision = record(&state, "ArchitecturalDecision", "Missing substrate");

    let err = graph::link_code(
        &state,
        &decision,
        "concerns",
        &selector_public_position(),
        "tester",
    )
    .expect_err("missing substrate");

    assert!(err.to_string().contains("substrate unavailable"));
}

#[test]
fn illegal_predicate_lists_legal_alternatives() {
    let state = state_with_substrate("illegal");
    let decision = record(&state, "ArchitecturalDecision", "Illegal predicate");

    let err = graph::link_code(
        &state,
        &decision,
        "weighs",
        &selector_public_position(),
        "tester",
    )
    .expect_err("illegal predicate");
    let msg = err.to_string();

    assert!(msg.contains("weighs"));
    assert!(msg.contains("concerns"));
}

fn synthetic_substrate(stale: bool) -> Substrate {
    let mut index = Index::new();
    let mut document = doc("src/runtime.rs");
    document.symbols.push(info(
        MODULE_SYMBOL,
        "runtime",
        symbol_information::Kind::Module,
        "pub mod runtime",
    ));
    document
        .occurrences
        .push(occ(MODULE_SYMBOL, vec![0, 0, 30, 0], 1));
    document.symbols.push(info(
        PUBLIC_SYMBOL,
        "build_server",
        symbol_information::Kind::Function,
        "pub fn build_server()",
    ));
    document
        .occurrences
        .push(occ(PUBLIC_SYMBOL, vec![7, 4, 16], 1));
    document.symbols.push(info(
        PRIVATE_SYMBOL,
        "private_helper",
        symbol_information::Kind::Function,
        "fn private_helper()",
    ));
    document
        .occurrences
        .push(occ(PRIVATE_SYMBOL, vec![11, 4, 18], 1));
    document.symbols.push(info(
        LOCAL_SYMBOL,
        "tmp",
        symbol_information::Kind::Variable,
        "let tmp",
    ));
    document
        .occurrences
        .push(occ(LOCAL_SYMBOL, vec![20, 8, 11], 0));
    index.documents.push(document);

    Substrate::from_index(index, meta(), stale).expect("synthetic substrate")
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
    SubstrateMeta {
        schema_version: moosedev::code::substrate::meta::CURRENT_SCHEMA_VERSION,
        indexed_commit: "abc123".to_string(),
        indexed_at: DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        producer: "rust-analyzer".to_string(),
        producer_version: "1.0.0".to_string(),
        mode: "scip".to_string(),
        documents: 1,
        occurrences: 4,
    }
}
