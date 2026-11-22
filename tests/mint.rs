//! CodeEntity minting integration tests.

use std::path::Path;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{symbols, DefinitionEntry, Substrate, SubstrateMeta};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::validation;
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use scip::types::Index;

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

    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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

    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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
    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();

    // Lazily mint the out-of-scope private definition, as link_code would.
    let ensured = graph::ensure_entity(&state, &terms, &components, &private, "tester").unwrap();
    assert!(ensured.created);

    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 0);
    assert_eq!(
        plan.orphaned,
        Vec::<(String, String)>::new(),
        "a lazily minted private entity whose symbol is still defined must not be an orphan"
    );

    // A symbol truly gone from the substrate is still reported.
    let plan = graph::plan_mint(
        &state,
        &definitions[..1],
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    assert_eq!(plan.orphaned.len(), 1);
    assert_eq!(plan.orphaned[0].1, private.normalized_symbol);
}

#[test]
fn an_entity_narrowed_out_of_scope_is_reported_rather_than_vanishing() {
    // Narrowing the mint scope leaves already-minted entities alive in the
    // substrate but unmanaged by minting. They are not orphans (the symbol is
    // still defined) and not `unchanged` (they are never planned), so without
    // the out_of_scope bucket they appear in no line of the report at all.
    let state = bootstrap("narrowed-out-of-scope");
    seed_component(&state, "runtime component", "src/");
    let components = graph::load_components(&state).unwrap();
    let terms = graph::CodeTerms::resolve(&state).unwrap();

    let in_scope = def(
        "runtime/widget().",
        "src/runtime.rs",
        Some("widget"),
        Some("Function"),
        Some("pub fn widget()"),
        false,
        true,
    );
    let plan = graph::plan_mint(
        &state,
        std::slice::from_ref(&in_scope),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    assert_eq!(plan.create.len(), 1);
    assert_eq!(plan.out_of_scope, Vec::<(String, String)>::new());
    graph::apply_mint(&state, &plan, &terms).unwrap();

    // The same definition, now judged out of scope — exactly what a narrowed
    // visibility predicate does to a previously minted symbol.
    let narrowed = DefinitionEntry {
        is_public: false,
        ..in_scope.clone()
    };
    let plan = graph::plan_mint(
        &state,
        std::slice::from_ref(&narrowed),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();

    assert_eq!(plan.create.len(), 0);
    assert_eq!(plan.update.len(), 0);
    assert_eq!(plan.unchanged, 0, "it is no longer planned");
    assert_eq!(
        plan.orphaned,
        Vec::<(String, String)>::new(),
        "its symbol is still a live workspace definition"
    );
    assert_eq!(plan.out_of_scope.len(), 1);
    assert_eq!(plan.out_of_scope[0].1, narrowed.normalized_symbol);
}

#[test]
fn syntactic_entity_is_orphaned_only_when_the_substrate_proves_it_dead() {
    let state = bootstrap("syntactic-orphan");
    seed_component(&state, "fixture component", "tests/fixtures/");
    let repo_root = state.data_dir.join("fixture-repo");
    let fixture_path = repo_root.join("tests/fixtures/ts_fallback.rs");
    std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture_path, include_str!("fixtures/ts_fallback.rs")).unwrap();
    let substrate = Substrate::from_index_rooted(Index::new(), substrate_meta(), false, repo_root)
        .expect("rooted substrate");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let components = graph::load_components(&state).unwrap();
    let identity = "ts:rust:tests/fixtures/ts_fallback.rs:fn:<Widget as Render>::render";
    let entry = DefinitionEntry {
        producer: "tree-sitter".to_string(),
        symbol: identity.to_string(),
        normalized_symbol: identity.to_string(),
        display_name: Some("render".to_string()),
        kind: Some("fn".to_string()),
        signature: None,
        file: "tests/fixtures/ts_fallback.rs".to_string(),
        is_module: false,
        is_public: false,
    };
    graph::ensure_entity(&state, &terms, &components, &entry, "tester").unwrap();

    let alive = graph::plan_mint(&state, &[], &terms, &components, Some(&substrate)).unwrap();
    assert!(alive.orphaned.is_empty());

    std::fs::remove_file(&fixture_path).unwrap();
    let dead = graph::plan_mint(&state, &[], &terms, &components, Some(&substrate)).unwrap();
    assert_eq!(dead.orphaned.len(), 1);
    assert_eq!(dead.orphaned[0].1, identity);

    let unverifiable = graph::plan_mint(&state, &[], &terms, &components, None).unwrap();
    assert!(unverifiable.orphaned.is_empty());
}

fn substrate_meta() -> SubstrateMeta {
    SubstrateMeta::single(
        "rust-analyzer",
        "abc123",
        DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc),
        0,
        0,
    )
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
    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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
    let plan = graph::plan_mint(
        &state,
        &[renamed, stable.clone()],
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();

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
    let plan = graph::plan_mint(
        &state,
        std::slice::from_ref(&original),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&original.normalized_symbol].clone();

    let mut moved = original.clone();
    moved.file = "src/server/runtime.rs".to_string();
    let plan = graph::plan_mint(
        &state,
        &[moved],
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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
    let plan = graph::plan_mint(
        &state,
        std::slice::from_ref(&entry),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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
    let plan = graph::plan_mint(
        &state,
        std::slice::from_ref(&public),
        &terms,
        &components,
        state.substrate().as_deref(),
    )
    .unwrap();
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
    let plan = graph::plan_mint(
        &state,
        &definitions,
        &terms,
        &components,
        state.substrate().as_deref(),
    )?;

    // A sanity BAND, not a fixture: the exact count tracks repo size, since the
    // substrate spans the whole Rust crate (tests/ and examples/ included) plus
    // the ui/ TypeScript top-level surface, where export-ness is not encoded so
    // private top-level declarations are included by design. The band catches a
    // plan that has collapsed to nothing or exploded, and deliberately leaves
    // room to grow — a tight ceiling only fails later for the wrong reason.
    assert!(
        (300..=6000).contains(&plan.create.len()),
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

/// A public rust-analyzer function defined in `package` (issue #59 fixtures).
fn def_in(package: &str, descriptor: &str, file: &str, name: &str) -> DefinitionEntry {
    let symbol = format!("rust-analyzer cargo {package} 0.11.0 {descriptor}");
    DefinitionEntry {
        producer: "rust-analyzer".to_string(),
        normalized_symbol: symbols::normalize_symbol(&symbol).expect("valid scip symbol"),
        symbol,
        display_name: Some(name.to_string()),
        kind: Some("Function".to_string()),
        signature: Some(format!("pub fn {name}()")),
        file: file.to_string(),
        is_module: false,
        is_public: true,
    }
}

/// Plan against `definitions` with the given remaps.
fn plan_with(
    state: &AppState,
    definitions: &[DefinitionEntry],
    remaps: &[graph::PathRemap],
) -> graph::MintPlan {
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let components = graph::load_components(state).unwrap();
    graph::plan_mint_with_remaps(state, definitions, &terms, &components, None, remaps).unwrap()
}

/// Mint `definitions` and return normalized symbol -> IRI.
fn mint_all(
    state: &AppState,
    definitions: &[DefinitionEntry],
) -> std::collections::BTreeMap<String, String> {
    let terms = graph::CodeTerms::resolve(state).unwrap();
    graph::apply_mint(state, &plan_with(state, definitions, &[]), &terms).unwrap();
    graph::entities_by_symbol(state, &terms).unwrap()
}

#[test]
fn crate_split_rehomes_entity_in_place_and_keeps_its_links() {
    let state = bootstrap("rehome-crate-split");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let core = seed_component(&state, "core", "src/");
    let rdf = seed_component(&state, "rdf crate", "crates/trivyn-rdf/");
    let old = def_in(
        "trivyn",
        "rdf/transform/apply().",
        "src/rdf/transform.rs",
        "apply",
    );
    let entity = mint_all(&state, std::slice::from_ref(&old))[&old.normalized_symbol].clone();
    assert!(has_edge(&state, &entity, &terms.realizes, &core));
    // A stored inverse of the old link: left behind, enrichment would
    // re-derive the forward edge from it.
    insert_edge(&state, &core, &is_realized_by(&state), &entity);
    let constraint = record(&state, "Constraint", "Transforms are pure");
    graph::relate(&state, &constraint, "constrains", &entity).unwrap();

    let moved = def_in(
        "trivyn-rdf",
        "rdf/transform/apply().",
        "crates/trivyn-rdf/src/rdf/transform.rs",
        "apply",
    );
    let plan = plan_with(&state, std::slice::from_ref(&moved), &[]);
    assert_eq!(plan.rehome.len(), 1, "{plan:#?}");
    assert_eq!(plan.rehome[0].iri, entity);
    assert_eq!(plan.rehome[0].basis, graph::RehomeBasis::PackageMove);
    assert_eq!(
        plan.rehome[0].old_path.as_deref(),
        Some("src/rdf/transform.rs")
    );
    assert!(
        plan.create.is_empty(),
        "the successor must not be minted fresh"
    );
    assert!(plan.orphaned.is_empty());
    assert!(
        plan.realizes_changes.is_empty(),
        "the re-home writes the new link itself: {:?}",
        plan.realizes_changes
    );

    let outcome = graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!(outcome.rehomed, 1);
    assert_eq!((outcome.realizes_moved, outcome.realizes_dropped), (0, 0));
    state.ensure_enriched();
    assert!(!linked_either_way(&state, &entity, &core));
    let after = graph::entities_by_symbol(&state, &terms).unwrap();
    assert_eq!(after.get(&moved.normalized_symbol), Some(&entity));
    assert!(!after.contains_key(&old.normalized_symbol));
    assert_eq!(
        literal_values(&state, &entity, &terms.defined_in_path),
        vec!["crates/trivyn-rdf/src/rdf/transform.rs".to_string()]
    );
    assert!(has_edge(&state, &entity, &terms.realizes, &rdf));
    assert!(
        !has_edge(&state, &entity, &terms.realizes, &core),
        "realizes is recomputed from the new path, not topped up"
    );
    let constrains = state.resolve_object_property("constrains").unwrap();
    assert!(
        has_edge(&state, &constraint, &constrains, &entity)
            || has_edge(
                &state,
                &entity,
                &state.resolve_object_property("isConstrainedBy").unwrap(),
                &constraint
            ),
        "the record link follows the entity"
    );

    let again = plan_with(&state, std::slice::from_ref(&moved), &[]);
    assert!(again.rehome.is_empty() && again.create.is_empty() && again.update.is_empty());
    assert_eq!(again.unchanged, 1);
    assert!(validation::validate_project(&state).unwrap().conforms());
}

#[test]
fn same_symbol_in_an_unrelated_crate_does_not_adopt_a_deleted_entity() {
    let state = bootstrap("rehome-unrelated");
    let old = def_in("a", "util/helper().", "crates/a/src/util.rs", "helper");
    let entity = mint_all(&state, std::slice::from_ref(&old))[&old.normalized_symbol].clone();
    let constraint = record(&state, "Constraint", "Helpers never allocate");
    graph::relate(&state, &constraint, "constrains", &entity).unwrap();

    let other = def_in("b", "util/helper().", "crates/b/src/util.rs", "helper");
    let plan = plan_with(&state, std::slice::from_ref(&other), &[]);
    assert!(plan.rehome.is_empty(), "{plan:#?}");
    assert_eq!(plan.create.len(), 1);
    assert_eq!(plan.orphaned, vec![(entity.clone(), old.normalized_symbol)]);
    assert_eq!(plan.unresolved_rehomes.len(), 1);
    assert_eq!(
        plan.unresolved_rehomes[0].candidates,
        vec!["crates/b/src/util.rs".to_string()]
    );
    let links = &plan.orphan_links[&entity];
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].other, constraint);
    assert_eq!(links[0].label.as_deref(), Some("Helpers never allocate"));
}

#[test]
fn two_plausible_successors_are_reported_not_guessed() {
    let state = bootstrap("rehome-ambiguous");
    let old = def_in("app", "util/helper().", "src/util.rs", "helper");
    let entity = mint_all(&state, std::slice::from_ref(&old))[&old.normalized_symbol].clone();

    let a = def_in("a", "util/helper().", "crates/a/src/util.rs", "helper");
    let b = def_in("b", "util/helper().", "crates/b/src/util.rs", "helper");
    let plan = plan_with(&state, &[a, b], &[]);
    assert!(plan.rehome.is_empty(), "{plan:#?}");
    assert_eq!(plan.create.len(), 2);
    assert_eq!(plan.orphaned.len(), 1);
    assert_eq!(plan.unresolved_rehomes.len(), 1);
    assert_eq!(plan.unresolved_rehomes[0].iri, entity);
    assert_eq!(plan.unresolved_rehomes[0].candidates.len(), 2);
    assert!(
        !plan.orphan_links.contains_key(&entity),
        "an orphan with no links carries no recorded knowledge"
    );
}

#[test]
fn remap_rehomes_a_move_that_changed_the_symbol() {
    let state = bootstrap("rehome-remap");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    // The crate root moved down a level, so module paths lost `rdf/` as well
    // as changing package: no package-free key can follow that.
    let old_thing = def_in(
        "app",
        "rdf/transform/Thing#new().",
        "src/rdf/transform.rs",
        "new",
    );
    let old_other = def_in(
        "app",
        "rdf/transform/Other#new().",
        "src/rdf/transform.rs",
        "new",
    );
    let entities = mint_all(&state, &[old_thing.clone(), old_other.clone()]);
    let new_thing = def_in(
        "rdf",
        "transform/Thing#new().",
        "crates/rdf/src/transform.rs",
        "new",
    );
    let new_other = def_in(
        "rdf",
        "transform/Other#new().",
        "crates/rdf/src/transform.rs",
        "new",
    );
    let moved = [new_thing.clone(), new_other.clone()];

    let without = plan_with(&state, &moved, &[]);
    assert!(without.rehome.is_empty());
    assert_eq!(without.orphaned.len(), 2);

    let remap = graph::PathRemap::parse("src/rdf/=crates/rdf/src/").unwrap();
    let plan = plan_with(&state, &moved, &[remap]);
    assert_eq!(plan.rehome.len(), 2, "{plan:#?}");
    assert!(plan
        .rehome
        .iter()
        .all(|rehome| rehome.basis == graph::RehomeBasis::PathRemap));
    assert!(plan.create.is_empty() && plan.orphaned.is_empty());
    assert!(plan.unresolved_rehomes.is_empty());

    graph::apply_mint(&state, &plan, &terms).unwrap();
    let after = graph::entities_by_symbol(&state, &terms).unwrap();
    // Same-named methods in one file are told apart by logical path.
    assert_eq!(
        after[&new_thing.normalized_symbol],
        entities[&old_thing.normalized_symbol]
    );
    assert_eq!(
        after[&new_other.normalized_symbol],
        entities[&old_other.normalized_symbol]
    );
}

#[test]
fn remap_that_finds_nothing_says_so() {
    let state = bootstrap("rehome-remap-miss");
    let old = def_in("app", "rdf/x/gone().", "src/rdf/x.rs", "gone");
    let entity = mint_all(&state, std::slice::from_ref(&old))[&old.normalized_symbol].clone();
    let remap = graph::PathRemap::parse("src/rdf=crates/rdf/src").unwrap();
    let plan = plan_with(&state, &[], &[remap]);
    assert_eq!(plan.orphaned.len(), 1);
    assert_eq!(plan.unresolved_rehomes.len(), 1);
    assert_eq!(plan.unresolved_rehomes[0].iri, entity);
    assert!(plan.unresolved_rehomes[0]
        .reason
        .contains("crates/rdf/src/x.rs"));
}

#[test]
fn enrichment_inverses_are_not_reported_as_recorded_knowledge() {
    let state = bootstrap("rehome-enriched");
    seed_component(&state, "core", "src/");
    let old = def_in("app", "util/gone().", "src/util.rs", "gone");
    let entity = mint_all(&state, std::slice::from_ref(&old))[&old.normalized_symbol].clone();
    // Materializes `isRealizedBy` from the component back to the entity.
    state.ensure_enriched();

    let plan = plan_with(&state, &[], &[]);
    assert_eq!(plan.orphaned.len(), 1);
    assert!(
        !plan.orphan_links.contains_key(&entity),
        "a component realization is not recorded knowledge: {:?}",
        plan.orphan_links
    );
}

/// Insert one project-graph object edge, bypassing every write-path check.
fn insert_edge(state: &AppState, subject: &str, predicate: &str, object: &str) {
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

fn is_realized_by(state: &AppState) -> String {
    state.resolve_object_property("isRealizedBy").unwrap()
}

/// The components `entity` realizes (forward orientation), sorted.
fn realized(state: &AppState, entity: &str) -> Vec<String> {
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap();
    let mut out = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(entity).unwrap().into()),
            Some(NamedNodeRef::new(&terms.realizes).unwrap()),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::NamedNode(node) => Some(node.into_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    out.sort();
    out
}

/// Any `realizes` / `isRealizedBy` quad between the two, in any orientation.
fn linked_either_way(state: &AppState, entity: &str, component: &str) -> bool {
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let inverse = is_realized_by(state);
    [terms.realizes.as_str(), inverse.as_str()]
        .iter()
        .any(|p| has_edge(state, entity, p, component) || has_edge(state, component, p, entity))
}

fn declare(state: &AppState, component: &str, path: &str) {
    graph::declare_component_paths(state, component, &[path.to_string()]).unwrap();
}

fn undeclare(state: &AppState, component: &str, path: &str) {
    graph::remove_component_paths(state, component, &[path.to_string()]).unwrap();
}

fn retire(state: &AppState, component: &str) {
    graph::retire_component(
        state,
        component,
        "Moved out of this repository.",
        None,
        false,
        "tester",
        Utc::now(),
    )
    .unwrap();
}

fn named(iri: &str, name: &str) -> (String, String) {
    (iri.to_string(), name.to_string())
}

#[test]
fn moved_coverage_repoints_realizes_and_replanning_is_a_no_op() {
    let state = bootstrap("realizes-moved");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "ingest", "src/");
    let b = seed_component(&state, "pipeline", "lib/");
    let entry = def_in("app", "runtime/serve().", "src/runtime.rs", "serve");
    let defs = std::slice::from_ref(&entry);
    let entity = mint_all(&state, defs)[&entry.normalized_symbol].clone();
    assert_eq!(realized(&state, &entity), vec![a.clone()]);

    undeclare(&state, &a, "src/");
    declare(&state, &b, "src/");
    let plan = plan_with(&state, defs, &[]);
    assert_eq!(
        plan.realizes_changes,
        vec![graph::RealizesChange {
            entity_iri: entity.clone(),
            symbol: entry.normalized_symbol.clone(),
            file: "src/runtime.rs".to_string(),
            from: vec![named(&a, "ingest")],
            to: Some(named(&b, "pipeline")),
        }]
    );
    assert!(
        plan.update.is_empty(),
        "a link-only change is not an update"
    );
    assert_eq!(plan.unchanged, 0, "nor is it unchanged");

    let outcome = graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!((outcome.realizes_moved, outcome.realizes_dropped), (1, 0));
    assert_eq!(outcome.updated, 0);
    assert_eq!(realized(&state, &entity), vec![b]);

    let again = plan_with(&state, defs, &[]);
    assert!(again.realizes_changes.is_empty(), "{again:#?}");
    assert!(again.update.is_empty() && again.create.is_empty());
    assert_eq!(again.unchanged, 1);
    state.ensure_enriched();
    assert!(validation::validate_project(&state).unwrap().conforms());
}

#[test]
fn a_more_specific_component_takes_over_only_the_files_under_it() {
    let state = bootstrap("realizes-specific");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let core = seed_component(&state, "core", "src/");
    let store = def_in("app", "graph/store/open().", "src/graph/store.rs", "open");
    let runtime = def_in("app", "runtime/serve().", "src/runtime.rs", "serve");
    let defs = [store.clone(), runtime.clone()];
    let entities = mint_all(&state, &defs);
    let (store_iri, runtime_iri) = (
        entities[&store.normalized_symbol].clone(),
        entities[&runtime.normalized_symbol].clone(),
    );

    let graph_layer = seed_component(&state, "graph layer", "src/graph/");
    let plan = plan_with(&state, &defs, &[]);
    assert_eq!(plan.realizes_changes.len(), 1, "{plan:#?}");
    assert_eq!(plan.realizes_changes[0].entity_iri, store_iri);
    assert_eq!(plan.realizes_changes[0].from, vec![named(&core, "core")]);
    assert_eq!(
        plan.realizes_changes[0].to,
        Some(named(&graph_layer, "graph layer"))
    );
    assert_eq!(plan.unchanged, 1, "the runtime entity stays with core");

    graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!(realized(&state, &store_iri), vec![graph_layer]);
    assert_eq!(realized(&state, &runtime_iri), vec![core]);
    let again = plan_with(&state, &defs, &[]);
    assert!(again.realizes_changes.is_empty());
    assert_eq!(again.unchanged, 2);
}

/// Plain retirement: an entity whose file an enclosing component still covers
/// falls back to it; one nothing else covers loses its link. Also the report.
#[test]
fn retired_component_links_fall_back_to_an_enclosing_component_or_are_dropped() {
    let state = bootstrap("realizes-retired");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let outer = seed_component(&state, "outer", "src/");
    let inner = seed_component(&state, "inner", "src/graph/");
    let solo = seed_component(&state, "solo", "tools/");
    let store = def_in("app", "graph/store/open().", "src/graph/store.rs", "open");
    let gen = def_in("app", "gen/run().", "tools/gen.rs", "run");
    let defs = [store.clone(), gen.clone()];
    let entities = mint_all(&state, &defs);
    let (store_iri, gen_iri) = (
        entities[&store.normalized_symbol].clone(),
        entities[&gen.normalized_symbol].clone(),
    );
    assert_eq!(realized(&state, &store_iri), vec![inner.clone()]);
    assert_eq!(realized(&state, &gen_iri), vec![solo.clone()]);

    retire(&state, &inner);
    retire(&state, &solo);
    let plan = plan_with(&state, &defs, &[]);
    let by_entity = plan
        .realizes_changes
        .iter()
        .map(|change| (change.entity_iri.as_str(), change))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(by_entity.len(), 2, "{plan:#?}");
    // A retired target still prints by name.
    assert_eq!(
        by_entity[store_iri.as_str()].from,
        vec![named(&inner, "inner")]
    );
    assert_eq!(
        by_entity[store_iri.as_str()].to,
        Some(named(&outer, "outer"))
    );
    assert_eq!(by_entity[gen_iri.as_str()].from, vec![named(&solo, "solo")]);
    assert_eq!(by_entity[gen_iri.as_str()].to, None);
    assert!(plan.unmapped_paths.contains("tools/gen.rs"));

    // The report: one group per `from -> to`, each entity listed.
    assert_eq!(
        plan.summary_line(),
        "summary: create=0 update=0 unchanged=0 rehome=0 not-rehomed=0 skipped-scope=0 \
         skipped-tests=0 collisions=0 unmapped=1 orphaned=0 orphaned-with-knowledge=0 \
         out-of-scope=0 realizes-moved=1 realizes-dropped=1"
    );
    assert_eq!(
        plan.realizes_report(),
        vec![
            "realizes links that no longer match component coverage:".to_string(),
            "  inner -> outer: 1 entity".to_string(),
            format!(
                "    {} (src/graph/store.rs) {store_iri}",
                store.normalized_symbol
            ),
            "  solo -> (none: no current component covers the file): 1 entity".to_string(),
            format!("    {} (tools/gen.rs) {gen_iri}", gen.normalized_symbol),
        ]
    );

    let outcome = graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!((outcome.realizes_moved, outcome.realizes_dropped), (1, 1));
    assert_eq!(realized(&state, &store_iri), vec![outer]);
    assert!(realized(&state, &gen_iri).is_empty());
    let again = plan_with(&state, &defs, &[]);
    assert!(again.realizes_changes.is_empty() && again.update.is_empty());
    assert_eq!(again.unchanged, 2);
    assert!(again.realizes_report().is_empty());
    assert!(again
        .summary_line()
        .ends_with(" realizes-moved=0 realizes-dropped=0"));
}

#[test]
fn removed_coverage_with_nothing_else_covering_drops_the_link() {
    let state = bootstrap("realizes-uncovered");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "ingest", "src/");
    let entry = def_in("app", "runtime/serve().", "src/runtime.rs", "serve");
    let defs = std::slice::from_ref(&entry);
    let entity = mint_all(&state, defs)[&entry.normalized_symbol].clone();

    undeclare(&state, &a, "src/");
    let plan = plan_with(&state, defs, &[]);
    assert_eq!(plan.realizes_changes.len(), 1);
    assert_eq!(plan.realizes_changes[0].from, vec![named(&a, "ingest")]);
    assert_eq!(plan.realizes_changes[0].to, None);
    assert_eq!((plan.realizes_moved(), plan.realizes_dropped()), (0, 1));

    graph::apply_mint(&state, &plan, &terms).unwrap();
    assert!(realized(&state, &entity).is_empty());
    let again = plan_with(&state, defs, &[]);
    assert!(again.realizes_changes.is_empty() && again.update.is_empty());
    assert_eq!(again.unchanged, 1);
}

#[test]
fn an_entity_with_two_realizes_links_ends_with_exactly_one() {
    let state = bootstrap("realizes-two");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "ingest", "src/");
    let b = seed_component(&state, "pipeline", "lib/");
    let entry = def_in("app", "runtime/serve().", "src/runtime.rs", "serve");
    let defs = std::slice::from_ref(&entry);
    let entity = mint_all(&state, defs)[&entry.normalized_symbol].clone();
    // Hand-set, disagreeing with path coverage.
    insert_edge(&state, &entity, &terms.realizes, &b);

    let plan = plan_with(&state, defs, &[]);
    assert_eq!(plan.realizes_changes.len(), 1, "{plan:#?}");
    let mut from = plan.realizes_changes[0].from.clone();
    from.sort();
    let mut expected = vec![named(&a, "ingest"), named(&b, "pipeline")];
    expected.sort();
    assert_eq!(from, expected);
    assert_eq!(plan.realizes_changes[0].to, Some(named(&a, "ingest")));

    graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!(realized(&state, &entity), vec![a]);
    assert!(plan_with(&state, defs, &[]).realizes_changes.is_empty());
}

/// Out-of-scope entities are never created or updated by mint, but a stale
/// link on one is re-pointed. One minted without a link does not gain one.
#[test]
fn a_lazily_minted_entity_with_a_stale_link_is_repointed() {
    let state = bootstrap("realizes-lazy");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "ingest", "src/");
    let b = seed_component(&state, "pipeline", "lib/");
    let private = DefinitionEntry {
        is_public: false,
        ..def_in("app", "runtime/helper().", "src/runtime.rs", "helper")
    };
    let bare = DefinitionEntry {
        is_public: false,
        ..def_in("app", "gen/helper().", "tools/gen.rs", "helper")
    };
    let components = graph::load_components(&state).unwrap();
    // What `link_code` does for a private definition.
    let linked = graph::ensure_entity(&state, &terms, &components, &private, "tester")
        .unwrap()
        .iri;
    let unlinked = graph::ensure_entity(&state, &terms, &components, &bare, "tester")
        .unwrap()
        .iri;
    assert_eq!(realized(&state, &linked), vec![a.clone()]);
    assert!(realized(&state, &unlinked).is_empty());

    undeclare(&state, &a, "src/");
    declare(&state, &b, "src/");
    declare(&state, &b, "tools/");
    let defs = [private.clone(), bare];
    let plan = plan_with(&state, &defs, &[]);
    assert_eq!(plan.out_of_scope.len(), 2);
    assert!(plan.create.is_empty() && plan.update.is_empty());
    assert_eq!(
        plan.realizes_changes,
        vec![graph::RealizesChange {
            entity_iri: linked.clone(),
            symbol: private.normalized_symbol.clone(),
            file: "src/runtime.rs".to_string(),
            from: vec![named(&a, "ingest")],
            to: Some(named(&b, "pipeline")),
        }]
    );

    graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!(realized(&state, &linked), vec![b]);
    assert!(realized(&state, &unlinked).is_empty());
    assert!(plan_with(&state, &defs, &[]).realizes_changes.is_empty());
}

/// A stale link held by a stored inverse, or materialized by the reasoner,
/// must not survive the re-point in either orientation, nor reach `kg.nq`.
#[test]
fn repointing_removes_stored_inverses_and_reasoner_copies() {
    let state = bootstrap("realizes-inverses");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "ingest", "src/");
    let b = seed_component(&state, "pipeline", "lib/");
    let legacy = seed_component(&state, "legacy", "old/");
    let entry = def_in("app", "runtime/serve().", "src/runtime.rs", "serve");
    let defs = std::slice::from_ref(&entry);
    let entity = mint_all(&state, defs)[&entry.normalized_symbol].clone();
    let inverse = is_realized_by(&state);
    // Both orientations stored for `a`; only the inverse for `legacy`.
    insert_edge(&state, &a, &inverse, &entity);
    insert_edge(&state, &legacy, &inverse, &entity);
    state.ensure_enriched();
    assert!(
        has_edge(&state, &entity, &terms.realizes, &legacy),
        "the reasoner materializes the forward edge from the stored inverse"
    );

    undeclare(&state, &a, "src/");
    declare(&state, &b, "src/");
    let plan = plan_with(&state, defs, &[]);
    assert_eq!(plan.realizes_changes.len(), 1, "{plan:#?}");
    let mut from = plan.realizes_changes[0].from.clone();
    from.sort();
    let mut expected = vec![named(&a, "ingest"), named(&legacy, "legacy")];
    expected.sort();
    assert_eq!(from, expected);

    graph::apply_mint(&state, &plan, &terms).unwrap();
    // Gone in the apply itself, reasoner copies and their tags included, not
    // only after the next enrichment.
    for old in [&a, &legacy] {
        assert!(
            !linked_either_way(&state, &entity, old),
            "linked to {old} before enrichment"
        );
    }
    let tagged =
        moosedev::provenance::reasoner_inferred_data_quads(&state.store, PROJECT_KG_GRAPH_IRI)
            .unwrap();
    assert!(
        !tagged.iter().any(|quad| {
            let text = quad.to_string();
            text.contains(&entity) && (text.contains(&a) || text.contains(&legacy))
        }),
        "an inference tag of a removed link survived: {tagged:?}"
    );
    state.ensure_enriched();
    for old in [&a, &legacy] {
        assert!(!linked_either_way(&state, &entity, old), "linked to {old}");
    }
    assert_eq!(realized(&state, &entity), vec![b.clone()]);

    let text = moosedev::export::export_canonical_project(&state.store)
        .unwrap()
        .text;
    let entity_ref = format!("<{entity}>");
    let link_lines = text
        .lines()
        .filter(|line| line.contains(&entity_ref))
        .filter(|line| {
            line.contains(&format!("<{}>", terms.realizes))
                || line.contains(&format!("<{inverse}>"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        link_lines,
        vec![format!(
            "{entity_ref} <{}> <{b}> <{PROJECT_KG_GRAPH_IRI}> .",
            terms.realizes
        )]
    );
    assert!(plan_with(&state, defs, &[]).realizes_changes.is_empty());
    assert!(validation::validate_project(&state).unwrap().conforms());
}

/// A live tree-sitter fallback entity is never in `definitions`, but its link
/// goes stale like any other; one the substrate cannot verify is left alone.
#[test]
fn a_live_syntactic_entity_with_a_stale_link_is_repointed() {
    let state = bootstrap("realizes-syntactic");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let a = seed_component(&state, "fixtures", "tests/fixtures/");
    let b = seed_component(&state, "fallback code", "other/");
    let repo_root = state.data_dir.join("fixture-repo");
    let fixture_path = repo_root.join("tests/fixtures/ts_fallback.rs");
    std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture_path, include_str!("fixtures/ts_fallback.rs")).unwrap();
    let substrate = Substrate::from_index_rooted(Index::new(), substrate_meta(), false, repo_root)
        .expect("rooted substrate");
    let identity = "ts:rust:tests/fixtures/ts_fallback.rs:fn:<Widget as Render>::render";
    let entry = substrate
        .definition_for_symbol(identity)
        .expect("the fixture declaration is alive");
    let components = graph::load_components(&state).unwrap();
    let entity = graph::ensure_entity(&state, &terms, &components, &entry, "tester")
        .unwrap()
        .iri;
    assert_eq!(realized(&state, &entity), vec![a.clone()]);

    undeclare(&state, &a, "tests/fixtures/");
    declare(&state, &b, "tests/fixtures/");
    let components = graph::load_components(&state).unwrap();
    let unverifiable = graph::plan_mint(&state, &[], &terms, &components, None).unwrap();
    assert!(
        unverifiable.realizes_changes.is_empty(),
        "without a substrate the anchor cannot be verified, so it is left alone"
    );
    let plan = graph::plan_mint(&state, &[], &terms, &components, Some(&substrate)).unwrap();
    assert!(plan.orphaned.is_empty());
    assert_eq!(
        plan.realizes_changes,
        vec![graph::RealizesChange {
            entity_iri: entity.clone(),
            symbol: identity.to_string(),
            file: "tests/fixtures/ts_fallback.rs".to_string(),
            from: vec![named(&a, "fixtures")],
            to: Some(named(&b, "fallback code")),
        }]
    );

    graph::apply_mint(&state, &plan, &terms).unwrap();
    assert_eq!(realized(&state, &entity), vec![b]);
    let again = graph::plan_mint(&state, &[], &terms, &components, Some(&substrate)).unwrap();
    assert!(again.realizes_changes.is_empty());
}
