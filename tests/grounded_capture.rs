//! Grounded capture + generalized ratification queue (v2.2 CAPTURE verb).
//!
//! Proves the capture contract: a decision point mints a `proposed` record with
//! authorship provenance that is never auto-accepted and touches no dossier or
//! why-coverage number until ratified; entity links ride the v2.1 `ProposedLink`
//! queue; `isMotivatedBy` links only an existing Requirement and is omitted —
//! never invented — otherwise; and accept/reject dispatch by queue-entry kind.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AcceptOutcome, AppState, DossierTarget, ProposalKind, RecordInput};
use moosedev::provenance;
use oxigraph::model::{GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/";
const ALPHA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/alpha().";
const ALPHA_NORM: &str = "rust-analyzer cargo testpkg . foo/alpha().";
const FILE: &str = "src/foo/a.rs";

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

fn add_def(d: &mut Document, symbol: &str, name: &str, kind: symbol_information::Kind, line: i32) {
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.display_name = name.to_string();
    info.kind = EnumOrUnknown::new(kind);
    let mut sig = Signature::new();
    sig.text = format!("pub {name}");
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
    let mut module_doc = doc(FILE);
    add_def(
        &mut module_doc,
        MODULE_RAW,
        "foo",
        symbol_information::Kind::Module,
        0,
    );
    add_def(
        &mut module_doc,
        ALPHA_RAW,
        "alpha",
        symbol_information::Kind::Function,
        1,
    );
    index.documents.push(module_doc);
    let occurrences = index
        .documents
        .iter()
        .map(|d| d.occurrences.len())
        .sum::<usize>();
    let meta = SubstrateMeta::single("rust-analyzer", "commit0", Utc::now(), 1, occurrences);
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
}

fn record(state: &AppState, kind: &str, title: &str, status: &str) -> String {
    record_at(state, kind, title, status, "tester", Utc::now())
}

fn record_at(
    state: &AppState,
    kind: &str,
    title: &str,
    status: &str,
    author: &str,
    when: DateTime<Utc>,
) -> String {
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
        author,
        when,
    )
    .expect("record instance")
}

fn insert_quad(state: &AppState, subject: &str, predicate: &str, object: Term) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(predicate).unwrap(),
        object,
        oxigraph::model::GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    let mut txn = state.store.start_transaction().unwrap();
    txn.insert(quad.as_ref());
    txn.commit().unwrap();
}

struct Fixture {
    state: AppState,
    alpha: String,
}

fn setup(tag: &str) -> Fixture {
    let state = AppState::bootstrap(&fresh_dir(tag), &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(synthetic_substrate()));

    let component = record(&state, "SystemComponent", "foo", "accepted");
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
    Fixture { state, alpha }
}

fn has_records(state: &AppState, iri: &str) -> bool {
    graph::get_entity_dossier(state, &DossierTarget::Iri(iri.to_string()))
        .unwrap()
        .map(|d| !d.direct_records.is_empty())
        .unwrap_or(false)
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

fn literal_of(state: &AppState, subject: &str, predicate: &str) -> Option<String> {
    state
        .store
        .quads_for_pattern(
            Some(NamedNode::new(subject).unwrap().as_ref().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .flatten()
        .find_map(|q| match q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
}

fn status_of(state: &AppState, iri: &str) -> String {
    literal_of(state, iri, &state.capture.status).unwrap_or_default()
}

fn capture(
    state: &AppState,
    summary: Option<&str>,
    requirement: Option<&str>,
    entities: &[String],
) -> graph::GroundedCapture {
    graph::capture_decision_point(
        state,
        &[FILE.to_string()],
        summary,
        requirement,
        entities,
        "tester",
        Utc::now(),
    )
    .expect("capture decision point")
}

#[test]
fn automatic_capture_abstains_only_for_newer_same_author_working_records() {
    let f = setup("gc-abstention");
    let cutoff = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    // None of these is an authoritative capture by this adapter in the window.
    record_at(
        &f.state,
        "Lesson",
        "Equal boundary",
        "accepted",
        "claude-code",
        cutoff,
    );
    record_at(
        &f.state,
        "Constraint",
        "Different author",
        "accepted",
        "other-agent",
        cutoff + TimeDelta::seconds(9),
    );
    for (status, seconds) in [("proposed", 6), ("rejected", 7), ("superseded", 8)] {
        record_at(
            &f.state,
            "ArchitecturalDecision",
            &format!("Ignored {status}"),
            status,
            "claude-code",
            cutoff + TimeDelta::seconds(seconds),
        );
    }
    assert_eq!(
        graph::working_record_authored_since(&f.state, "claude-code", cutoff).unwrap(),
        None
    );

    let older_match = record_at(
        &f.state,
        "Lesson",
        "Deliberate lesson",
        "accepted",
        "claude-code",
        cutoff + TimeDelta::seconds(1),
    );
    let newest_match = record_at(
        &f.state,
        "Requirement",
        "Deliberate requirement",
        "accepted",
        "claude-code",
        cutoff + TimeDelta::seconds(2),
    );
    assert_ne!(older_match, newest_match);
    assert_eq!(
        graph::working_record_authored_since(&f.state, "claude-code", cutoff).unwrap(),
        Some(newest_match)
    );
}

#[test]
fn automatic_capture_abstention_tie_break_is_deterministic() {
    let f = setup("gc-abstention-tie");
    let cutoff = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let when = cutoff + TimeDelta::seconds(1);
    let a = record_at(
        &f.state,
        "Lesson",
        "Tie A",
        "accepted",
        "opencode-moosedev",
        when,
    );
    let b = record_at(
        &f.state,
        "Lesson",
        "Tie B",
        "accepted",
        "opencode-moosedev",
        when,
    );
    let expected = std::cmp::max(a, b);
    assert_eq!(
        graph::working_record_authored_since(&f.state, "opencode-moosedev", cutoff).unwrap(),
        Some(expected)
    );
}

#[test]
fn capture_is_proposed_only_with_provenance_and_queued_links() {
    let f = setup("gc-proposed");
    let captured = capture(
        &f.state,
        Some("Refactored the foo pipeline"),
        None,
        &[ALPHA_RAW.to_string()],
    );

    // Proposed, titled by the host's own words, provenance-stamped.
    assert_eq!(captured.title, "Refactored the foo pipeline");
    assert_eq!(status_of(&f.state, &captured.record_iri), "proposed");
    assert_eq!(
        literal_of(&f.state, &captured.record_iri, &f.state.capture.author).as_deref(),
        Some("tester")
    );
    let prov =
        provenance::read_provenance(&f.state.store, &captured.record_iri).expect("read provenance");
    assert!(prov.is_some(), "capture stamps edit provenance");

    // One link proposal per changed file's module + one per named entity.
    assert_eq!(captured.proposed_links.len(), 2);
    assert!(captured.unanchored.is_empty());

    // Queue lists the record entry next to the link entries.
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 3);
    assert_eq!(graph::pending_count(&f.state).unwrap(), 3);
    let record_entry = pending
        .iter()
        .find(|p| p.kind == ProposalKind::Record)
        .expect("record entry listed");
    assert_eq!(record_entry.iri, captured.record_iri);
    assert_eq!(
        record_entry.record_class.as_deref(),
        Some("ArchitecturalDecision")
    );
    assert_eq!(record_entry.label, "Refactored the foo pipeline");

    // Nothing is visible anywhere until ratified: no dossier, no debt movement.
    assert!(!has_records(&f.state, &f.alpha));
    assert_eq!(foo_numerator(&f.state), 0);

    // No isMotivatedBy was invented.
    let motivated = f.state.resolve_object_property("isMotivatedBy").unwrap();
    let has_motivation = f
        .state
        .store
        .quads_for_pattern(
            Some(
                NamedNode::new(&captured.record_iri)
                    .unwrap()
                    .as_ref()
                    .into(),
            ),
            Some(NamedNodeRef::new(&motivated).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .next()
        .is_some();
    assert!(!has_motivation, "isMotivatedBy must never be invented");
}

#[test]
fn requirement_links_existing_or_fails_atomically() {
    let f = setup("gc-requirement");
    let requirement = record(&f.state, "Requirement", "make foo fast", "accepted");

    // A resolvable Requirement is linked.
    let captured = capture(&f.state, Some("Tuned foo"), Some("make foo fast"), &[]);
    let motivated = f.state.resolve_object_property("isMotivatedBy").unwrap();
    let linked = f
        .state
        .store
        .quads_for_pattern(
            Some(
                NamedNode::new(&captured.record_iri)
                    .unwrap()
                    .as_ref()
                    .into(),
            ),
            Some(NamedNodeRef::new(&motivated).unwrap()),
            Some(NamedNodeRef::new(&requirement).unwrap().into()),
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .next()
        .is_some();
    assert!(linked, "existing Requirement is linked");

    // An unresolvable one fails the whole capture — no orphan record.
    let before = graph::pending_count(&f.state).unwrap();
    let result = graph::capture_decision_point(
        &f.state,
        &[FILE.to_string()],
        Some("Tuned foo again"),
        Some("no such requirement anywhere"),
        &[],
        "tester",
        Utc::now(),
    );
    assert!(result.is_err(), "unknown Requirement must fail the capture");
    assert_eq!(
        graph::pending_count(&f.state).unwrap(),
        before,
        "failed capture writes nothing"
    );
}

#[test]
fn accept_ratifies_record_in_place_and_link_materializes_edge() {
    let f = setup("gc-accept");
    let captured = capture(&f.state, Some("Split foo"), None, &[ALPHA_RAW.to_string()]);

    // Ratify the record: proposed → accepted, and it leaves the queue view.
    let outcome = graph::accept_proposal(&f.state, &captured.record_iri, "tester").unwrap();
    match outcome {
        AcceptOutcome::Record { title, .. } => assert_eq!(title, "Split foo"),
        other => panic!("record entry must ratify as a record, got {other:?}"),
    }
    assert_eq!(status_of(&f.state, &captured.record_iri), "accepted");
    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert!(pending.iter().all(|p| p.kind == ProposalKind::Link));

    // Ratify the alpha link: the concerns edge materializes and debt moves.
    let alpha_link = pending
        .iter()
        .find(|p| p.target_symbol == ALPHA_RAW)
        .expect("alpha link queued");
    let outcome = graph::accept_proposal(&f.state, &alpha_link.iri, "tester").unwrap();
    assert!(matches!(outcome, AcceptOutcome::Link(_)));
    assert!(has_records(&f.state, &f.alpha), "edge materialized");
    assert_eq!(
        foo_numerator(&f.state),
        1,
        "debt moves on ratification only"
    );

    // Re-accepting the record is a guarded error.
    assert!(graph::accept_proposal(&f.state, &captured.record_iri, "tester").is_err());
}

#[test]
fn link_accept_requires_a_ratified_subject_record() {
    let f = setup("gc-order");
    let captured = capture(
        &f.state,
        Some("Order matters"),
        None,
        &[ALPHA_RAW.to_string()],
    );
    // Files queue before entities; pick the alpha ENTITY link explicitly.
    let link = graph::list_proposals(&f.state, Some("proposed"))
        .unwrap()
        .into_iter()
        .find(|p| p.target_symbol == ALPHA_RAW)
        .expect("alpha link queued")
        .iri;

    // Accepting a link while its subject record is still proposed would
    // smuggle unratified knowledge into dossiers and why-coverage.
    let err = graph::accept_proposal(&f.state, &link, "tester").unwrap_err();
    assert!(
        err.to_string().contains("still proposed"),
        "refusal names the cause: {err}"
    );
    assert!(!has_records(&f.state, &f.alpha), "no edge materialized");
    assert_eq!(foo_numerator(&f.state), 0);
    assert_eq!(status_of(&f.state, &link), "proposed", "link stays pending");

    // Ratify the record → the same link now accepts.
    graph::accept_proposal(&f.state, &captured.record_iri, "tester").unwrap();
    graph::accept_proposal(&f.state, &link, "tester").unwrap();
    assert!(has_records(&f.state, &f.alpha));
    assert_eq!(foo_numerator(&f.state), 1);
}

#[test]
fn reject_keeps_record_out_of_every_surface() {
    let f = setup("gc-reject");
    let captured = capture(
        &f.state,
        Some("Abandoned idea"),
        None,
        &[ALPHA_RAW.to_string()],
    );

    graph::reject_proposal(&f.state, &captured.record_iri, "tester").unwrap();
    assert_eq!(status_of(&f.state, &captured.record_iri), "rejected");

    // Rejecting the record cascade-rejected its queued links: a declined
    // record's links are dead and cannot resurrect it later.
    for link in &captured.proposed_links {
        assert_eq!(status_of(&f.state, link), "rejected", "cascade-rejected");
        assert!(graph::accept_proposal(&f.state, link, "tester").is_err());
    }
    assert!(!has_records(&f.state, &f.alpha), "reject creates no edge");
    assert_eq!(foo_numerator(&f.state), 0);
    assert_eq!(graph::pending_count(&f.state).unwrap(), 0);

    // Re-rejecting is a guarded error.
    assert!(graph::reject_proposal(&f.state, &captured.record_iri, "tester").is_err());
}

#[test]
fn cluster_satellites_are_never_queue_entries() {
    let f = setup("gc-satellite");
    // A legacy-default Alternative left at `proposed` (the pre-normalization
    // corpus shape) must neither list in the queue nor be ratifiable there.
    let alternative = record(&f.state, "Alternative", "a rejected option", "proposed");
    let consequence = record(&f.state, "Consequence", "a trade-off", "proposed");

    let pending = graph::list_proposals(&f.state, Some("proposed")).unwrap();
    assert!(
        pending
            .iter()
            .all(|p| p.iri != alternative && p.iri != consequence),
        "satellites must not appear in the queue"
    );
    assert_eq!(graph::pending_count(&f.state).unwrap(), 0);

    let err = graph::accept_proposal(&f.state, &alternative, "tester").unwrap_err();
    assert!(
        err.to_string().contains("cluster-satellite"),
        "accepting a satellite is refused with the reason: {err}"
    );
    assert!(graph::reject_proposal(&f.state, &consequence, "tester").is_err());
}

#[test]
fn ungrounded_or_unanchored_captures_are_honest() {
    let f = setup("gc-honest");

    // No files and no summary → nothing to ground; refuse.
    let result =
        graph::capture_decision_point(&f.state, &[], None, None, &[], "tester", Utc::now());
    assert!(result.is_err());

    // A file outside the substrate is reported, never silently dropped.
    let captured = graph::capture_decision_point(
        &f.state,
        &["docs/notes.md".to_string()],
        Some("Documented the plan"),
        None,
        &[],
        "tester",
        Utc::now(),
    )
    .expect("capture with unanchored file");
    assert!(captured.proposed_links.is_empty());
    assert_eq!(captured.unanchored, vec!["docs/notes.md".to_string()]);

    // An unknown entity symbol is reported the same way.
    let captured = graph::capture_decision_point(
        &f.state,
        &[],
        Some("Touched a ghost"),
        None,
        &["rust-analyzer cargo ghost 1.0.0 spook#".to_string()],
        "tester",
        Utc::now(),
    )
    .expect("capture with unknown symbol");
    assert!(captured.proposed_links.is_empty());
    assert_eq!(
        captured.unanchored,
        vec!["symbol:rust-analyzer cargo ghost 1.0.0 spook#".to_string()]
    );
}
