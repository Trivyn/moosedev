//! Why-coverage debt metric integration test (v2.1).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn doc(path: &str) -> Document {
    let mut d = Document::new();
    d.relative_path = path.to_string();
    d.position_encoding = EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    d
}

/// Push a definition (symbol + its definition occurrence) into a document.
fn add_def(d: &mut Document, symbol: &str, name: &str, signature: &str, line: i32) {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = name.to_string();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut sig = Signature::new();
    sig.text = signature.to_string();
    info.signature_documentation = MessageField::some(sig);
    d.symbols.push(info);

    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.range = vec![line, 0, 10];
    occ.symbol_roles = 1; // definition role
    occ.enclosing_range = vec![line, 0, 10];
    d.occurrences.push(occ);
}

fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();

    let mut module_doc = doc("src/foo/a.rs");
    add_def(
        &mut module_doc,
        "rust-analyzer cargo testpkg 0.1.0 foo/alpha().",
        "alpha",
        "pub fn alpha()",
        0,
    );
    add_def(
        &mut module_doc,
        "rust-analyzer cargo testpkg 0.1.0 foo/beta().",
        "beta",
        "pub fn beta()",
        1,
    );
    add_def(
        &mut module_doc,
        "rust-analyzer cargo testpkg 0.1.0 foo/helper().",
        "helper",
        "fn helper()",
        2,
    );
    index.documents.push(module_doc);

    let mut orphan = doc("src/orphan/z.rs");
    add_def(
        &mut orphan,
        "rust-analyzer cargo testpkg 0.1.0 orphan/gamma().",
        "gamma",
        "pub fn gamma()",
        0,
    );
    index.documents.push(orphan);

    let occurrences = index
        .documents
        .iter()
        .map(|d| d.occurrences.len())
        .sum::<usize>();
    let meta = SubstrateMeta::single(
        "rust-analyzer",
        "commit0",
        Utc::now(),
        index.documents.len(),
        occurrences,
    );
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    record_with_status(state, kind, title, "accepted")
}

fn record_with_status(state: &AppState, kind: &str, title: &str, status: &str) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), status.to_string()),
            ],
        },
        "tester",
        Utc::now(),
    )
    .expect("record instance")
}

fn insert_quad(state: &AppState, subject: &str, predicate: &str, object: oxigraph::model::Term) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(predicate).unwrap(),
        object,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
}

#[test]
fn why_coverage_counts_documented_public_surface() {
    let state = AppState::bootstrap(&fresh_dir("debt-data"), &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(synthetic_substrate()));

    // Component "foo" owns src/foo/; src/orphan/ is owned by nobody.
    let foo = record(&state, "SystemComponent", "foo");
    insert_quad(
        &state,
        &foo,
        COVERS_PATH,
        Literal::new_simple_literal("src/foo/").into(),
    );

    // Mint public entities from the substrate (alpha, beta, gamma).
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();
    let defs = state.substrate().unwrap().definitions();
    let plan = graph::plan_mint(
        &state,
        &defs,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    // Document alpha with accepted knowledge. A proposed record concerns beta,
    // but inbox material must not increase authoritative why-coverage.
    let entities = graph::entities_by_symbol(&state, &terms).unwrap();
    let alpha = entities
        .get("rust-analyzer cargo testpkg . foo/alpha().")
        .expect("alpha minted")
        .clone();
    let constraint = record(&state, "Constraint", "alpha must stay pure");
    let concerns = state.resolve_object_property("concerns").unwrap();
    insert_quad(
        &state,
        &constraint,
        &concerns,
        NamedNode::new(&alpha).unwrap().into(),
    );
    let beta = entities
        .get("rust-analyzer cargo testpkg . foo/beta().")
        .expect("beta minted")
        .clone();
    let proposed = record_with_status(
        &state,
        "ArchitecturalDecision",
        "proposed beta rationale",
        "proposed",
    );
    insert_quad(
        &state,
        &proposed,
        &concerns,
        NamedNode::new(&beta).unwrap().into(),
    );

    let report = graph::compute_why_coverage(&state).unwrap();
    let foo_cov = report
        .components
        .iter()
        .find(|c| c.name == "foo")
        .expect("foo component present");

    assert_eq!(
        foo_cov.denominator, 2,
        "alpha + beta are public, non-module, non-test; helper (private) is excluded"
    );
    assert_eq!(
        foo_cov.numerator, 1,
        "only alpha has a dossier-visible linked record"
    );
    assert_eq!(
        foo_cov.undocumented,
        vec!["beta".to_string()],
        "beta is the undocumented public entity"
    );
    assert_eq!(foo_cov.ratio(), Some(0.5));
    assert_eq!(
        report.unmapped, 1,
        "gamma in src/orphan/ maps to no component"
    );
}
