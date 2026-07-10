//! CodeEntity dossier integration tests.
//!
//! The harness mirrors `tests/link_code.rs`: each test gets an isolated
//! AppState, a tiny SCIP substrate, and synthetic records linked through the
//! public graph APIs.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use moosedev::code::substrate::{symbols, DefinitionEntry, Substrate, SubstrateMeta};
use moosedev::graph::{
    self, AppState, CodeSelector, DossierTarget, RecordInput, SupersedeInput, PROJECT_KG_GRAPH_IRI,
};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

const COVERS_PATH: &str = "https://trivyn.io/ontologies/software/architecture#coversPath";
const MODULE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/";
const PUBLIC_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
const PRIVATE_SYMBOL: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/private_helper().";
const LOCAL_SYMBOL: &str = "local 0";

/// Create an isolated graph state so dossier tests never touch the repo store.
fn bootstrap(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-dossier-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state")
}

/// Bootstrap an isolated state with the synthetic substrate and component map.
fn state_with_substrate(name: &str) -> AppState {
    let state = bootstrap(name);
    state.set_substrate(Arc::new(synthetic_substrate(false)));
    seed_component(&state, "runtime component", "src/");
    state
}

/// Record a minimal typed knowledge node with title and accepted status.
fn record(state: &AppState, kind: &str, title: &str) -> String {
    record_with_description(state, kind, title, None)
}

fn record_with_description(
    state: &AppState,
    kind: &str,
    title: &str,
    description: Option<&str>,
) -> String {
    let class_iri = state.resolve_class(kind).expect("known class");
    let mut properties = vec![
        (moose::RDFS_LABEL.to_string(), title.to_string()),
        (state.capture.title.clone(), title.to_string()),
        (state.capture.status.clone(), "accepted".to_string()),
    ];
    if let Some(description) = description {
        properties.push((state.capture.description.clone(), description.to_string()));
    }
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: kind.to_string(),
            properties,
        },
        "tester",
        Utc::now(),
    )
    .expect("record item")
}

/// Seed a SystemComponent plus a coversPath literal for CodeEntity realization.
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

/// Selector for the synthetic public `build_server` definition.
fn public_position() -> DossierTarget {
    DossierTarget::Position {
        file: "src/runtime.rs".to_string(),
        line: 8,
        col: 5,
    }
}

/// Link a record to the synthetic public function and return the entity IRI.
fn link_public(state: &AppState, record_iri: &str, predicate: &str) -> String {
    graph::link_code(
        state,
        record_iri,
        predicate,
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

/// Mint the synthetic public function ahead of dossier reads that must not write.
fn pre_mint_public(state: &AppState) -> String {
    let substrate = state.substrate().expect("substrate");
    let terms = graph::CodeTerms::resolve(state).unwrap();
    let components = graph::load_components(state).unwrap();
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(state, &definitions, &terms, &components).unwrap();
    graph::apply_mint(state, &plan, &terms).unwrap();
    graph::entities_by_symbol(state, &terms).unwrap()[&normalize(PUBLIC_SYMBOL)].clone()
}

/// Build a DefinitionEntry without loading a substrate, for no-substrate reads.
fn def(descriptor: &str, file: &str, display_name: &str) -> DefinitionEntry {
    let symbol = format!("rust-analyzer cargo moosedev 0.6.3 {descriptor}");
    DefinitionEntry {
        normalized_symbol: symbols::normalize_symbol(&symbol).expect("valid scip symbol"),
        symbol,
        display_name: Some(display_name.to_string()),
        kind: Some("Function".to_string()),
        signature: Some(format!("pub fn {display_name}()")),
        file: file.to_string(),
        is_module: false,
        is_public: true,
    }
}

/// Normalize a SCIP symbol using the production helper under test.
fn normalize(symbol: &str) -> String {
    symbols::normalize_symbol(symbol).expect("normalizable symbol")
}

/// Direct links of different record classes are ordered deterministically and
/// rendered in Markdown with entity metadata.
#[test]
fn full_dossier_ordering_and_markdown() {
    let state = state_with_substrate("full");
    let decision = record(&state, "ArchitecturalDecision", "Runtime builder decision");
    let constraint = record(&state, "Constraint", "Runtime builder constraint");
    let requirement = record(&state, "Requirement", "Runtime builder requirement");
    link_public(&state, &decision, "concerns");
    link_public(&state, &constraint, "concerns");
    link_public(&state, &requirement, "satisfies");

    let dossier = graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .expect("dossier");

    assert_eq!(dossier.direct_records.len(), 3);
    assert_eq!(dossier.direct_records[0].kind, "Constraint");
    assert_eq!(dossier.direct_records[1].kind, "ArchitecturalDecision");
    assert_eq!(dossier.direct_records[2].kind, "Requirement");
    assert_eq!(dossier.direct_records[2].predicate_local, "satisfies");
    assert_eq!(dossier.kind, "Function");
    assert_eq!(dossier.display_name, "build_server");
    assert_eq!(dossier.defined_in.as_deref(), Some("src/runtime.rs"));

    let markdown = graph::render_markdown(&dossier);
    assert!(markdown.contains("build_server"));
    assert!(markdown.contains("Runtime builder decision"));
    assert!(markdown.contains("Runtime builder constraint"));
    assert!(markdown.contains("Runtime builder requirement"));
    assert!(markdown.contains("[Constraint]"));
    assert!(markdown.contains("[ArchitecturalDecision]"));
    assert!(markdown.contains("[Requirement]"));
    assert!(markdown.contains("- [Constraint] Runtime builder constraint -"));
    assert!(!markdown.contains("]("));
}

#[test]
fn record_summaries_include_description_and_workbench_link_when_published() {
    let state = state_with_substrate("workbench-link");
    let constraint = record_with_description(
        &state,
        "Constraint",
        "Workbench constraint",
        Some("Keep this record addressable."),
    );
    std::fs::write(state.data_dir.join("http.addr"), "127.0.0.1:7474\n")
        .expect("write published HTTP address");
    link_public(&state, &constraint, "constrains");

    let dossier = graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .expect("dossier");
    let record = dossier.direct_records.first().expect("constraint summary");
    let expected_url = format!(
        "http://127.0.0.1:7474/#/constraints/{}",
        record.iri.rsplit('/').next().expect("record local name")
    );

    assert_eq!(
        record.description.as_deref(),
        Some("Keep this record addressable.")
    );
    assert_eq!(record.workbench_url.as_deref(), Some(expected_url.as_str()));
    assert!(graph::render_markdown(&dossier)
        .contains(&format!("[Workbench constraint]({expected_url})")));
}

#[test]
fn workbench_links_use_typed_uuid_routes_with_a_generic_fallback() {
    let state = state_with_substrate("typed-workbench-links");
    std::fs::write(state.data_dir.join("http.addr"), "127.0.0.1:7474\n")
        .expect("write published HTTP address");
    let cases = [
        ("ArchitecturalDecision", "adrs"),
        ("Requirement", "requirements"),
        ("Lesson", "lessons"),
        ("Constraint", "constraints"),
        ("Pattern", "record"),
    ];

    for (kind, _) in cases {
        let iri = record(&state, kind, &format!("{kind} link"));
        link_public(&state, &iri, "concerns");
    }

    let dossier = graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .expect("dossier");
    for (kind, route) in cases {
        let summary = dossier
            .direct_records
            .iter()
            .find(|record| record.kind == kind)
            .expect("typed record summary");
        let uuid = summary.iri.rsplit('/').next().expect("record UUID");
        let expected = format!("http://127.0.0.1:7474/#/{route}/{uuid}");
        assert_eq!(summary.workbench_url.as_deref(), Some(expected.as_str()));
    }
}

/// A minted entity with no direct record links intentionally produces silence.
#[test]
fn zero_direct_records_is_silence() {
    let state = state_with_substrate("zero-direct");
    pre_mint_public(&state);

    let dossier = graph::get_entity_dossier(&state, &public_position()).unwrap();

    assert!(dossier.is_none());
}

/// Component-only knowledge must not create a dossier by itself.
#[test]
fn transitive_component_only_records_are_silence() {
    let state = state_with_substrate("transitive-only");
    let entity_iri = pre_mint_public(&state);
    let component_iri = graph::CodeTerms::resolve(&state)
        .ok()
        .and_then(|terms| {
            let dossier =
                graph::get_entity_dossier(&state, &DossierTarget::Iri(entity_iri.clone()))
                    .unwrap_or(None);
            dossier.and_then(|d| d.realizes.map(|r| (terms, r.0)))
        })
        .map(|(_, iri)| iri)
        .unwrap_or_else(|| {
            graph::load_components(&state).unwrap()[0]
                .iri
                .clone()
                .unwrap()
        });
    let component_record = record(&state, "ArchitecturalDecision", "Component-only decision");
    graph::relate(&state, &component_record, "concerns", &component_iri).unwrap();

    let dossier = graph::get_entity_dossier(&state, &DossierTarget::Iri(entity_iri)).unwrap();

    assert!(dossier.is_none());
}

/// Component records appear only as secondary context once a direct link exists.
#[test]
fn component_records_are_secondary() {
    let state = state_with_substrate("component-secondary");
    let entity_iri = pre_mint_public(&state);
    let component_iri = graph::load_components(&state).unwrap()[0]
        .iri
        .clone()
        .unwrap();
    let direct = record(&state, "ArchitecturalDecision", "Direct entity decision");
    let component = record(
        &state,
        "ArchitecturalDecision",
        "Component context decision",
    );
    let both = record(&state, "ArchitecturalDecision", "Both places decision");
    graph::relate(&state, &direct, "concerns", &entity_iri).unwrap();
    graph::relate(&state, &component, "concerns", &component_iri).unwrap();
    graph::relate(&state, &both, "concerns", &entity_iri).unwrap();
    graph::relate(&state, &both, "concerns", &component_iri).unwrap();

    let dossier = graph::get_entity_dossier(&state, &DossierTarget::Iri(entity_iri))
        .unwrap()
        .expect("dossier");

    assert!(dossier
        .direct_records
        .iter()
        .any(|record| record.title == "Direct entity decision"));
    assert!(dossier
        .direct_records
        .iter()
        .any(|record| record.title == "Both places decision"));
    assert!(dossier
        .component_records
        .iter()
        .any(|record| record.title == "Component context decision"));
    assert!(!dossier
        .component_records
        .iter()
        .any(|record| record.title == "Both places decision"));
    assert!(graph::render_markdown(&dossier).contains("Via component"));
}

/// Deprecated records are hidden, while superseded directly linked records remain visible.
#[test]
fn retracted_filtered_and_superseded_shown() {
    let state = state_with_substrate("lifecycle");
    let retracted = record(&state, "ArchitecturalDecision", "Retracted entity decision");
    let superseded = record(
        &state,
        "ArchitecturalDecision",
        "Superseded entity decision",
    );
    link_public(&state, &retracted, "concerns");
    link_public(&state, &superseded, "concerns");
    graph::retract_decision(&state, &retracted, "withdrawn", "tester", Utc::now()).unwrap();
    graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: superseded.clone(),
            new: RecordInput {
                class_iri: String::new(),
                class_local: String::new(),
                properties: vec![
                    (
                        moose::RDFS_LABEL.to_string(),
                        "Successor decision".to_string(),
                    ),
                    (
                        state.capture.title.clone(),
                        "Successor decision".to_string(),
                    ),
                ],
            },
            rationale: "changed".to_string(),
        },
        "tester",
        Utc::now(),
    )
    .unwrap();

    let dossier = graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .expect("dossier");

    assert!(!dossier
        .direct_records
        .iter()
        .any(|record| record.title == "Retracted entity decision"));
    assert!(dossier.direct_records.iter().any(|record| {
        record.title == "Superseded entity decision" && record.status == "superseded"
    }));
    assert!(!dossier
        .direct_records
        .iter()
        .any(|record| record.title == "Successor decision"));
}

/// Position, normalized symbol, and IRI selectors resolve to the same dossier.
#[test]
fn selector_agreement() {
    let state = state_with_substrate("selectors");
    let decision = record(&state, "ArchitecturalDecision", "Selector decision");
    let entity_iri = link_public(&state, &decision, "concerns");

    let by_position = graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .expect("position");
    let by_symbol =
        graph::get_entity_dossier(&state, &DossierTarget::Symbol(normalize(PUBLIC_SYMBOL)))
            .unwrap()
            .expect("symbol");
    let by_iri = graph::get_entity_dossier(&state, &DossierTarget::Iri(entity_iri))
        .unwrap()
        .expect("iri");

    assert_eq!(by_position.entity_iri, by_symbol.entity_iri);
    assert_eq!(by_position.entity_iri, by_iri.entity_iri);
    assert_eq!(by_position.direct_records, by_symbol.direct_records);
    assert_eq!(by_position.direct_records, by_iri.direct_records);
}

/// Position reads degrade to silence without a substrate, but symbol and IRI
/// reads still work because minted entities are already in the KG.
#[test]
fn no_substrate_position_silent_but_symbol_and_iri_work() {
    let state = bootstrap("no-substrate");
    let terms = graph::CodeTerms::resolve(&state).unwrap();
    let entry = def("runtime/build_server().", "src/runtime.rs", "build_server");
    let plan = graph::MintPlan {
        create: vec![graph::PlannedEntity {
            entry: entry.clone(),
            iri: None,
            realizes: None,
        }],
        ..graph::MintPlan::default()
    };
    graph::apply_mint(&state, &plan, &terms).unwrap();
    let entity_iri =
        graph::entities_by_symbol(&state, &terms).unwrap()[&entry.normalized_symbol].clone();
    let decision = record(&state, "ArchitecturalDecision", "No substrate decision");
    graph::relate(&state, &decision, "concerns", &entity_iri).unwrap();

    assert!(graph::get_entity_dossier(&state, &public_position())
        .unwrap()
        .is_none());
    assert!(
        graph::get_entity_dossier(&state, &DossierTarget::Symbol(entry.normalized_symbol),)
            .unwrap()
            .is_some()
    );
    assert!(
        graph::get_entity_dossier(&state, &DossierTarget::Iri(entity_iri))
            .unwrap()
            .is_some()
    );
}

/// Non-semantic source positions return silence instead of errors or empty dossiers.
#[test]
fn whitespace_and_local_positions_are_silence() {
    let state = state_with_substrate("misses");
    let decision = record(&state, "ArchitecturalDecision", "Visible direct link");
    link_public(&state, &decision, "concerns");

    assert!(graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line: 2,
            col: 1,
        },
    )
    .unwrap()
    .is_none());
    assert!(graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line: 21,
            col: 9,
        },
    )
    .unwrap()
    .is_none());
}

/// Whole-file Module definitions are mintable by symbol but not resolved as
/// source tokens for comment/blank positions inside the file.
#[test]
fn whole_file_module_dossier_does_not_surface_at_tokenless_position() {
    let state = state_with_substrate("whole-file-module");
    let decision = record(&state, "ArchitecturalDecision", "Module-level decision");
    graph::link_code(
        &state,
        &decision,
        "concerns",
        &CodeSelector::Symbol(MODULE_SYMBOL.to_string()),
        "tester",
    )
    .expect("link module by symbol");

    assert!(graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line: 2,
            col: 1,
        },
    )
    .unwrap()
    .is_none());
}

/// When the real repo substrate exists, verify the position selector against an
/// actual source definition while still writing only to an isolated temp state.
#[test]
fn repo_substrate_dossier_by_position_when_index_present() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let data_dir = repo_root.join(".moosedev");
    let meta_path = data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping dossier substrate integration: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }

    let substrate = Substrate::load(&data_dir, repo_root)?;
    let state = bootstrap("repo-substrate");
    state.set_substrate(Arc::new(substrate));
    seed_component(&state, "source component", "src/");
    let scratch = record(
        &state,
        "ArchitecturalDecision",
        "Scratch build_server dossier",
    );
    let runtime = std::fs::read_to_string(repo_root.join("src/runtime.rs"))?;
    let (line, col) = runtime
        .lines()
        .enumerate()
        .find_map(|(idx, line)| {
            line.find("fn build_server")
                .map(|offset| (idx as u32 + 1, offset as u32 + 4))
        })
        .expect("build_server definition");
    graph::link_code(
        &state,
        &scratch,
        "concerns",
        &CodeSelector::Position {
            file: "src/runtime.rs".to_string(),
            line,
            col,
        },
        "tester",
    )?;

    let dossier = graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line,
            col,
        },
    )?
    .expect("repo dossier");
    assert!(dossier
        .direct_records
        .iter()
        .any(|record| record.title == "Scratch build_server dossier"));

    let (comment_line, comment_col) = runtime
        .lines()
        .enumerate()
        .find_map(|(idx, line)| {
            line.find("//")
                .map(|offset| (idx as u32 + 1, offset as u32 + 1))
        })
        .expect("comment line");
    assert!(graph::get_entity_dossier(
        &state,
        &DossierTarget::Position {
            file: "src/runtime.rs".to_string(),
            line: comment_line,
            col: comment_col,
        },
    )?
    .is_none());
    Ok(())
}

/// Synthetic SCIP index with one public definition, one private definition, and one local.
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

/// Build one SCIP document with UTF-8 column semantics.
fn doc(relative_path: &str) -> Document {
    let mut document = Document::new();
    document.relative_path = relative_path.to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    document
}

/// Build one SCIP occurrence range.
fn occ(symbol: &str, range: Vec<i32>, symbol_roles: i32) -> Occurrence {
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.to_string();
    occurrence.range = range;
    occurrence.symbol_roles = symbol_roles;
    occurrence.enclosing_range = vec![0, 0, 30, 0];
    occurrence
}

/// Build one SCIP symbol record with signature documentation.
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

/// Deterministic substrate metadata for synthetic indexes.
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
