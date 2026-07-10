//! CodeEntity minting integration tests.

use std::path::Path;

use chrono::Utc;
use moosedev::code::substrate::{symbols, DefinitionEntry, Substrate};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";

/// Create an isolated graph state so mint tests never touch the repo store.
fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-mint-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

/// Record a typed architecture node using the same minimal harness as relate tests.
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

/// Build a realistic rust-analyzer DefinitionEntry with normalized SCIP identity.
fn def(
    descriptor: &str,
    file: &str,
    display_name: Option<&str>,
    kind: Option<&str>,
    signature: Option<&str>,
    is_module: bool,
    is_public: bool,
) -> DefinitionEntry {
    let symbol = format!("rust-analyzer cargo moosedev 0.6.3 {descriptor}");
    DefinitionEntry {
        producer: "rust-analyzer".to_string(),
        normalized_symbol: symbols::normalize_symbol(&symbol).expect("valid scip symbol"),
        symbol,
        display_name: display_name.map(str::to_string),
        kind: kind.map(str::to_string),
        signature: signature.map(str::to_string),
        file: file.to_string(),
        is_module,
        is_public,
    }
}

/// Seed one SystemComponent plus a `coversPath` literal for path mapping.
fn seed_component(state: &AppState, title: &str, covers_path: &str) -> String {
    let iri = record(state, "SystemComponent", title);
    insert_literal(state, &iri, COVERS_PATH, covers_path);
    iri
}

/// Insert one project-graph literal used by test setup.
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

/// Return whether the exact project-graph object edge exists.
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

/// Read sorted literal values for one subject/predicate in the project graph.
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

/// Count project-graph quads to prove `ensure_entity` avoids writes on hits.
fn project_quad_count(state: &AppState) -> usize {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    state
        .store
        .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph)))
        .flatten()
        .count()
}

#[test]
fn mint_twice_is_idempotent_and_validates() {
    let state = bootstrap("idempotent");
    let component = seed_component(&state, "runtime component", "src/runtime.rs");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let covered = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let unmapped = def(
        "graph/",
        "src/graph/mod.rs",
        Some("graph"),
        Some("Module"),
        None,
        true,
        false,
    );
    let definitions = vec![covered.clone(), unmapped.clone()];

    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 2);
    assert_eq!(
        plan.create.iter().filter(|p| p.realizes.is_some()).count(),
        1
    );
    assert!(plan.unmapped_paths.contains("src/graph/mod.rs"));

    graph::apply_mint(&state, &plan, &terms).unwrap();
    let first_entities = graph::entities_by_symbol(&state, &terms).unwrap();
    let covered_iri = first_entities
        .get(&covered.normalized_symbol)
        .expect("covered entity")
        .clone();
    assert!(has_edge(&state, &covered_iri, &terms.realizes, &component));
    assert_eq!(
        literal_values(&state, &covered_iri, &terms.has_substrate_symbol),
        vec![covered.normalized_symbol.clone()]
    );
    assert_eq!(
        literal_values(&state, &covered_iri, &terms.has_entity_kind),
        vec!["Function"]
    );
    assert_eq!(
        literal_values(&state, &covered_iri, &terms.has_code_name),
        vec!["build_server"]
    );
    assert_eq!(
        literal_values(&state, &covered_iri, &terms.defined_in_path),
        vec!["src/runtime.rs"]
    );

    let report = validation::validate_project(&state).unwrap();
    assert!(report.conforms(), "{}", validation::format_report(&report));

    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 0);
    assert_eq!(plan.orphaned.len(), 0);
    assert_eq!(plan.unchanged, 2);
    assert_eq!(
        graph::entities_by_symbol(&state, &terms).unwrap(),
        first_entities
    );
}

#[test]
fn lazy_minted_private_entity_is_not_reported_as_orphan() {
    let state = bootstrap("lazy-not-orphan");
    seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let public = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let private = def(
        "runtime/private_helper().",
        "src/runtime.rs",
        Some("private_helper"),
        Some("Function"),
        Some("fn private_helper()"),
        false,
        false,
    );
    let definitions = vec![public, private.clone()];
    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    // Lazily mint the out-of-scope private definition, as link_code would.
    let ensured = graph::ensure_entity(&state, &terms, &components, &private, "tester").unwrap();
    assert!(ensured.created);

    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 0);
    assert_eq!(
        plan.orphaned,
        Vec::<(String, String)>::new(),
        "a lazily minted private entity whose symbol is still defined must not be an orphan"
    );

    // A symbol truly gone from the substrate is still reported.
    let plan = graph::plan_mint(&state, &definitions[..1], &terms, &components).unwrap();
    assert_eq!(plan.orphaned.len(), 1);
    assert_eq!(plan.orphaned[0].1, private.normalized_symbol);
}

#[test]
fn rename_reports_create_and_orphan_without_changing_other_iri() {
    let state = bootstrap("rename");
    seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let original = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let stable = def(
        "graph/load_components().",
        "src/graph/components.rs",
        Some("load_components"),
        Some("Function"),
        Some("pub fn load_components()"),
        false,
        true,
    );
    let definitions = vec![original.clone(), stable.clone()];
    let plan = graph::plan_mint(&state, &definitions, &terms, &components).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let before = graph::entities_by_symbol(&state, &terms).unwrap();
    let stable_iri = before.get(&stable.normalized_symbol).unwrap().clone();

    let renamed = def(
        "runtime/build_backend().",
        "src/runtime.rs",
        Some("build_backend"),
        Some("Function"),
        Some("pub fn build_backend()"),
        false,
        true,
    );
    let plan = graph::plan_mint(&state, &[renamed, stable.clone()], &terms, &components).unwrap();

    assert_eq!(plan.create.len(), 1);
    assert_eq!(plan.orphaned.len(), 1);
    assert_eq!(plan.orphaned[0].1, original.normalized_symbol);
    assert_eq!(plan.unchanged, 1);
    assert_eq!(
        graph::entities_by_symbol(&state, &terms)
            .unwrap()
            .get(&stable.normalized_symbol),
        Some(&stable_iri)
    );
}

#[test]
fn update_path_replaces_defined_in_path_without_changing_iri() {
    let state = bootstrap("update-path");
    seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let original = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let plan = graph::plan_mint(&state, &[original.clone()], &terms, &components).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&original.normalized_symbol].clone();

    let mut moved = original.clone();
    moved.file = "src/server/runtime.rs".to_string();
    let plan = graph::plan_mint(&state, &[moved], &terms, &components).unwrap();
    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 1);
    graph::apply_mint(&state, &plan, &terms).unwrap();

    assert_eq!(
        graph::entities_by_symbol(&state, &terms).unwrap()[&original.normalized_symbol],
        iri
    );
    assert_eq!(
        literal_values(&state, &iri, &terms.defined_in_path),
        vec!["src/server/runtime.rs"]
    );
    assert!(!literal_values(&state, &iri, &terms.defined_in_path)
        .contains(&"src/runtime.rs".to_string()));
}

#[test]
fn minted_entities_participate_in_relate_validation() {
    let state = bootstrap("relate");
    let component = seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let entry = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let plan = graph::plan_mint(&state, &[entry.clone()], &terms, &components).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let entity_iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&entry.normalized_symbol].clone();
    let decision = record(&state, "ArchitecturalDecision", "Runtime builder decision");

    graph::relate(&state, &decision, "concerns", &entity_iri).expect("AD concerns CodeEntity");
    assert!(
        graph::relate(&state, &decision, "realizes", &component).is_err(),
        "realizes domain is CodeEntity, not ArchitecturalDecision"
    );
}

#[test]
fn ensure_entity_reuses_existing_and_mints_private_symbols() {
    let state = bootstrap("ensure");
    seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let public = def(
        "runtime/build_server().",
        "src/runtime.rs",
        Some("build_server"),
        Some("Function"),
        Some("pub fn build_server()"),
        false,
        true,
    );
    let plan = graph::plan_mint(&state, &[public.clone()], &terms, &components).unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let existing_iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&public.normalized_symbol].clone();
    let count_before = project_quad_count(&state);

    let ensured =
        graph::ensure_entity(&state, &terms, &components, &public, "moosedev-mint").unwrap();
    assert!(!ensured.created);
    assert_eq!(ensured.iri, existing_iri);
    assert_eq!(project_quad_count(&state), count_before);

    let private = def(
        "runtime/private_helper().",
        "src/runtime.rs",
        Some("private_helper"),
        Some("Function"),
        Some("fn private_helper()"),
        false,
        false,
    );
    let ensured =
        graph::ensure_entity(&state, &terms, &components, &private, "moosedev-mint").unwrap();
    assert!(ensured.created);
    assert_eq!(
        literal_values(&state, &ensured.iri, &terms.has_substrate_symbol),
        vec![private.normalized_symbol]
    );
    assert_eq!(
        literal_values(&state, &ensured.iri, &terms.has_code_name),
        vec!["private_helper"]
    );
}

#[test]
fn repo_substrate_plan_counts_when_index_present() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let data_dir = repo_root.join(".moosedev");
    let meta_path = data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping mint substrate plan: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }

    let substrate = Substrate::load(&data_dir, repo_root)?;
    let state = bootstrap("repo-plan");
    seed_component(&state, "source component", "src/");
    let components = graph::load_components(&state)?;
    let terms = graph::CodeTerms::resolve(&state)?;
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(&state, &definitions, &terms, &components)?;

    assert!(
        (300..=1500).contains(&plan.create.len()),
        "expected repo mint creates in range, got {}",
        plan.create.len()
    );
    assert!(
        plan.create.iter().any(|planned| {
            planned
                .entry
                .normalized_symbol
                .contains("runtime/build_server().")
                && planned.realizes.is_some()
        }),
        "build_server should be planned with a realizes edge"
    );
    Ok(())
}
