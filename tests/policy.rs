//! Policy-engine gate/push semantics + fire telemetry (v2.2).
//!
//! Proves the active-agency contract on a synthetic substrate: an `accepted`
//! Constraint linked by `constrains` escalates to RequireRatification, an
//! asserted `violates` edge denies, non-accepted records never gate, push
//! injects byte-identical dossier markdown to hover, and every acted decision
//! (and only those) appends one parseable line to `fires.jsonl`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, DossierTarget, RecordInput};
use moosedev::policy::fires::fires_log_path_for;
use moosedev::policy::{evaluate, evaluate_and_fire, GateDisposition, PolicyDecision, PolicyEvent};
use oxigraph::model::{Literal, NamedNode, Quad, Term};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/";
const ALPHA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/alpha().";
const BETA_RAW: &str = "rust-analyzer cargo testpkg 0.1.0 foo/beta().";
const ALPHA_NORM: &str = "rust-analyzer cargo testpkg . foo/alpha().";
const BETA_NORM: &str = "rust-analyzer cargo testpkg . foo/beta().";
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
    let mut module_doc = doc(FILE);
    // rust-analyzer's synthetic whole-file module container.
    let mut module = SymbolInformation::new();
    module.symbol = MODULE_RAW.to_string();
    module.display_name = "foo".to_string();
    module.kind = EnumOrUnknown::new(symbol_information::Kind::Module);
    module_doc.symbols.push(module);
    let mut module_occ = Occurrence::new();
    module_occ.symbol = MODULE_RAW.to_string();
    module_occ.range = vec![0, 0, 2, 0];
    module_occ.symbol_roles = 1;
    module_occ.enclosing_range = vec![0, 0, 2, 0];
    module_doc.occurrences.push(module_occ);
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
    repo_root: PathBuf,
    alpha: String,
    beta: String,
    constraint: String,
}

fn setup(tag: &str) -> Fixture {
    let state = AppState::bootstrap(&fresh_dir(tag), &ontology_dir()).expect("bootstrap");
    state.set_substrate(Arc::new(synthetic_substrate()));

    // On-disk working tree matching the synthetic substrate, for anchor lookup.
    let repo_root = fresh_dir(&format!("{tag}-repo"));
    std::fs::create_dir_all(repo_root.join("src/foo")).unwrap();
    std::fs::write(
        repo_root.join(FILE),
        "pub fn alpha() {}\npub fn beta() {}\n",
    )
    .unwrap();

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
    let beta = entities.get(BETA_NORM).expect("beta minted").clone();
    let constraint = record(&state, "Constraint", "must stay pure", "accepted");
    Fixture {
        state,
        repo_root,
        alpha,
        beta,
        constraint,
    }
}

fn edit(anchor: Option<&str>) -> PolicyEvent {
    PolicyEvent::EditProposed {
        file: FILE.to_string(),
        line: None,
        col: None,
        anchor: anchor.map(str::to_string),
    }
}

fn fire_lines(state: &AppState) -> Vec<serde_json::Value> {
    let path = fires_log_path_for(&state.data_dir);
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("parseable fire line"))
        .collect()
}

#[test]
fn unconstrained_edit_allows_without_fire() {
    let f = setup("policy-allow");
    let decision = evaluate_and_fire(&f.state, &f.repo_root, &edit(Some("beta")), "test-host")
        .expect("evaluate");
    assert!(matches!(decision, PolicyDecision::Allow));
    assert!(fire_lines(&f.state).is_empty(), "Allow must not fire");
}

#[test]
fn accepted_constraint_requires_ratification_and_fires() {
    let f = setup("policy-ratify");
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains edge");

    let decision = evaluate_and_fire(&f.state, &f.repo_root, &edit(Some("alpha")), "test-host")
        .expect("evaluate");
    let PolicyDecision::Gate {
        disposition,
        reason,
        records,
        entities,
    } = decision
    else {
        panic!("expected a gate");
    };
    assert_eq!(disposition, GateDisposition::RequireRatification);
    assert!(reason.contains("must stay pure"), "reason cites the title");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].iri, f.constraint);
    assert_eq!(records[0].kind, "Constraint");
    assert_eq!(entities, vec![f.alpha.clone()]);

    let fires = fire_lines(&f.state);
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0]["verb"], "gate");
    assert_eq!(fires[0]["decision"], "require_ratification");
    assert_eq!(fires[0]["host"], "test-host");
    assert_eq!(fires[0]["entity"], f.alpha.as_str());
    assert_eq!(fires[0]["records_cited"][0], f.constraint.as_str());
}

#[test]
fn fire_write_failure_does_not_fail_an_acted_policy_decision() {
    let f = setup("policy-fire-write-failure");
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains edge");
    std::fs::create_dir(fires_log_path_for(&f.state.data_dir))
        .expect("make fire-log path unwritable as a file");

    let decision = evaluate_and_fire(&f.state, &f.repo_root, &edit(Some("alpha")), "test-host")
        .expect("telemetry failure must not fail policy evaluation");

    assert!(matches!(
        decision,
        PolicyDecision::Gate {
            disposition: GateDisposition::RequireRatification,
            ..
        }
    ));
}

#[test]
fn violates_edge_denies_over_constrains() {
    let f = setup("policy-deny");
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains edge");
    graph::relate(&f.state, &f.alpha, "violates", &f.constraint).expect("violates edge");

    let decision = evaluate_and_fire(&f.state, &f.repo_root, &edit(Some("alpha")), "test-host")
        .expect("evaluate");
    let PolicyDecision::Gate { disposition, .. } = decision else {
        panic!("expected a gate");
    };
    assert_eq!(disposition, GateDisposition::Deny, "deny beats ask");

    let fires = fire_lines(&f.state);
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0]["decision"], "deny");
}

#[test]
fn non_accepted_records_never_gate() {
    let f = setup("policy-proposed");
    let draft = record(&f.state, "Constraint", "still a draft", "proposed");
    graph::relate(&f.state, &draft, "constrains", &f.alpha).expect("constrains edge");

    let decision = evaluate(&f.state, &f.repo_root, &edit(Some("alpha"))).expect("evaluate");
    assert!(
        matches!(decision, PolicyDecision::Allow),
        "a proposed-status record must not gate"
    );
}

#[test]
fn anchor_scopes_gate_to_overlapping_definitions() {
    let f = setup("policy-anchor");
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains edge");

    // Anchor on beta's line: alpha's constraint must not gate the edit.
    let decision = evaluate(&f.state, &f.repo_root, &edit(Some("beta"))).expect("evaluate");
    assert!(matches!(decision, PolicyDecision::Allow));

    // No anchor: the conservative whole-file scan picks alpha up.
    let decision = evaluate(&f.state, &f.repo_root, &edit(None)).expect("evaluate");
    assert!(matches!(
        decision,
        PolicyDecision::Gate {
            disposition: GateDisposition::RequireRatification,
            ..
        }
    ));
}

#[test]
fn ratified_criticality_high_gates_but_proposed_never_does() {
    let f = setup("policy-criticality");
    graph::ensure_taxonomy_individuals(&f.state).unwrap();
    let high = graph::criticality_iri("high");
    let judgment = graph::propose_judgment(
        &f.state,
        &f.alpha,
        "hasCriticality",
        &high,
        0.6,
        graph::ESCALATED,
        "fan-in P92",
        "moosedev-classifier",
        Utc::now(),
    )
    .unwrap();

    // Proposed judgments can never gate — no edge exists.
    let decision = evaluate(&f.state, &f.repo_root, &edit(Some("alpha"))).expect("evaluate");
    assert!(
        matches!(decision, PolicyDecision::Allow),
        "a proposed judgment must not gate"
    );

    // Ratify → the gate now asks, citing the accepted judgment node.
    graph::accept_proposal(&f.state, &judgment, "james").unwrap();
    let decision = evaluate(&f.state, &f.repo_root, &edit(Some("alpha"))).expect("evaluate");
    let PolicyDecision::Gate {
        disposition,
        reason,
        records,
        entities,
    } = decision
    else {
        panic!("expected a gate");
    };
    assert_eq!(disposition, GateDisposition::RequireRatification);
    assert!(reason.contains("criticality: high (ratified judgment)"));
    assert_eq!(records[0].iri, judgment, "cites the judgment node");
    assert_eq!(records[0].kind, "Judgment");
    assert_eq!(entities, vec![f.alpha.clone()]);
}

#[test]
fn push_injects_hover_bytes_and_fires() {
    let f = setup("policy-push");
    graph::relate(&f.state, &f.constraint, "concerns", &f.alpha).expect("concerns edge");

    // 1-based position on alpha's definition line.
    let event = PolicyEvent::EntityTouched {
        file: FILE.to_string(),
        line: Some(1),
        col: Some(1),
        max_bytes: None,
    };
    let decision =
        evaluate_and_fire(&f.state, &f.repo_root, &event, "test-host").expect("evaluate");
    let PolicyDecision::Inject {
        dossier_markdown,
        entities,
        records,
    } = decision
    else {
        panic!("expected an inject");
    };

    let hover = graph::render_markdown(
        &graph::get_entity_dossier(
            &f.state,
            &DossierTarget::Position {
                file: FILE.to_string(),
                line: 1,
                col: 1,
            },
        )
        .unwrap()
        .expect("dossier exists"),
    );
    assert_eq!(dossier_markdown, hover, "push == MCP dossier bytes");
    assert_eq!(entities, vec![f.alpha.clone()]);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].iri, f.constraint);

    let fires = fire_lines(&f.state);
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0]["verb"], "push");
    assert_eq!(fires[0]["decision"], "inject");
    assert_eq!(fires[0]["entity"], f.alpha.as_str());
}

#[test]
fn file_touch_without_records_is_silent() {
    let f = setup("policy-silent");
    let event = PolicyEvent::EntityTouched {
        file: FILE.to_string(),
        line: None,
        col: None,
        max_bytes: None,
    };
    let decision =
        evaluate_and_fire(&f.state, &f.repo_root, &event, "test-host").expect("evaluate");
    assert!(matches!(decision, PolicyDecision::Allow));
    assert!(fire_lines(&f.state).is_empty());
    let _ = f.beta; // beta stays unlinked by design in this fixture
}

#[test]
fn file_touch_pushes_all_knowledge_bearing_entities() {
    let f = setup("policy-file-push");
    graph::relate(&f.state, &f.constraint, "concerns", &f.alpha).expect("concerns alpha");
    let lesson = record(&f.state, "Lesson", "beta gotcha", "accepted");
    graph::relate(&f.state, &lesson, "concerns", &f.beta).expect("concerns beta");

    let event = PolicyEvent::EntityTouched {
        file: FILE.to_string(),
        line: None,
        col: None,
        max_bytes: None,
    };
    let decision = evaluate(&f.state, &f.repo_root, &event).expect("evaluate");
    let PolicyDecision::Inject {
        dossier_markdown,
        entities,
        ..
    } = decision
    else {
        panic!("expected an inject");
    };
    assert_eq!(entities.len(), 2, "both entities pushed");
    assert!(dossier_markdown.contains("must stay pure"));
    assert!(dossier_markdown.contains("beta gotcha"));
}

#[test]
fn file_touch_shows_a_shared_record_claim_once() {
    let f = setup("policy-file-dedup");
    insert_quad(
        &f.state,
        &f.constraint,
        &f.state.capture.description,
        Literal::new_simple_literal("Pure functions only.").into(),
    );
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains alpha");
    graph::relate(&f.state, &f.constraint, "constrains", &f.beta).expect("constrains beta");

    let event = PolicyEvent::EntityTouched {
        file: FILE.to_string(),
        line: None,
        col: None,
        max_bytes: None,
    };
    let decision = evaluate(&f.state, &f.repo_root, &event).expect("evaluate");
    let PolicyDecision::Inject {
        dossier_markdown,
        entities,
        ..
    } = decision
    else {
        panic!("expected an inject");
    };
    assert_eq!(entities.len(), 2, "both entities pushed");
    assert_eq!(
        dossier_markdown
            .matches("hasDescription: Pure functions only.")
            .count(),
        1,
        "{dossier_markdown}"
    );
    assert!(
        dossier_markdown.contains("\nclaim shown above for `alpha`\n"),
        "{dossier_markdown}"
    );
}

#[test]
fn file_touch_within_max_bytes_shortens_later_sections_to_protected_knowledge() {
    let f = setup("policy-max-bytes");
    let component = graph::load_components(&f.state).unwrap()[0]
        .iri
        .clone()
        .expect("component IRI");
    // alpha carries the component's records directly, so beta's section is the
    // first to list them as component context.
    graph::relate(&f.state, &f.constraint, "constrains", &f.alpha).expect("constrains alpha");
    let rule = record(&f.state, "Constraint", "foo component rule", "accepted");
    let decision = record(
        &f.state,
        "ArchitecturalDecision",
        "foo component decision",
        "accepted",
    );
    for iri in [&rule, &decision] {
        graph::relate(&f.state, iri, "concerns", &component).expect("concerns component");
        graph::relate(&f.state, iri, "concerns", &f.alpha).expect("concerns alpha");
    }
    let lesson = record(&f.state, "Lesson", "beta gotcha", "accepted");
    insert_quad(
        &f.state,
        &lesson,
        &f.state.capture.description,
        Literal::new_simple_literal("beta caches its input.").into(),
    );
    graph::relate(&f.state, &lesson, "concerns", &f.beta).expect("concerns beta");

    let push = |max_bytes: Option<usize>| {
        let event = PolicyEvent::EntityTouched {
            file: FILE.to_string(),
            line: None,
            col: None,
            max_bytes,
        };
        match evaluate(&f.state, &f.repo_root, &event).expect("evaluate") {
            PolicyDecision::Inject {
                dossier_markdown,
                entities,
                records,
            } => (dossier_markdown, entities, records),
            other => panic!("expected an inject, got {other:?}"),
        }
    };

    let (whole, whole_entities, whole_records) = push(None);
    assert_eq!(
        push(Some(1_000_000)).0,
        whole,
        "a bound that fits changes nothing"
    );
    assert_eq!(
        whole
            .matches("- [ArchitecturalDecision] foo component decision")
            .count(),
        2,
        "{whole}"
    );

    let (bounded, entities, records) = push(Some(1));
    assert_eq!(entities, whole_entities, "every entity still renders");
    assert_eq!(records.len(), whole_records.len());
    // beta keeps its direct record, its claim and the component's accepted
    // Constraint title; only the other component title is left out.
    assert!(
        bounded.contains("- [Lesson] beta gotcha - accepted"),
        "{bounded}"
    );
    assert!(
        bounded.contains("\nhasDescription: beta caches its input.\n"),
        "{bounded}"
    );
    assert_eq!(
        bounded.matches("- [Constraint] foo component rule").count(),
        2,
        "{bounded}"
    );
    assert_eq!(
        bounded
            .matches("- [ArchitecturalDecision] foo component decision")
            .count(),
        1,
        "{bounded}"
    );
    assert!(
        bounded.contains("\n1 further record concerns component foo (ArchitecturalDecision: 1), not necessarily this code; search project knowledge for records about foo\n"),
        "{bounded}"
    );
    assert!(
        bounded.ends_with("\n1 entity section shortened to direct records and accepted Constraints by the host's 1-byte bound: `beta`; retrieve it in full with get_entity_dossier\n"),
        "{bounded}"
    );
}

/// Rust's synthetic whole-file module is never a position target, but knowledge
/// anchored to it reaches every push of its file.
#[test]
fn file_touch_pushes_knowledge_anchored_to_the_whole_file_module() {
    let f = setup("policy-module-anchor");
    let lesson = record(&f.state, "Lesson", "foo module gotcha", "accepted");
    let module = graph::link_code(
        &f.state,
        &lesson,
        "concerns",
        &graph::CodeSelector::Symbol(MODULE_RAW.to_string()),
        "tester",
    )
    .expect("link the module")
    .entity_iri;

    let touch = |line: Option<u32>, col: Option<u32>| {
        let event = PolicyEvent::EntityTouched {
            file: FILE.to_string(),
            line,
            col,
            max_bytes: None,
        };
        evaluate(&f.state, &f.repo_root, &event).expect("evaluate")
    };
    let PolicyDecision::Inject {
        dossier_markdown,
        entities,
        ..
    } = touch(None, None)
    else {
        panic!("a file push carries the module's knowledge");
    };
    assert_eq!(entities, vec![module]);
    assert!(
        dossier_markdown.contains("foo module gotcha"),
        "{dossier_markdown}"
    );
    // 1:1 is alpha's token, never the module container.
    assert!(matches!(touch(Some(1), Some(1)), PolicyDecision::Allow));
}

#[test]
fn decision_point_returns_capture_spec_without_fire() {
    let f = setup("policy-capture");
    let event = PolicyEvent::DecisionPoint {
        files: vec![FILE.to_string()],
        summary: Some("refactored foo".to_string()),
    };
    let decision =
        evaluate_and_fire(&f.state, &f.repo_root, &event, "test-host").expect("evaluate");
    let PolicyDecision::CaptureTrigger { spec } = decision else {
        panic!("expected a capture trigger");
    };
    assert_eq!(spec.files, vec![FILE.to_string()]);
    assert_eq!(spec.summary.as_deref(), Some("refactored foo"));
    assert!(
        fire_lines(&f.state).is_empty(),
        "capture fires only when the record is written"
    );
}
