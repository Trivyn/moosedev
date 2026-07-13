//! Ratification queue (`ProposedLink`) lifecycle test (v2.1).
//!
//! Proves the D1 interlock: a proposed link is invisible to the dossier and the
//! why-coverage metric until accepted; accept materializes the real edge and
//! moves the number; reject creates no edge; and GROWL never infers
//! `rdf:type InformationRecord` onto a proposal from its reused arch predicates.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, DossierTarget, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const ALPHA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/alpha().";
const ALPHA_NORM: &str = "rust-analyzer cargo testpkg . foo/alpha().";
const BETA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/beta().";
const BETA_NORM: &str = "rust-analyzer cargo testpkg . foo/beta().";

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
    add_def(&mut module_doc, ALPHA_RAW, "alpha", "pub fn alpha()", 0);
    add_def(&mut module_doc, BETA_RAW, "beta", "pub fn beta()", 1);
    index.documents.push(module_doc);
    let occurrences = index
        .documents
        .iter()
        .map(|d| d.occurrences.len())
        .sum::<usize>();
    let meta = SubstrateMeta::single("rust-analyzer", "commit0", Utc::now(), 1, occurrences);
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
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
    .expect("record instance")
}

fn insert_quad(state: &AppState, subject: &str, predicate: &str, object: Term) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(predicate).unwrap(),
        object,
        oxigraph::model::GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
}

struct Fixture {
    state: AppState,
    alpha: String,
    beta: String,
    constraint: String,
}

fn setup(tag: &str) -> Fixture {
    let state = AppState::bootstrap(&fresh_dir(tag), &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(synthetic_substrate()));

    let foo = record(&state, "SystemComponent", "foo");
    insert_quad(
        &state,
        &foo,
        COVERS_PATH,
        Literal::new_simple_literal("src/foo/").into(),
    );

    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();
    let defs = state.substrate().unwrap().definitions();
    let plan =
        graph::plan_mint(&state, &defs, &terms, &components, state.substrate().as_deref()).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    let entities = graph::entities_by_symbol(&state, &terms).unwrap();
    let alpha = entities.get(ALPHA_NORM).expect("alpha minted").clone();
    let beta = entities.get(BETA_NORM).expect("beta minted").clone();
    let constraint = record(&state, "Constraint", "must stay pure");
    Fixture {
        state,
        alpha,
        beta,
        constraint,
    }
}

fn foo_numerator(state: &AppState) -> usize {
    graph::compute_why_coverage(state)
        .unwrap()
        .components
        .iter()
        .find(|c| c.name == "foo")
        .expect("foo component")
        .numerator
}

fn has_records(state: &AppState, iri: &str) -> bool {
    graph::get_entity_dossier(state, &DossierTarget::Iri(iri.to_string()))
        .unwrap()
        .map(|d| !d.direct_records.is_empty())
        .unwrap_or(false)
}

#[test]
fn proposed_link_is_invisible_until_accepted() {
    let f = setup("proposals-accept");
    let p = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "cited in prose",
        "tester",
        Utc::now(),
    )
    .unwrap();

    // Pending + listable, but invisible to the dossier and the metric.
    assert_eq!(graph::pending_count(&f.state).unwrap(), 1);
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].subject_iri, f.constraint);
    assert_eq!(pending[0].predicate_local, "concerns");
    assert_eq!(pending[0].target_symbol, ALPHA_RAW);
    assert!(
        !has_records(&f.state, &f.alpha),
        "a proposal must not create the concerns edge"
    );
    assert_eq!(
        foo_numerator(&f.state),
        0,
        "a proposal must not inflate why-coverage"
    );

    // GROWL must not infer rdf:type InformationRecord from the reused arch predicates.
    f.state.ensure_enriched();
    let info_record = f.state.resolve_class("InformationRecord").unwrap();
    let has_info_type = f
        .state
        .store
        .quads_for_pattern(
            Some(NamedNode::new(&p).unwrap().as_ref().into()),
            Some(NamedNodeRef::new(moose::RDF_TYPE).unwrap()),
            Some(NamedNodeRef::new(&info_record).unwrap().into()),
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .next()
        .is_some();
    assert!(
        !has_info_type,
        "ProposedLink must never gain rdf:type InformationRecord via GROWL"
    );

    // Accept materializes the edge, flips status, and moves the metric.
    graph::accept_proposal(&f.state, &p, "tester").unwrap();
    assert!(
        has_records(&f.state, &f.alpha),
        "accept materializes the concerns edge"
    );
    assert_eq!(foo_numerator(&f.state), 1, "accept documents alpha");
    assert_eq!(graph::pending_count(&f.state).unwrap(), 0);
    assert_eq!(
        graph::list_proposals(&f.state, Some("accepted")).unwrap().len(),
        1
    );

    // Re-accepting a resolved proposal is a guarded error.
    assert!(graph::accept_proposal(&f.state, &p, "tester").is_err());
}

#[test]
fn rejected_link_creates_no_edge() {
    let f = setup("proposals-reject");
    let p = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        BETA_RAW,
        "src/foo/a.rs",
        "cited in prose",
        "tester",
        Utc::now(),
    )
    .unwrap();
    graph::reject_proposal(&f.state, &p, "tester").unwrap();

    assert!(
        !has_records(&f.state, &f.beta),
        "reject must not create an edge"
    );
    let rejected = graph::list_proposals(&f.state, Some("rejected")).unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].iri, p);
    assert_eq!(graph::pending_count(&f.state).unwrap(), 0);

    // Re-rejecting is a guarded error; a bogus IRI is rejected too.
    assert!(graph::reject_proposal(&f.state, &p, "tester").is_err());
    assert!(graph::accept_proposal(
        &f.state,
        "https://moosedev.dev/kg/ProposedLink/does-not-exist",
        "tester"
    )
    .is_err());
}
