//! SystemComponent lifecycle (issue #69): retire, merge into a successor, and
//! rename; the readers that must stop treating a retired component as current;
//! and the write paths that stay closed.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{Substrate, SubstrateMeta};
use moosedev::export::export_canonical_project;
use moosedev::graph::{
    self, AppState, CodeSelector, DossierTarget, LinkedRecord, MovedEdge, RecordInput,
    RetireOutcome, PROJECT_KG_GRAPH_IRI,
};
use moosedev::stories::{
    enrich_summary, StoryFocus, StoryRecipe, StoryRecipeSubject, StoryResolutionIndex, StoryStatus,
    StorySummary,
};
use oxigraph::model::{
    GraphName, GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term,
};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const COMPONENT_NAME: &str = "https://trivyn.io/ontologies/software/architecture#hasComponentName";
const SYSTEM_COMPONENT: &str = "https://trivyn.io/ontologies/software/architecture#SystemComponent";
/// Software-engineering `dependsOn`: an edge a merge does not move.
const DEPENDS_ON: &str = "https://trivyn.io/ontologies/software/engineering#dependsOn";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";

fn data_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-component-lifecycle-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn ontology_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn bootstrap(name: &str) -> AppState {
    AppState::bootstrap(&data_dir(name), &ontology_dir()).expect("bootstrap app state")
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
    .expect("record item")
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    record_with_status(state, kind, title, "accepted")
}

fn component(state: &AppState, name: &str, paths: &[&str]) -> String {
    let iri = record(state, "SystemComponent", name);
    if !paths.is_empty() {
        graph::declare_component_paths(
            state,
            &iri,
            &paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        )
        .expect("declare paths");
    }
    iri
}

fn predicate(state: &AppState, local: &str) -> String {
    state
        .resolve_object_property(local)
        .expect("known relation")
}

fn insert_raw(state: &AppState, subject: &str, predicate: &str, object: &str) {
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(subject).unwrap(),
            NamedNode::new(predicate).unwrap(),
            NamedNode::new(object).unwrap(),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}

fn edge_count(state: &AppState, subject: &str, predicate: &str, object: &str) -> usize {
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            Some(NamedNodeRef::new(object).unwrap().into()),
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .count()
}

fn has_edge(state: &AppState, subject: &str, predicate: &str, object: &str) -> bool {
    edge_count(state, subject, predicate, object) > 0
}

fn literals(state: &AppState, subject: &str, predicate: &str) -> Vec<String> {
    let mut values = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject).unwrap().into()),
            Some(NamedNodeRef::new(predicate).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .flatten()
        .filter_map(|quad| match quad.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn status(state: &AppState, iri: &str) -> Vec<String> {
    literals(state, iri, &state.capture.status)
}

/// Every project-graph quad, for "nothing was written" comparisons.
fn project_dump(state: &AppState) -> BTreeSet<String> {
    state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .map(|quad| quad.unwrap().to_string())
        .collect()
}

/// Predicate local names of every project-graph edge incident to `iri`, with
/// the other end, split by direction.
fn incident(state: &AppState, iri: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let node = NamedNodeRef::new(iri).unwrap();
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap());
    let local = |p: &str| p.rsplit(['#', '/']).next().unwrap().to_string();
    let outgoing = state
        .store
        .quads_for_pattern(Some(node.into()), None, None, Some(graph))
        .flatten()
        .filter(|q| matches!(q.object, Term::NamedNode(_)))
        .filter(|q| q.predicate.as_str() != moose::RDF_TYPE)
        .map(|q| local(q.predicate.as_str()))
        .collect();
    let incoming = state
        .store
        .quads_for_pattern(None, None, Some(node.into()), Some(graph))
        .flatten()
        .filter(|q| matches!(q.subject, NamedOrBlankNode::NamedNode(_)))
        .map(|q| local(q.predicate.as_str()))
        .collect();
    (outgoing, incoming)
}

fn current_names(state: &AppState) -> Vec<String> {
    graph::load_components(state)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect()
}

fn all_names(state: &AppState) -> Vec<String> {
    graph::load_all_components(state)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect()
}

fn titles(records: &[LinkedRecord]) -> Vec<&str> {
    records.iter().map(|r| r.title.as_str()).collect()
}

fn runtime_position() -> DossierTarget {
    DossierTarget::Position {
        file: "src/runtime.rs".to_string(),
        line: 8,
        col: 5,
    }
}

/// Link `record_iri` to the synthetic `build_server`, minting it (it realizes
/// whichever component covers `src/runtime.rs` at that moment).
fn link_runtime(state: &AppState, record_iri: &str) -> String {
    graph::link_code(
        state,
        record_iri,
        "concerns",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line: 8,
            col: 5,
        },
        "tester",
    )
    .expect("link code")
    .entity_iri
}

fn mint_plan(state: &AppState) -> graph::MintPlan {
    let substrate = state.substrate().expect("substrate");
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let components = graph::load_components(state).unwrap();
    graph::plan_mint(
        state,
        &substrate.definitions(),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap()
}

/// The graph a merge works on: `ingest files` (src/, shared/) folds into
/// `pipeline` (lib/, shared/). It carries every kind of edge a merge handles.
struct MergeFixture {
    state: AppState,
    a: String,
    b: String,
    storage: String,
    decision: String,
    dual: String,
    concern_constraint: String,
    constrain_constraint: String,
    inverse_only: String,
    entity: String,
}

fn merge_fixture(name: &str) -> MergeFixture {
    let state = bootstrap(name);
    state.set_substrate(Arc::new(synthetic_substrate()));
    let a = component(&state, "ingest files", &["src/", "shared/"]);
    let b = component(&state, "pipeline", &["lib/", "shared/"]);
    let storage = component(&state, "storage", &[]);

    let decision = record(&state, "ArchitecturalDecision", "Ingest decision");
    graph::relate(&state, &decision, "concerns", &a).unwrap();
    // Dual-anchored, the shape issue #69 reports: concerns both A and B.
    let dual = record(&state, "ArchitecturalDecision", "Dual-anchored decision");
    graph::relate(&state, &dual, "concerns", &a).unwrap();
    graph::relate(&state, &dual, "concerns", &b).unwrap();
    let concern_constraint = record(&state, "Constraint", "Ingest constraint");
    graph::relate(&state, &concern_constraint, "concerns", &a).unwrap();
    let constrain_constraint = record(&state, "Constraint", "Ingest limit");
    graph::relate(&state, &constrain_constraint, "constrains", &a).unwrap();
    // Stored inverse orientations only.
    let inverse_only = record(&state, "Lesson", "Stored-inverse lesson");
    insert_raw(
        &state,
        &a,
        &predicate(&state, "isConcernedBy"),
        &inverse_only,
    );
    insert_raw(
        &state,
        &a,
        &predicate(&state, "isConstrainedBy"),
        &constrain_constraint,
    );
    // An edge no merge moves.
    insert_raw(&state, &a, DEPENDS_ON, &storage);

    let lesson = record(&state, "Lesson", "Runtime lesson");
    let entity = link_runtime(&state, &lesson);
    assert!(has_edge(
        &state,
        &entity,
        &predicate(&state, "realizes"),
        &a
    ));
    // Inferred inverses now exist; a merge must skip them, not move them.
    state.ensure_enriched();
    assert!(has_edge(
        &state,
        &a,
        &predicate(&state, "isConcernedBy"),
        &decision
    ));

    MergeFixture {
        state,
        a,
        b,
        storage,
        decision,
        dual,
        concern_constraint,
        constrain_constraint,
        inverse_only,
        entity,
    }
}

fn merge(f: &MergeFixture, dry_run: bool) -> RetireOutcome {
    graph::retire_component(
        &f.state,
        "Ingest Files",
        "The ingest files are one stage of the pipeline.",
        Some("pipeline"),
        dry_run,
        "tester",
        Utc::now(),
    )
    .expect("merge")
}

#[test]
fn plain_retire_deprecates_and_leaves_the_current_set() {
    let state = bootstrap("plain-retire");
    state.set_substrate(Arc::new(synthetic_substrate()));
    let a = component(&state, "ingest files", &["src/"]);
    component(&state, "pipeline", &["lib/"]);
    let constraint = record(&state, "Constraint", "Ingest constraint");
    graph::relate(&state, &constraint, "concerns", &a).unwrap();
    let decision = record(&state, "ArchitecturalDecision", "Ingest decision");
    graph::relate(&state, &decision, "concerns", &a).unwrap();
    let rejected = record_with_status(&state, "Constraint", "Rejected constraint", "rejected");
    graph::relate(&state, &rejected, "concerns", &a).unwrap();
    let lesson = record(&state, "Lesson", "Runtime lesson");
    link_runtime(&state, &lesson);

    // Before: A governs the runtime file everywhere.
    let before = graph::get_entity_dossier(&state, &runtime_position())
        .unwrap()
        .expect("dossier");
    assert_eq!(
        before.realizes.as_ref().map(|r| r.0.as_str()),
        Some(a.as_str())
    );
    assert!(before
        .component_records
        .iter()
        .any(|r| r.title == "Ingest constraint"));
    assert!(moosedev::validation::stale_component_coverage(&state)
        .unwrap()
        .iter()
        .any(|s| s.component_iri == a));
    assert!(mint_plan(&state).unmapped_paths.is_empty());

    let out = graph::retire_component(
        &state,
        &a,
        "Ingest moved out of this repository.",
        None,
        false,
        "tester",
        Utc::now(),
    )
    .expect("retire");

    assert_eq!(out.status, "deprecated");
    assert_eq!(out.component_iri, a);
    assert_eq!(out.component_name, "ingest files");
    assert!(out.successor.is_none());
    assert!(!out.dry_run);
    assert_eq!(out.remaining_paths, vec!["src/"]);
    assert!(out.moved_edges.is_empty() && out.moved_paths.is_empty());
    assert_eq!(
        titles(&out.remaining_records),
        vec![
            "Ingest constraint",
            "Ingest decision",
            "Rejected constraint"
        ]
    );
    // Only current Constraints stop being delivered; the rejected one never was.
    assert_eq!(titles(&out.affected_constraints), vec!["Ingest constraint"]);
    assert_eq!(out.affected_constraints[0].iri, constraint);

    assert_eq!(status(&state, &a), vec!["deprecated"]);
    let rationale = out.rationale_iri.expect("rationale minted");
    assert!(has_edge(
        &state,
        &a,
        &predicate(&state, "hasRationale"),
        &rationale
    ));
    assert_eq!(status(&state, &rationale), vec!["accepted"]);
    assert_eq!(
        literals(&state, &rationale, &state.capture.description),
        vec!["Ingest moved out of this repository."]
    );
    assert_eq!(
        literals(&state, &rationale, moose::RDFS_LABEL),
        vec!["Rationale: retire ingest files"]
    );
    // History stays on the component.
    assert_eq!(literals(&state, &a, COVERS_PATH), vec!["src/"]);
    assert!(has_edge(
        &state,
        &decision,
        &predicate(&state, "concerns"),
        &a
    ));

    assert_eq!(current_names(&state), vec!["pipeline"]);
    assert_eq!(all_names(&state), vec!["ingest files", "pipeline"]);
    let after = graph::get_entity_dossier(&state, &runtime_position())
        .unwrap()
        .expect("dossier");
    assert!(after.realizes.is_none(), "{:?}", after.realizes);
    assert!(after.component_records.is_empty());
    assert!(!moosedev::validation::stale_component_coverage(&state)
        .unwrap()
        .iter()
        .any(|s| s.component_iri == a));
    let plan = mint_plan(&state);
    assert!(plan.unmapped_paths.contains("src/runtime.rs"));
    assert!(plan
        .create
        .iter()
        .chain(&plan.update)
        .all(|entity| entity.realizes.as_deref() != Some(a.as_str())));

    state.ensure_enriched();
    let report = moosedev::validation::validate_project(&state).unwrap();
    assert!(report.conforms(), "{report:?}");
}

#[test]
fn merge_moves_edges_and_paths_and_records_the_supersession() {
    let f = merge_fixture("merge");
    let state = &f.state;
    let before = graph::get_entity_dossier(state, &runtime_position())
        .unwrap()
        .expect("dossier");
    assert_eq!(
        before.realizes.as_ref().map(|r| r.0.as_str()),
        Some(f.a.as_str())
    );

    let out = merge(&f, false);

    assert_eq!(out.status, "superseded");
    assert_eq!(out.successor, Some((f.b.clone(), "pipeline".to_string())));
    assert_eq!(out.moved_paths, vec!["src/"]);
    assert_eq!(out.paths_already_on_successor, vec!["shared/"]);
    assert!(out.remaining_paths.is_empty() && out.remaining_records.is_empty());
    let moved: BTreeSet<(String, String, bool)> = out
        .moved_edges
        .iter()
        .map(
            |MovedEdge {
                 predicate_local,
                 subject_iri,
                 already_on_successor,
             }| {
                (
                    predicate_local.clone(),
                    subject_iri.clone(),
                    *already_on_successor,
                )
            },
        )
        .collect();
    let expected: BTreeSet<(String, String, bool)> = [
        ("concerns", &f.decision, false),
        ("concerns", &f.dual, true),
        ("concerns", &f.concern_constraint, false),
        ("concerns", &f.inverse_only, false),
        ("constrains", &f.constrain_constraint, false),
        ("realizes", &f.entity, false),
    ]
    .into_iter()
    .map(|(p, s, already)| (p.to_string(), s.clone(), already))
    .collect();
    assert_eq!(moved, expected);
    // Inferred inverses and type facts are not "not moved"; the SE edge is.
    assert_eq!(out.not_moved_edges.len(), 1, "{:?}", out.not_moved_edges);
    assert_eq!(out.not_moved_edges[0].predicate_local, "dependsOn");
    assert_eq!(out.not_moved_edges[0].other_iri, f.storage);
    assert!(out.not_moved_edges[0].outgoing);
    assert_eq!(
        titles(&out.affected_constraints),
        vec!["Ingest constraint", "Ingest limit"]
    );

    // Lifecycle.
    assert_eq!(status(state, &f.a), vec!["superseded"]);
    assert_eq!(status(state, &f.b), vec!["accepted"]);
    assert!(has_edge(state, &f.b, &predicate(state, "supersedes"), &f.a));
    assert!(has_edge(
        state,
        &f.a,
        &predicate(state, "isSupersededBy"),
        &f.b
    ));
    let rationale = out.rationale_iri.clone().expect("rationale");
    assert!(has_edge(
        state,
        &f.b,
        &predicate(state, "hasRationale"),
        &rationale
    ));
    assert_eq!(
        literals(state, &rationale, moose::RDFS_LABEL),
        vec!["Rationale: merge ingest files into pipeline"]
    );

    // Edges, both stored orientations.
    let concerns = predicate(state, "concerns");
    let constrains = predicate(state, "constrains");
    let realizes = predicate(state, "realizes");
    for record in [&f.decision, &f.dual, &f.concern_constraint] {
        assert_eq!(edge_count(state, record, &concerns, &f.b), 1);
        assert_eq!(edge_count(state, record, &concerns, &f.a), 0);
    }
    assert!(has_edge(
        state,
        &f.b,
        &predicate(state, "isConcernedBy"),
        &f.inverse_only
    ));
    assert!(!has_edge(
        state,
        &f.a,
        &predicate(state, "isConcernedBy"),
        &f.inverse_only
    ));
    assert!(has_edge(state, &f.constrain_constraint, &constrains, &f.b));
    assert!(!has_edge(state, &f.constrain_constraint, &constrains, &f.a));
    assert!(has_edge(
        state,
        &f.b,
        &predicate(state, "isConstrainedBy"),
        &f.constrain_constraint
    ));
    assert!(has_edge(state, &f.entity, &realizes, &f.b));
    assert!(!has_edge(state, &f.entity, &realizes, &f.a));
    assert!(has_edge(state, &f.a, DEPENDS_ON, &f.storage));

    // Paths.
    assert!(literals(state, &f.a, COVERS_PATH).is_empty());
    assert_eq!(
        literals(state, &f.b, COVERS_PATH),
        vec!["lib/", "shared/", "src/"]
    );
    assert_eq!(current_names(state), vec!["pipeline", "storage"]);

    // The successor now governs the file under A's old path, with A's rules.
    let dossier = graph::get_entity_dossier(state, &runtime_position())
        .unwrap()
        .expect("dossier");
    assert_eq!(
        dossier.realizes,
        Some((f.b.clone(), "pipeline".to_string()))
    );
    assert!(dossier
        .component_records
        .iter()
        .any(|r| r.iri == f.concern_constraint));

    // Re-enrichment rebuilds inverses on B and brings nothing back to A.
    state.ensure_enriched();
    let moved_locals = [
        "concerns",
        "isConcernedBy",
        "constrains",
        "isConstrainedBy",
        "realizes",
        "isRealizedBy",
    ];
    let (outgoing, incoming) = incident(state, &f.a);
    for local in moved_locals {
        assert!(
            !outgoing.contains(local) && !incoming.contains(local),
            "{local} at A"
        );
    }
    assert!(incoming.contains("supersedes") && outgoing.contains("isSupersededBy"));
    assert!(has_edge(
        state,
        &f.b,
        &predicate(state, "isConcernedBy"),
        &f.decision
    ));

    let report = moosedev::validation::validate_project(state).unwrap();
    assert!(report.conforms(), "{:?}", report.violations);
    assert!(!report
        .violations
        .iter()
        .any(|v| v.kind
            == moosedev::validation::ViolationKind::Other("UnflippedSupersession".into())));
}

#[test]
fn dry_run_reports_exactly_what_the_write_does_and_writes_nothing() {
    let f = merge_fixture("dry-run");
    let before = project_dump(&f.state);

    let preview = merge(&f, true);
    assert_eq!(
        project_dump(&f.state),
        before,
        "a dry run must write nothing"
    );
    assert!(preview.dry_run && preview.rationale_iri.is_none());

    let real = merge(&f, false);
    assert_ne!(project_dump(&f.state), before);
    assert_eq!(
        RetireOutcome {
            rationale_iri: None,
            dry_run: true,
            ..real
        },
        preview
    );
}

#[test]
fn refusals_write_nothing() {
    let state = bootstrap("refusals");
    let a = component(&state, "ingest files", &["src/"]);
    let b = component(&state, "pipeline", &["lib/"]);
    let gone = component(&state, "legacy", &[]);
    record_with_status(&state, "SystemComponent", "draft area", "proposed");
    graph::retire_component(
        &state,
        &gone,
        "Gone.",
        Some(&b),
        false,
        "tester",
        Utc::now(),
    )
    .unwrap();
    let before = project_dump(&state);

    let refuse = |component: &str, rationale: &str, into: Option<&str>| {
        let err = graph::retire_component(
            &state,
            component,
            rationale,
            into,
            false,
            "tester",
            Utc::now(),
        )
        .expect_err("refused")
        .to_string();
        assert_eq!(project_dump(&state), before, "refusal wrote: {err}");
        err
    };

    let twice = refuse("legacy", "Again.", None);
    assert!(twice.contains("already retired"), "{twice}");
    assert!(twice.contains("merged into \"pipeline\""), "{twice}");
    let itself = refuse(&a, "Self.", Some("Ingest Files"));
    assert!(itself.contains("into itself"), "{itself}");
    let into_retired = refuse(&a, "Retired target.", Some("legacy"));
    assert!(into_retired.contains("superseded"), "{into_retired}");
    let into_proposed = refuse(&a, "Proposed target.", Some("draft area"));
    assert!(into_proposed.contains("proposed"), "{into_proposed}");
    let unknown = refuse("no such component", "Unknown.", None);
    assert!(unknown.contains("unknown SystemComponent"), "{unknown}");
    assert!(unknown.contains("ingest files, pipeline"), "{unknown}");
    let empty = refuse(&a, "   ", None);
    assert!(empty.contains("rationale"), "{empty}");
    // Even a dry run refuses what the write would.
    let err = graph::retire_component(&state, &a, "Self.", Some(&a), true, "tester", Utc::now())
        .expect_err("dry run refuses too");
    assert!(err.to_string().contains("into itself"));

    // Path coverage on a retired component names its successor.
    let err = graph::declare_component_paths(&state, "legacy", &["x/".to_string()])
        .expect_err("retired component covers nothing")
        .to_string();
    assert!(err.contains("merged into \"pipeline\""), "{err}");
    let err = graph::remove_component_paths(&state, &gone, &["x/".to_string()])
        .expect_err("retired by IRI too")
        .to_string();
    assert!(err.contains("merged into \"pipeline\""), "{err}");
    assert_eq!(project_dump(&state), before);
}

#[test]
fn rename_keeps_the_iri_and_refuses_a_clash() {
    let state = bootstrap("rename");
    let a = component(&state, "ingest files", &["src/"]);
    let b = component(&state, "pipeline", &["lib/"]);

    let out = graph::rename_component(&state, "Pipeline", "  data pipeline ").unwrap();
    assert_eq!(out.component_iri, b);
    assert_eq!(out.old_name, "pipeline");
    assert_eq!(out.new_name, "data pipeline");
    assert!(out.changed);
    assert_eq!(
        literals(&state, &b, moose::RDFS_LABEL),
        vec!["data pipeline"]
    );
    assert_eq!(literals(&state, &b, COMPONENT_NAME), vec!["data pipeline"]);
    assert_eq!(current_names(&state), vec!["data pipeline", "ingest files"]);
    let declared =
        graph::declare_component_paths(&state, "data pipeline", &["etl/".to_string()]).unwrap();
    assert_eq!(declared.component_iri, b);

    let before = project_dump(&state);
    let same = graph::rename_component(&state, &b, "data pipeline").unwrap();
    assert!(!same.changed);
    let err = graph::rename_component(&state, &b, "INGEST FILES")
        .expect_err("clash")
        .to_string();
    assert!(err.contains(&a), "{err}");
    assert!(graph::rename_component(&state, &b, "  ").is_err());
    assert_eq!(project_dump(&state), before);

    // A retired component's name is free; a current one then wins resolution
    // in path declarations and capture relation targets alike.
    graph::retire_component(
        &state,
        &a,
        "Folded in.",
        Some(&b),
        false,
        "tester",
        Utc::now(),
    )
    .unwrap();
    assert!(graph::rename_component(&state, &a, "anything").is_err());
    graph::rename_component(&state, &b, "ingest files").unwrap();
    let declared =
        graph::declare_component_paths(&state, "ingest files", &["more/".to_string()]).unwrap();
    assert_eq!(declared.component_iri, b);
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let captured = graph::record_instance_with_relation_args(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![(state.capture.title.clone(), "New decision".to_string())],
        },
        &[("concerns".to_string(), "ingest files".to_string())],
        "tester",
        Utc::now(),
    )
    .expect("capture resolves the current component");
    assert_eq!(captured.applied_edges[0].object_iri, b);
}

#[test]
fn relate_and_capture_keep_component_lifecycle_edges_closed() {
    let state = bootstrap("closed-writes");
    let a = component(&state, "ingest files", &["src/"]);
    let b = component(&state, "pipeline", &["lib/"]);
    let rationale = record(&state, "Rationale", "Hand-made rationale");
    let decision = record(&state, "ArchitecturalDecision", "Later decision");
    let constraint = record(&state, "Constraint", "Later constraint");
    let before = project_dump(&state);

    for (subject, local, object) in [
        (&b, "supersedes", &a),
        (&a, "isSupersededBy", &b),
        (&b, "hasRationale", &rationale),
        (&rationale, "isRationaleFor", &a),
    ] {
        let err = match graph::relate(&state, subject, local, object) {
            Ok(_) => panic!("{local} on a component must be refused"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("retire_component"), "{local}: {err}");
    }
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let proposed = |relations: &[(&str, &str)]| {
        graph::record_instance_with_relation_args(
            &state,
            &RecordInput {
                class_iri: class_iri.clone(),
                class_local: "ArchitecturalDecision".to_string(),
                properties: vec![
                    (
                        state.capture.title.clone(),
                        "Proposed replacement".to_string(),
                    ),
                    (state.capture.status.clone(), "proposed".to_string()),
                ],
            },
            &relations
                .iter()
                .map(|(p, t)| (p.to_string(), t.to_string()))
                .collect::<Vec<_>>(),
            "tester",
            Utc::now(),
        )
    };
    let err = proposed(&[("supersedes", &a)]).expect_err("capture refuses too");
    assert!(err.to_string().contains("retire_component"), "{err}");
    assert_eq!(project_dump(&state), before);

    graph::retire_component(
        &state,
        &a,
        "Folded in.",
        Some(&b),
        false,
        "tester",
        Utc::now(),
    )
    .unwrap();
    let before = project_dump(&state);
    let err = match graph::relate(&state, &decision, "concerns", &a) {
        Ok(_) => panic!("concerns to a retired component must be refused"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("merged into \"pipeline\"") && err.contains(&b),
        "{err}"
    );
    let err = match graph::relate(&state, &constraint, "constrains", &a) {
        Ok(_) => panic!("constrains to a retired component must be refused"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("merged into \"pipeline\""), "{err}");
    let err = match graph::relate(&state, &a, "isConcernedBy", &decision) {
        Ok(_) => panic!("the inverse orientation must be refused too"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("merged into \"pipeline\""), "{err}");
    let err = proposed(&[("concerns", "ingest files")]).expect_err("capture by name");
    assert!(
        err.to_string().contains("merged into \"pipeline\""),
        "{err}"
    );
    assert_eq!(project_dump(&state), before);

    // The successor still takes new links.
    graph::relate(&state, &decision, "concerns", &b).unwrap();
}

/// Capture validates the subject it is writing, not only existing targets: a
/// component captured already retired takes no new `concerns`/`constrains`
/// links in the inverse orientation either.
#[test]
fn capturing_a_retired_component_with_inverse_links_is_refused() {
    let state = bootstrap("capture-retired-subject");
    let decision = record(&state, "ArchitecturalDecision", "Some decision");
    let constraint = record(&state, "Constraint", "Some constraint");
    let class_iri = state.resolve_class("SystemComponent").unwrap();
    let capture = |name: &str, status: &str, relation: (&str, &str)| {
        graph::record_instance_with_relation_args(
            &state,
            &RecordInput {
                class_iri: class_iri.clone(),
                class_local: "SystemComponent".to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), name.to_string()),
                    (state.capture.status.clone(), status.to_string()),
                ],
            },
            &[(relation.0.to_string(), relation.1.to_string())],
            "tester",
            Utc::now(),
        )
    };
    let before = project_dump(&state);
    for status in ["deprecated", "superseded", "rejected"] {
        for relation in [
            ("isConcernedBy", decision.as_str()),
            ("isConstrainedBy", constraint.as_str()),
        ] {
            let err = capture("born retired", status, relation)
                .expect_err("refused")
                .to_string();
            assert!(
                err.contains("being captured is"),
                "{status} {relation:?}: {err}"
            );
            assert_eq!(project_dump(&state), before);
        }
    }
    // The same links on a current component are fine.
    let ok = capture("alive", "accepted", ("isConcernedBy", &decision)).unwrap();
    assert!(has_edge(
        &state,
        &ok.iri,
        &predicate(&state, "isConcernedBy"),
        &decision
    ));
}

/// The widened shapes make component --supersedes--> component catalogue-legal,
/// so the cluster path must refuse it like `relate` and inline relations do.
#[test]
fn cluster_capture_cannot_write_component_lifecycle_edges() {
    let state = bootstrap("cluster-closed");
    let before = project_dump(&state);
    let labels = vec!["older component".to_string()];
    let err = graph::record_decision_with_cluster(
        &state,
        &RecordInput {
            class_iri: state.resolve_class("SystemComponent").unwrap(),
            class_local: "SystemComponent".to_string(),
            properties: vec![(moose::RDFS_LABEL.to_string(), "newer component".to_string())],
        },
        &[],
        &[graph::ClusterSlot {
            predicate_local: "supersedes",
            range_class_local: "SystemComponent",
            labels: &labels,
        }],
        "tester",
        Utc::now(),
    )
    .expect_err("refused")
    .to_string();
    assert!(err.contains("retire_component"), "{err}");
    assert_eq!(project_dump(&state), before);
}

/// Two status literals (a store merged from diverged copies): the retiring one
/// wins in Stories exactly as in `load_components`, and Stories still keep
/// `proposed` components out.
#[test]
fn stories_read_component_status_like_the_loader() {
    let state = bootstrap("stories-two-statuses");
    let mut doubled = Vec::new();
    for retired in ["superseded", "deprecated", "rejected"] {
        let iri = component(&state, &format!("doubled {retired}"), &[]);
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(&iri).unwrap(),
                NamedNode::new(&state.capture.status).unwrap(),
                oxigraph::model::Literal::new_simple_literal(retired),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
        assert_eq!(
            status(&state, &iri),
            vec!["accepted".to_string(), retired.to_string()]
        );
        doubled.push(iri);
    }
    let proposed = record_with_status(&state, "SystemComponent", "draft", "proposed");
    let current = component(&state, "current", &[]);

    assert_eq!(current_names(&state), vec!["current", "draft"]);
    let index = StoryResolutionIndex::build(&state).unwrap();
    for iri in &doubled {
        assert!(
            index.resolve_entity(&state, iri).is_err(),
            "{iri} offered as current"
        );
    }
    assert!(index.resolve_entity(&state, &proposed).is_err());
    assert!(index.resolve_entity(&state, &current).is_ok());
    let subjects = moosedev::stories::story_subjects(&state, None, 100).unwrap();
    let offered = subjects.iter().map(|s| s.iri.as_str()).collect::<Vec<_>>();
    assert!(offered.contains(&current.as_str()));
    assert!(doubled.iter().all(|iri| !offered.contains(&iri.as_str())));
}

/// Code is no more a retired component's than knowledge is: a new `realizes`
/// edge to it, in either orientation, names the successor instead.
#[test]
fn realizes_to_a_retired_component_is_refused() {
    let f = merge_fixture("realizes-retired");
    let state = &f.state;
    merge(&f, false);
    let before = project_dump(state);

    for (subject, local, object) in [
        (&f.entity, "realizes", &f.a),
        (&f.a, "isRealizedBy", &f.entity),
    ] {
        let err = match graph::relate(state, subject, local, object) {
            Ok(_) => panic!("{local} to a retired component must be refused"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("merged into \"pipeline\""), "{local}: {err}");
    }
    assert_eq!(project_dump(state), before);
    // The successor still takes it (already present: a no-op write).
    graph::relate(state, &f.entity, "realizes", &f.b).unwrap();
}

/// Cluster slots apply the retired-subject rule inline capture applies.
#[test]
fn cluster_capture_of_a_retired_component_takes_no_new_links() {
    let state = bootstrap("cluster-retired-subject");
    let labels = vec!["Some decision".to_string()];
    let capture = |status: &str| {
        graph::record_decision_with_cluster(
            &state,
            &RecordInput {
                class_iri: state.resolve_class("SystemComponent").unwrap(),
                class_local: "SystemComponent".to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), format!("{status} component")),
                    (state.capture.status.clone(), status.to_string()),
                ],
            },
            &[],
            &[graph::ClusterSlot {
                predicate_local: "isConcernedBy",
                range_class_local: "ArchitecturalDecision",
                labels: &labels,
            }],
            "tester",
            Utc::now(),
        )
    };
    let before = project_dump(&state);
    let err = capture("deprecated").expect_err("refused").to_string();
    assert!(err.contains("being captured is deprecated"), "{err}");
    assert_eq!(project_dump(&state), before);
    // The same slot on a current component is legal.
    let (outcome, minted) = capture("accepted").unwrap();
    assert!(has_edge(
        &state,
        &outcome.iri,
        &predicate(&state, "isConcernedBy"),
        &minted[0].iri
    ));
}

#[test]
fn stories_still_know_a_retired_component_but_never_offer_it_as_current() {
    let state = bootstrap("stories");
    let a = component(&state, "ingest files", &["src/"]);
    let b = component(&state, "pipeline", &["lib/"]);
    graph::retire_component(
        &state,
        &a,
        "Folded in.",
        Some(&b),
        false,
        "tester",
        Utc::now(),
    )
    .unwrap();

    let index = StoryResolutionIndex::build(&state).unwrap();
    assert!(index.resolve_entity(&state, &a).is_err());
    assert!(index.resolve_entity(&state, &b).is_ok());

    let recipe = StoryRecipe {
        id: "ingest".to_string(),
        title: "Ingest".to_string(),
        schema_version: 3,
        subject: Some(StoryRecipeSubject::Entity { iri: a.clone() }),
        subject_component_iri: None,
        goal: "Explain ingest".to_string(),
        audience: "maintainers".to_string(),
        focus: StoryFocus::default(),
        curator_context: None,
        beats: Vec::new(),
        status: StoryStatus::Draft,
        curator: "tester".to_string(),
        updated_at: None,
    };
    let mut summary = StorySummary::from(&recipe);
    enrich_summary(&state, &index, &recipe, &mut summary).unwrap();
    assert_eq!(summary.subject_kind, "SystemComponent");
    assert_eq!(summary.subject_label, "ingest files");
}

#[test]
fn a_merge_round_trips_through_the_canonical_text() {
    let f = merge_fixture("round-trip");
    let out = merge(&f, false);
    let text = export_canonical_project(&f.state.store).unwrap().text;

    let dir = data_dir("round-trip-hydrated");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(moosedev::canonical::canonical_path(&dir), &text).unwrap();
    let hydrated = AppState::bootstrap(&dir, &ontology_dir()).expect("hydrate");

    assert_eq!(
        export_canonical_project(&hydrated.store).unwrap().text,
        text
    );
    assert_eq!(status(&hydrated, &f.a), vec!["superseded"]);
    assert_eq!(status(&hydrated, &f.b), vec!["accepted"]);
    assert_eq!(current_names(&hydrated), vec!["pipeline", "storage"]);
    assert!(has_edge(
        &hydrated,
        &f.b,
        &predicate(&hydrated, "supersedes"),
        &f.a
    ));
    assert!(has_edge(
        &hydrated,
        &f.a,
        &predicate(&hydrated, "isSupersededBy"),
        &f.b
    ));
    assert!(has_edge(
        &hydrated,
        &f.b,
        &predicate(&hydrated, "hasRationale"),
        out.rationale_iri.as_deref().unwrap()
    ));
    assert!(has_edge(
        &hydrated,
        &f.decision,
        &predicate(&hydrated, "concerns"),
        &f.b
    ));
    assert!(!has_edge(
        &hydrated,
        &f.decision,
        &predicate(&hydrated, "concerns"),
        &f.a
    ));
    assert!(has_edge(
        &hydrated,
        &f.entity,
        &predicate(&hydrated, "realizes"),
        &f.b
    ));
    assert_eq!(
        literals(&hydrated, &f.b, COVERS_PATH),
        vec!["lib/", "shared/", "src/"]
    );
    assert!(literals(&hydrated, &f.a, COVERS_PATH).is_empty());
}

/// The committed graph (a v0.13.0-shaped store) loads and conforms under the
/// widened ontology, and every non-retired component is still current. Read
/// only: it is hydrated into a temporary store.
#[test]
fn the_committed_graph_upgrades_without_migration() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join(".moosedev/kg.nq");
    let Ok(text) = std::fs::read_to_string(&committed) else {
        eprintln!("no committed graph at {}; skipping", committed.display());
        return;
    };
    let dir = data_dir("committed");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(moosedev::canonical::canonical_path(&dir), text).unwrap();
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("hydrate committed graph");

    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE).unwrap();
    let non_retired = state
        .store
        .quads_for_pattern(
            None,
            Some(rdf_type),
            Some(NamedNodeRef::new(SYSTEM_COMPONENT).unwrap().into()),
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .flatten()
        .filter(|quad| {
            status(&state, quad.subject.to_string().trim_matches(['<', '>']))
                .iter()
                .all(|s| !["rejected", "superseded", "deprecated"].contains(&s.as_str()))
        })
        .count();
    assert!(non_retired > 0);
    assert_eq!(graph::load_components(&state).unwrap().len(), non_retired);

    state.ensure_enriched();
    let report = moosedev::validation::validate_project(&state).unwrap();
    assert!(report.conforms(), "{:?}", report.violations);
}

fn synthetic_substrate() -> Substrate {
    let mut index = Index::new();
    let mut document = Document::new();
    document.relative_path = "src/runtime.rs".to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    let mut info = SymbolInformation::new();
    info.symbol = PUBLIC_SYMBOL.to_string();
    info.display_name = "build_server".to_string();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut signature = Signature::new();
    signature.text = "pub fn build_server()".to_string();
    info.signature_documentation = MessageField::some(signature);
    document.symbols.push(info);
    let mut occurrence = Occurrence::new();
    occurrence.symbol = PUBLIC_SYMBOL.to_string();
    occurrence.range = vec![7, 4, 16];
    occurrence.symbol_roles = 1;
    occurrence.enclosing_range = vec![0, 0, 30, 0];
    document.occurrences.push(occurrence);
    index.documents.push(document);
    let meta = SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        1,
        1,
    );
    Substrate::from_index(index, meta, false).expect("synthetic substrate")
}
