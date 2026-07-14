//! Classifier plan/apply integration (judgment stratum).
//!
//! Proves the full-accounting invariant (population = judgments plus
//! unclassified plus skipped), rule behavior over a synthetic substrate with
//! real fan-in and churn signals, dispositions from the confidence gate,
//! idempotent re-runs, and post-apply SHACL conformance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use moosedev::code::substrate::{ChurnIndex, FileChurn, Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, ProposalKind, RecordInput};
use oxigraph::model::{Literal, NamedNode, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const HANDLER_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 handlers/handler().";
const HOT_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 hot/hot().";
const PLAIN_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 plain/plain().";
const PLAIN_NORM: &str = "rust-analyzer cargo testpkg . plain/plain().";
const QUIET_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 quiet/quiet().";

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

fn add_def(d: &mut Document, symbol: &str, name: &str, line: i32) {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = name.to_string();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut sig = Signature::new();
    sig.text = format!("pub fn {name}()");
    info.signature_documentation = MessageField::some(sig);
    d.symbols.push(info);

    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.range = vec![line, 0, 10];
    occ.symbol_roles = 1;
    d.occurrences.push(occ);
}

fn add_ref(d: &mut Document, symbol: &str, line: i32) {
    let mut occ = Occurrence::new();
    occ.symbol = symbol.to_string();
    occ.range = vec![line, 0, 10];
    occ.symbol_roles = 0; // reference
    d.occurrences.push(occ);
}

/// Four public functions: a boundary-path handler, a hot low-churn function
/// (5 cross-file references), and two plain ones (one later constrained).
fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();

    let mut handlers = doc("src/api/handlers/policy.rs");
    add_def(&mut handlers, HANDLER_RAW, "handler", 0);
    index.documents.push(handlers);

    let mut hot = doc("src/graph/hot.rs");
    add_def(&mut hot, HOT_RAW, "hot", 0);
    index.documents.push(hot);

    let mut plain = doc("src/graph/plain.rs");
    add_def(&mut plain, PLAIN_RAW, "plain", 0);
    add_def(&mut plain, QUIET_RAW, "quiet", 1);
    // Five references to `hot` push its fan-in over P90.
    for line in 2..7 {
        add_ref(&mut plain, HOT_RAW, line);
    }
    index.documents.push(plain);

    let occurrences = index
        .documents
        .iter()
        .map(|d| d.occurrences.len())
        .sum::<usize>();
    let meta = SubstrateMeta::single("rust-analyzer", "commit0", Utc::now(), 3, occurrences);
    let churn = ChurnIndex {
        schema_version: 1,
        window_months: 24,
        anchored_commit: "commit0".to_string(),
        files: [(
            "src/graph/plain.rs".to_string(),
            FileChurn {
                commits: 3,
                last_commit: "2026-07-01T00:00:00+00:00".to_string(),
                distinct_authors: 1,
                top_author_share: 1.0,
            },
        )]
        .into(),
    };
    Substrate::from_index(index, meta, false)
        .expect("synthetic substrate")
        .with_churn(churn)
}

fn record(state: &AppState, kind: &str, title: &str, status: &str) -> String {
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

struct Fixture {
    state: AppState,
    substrate: Arc<Substrate>,
    repo_root: PathBuf,
}

fn setup(tag: &str) -> Fixture {
    let state = AppState::bootstrap(&fresh_dir(tag), &ontology_dir()).expect("bootstrap");
    let substrate = Arc::new(synthetic_substrate());
    state.set_substrate(substrate.clone());
    let repo_root = fresh_dir(&format!("{tag}-repo")); // no generated markers on disk

    let component = record(&state, "SystemComponent", "everything", "accepted");
    let quad = Quad::new(
        NamedNode::new(&component).unwrap(),
        NamedNode::new(COVERS_PATH).unwrap(),
        Term::from(Literal::new_simple_literal("src/")),
        oxigraph::model::GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();

    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();
    let defs = substrate.definitions();
    let plan = graph::plan_mint(&state, &defs, &terms, &components, Some(&*substrate)).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    // `plain` carries an accepted Constraint → criticality high, escalated.
    let entities = graph::entities_by_symbol(&state, &terms).unwrap();
    let plain = entities.get(PLAIN_NORM).expect("plain minted").clone();
    let constraint = record(&state, "Constraint", "plain is contract-bound", "accepted");
    graph::relate(&state, &constraint, "constrains", &plain).expect("constrains edge");

    Fixture {
        state,
        substrate,
        repo_root,
    }
}

#[test]
fn plan_is_fully_accounted_and_rules_fire() {
    let f = setup("classify-plan");
    let plan = graph::plan_classify(&f.state, &f.substrate, &f.repo_root).unwrap();

    // Population = 4 public functions; every one lands in exactly one bucket.
    assert_eq!(
        plan.role.len() + plan.unclassified.len() + plan.skipped_existing,
        4,
        "full accounting on the role axis"
    );
    assert!(plan.missing_entities.is_empty());
    assert_eq!(plan.skipped_existing, 0);

    // R2 boundary on the handler path, auto-held.
    let boundary = plan
        .role
        .iter()
        .find(|j| j.target_local == "boundary")
        .expect("handler classified boundary");
    assert_eq!(boundary.display_name, "handler");
    assert_eq!(boundary.escalation, "auto-held");
    assert!(boundary.evidence.contains("R2"));

    // R4 core-algorithm on hot (fan-in 5 ≥ P90, churn 0 ≤ median), escalated
    // because its criticality is proposed high.
    let core = plan
        .role
        .iter()
        .find(|j| j.target_local == "core-algorithm")
        .expect("hot classified core");
    assert_eq!(core.display_name, "hot");
    assert_eq!(core.escalation, "escalated");
    assert!(core.evidence.contains("fan-in 5"));

    // plain + quiet abstain — never guessed.
    assert_eq!(plan.unclassified.len(), 2);

    // Criticality deviations: hot (fan-in) + plain (constrains), both escalated.
    assert_eq!(plan.criticality.len(), 2);
    assert!(plan
        .criticality
        .iter()
        .all(|j| j.target_local == "high" && j.escalation == "escalated"));
    assert!(plan
        .criticality
        .iter()
        .any(|j| j.evidence.contains("constrains")));

    assert_eq!(plan.projected_quads, (2 + 2) * 11);
}

#[test]
fn apply_is_idempotent_and_conformant() {
    let f = setup("classify-apply");
    let plan = graph::plan_classify(&f.state, &f.substrate, &f.repo_root).unwrap();
    let outcome = graph::apply_classify(&f.state, &plan).unwrap();
    assert_eq!(outcome.taxonomy_created, 9, "6 roles + 3 criticalities");
    assert_eq!(outcome.proposed, 4, "2 role + 2 criticality judgments");

    // All proposed, none materialized, judgments never nudge.
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(
        pending
            .iter()
            .filter(|p| p.kind == ProposalKind::Judgment)
            .count(),
        4
    );
    assert_eq!(graph::pending_count(&f.state).unwrap(), 0);

    // Post-apply the graph conforms (shapeless proposal nodes + individuals).
    let report = moosedev::validation::validate_project(&f.state).unwrap();
    assert!(report.conforms());

    // Re-plan: everything already judged or honestly abstained; nothing new.
    let replan = graph::plan_classify(&f.state, &f.substrate, &f.repo_root).unwrap();
    assert!(replan.role.is_empty());
    assert!(replan.criticality.is_empty());
    assert_eq!(replan.skipped_existing, 2, "both judged entities skip");
    assert_eq!(replan.unclassified.len(), 2, "abstentions re-report");
    let outcome = graph::apply_classify(&f.state, &replan).unwrap();
    assert_eq!(outcome.proposed, 0);
    assert_eq!(outcome.taxonomy_created, 0, "taxonomy seeding idempotent");
}
