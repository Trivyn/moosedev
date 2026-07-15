//! Ratification queue (`ProposedLink`) lifecycle test (v2.1).
//!
//! Proves the D1 interlock: a proposed link is invisible to the dossier and the
//! why-coverage metric until accepted; accept materializes the real edge and
//! moves the number; reject creates no edge; and GROWL never infers
//! `rdf:type InformationRecord` onto a proposal from its reused arch predicates.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

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

    let component = record(&state, "SystemComponent", "foo");
    insert_quad(
        &state,
        &component,
        COVERS_PATH,
        Literal::new_simple_literal("src/foo/").into(),
    );

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
        graph::list_proposals(&f.state, Some("accepted"))
            .unwrap()
            .len(),
        1
    );

    // Re-accepting a resolved proposal is a guarded error.
    assert!(graph::accept_proposal(&f.state, &p, "tester").is_err());
}

fn has_edge(state: &AppState, subject: &str, predicate_local: &str, object: &str) -> bool {
    let predicate = state.resolve_object_property(predicate_local).unwrap();
    state
        .store
        .quads_for_pattern(
            Some(NamedNode::new(subject).unwrap().as_ref().into()),
            Some(NamedNodeRef::new(&predicate).unwrap()),
            Some(NamedNodeRef::new(object).unwrap().into()),
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .next()
        .is_some()
}

#[test]
fn judgment_lifecycle_proposes_ratifies_and_rejects() {
    let f = setup("judgment-lifecycle");
    assert!(graph::ensure_taxonomy_individuals(&f.state).unwrap() > 0);
    assert_eq!(
        graph::ensure_taxonomy_individuals(&f.state).unwrap(),
        0,
        "taxonomy seeding is idempotent"
    );

    let role = graph::role_iri("boundary");
    let p = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &role,
        0.75,
        graph::AUTO_HELD,
        "R2 boundary: public + api path pattern",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();

    // Advisory-until-ratified: no edge, no dossier records, no debt movement —
    // and judgments never nudge.
    assert!(!has_edge(&f.state, &f.alpha, "playsRole", &role));
    assert!(!has_records(&f.state, &f.alpha));
    assert_eq!(foo_numerator(&f.state), 0);
    assert_eq!(
        graph::pending_count(&f.state).unwrap(),
        0,
        "judgments (even pending) never count toward the nudge"
    );
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1, "but they list in the inbox");
    assert_eq!(pending[0].kind, graph::ProposalKind::Judgment);
    assert_eq!(pending[0].subject_iri, f.alpha);
    assert_eq!(pending[0].target_iri, role);
    assert_eq!(pending[0].confidence.as_deref(), Some("0.75"));
    assert_eq!(pending[0].escalation.as_deref(), Some("auto-held"));

    // Ratify: exactly the playing-relation edge materializes (shape-validated).
    let outcome = graph::accept_proposal(&f.state, &p, "james").unwrap();
    assert!(matches!(outcome, graph::AcceptOutcome::Judgment { .. }));
    assert!(has_edge(&f.state, &f.alpha, "playsRole", &role));
    assert_eq!(
        foo_numerator(&f.state),
        0,
        "a judgment is not a rationale record; why-coverage unmoved"
    );
    let judgments = graph::judgments_for_entity(&f.state, &f.alpha).unwrap();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].status, "accepted");
    assert_eq!(judgments[0].target_local, "boundary");
    assert_eq!(judgments[0].author, "moosedev-classifier");

    // Post-accept the graph still conforms (the new shape branches hold).
    let report = moosedev::validation::validate_project(&f.state).unwrap();
    assert!(report.conforms(), "graph conforms after judgment accept");

    // Re-accepting is guarded.
    assert!(graph::accept_proposal(&f.state, &p, "james").is_err());

    // The dossier now speaks (silence amendment: judgments count as direct
    // knowledge) and renders the ratified judgment with provenance, plus the
    // core-entity hotspot line since no rationale records are linked.
    let dossier = graph::get_entity_dossier(&f.state, &DossierTarget::Iri(f.alpha.clone()))
        .unwrap()
        .expect("judgment-only entity produces a dossier");
    assert_eq!(dossier.judgments.len(), 1);
    let markdown = graph::render_markdown(&dossier);
    assert!(markdown.contains("role: boundary — proposed by moosedev-classifier, ratified"));

    // An escalated criticality judgment, rejected → no edge, gone from readers.
    let crit = graph::criticality_iri("high");
    let p2 = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "hasCriticality",
        &crit,
        0.6,
        graph::ESCALATED,
        "fan-in P92; accepted constrains link",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();
    graph::reject_proposal(&f.state, &p2, "james").unwrap();
    assert!(!has_edge(&f.state, &f.alpha, "hasCriticality", &crit));
    let judgments = graph::judgments_for_entity(&f.state, &f.alpha).unwrap();
    assert_eq!(judgments.len(), 1, "rejected judgments drop out of readers");
}

#[test]
fn propose_judgment_is_idempotent_against_pending_duplicates() {
    let f = setup("judgment-dedup");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();

    let role = graph::role_iri("boundary");
    let propose = |author: &str| {
        graph::propose_judgment(
            &f.state,
            &f.alpha,
            "playsRole",
            &role,
            1.0,
            graph::ESCALATED,
            "asserted by a human from the editor",
            author,
            Utc::now(),
        )
        .unwrap()
    };

    // An identical pending (entity, predicate, target) returns the same node —
    // regardless of who repeats it.
    let first = propose("editor:neovim");
    assert_eq!(propose("editor:neovim"), first);
    assert_eq!(propose("moosedev-classifier"), first);
    assert_eq!(
        graph::list_proposals(&f.state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );

    // A different target on the same axis is REFUSED while one is pending —
    // `maxCount 1` means a second proposal could never be ratified, so the
    // writer enforces axis exclusivity for every submission path.
    let err = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("glue"),
        1.0,
        graph::ESCALATED,
        "second opinion",
        "editor:neovim",
        Utc::now(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("already has a playsRole judgment"),
        "axis guard names the conflict: {err}"
    );
    // A different AXIS is untouched by the guard.
    graph::propose_judgment(
        &f.state,
        &f.alpha,
        "hasCriticality",
        &graph::criticality_iri("high"),
        1.0,
        graph::ESCALATED,
        "orthogonal axis",
        "editor:neovim",
        Utc::now(),
    )
    .unwrap();

    // Once the pending twin is resolved, the axis is free again.
    graph::reject_proposal(&f.state, &first, "james").unwrap();
    let after_reject = propose("editor:neovim");
    assert_ne!(after_reject, first);

    // A RATIFIED judgment blocks the axis permanently (until superseded).
    graph::ensure_taxonomy_individuals(&f.state).unwrap();
    graph::accept_proposal(&f.state, &after_reject, "james").unwrap();
    let err = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("glue"),
        1.0,
        graph::ESCALATED,
        "late second opinion",
        "editor:neovim",
        Utc::now(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("accepted"),
        "ratified axis refuses new proposals: {err}"
    );
}

#[test]
fn concurrent_judgment_proposals_preserve_axis_exclusivity() {
    let f = setup("judgment-concurrent-axis");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();
    let entity = f.alpha.clone();
    let state = Arc::new(f.state);
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for target in ["boundary", "glue"] {
        let state = Arc::clone(&state);
        let barrier = Arc::clone(&barrier);
        let entity = entity.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            graph::propose_judgment(
                &state,
                &entity,
                "playsRole",
                &graph::role_iri(target),
                0.75,
                graph::AUTO_HELD,
                "concurrent classifier result",
                "moosedev-classifier",
                Utc::now(),
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one target may claim a judgment axis: {results:?}"
    );
    let judgments = graph::judgments_for_entity(&state, &entity).unwrap();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].status, "proposed");
}

#[test]
fn recategorize_rejects_classifier_target_and_ratifies_the_correction() {
    let f = setup("judgment-recategorize");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();

    // Classifier says core-algorithm; the human knows it's boundary.
    let proposal = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("core-algorithm"),
        0.6,
        graph::ESCALATED,
        "R4 core-algorithm: fan-in 19",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();

    let outcome = graph::recategorize_judgment(&f.state, &proposal, "boundary", "james").unwrap();
    let graph::AcceptOutcome::Judgment { target_iri, .. } = outcome else {
        panic!("recategorize materializes a judgment");
    };
    assert_eq!(target_iri, graph::role_iri("boundary"));

    // The corrected edge exists; the classifier's target never materialized.
    assert!(has_edge(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("boundary")
    ));
    assert!(!has_edge(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("core-algorithm")
    ));

    // One accepted human judgment visible; the original is rejected (audit) and
    // its evidence survives inside the correction's trace.
    let judgments = graph::judgments_for_entity(&f.state, &f.alpha).unwrap();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].status, "accepted");
    assert_eq!(judgments[0].target_local, "boundary");
    assert_eq!(judgments[0].author, "james");
    let rejected = graph::list_proposals(&f.state, Some("rejected")).unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].iri, proposal);

    // Wrong-axis target refused before any write; resolved entries refused too.
    let p2 = graph::propose_judgment(
        &f.state,
        &f.beta,
        "hasCriticality",
        &graph::criticality_iri("high"),
        0.6,
        graph::ESCALATED,
        "fan-in",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();
    assert!(graph::recategorize_judgment(&f.state, &p2, "boundary", "james").is_err());
    assert!(graph::recategorize_judgment(&f.state, &proposal, "glue", "james").is_err());
}

#[test]
fn recategorize_guards_same_target_and_settles_the_axis_with_human_provenance() {
    let f = setup("judgment-recat-guards");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();

    let original = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &graph::role_iri("boundary"),
        0.75,
        graph::AUTO_HELD,
        "R2 boundary",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();

    // Recategorizing to the proposal's own target is an accept, not a
    // correction — refused BEFORE any write (the original stays pending;
    // previously this accepted the original then errored rejecting it).
    let err = graph::recategorize_judgment(&f.state, &original, "boundary", "james").unwrap_err();
    assert!(
        err.to_string().contains("accept it instead"),
        "same-target recategorize refused with guidance: {err}"
    );
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1, "nothing was mutated");
    assert_eq!(pending[0].iri, original);

    // Recategorizing settles the whole axis: the correction carries the
    // HUMAN's provenance (never a reused pending twin's), and every other
    // pending judgment on the axis is rejected alongside the original.
    graph::recategorize_judgment(&f.state, &original, "glue", "james").unwrap();
    let judgments = graph::judgments_for_entity(&f.state, &f.alpha).unwrap();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].target_local, "glue");
    assert_eq!(judgments[0].status, "accepted");
    assert_eq!(
        judgments[0].author, "james",
        "the correction is the human's"
    );
    assert_eq!(
        graph::list_proposals(&f.state, Some("rejected"))
            .unwrap()
            .len(),
        1,
        "the classifier's original is rejected for audit"
    );

    // With the axis ratified, recategorizing anything onto it is refused
    // before writes.
    let crit = graph::propose_judgment(
        &f.state,
        &f.beta,
        "playsRole",
        &graph::role_iri("boundary"),
        0.75,
        graph::AUTO_HELD,
        "R2 boundary",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();
    graph::accept_proposal(&f.state, &crit, "james").unwrap();
    // beta's axis is now ratified; a fresh proposal cannot even queue, so a
    // recategorize can never race it (writer-level guard).
    assert!(graph::propose_judgment(
        &f.state,
        &f.beta,
        "playsRole",
        &graph::role_iri("glue"),
        1.0,
        graph::ESCALATED,
        "conflict",
        "james",
        Utc::now(),
    )
    .is_err());
}

#[test]
fn judgment_with_wrong_range_target_fails_honestly() {
    let f = setup("judgment-range");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();

    // Proposing an edge to a non-CodeRole target is possible (literals only)…
    let p = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "playsRole",
        &f.constraint,
        0.9,
        graph::AUTO_HELD,
        "bogus target",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();
    // …but accept runs the shape-validated relate, which refuses it, and the
    // judgment stays pending (honest skip, not a silent broken edge).
    assert!(graph::accept_proposal(&f.state, &p, "james").is_err());
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, "proposed");
}

#[test]
fn failed_recategorization_preserves_the_original_proposal() {
    let f = setup("judgment-recategorize-rollback");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();

    // Proposal nodes carry literals and can therefore contain a malformed
    // subject imported from an older store. Recategorization must validate the
    // corrected edge before its transaction rejects the original proposal.
    let original = graph::propose_judgment(
        &f.state,
        &f.constraint,
        "playsRole",
        &graph::role_iri("boundary"),
        0.75,
        graph::AUTO_HELD,
        "legacy malformed subject",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();

    let err = graph::recategorize_judgment(&f.state, &original, "glue", "james").unwrap_err();
    assert!(
        err.to_string()
            .contains("cannot materialize the recategorized judgment"),
        "endpoint validation should be the induced failure: {err:#}"
    );

    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1, "no correction was partially inserted");
    assert_eq!(pending[0].iri, original, "the original remains actionable");
    assert!(
        graph::list_proposals(&f.state, Some("rejected"))
            .unwrap()
            .is_empty(),
        "failure must not settle the original proposal"
    );
}

#[test]
fn rejected_records_can_never_become_documentation() {
    let f = setup("proposals-rejected-subject");

    // (a) Rejecting a captured record cascade-rejects its pending links, so a
    // later accept cannot resurrect it into dossiers.
    let record_iri = record_with_status(
        &f.state,
        "ArchitecturalDecision",
        "captured decision",
        "proposed",
    );
    let link = graph::propose_link(
        &f.state,
        &record_iri,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "file changed at this decision point",
        "tester",
        Utc::now(),
    )
    .unwrap();
    graph::reject_proposal(&f.state, &record_iri, "james").unwrap();
    let rejected = graph::list_proposals(&f.state, Some("rejected")).unwrap();
    assert!(
        rejected.iter().any(|p| p.iri == link),
        "rejecting the record cascade-rejects its pending links"
    );
    assert!(
        graph::accept_proposal(&f.state, &link, "james").is_err(),
        "the cascaded link is no longer pending"
    );
    assert!(!has_records(&f.state, &f.alpha));

    // (b) A link whose subject was rejected out-of-band is refused at accept.
    let dead = record_with_status(
        &f.state,
        "ArchitecturalDecision",
        "declined elsewhere",
        "rejected",
    );
    let orphan = graph::propose_link(
        &f.state,
        &dead,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "stale proposal",
        "tester",
        Utc::now(),
    )
    .unwrap();
    let err = graph::accept_proposal(&f.state, &orphan, "james").unwrap_err();
    assert!(err.to_string().contains("rejected"), "{err}");
    assert!(!has_records(&f.state, &f.alpha));

    // (c) Even a materialized edge from a rejected record never renders as
    // documentation or counts toward why-coverage.
    graph::relate(&f.state, &dead, "concerns", &f.alpha).unwrap();
    assert!(
        !has_records(&f.state, &f.alpha),
        "rejected records are invisible to dossiers"
    );
    assert_eq!(foo_numerator(&f.state), 0, "and to why-coverage");

    // (d) A link queued while its subject was current cannot reintroduce that
    // record after it is superseded. The link remains pending for explicit
    // rejection, but acceptance is refused by the subject's current status.
    let stale = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        BETA_RAW,
        "src/foo/a.rs",
        "queued before replacement",
        "tester",
        Utc::now(),
    )
    .unwrap();
    let constraint_class = f.state.resolve_class("Constraint").unwrap();
    graph::supersede_decision(
        &f.state,
        &graph::SupersedeInput {
            superseded_iri: f.constraint.clone(),
            new: RecordInput {
                class_iri: constraint_class,
                class_local: "Constraint".to_string(),
                properties: vec![(
                    f.state.capture.title.clone(),
                    "replacement constraint".to_string(),
                )],
            },
            rationale: "the constraint changed".to_string(),
        },
        "james",
        Utc::now(),
    )
    .unwrap();
    let err = graph::accept_proposal(&f.state, &stale, "james").unwrap_err();
    assert!(err.to_string().contains("superseded"), "{err}");
    assert!(
        !has_records(&f.state, &f.beta),
        "a queued link must not revive superseded knowledge"
    );
}

#[test]
fn duplicate_pending_link_proposals_are_not_minted() {
    let f = setup("proposals-dedup");
    let first = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "file changed at this decision point",
        "tester",
        Utc::now(),
    )
    .unwrap();
    // A repeated session capture proposes the identical link again…
    let second = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "file changed at this decision point (again)",
        "tester",
        Utc::now(),
    )
    .unwrap();
    // …and gets the existing pending entry instead of a duplicate.
    assert_eq!(first, second);
    assert_eq!(graph::pending_count(&f.state).unwrap(), 1);

    // Once resolved, an identical proposal may be minted again (a genuinely
    // new capture after a rejection is new information).
    graph::reject_proposal(&f.state, &first, "tester").unwrap();
    let third = graph::propose_link(
        &f.state,
        &f.constraint,
        "concerns",
        ALPHA_RAW,
        "src/foo/a.rs",
        "fresh capture",
        "tester",
        Utc::now(),
    )
    .unwrap();
    assert_ne!(first, third);
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
