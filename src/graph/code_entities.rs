//! CodeEntity mint planning and write helpers.
//!
//! The code substrate is regenerated freely; this module mints only the durable
//! CodeEntity continuants keyed by normalized SCIP symbol.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};

pub(crate) use crate::code::substrate::is_test_path;
use crate::code::substrate::{symbols, DefinitionEntry, Substrate};
use crate::provenance;

use super::components::{best_component_for_path, ComponentEntry};
use super::state::AppState;
use super::util::mint_instance_iri;
use super::PROJECT_KG_GRAPH_IRI;

/// Resolved code-ontology term IRIs, looked up once, fail-fast.
#[derive(Debug, Clone)]
pub struct CodeTerms {
    pub code_entity_class: String,
    pub has_substrate_symbol: String,
    pub has_entity_kind: String,
    pub has_code_name: String,
    pub has_logical_path: String,
    pub defined_in_path: String,
    pub realizes: String,
}

impl CodeTerms {
    /// Resolve every code-layer term the mint engine reads or writes.
    pub fn resolve(state: &AppState) -> anyhow::Result<Self> {
        Ok(Self {
            code_entity_class: state.resolve_code_class("CodeEntity")?,
            has_substrate_symbol: state.resolve_code_datatype_property("hasSubstrateSymbol")?,
            has_entity_kind: state.resolve_code_datatype_property("hasEntityKind")?,
            has_code_name: state.resolve_code_datatype_property("hasCodeName")?,
            has_logical_path: state.resolve_code_datatype_property("hasLogicalPath")?,
            defined_in_path: state.resolve_code_datatype_property("definedInPath")?,
            realizes: state.resolve_object_property("realizes")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEntity {
    pub entry: DefinitionEntry,
    /// Some = existing entity (updates), None = create.
    pub iri: Option<String>,
    /// Component IRI to link, when applicable.
    pub realizes: Option<String>,
}

impl PlannedEntity {
    /// Entity kind text that will be written if this plan is applied.
    pub fn display_kind(&self) -> String {
        desired_entity_kind(&self.entry)
    }

    /// Human-readable entity name that will be written if this plan is applied.
    pub fn display_name(&self) -> String {
        desired_name(&self.entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolCollision {
    pub normalized_symbol: String,
    pub kept_file: String,
    /// Files for dropped colliding entries.
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintPlan {
    pub create: Vec<PlannedEntity>,
    pub update: Vec<PlannedEntity>,
    pub unchanged: usize,
    pub skipped_scope: usize,
    pub skipped_tests: usize,
    pub collisions: Vec<SymbolCollision>,
    pub unmapped_paths: BTreeSet<String>,
    /// (iri, normalized_symbol) in KG but gone from the substrate's workspace
    /// definitions. Out-of-scope (lazily minted private) entities are NOT
    /// orphans while their symbol still exists. Report only.
    pub orphaned: Vec<(String, String)>,
    /// (iri, normalized_symbol) minted entities whose symbol is STILL a live
    /// workspace definition but which batch minting does not manage: they fall
    /// outside the scope rule. Not orphans (the symbol exists) and not
    /// `unchanged` (they were never planned), so without this bucket they are
    /// invisible in every line of the report.
    ///
    /// Deliberately includes legitimately lazy-minted private entities — the
    /// plan cannot tell those apart from entities a scope NARROWING left
    /// behind, and pretending otherwise would be a guess. Report only;
    /// deciding which are prunable needs evidence this plan does not hold.
    pub out_of_scope: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// (normalized_symbol, iri)
    pub created: Vec<(String, String)>,
    pub updated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredEntity {
    pub iri: String,
    pub created: bool,
}

/// Shown when a store has an indexed substrate but nothing minted from it.
/// Shared by the daemon log, the MCP dossier, and LSP hover so the three
/// surfaces cannot drift apart on the same diagnosis.
pub const UNMINTED_STORE_HINT: &str = "The code substrate is indexed and contains definitions \
    batch minting would cover, but this store has no CodeEntity records yet — so dossiers, hover, \
    and code lenses are empty everywhere, not just here. `moosedev index` runs automatically (git \
    hooks, editor saves); `mint` does not. Stop the backend and run `moosedev mint --apply` to \
    create them.";

/// True when this store looks like one where `mint` has never run: the substrate
/// offers definitions batch minting would cover, and the graph has none of them.
///
/// BOTH halves matter. Zero entities alone is also the honest state of a project
/// whose entire surface is private or test-only, where `mint --apply` is a no-op
/// and lazy `link_code` is the real path — telling that project to run mint is
/// advice that can never work.
pub fn looks_unminted(state: &AppState, terms: &CodeTerms) -> anyhow::Result<bool> {
    let Some(substrate) = state.substrate() else {
        return Ok(false);
    };
    if has_any_code_entities(state, terms)? {
        return Ok(false);
    }
    Ok(has_mintable_definitions(&substrate.definitions()))
}

/// True when the project graph holds at least one minted CodeEntity.
///
/// A store with a healthy substrate but zero entities means `mint` has never run
/// — every dossier, hover, and code lens is then silently empty with no hint why
/// (Requirement a0581252). Deliberately an existence probe rather than
/// [`entities_by_symbol`], which materializes every entity into a map and is
/// already on the per-hover path.
pub fn has_any_code_entities(state: &AppState, terms: &CodeTerms) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    Ok(state
        .store
        .quads_for_pattern(
            None,
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

/// Scan CodeEntity `hasSubstrateSymbol` literals in the project graph and return
/// normalized SCIP symbol -> subject IRI.
pub fn entities_by_symbol(
    state: &AppState,
    terms: &CodeTerms,
) -> anyhow::Result<BTreeMap<String, String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    let mut out = BTreeMap::new();
    for q in state.store.quads_for_pattern(
        None,
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let subject = match q.subject {
            oxigraph::model::NamedOrBlankNode::NamedNode(node) => node.as_str().to_string(),
            oxigraph::model::NamedOrBlankNode::BlankNode(_) => continue,
        };
        let Term::Literal(literal) = q.object else {
            continue;
        };
        let symbol = literal.value().to_string();
        out.entry(symbol)
            .and_modify(|existing: &mut String| {
                if subject < *existing {
                    *existing = subject.clone();
                }
            })
            .or_insert(subject);
    }
    Ok(out)
}

/// Build an idempotent mint plan from substrate definitions and component coverage.
pub fn plan_mint(
    state: &AppState,
    definitions: &[DefinitionEntry],
    terms: &CodeTerms,
    components: &[ComponentEntry],
    substrate: Option<&Substrate>,
) -> anyhow::Result<MintPlan> {
    let (candidates, skipped_scope, skipped_tests) = mint_candidates(definitions);
    let (kept, collisions) = dedupe_collisions(candidates);
    // Snapshotted from the same `kept` minting itself walks, so the scope test
    // below cannot drift from the scope actually applied.
    let in_scope = kept
        .iter()
        .map(|entry| entry.normalized_symbol.clone())
        .collect::<BTreeSet<_>>();
    let existing = entities_by_symbol(state, terms)?;
    // Orphans are judged against ALL workspace definitions, not the mint scope:
    // lazily minted private entities are alive in the substrate and must not be
    // reported as gone.
    let substrate_symbols = definitions
        .iter()
        .map(|entry| entry.normalized_symbol.clone())
        .collect::<BTreeSet<_>>();

    let mut plan = MintPlan {
        skipped_scope,
        skipped_tests,
        collisions,
        ..MintPlan::default()
    };

    for entry in kept {
        let mapped_iri = component_iri_for_path(&entry.file, components).map(str::to_string);
        if mapped_iri.is_none() {
            plan.unmapped_paths.insert(entry.file.clone());
        }

        match existing.get(&entry.normalized_symbol) {
            None => plan.create.push(PlannedEntity {
                entry,
                iri: None,
                realizes: mapped_iri,
            }),
            Some(iri) => {
                let needs_literal_update = literals_differ(state, terms, iri, &entry)?;
                let realizes = if has_realizes(state, terms, iri)? {
                    None
                } else {
                    mapped_iri
                };
                if needs_literal_update || realizes.is_some() {
                    plan.update.push(PlannedEntity {
                        entry,
                        iri: Some(iri.clone()),
                        realizes,
                    });
                } else {
                    plan.unchanged += 1;
                }
            }
        }
    }

    for (symbol, iri) in existing {
        if substrate_symbols.contains(&symbol) {
            if !in_scope.contains(&symbol) {
                plan.out_of_scope.push((iri, symbol));
            }
            continue;
        }
        if symbol.starts_with("ts:") {
            // Syntactic anchors are orphaned only when the substrate can
            // positively prove their declaration or file is gone.
            if substrate
                .and_then(|substrate| substrate.identity_alive(&symbol))
                .is_some_and(|alive| !alive)
            {
                plan.orphaned.push((iri, symbol));
            }
            continue;
        }
        plan.orphaned.push((iri, symbol));
    }
    Ok(plan)
}

/// Apply a mint plan in one transaction, then best-effort stamp provenance.
pub fn apply_mint(
    state: &AppState,
    plan: &MintPlan,
    terms: &CodeTerms,
) -> anyhow::Result<ApplyOutcome> {
    let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI)?;
    let mut created = Vec::new();
    let mut created_iris = Vec::new();
    let mut updated = 0usize;
    let mut inserts = Vec::new();
    let mut removes = Vec::new();

    for planned in &plan.create {
        let iri = mint_instance_iri("CodeEntity");
        inserts.extend(create_quads(
            terms,
            &iri,
            &planned.entry,
            planned.realizes.as_deref(),
        )?);
        created.push((planned.entry.normalized_symbol.clone(), iri.clone()));
        created_iris.push(iri);
    }

    for planned in &plan.update {
        let Some(iri) = planned.iri.as_deref() else {
            continue;
        };
        let subject = NamedNode::new(iri)?;
        for (predicate_iri, desired) in desired_literal_values(terms, &planned.entry) {
            if stored_literals(state, iri, &predicate_iri)? != literal_set(desired.as_ref()) {
                removes.extend(predicate_quads(state, iri, &predicate_iri)?);
                if let Some(value) = desired {
                    inserts.push(literal_quad(&subject, &predicate_iri, value, &graph)?);
                }
            }
        }
        if let Some(component_iri) = planned.realizes.as_deref() {
            inserts.push(Quad::new(
                subject,
                NamedNode::new(&terms.realizes)?,
                NamedNode::new(component_iri)?,
                GraphName::NamedNode(graph.clone()),
            ));
        }
        updated += 1;
    }

    commit_project_quads(state, &inserts, &removes, "code entity mint")?;

    for iri in &created_iris {
        if let Err(e) = provenance::record_provenance(&state.store, iri, "moosedev-mint") {
            tracing::warn!("failed to record code entity provenance for {iri}: {e}");
        }
    }

    Ok(ApplyOutcome { created, updated })
}

/// Lazily upsert one CodeEntity by normalized SCIP symbol.
pub fn ensure_entity(
    state: &AppState,
    terms: &CodeTerms,
    components: &[ComponentEntry],
    entry: &DefinitionEntry,
    agent: &str,
) -> anyhow::Result<EnsuredEntity> {
    if let Some(iri) = entity_for_symbol(state, terms, &entry.normalized_symbol)? {
        return Ok(EnsuredEntity {
            iri,
            created: false,
        });
    }

    let realizes = component_iri_for_path(&entry.file, components);
    let iri = mint_instance_iri("CodeEntity");
    let quads = create_quads(terms, &iri, entry, realizes)?;
    commit_project_quads(state, &quads, &[], "ensure code entity")?;

    if let Err(e) = provenance::record_provenance(&state.store, &iri, agent) {
        tracing::warn!("failed to record code entity provenance for {iri}: {e}");
    }

    Ok(EnsuredEntity { iri, created: true })
}

/// Build the complete create quad set shared by batch minting and lazy upsert.
fn create_quads(
    terms: &CodeTerms,
    iri: &str,
    entry: &DefinitionEntry,
    realizes: Option<&str>,
) -> anyhow::Result<Vec<Quad>> {
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let subject = NamedNode::new(iri)?;
    let mut quads = vec![Quad::new(
        subject.clone(),
        NamedNode::new(moose::RDF_TYPE)?,
        NamedNode::new(&terms.code_entity_class)?,
        graph.clone(),
    )];
    for (predicate_iri, desired) in desired_literal_values(terms, entry) {
        if let Some(value) = desired {
            quads.push(Quad::new(
                subject.clone(),
                NamedNode::new(&predicate_iri)?,
                Literal::new_simple_literal(value),
                graph.clone(),
            ));
        }
    }
    if let Some(component_iri) = realizes {
        quads.push(Quad::new(
            subject,
            NamedNode::new(&terms.realizes)?,
            NamedNode::new(component_iri)?,
            graph,
        ));
    }
    Ok(quads)
}

/// Commit project-graph quad inserts/removes and refresh graph-derived caches.
fn commit_project_quads(
    state: &AppState,
    inserts: &[Quad],
    removes: &[Quad],
    action: &str,
) -> anyhow::Result<()> {
    if inserts.is_empty() && removes.is_empty() {
        return Ok(());
    }

    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("{action} transaction: {e}"))?;
    for quad in removes {
        txn.remove(quad.as_ref());
    }
    txn.extend(inserts.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("{action} commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    state.mark_inferred_stale();
    Ok(())
}

/// Return a component IRI for a path, preserving `None` for unmapped paths.
fn component_iri_for_path<'a>(path: &str, components: &'a [ComponentEntry]) -> Option<&'a str> {
    best_component_for_path(path, components).and_then(|component| component.iri.as_deref())
}

/// Build one project-graph literal quad for an existing named subject.
fn literal_quad(
    subject: &NamedNode,
    predicate_iri: &str,
    value: String,
    graph: &NamedNode,
) -> anyhow::Result<Quad> {
    Ok(Quad::new(
        subject.clone(),
        NamedNode::new(predicate_iri)?,
        Literal::new_simple_literal(value),
        GraphName::NamedNode(graph.clone()),
    ))
}

/// Convert an optional desired singleton literal into the comparison set.
fn literal_set(value: Option<&String>) -> BTreeSet<String> {
    value.cloned().into_iter().collect()
}

/// Desired single-valued literals for a CodeEntity, including `rdfs:label`.
fn desired_literal_values(
    terms: &CodeTerms,
    entry: &DefinitionEntry,
) -> Vec<(String, Option<String>)> {
    vec![
        (
            terms.has_substrate_symbol.clone(),
            Some(entry.normalized_symbol.clone()),
        ),
        (
            terms.has_entity_kind.clone(),
            Some(desired_entity_kind(entry)),
        ),
        (terms.has_code_name.clone(), Some(desired_name(entry))),
        (
            terms.has_logical_path.clone(),
            symbols::logical_path(&entry.symbol),
        ),
        (terms.defined_in_path.clone(), Some(entry.file.clone())),
        (moose::RDFS_LABEL.to_string(), Some(desired_name(entry))),
    ]
}

/// Return the shape-required entity kind, with stable fallbacks for sparse data.
fn desired_entity_kind(entry: &DefinitionEntry) -> String {
    entry
        .kind
        .clone()
        .unwrap_or_else(|| if entry.is_module { "Module" } else { "Unknown" }.to_string())
}

/// Return the display name written to both `rdfs:label` and `hasCodeName`.
pub(crate) fn desired_name(entry: &DefinitionEntry) -> String {
    entry
        .display_name
        .clone()
        .or_else(|| {
            symbols::logical_path(&entry.symbol).and_then(|path| {
                path.rsplit("::")
                    .next()
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| entry.normalized_symbol.clone())
}

/// Find the existing entity for one normalized SCIP symbol, if any.
fn entity_for_symbol(
    state: &AppState,
    terms: &CodeTerms,
    normalized_symbol: &str,
) -> anyhow::Result<Option<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    let object = Literal::new_simple_literal(normalized_symbol);
    let mut hits = Vec::new();
    for q in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some((&object).into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = q.subject {
            hits.push(subject.as_str().to_string());
        }
    }
    hits.sort();
    Ok(hits.into_iter().next())
}

/// Compare stored singleton literal values against the desired mint projection.
fn literals_differ(
    state: &AppState,
    terms: &CodeTerms,
    iri: &str,
    entry: &DefinitionEntry,
) -> anyhow::Result<bool> {
    for (predicate, desired) in desired_literal_values(terms, entry) {
        let desired_set = literal_set(desired.as_ref());
        if stored_literals(state, iri, &predicate)? != desired_set {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read literal values for one subject/predicate in the project graph.
fn stored_literals(
    state: &AppState,
    subject_iri: &str,
    predicate_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    let mut out = BTreeSet::new();
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let Term::Literal(literal) = q.object {
            out.insert(literal.value().to_string());
        }
    }
    Ok(out)
}

/// Collect all stored quads for a subject/predicate so an update can replace them.
fn predicate_quads(
    state: &AppState,
    subject_iri: &str,
    predicate_iri: &str,
) -> anyhow::Result<Vec<Quad>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Return whether a CodeEntity already has any authoritative `realizes` edge.
fn has_realizes(state: &AppState, terms: &CodeTerms, iri: &str) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(iri)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

/// True when the substrate holds at least one definition batch minting would
/// create an entity for.
///
/// "Zero entities" alone does NOT mean mint never ran: the scope rule
/// deliberately skips private items and test paths (AD cb68b4de), so a project
/// whose whole surface is private or test-only mints to zero legitimately. Told
/// only the count, the diagnostic sent those projects to a `mint --apply` that
/// can never change anything, instead of to the lazy `link_code` path that is
/// actually how records attach there.
pub fn has_mintable_definitions(definitions: &[DefinitionEntry]) -> bool {
    definitions
        .iter()
        .any(|entry| (entry.is_module || entry.is_public) && !is_test_path(&entry.file))
}

/// Apply the batch minting scope rule and count the two skip classes.
fn mint_candidates(definitions: &[DefinitionEntry]) -> (Vec<DefinitionEntry>, usize, usize) {
    let mut kept = Vec::new();
    let mut skipped_scope = 0usize;
    let mut skipped_tests = 0usize;
    for entry in definitions {
        if is_test_path(&entry.file) {
            skipped_tests += 1;
        } else if !(entry.is_module || entry.is_public) {
            skipped_scope += 1;
        } else {
            kept.push(entry.clone());
        }
    }
    (kept, skipped_scope, skipped_tests)
}

/// Deterministically keep the first `(file, symbol)` entry per normalized symbol.
fn dedupe_collisions(
    entries: Vec<DefinitionEntry>,
) -> (Vec<DefinitionEntry>, Vec<SymbolCollision>) {
    let mut groups: BTreeMap<String, Vec<DefinitionEntry>> = BTreeMap::new();
    for entry in entries {
        groups
            .entry(entry.normalized_symbol.clone())
            .or_default()
            .push(entry);
    }

    let mut kept = Vec::new();
    let mut collisions = Vec::new();
    for (normalized_symbol, mut group) in groups {
        group.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.cmp(&b.symbol)));
        let first = group.remove(0);
        if !group.is_empty() {
            collisions.push(SymbolCollision {
                normalized_symbol,
                kept_file: first.file.clone(),
                dropped: group.iter().map(|entry| entry.file.clone()).collect(),
            });
        }
        kept.push(first);
    }
    kept.sort_by(|a, b| a.normalized_symbol.cmp(&b.normalized_symbol));
    (kept, collisions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(symbol: &str, file: &str, is_module: bool, is_public: bool) -> DefinitionEntry {
        DefinitionEntry {
            producer: "rust-analyzer".to_string(),
            symbol: symbol.to_string(),
            normalized_symbol: symbol.replace(" 0.6.3 ", " . "),
            display_name: Some("name".to_string()),
            kind: None,
            signature: None,
            file: file.to_string(),
            is_module,
            is_public,
        }
    }

    /// Zero entities is NOT the same as "mint never ran": a project whose whole
    /// surface is private or test-only mints to zero legitimately, and telling
    /// it to run `mint --apply` is advice that can never take effect.
    #[test]
    fn mintable_definitions_distinguish_never_minted_from_nothing_to_mint() {
        let eligible = [
            entry(
                "rust-analyzer cargo x 1.0 graph/",
                "src/graph.rs",
                true,
                false,
            ),
            entry("rust-analyzer cargo x 1.0 f().", "src/lib.rs", false, false),
        ];
        assert!(has_mintable_definitions(&eligible), "a module is mintable");

        let nothing_to_mint = [
            // private, non-module
            entry(
                "rust-analyzer cargo x 1.0 helper().",
                "src/lib.rs",
                false,
                false,
            ),
            // public but test-only
            entry("rust-analyzer cargo x 1.0 t().", "tests/it.rs", false, true),
        ];
        assert!(
            !has_mintable_definitions(&nothing_to_mint),
            "private + test-only definitions mint to zero legitimately"
        );

        assert!(
            !has_mintable_definitions(&[]),
            "no substrate, nothing to mint"
        );
    }

    #[test]
    fn mint_rule_keeps_modules_and_public_non_tests() {
        let module = entry(
            "rust-analyzer cargo moosedev 0.6.3 graph/",
            "src/graph.rs",
            true,
            false,
        );
        let public_fn = entry(
            "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().",
            "src/runtime.rs",
            false,
            true,
        );
        let private_fn = entry(
            "rust-analyzer cargo moosedev 0.6.3 runtime/helper().",
            "src/runtime.rs",
            false,
            false,
        );
        let test_fn = entry(
            "rust-analyzer cargo moosedev 0.6.3 tests/helper().",
            "tests/helper.rs",
            false,
            true,
        );

        let (kept, skipped_scope, skipped_tests) =
            mint_candidates(&[module.clone(), public_fn.clone(), private_fn, test_fn]);

        assert_eq!(kept, vec![module, public_fn]);
        assert_eq!(skipped_scope, 1);
        assert_eq!(skipped_tests, 1);
    }

    #[test]
    fn mint_rule_skips_rust_and_typescript_test_conventions() {
        let skipped = [
            "ui/src/App.test.ts",
            "ui/src/test/helper.ts",
            "ui/src/pages/RecordPage.test.tsx",
            "ui/src/example.spec.ts",
            "tests/foo.rs",
        ];
        let kept_files = ["ui/src/pages/RecordPage.tsx", "src/main.rs"];
        let definitions = skipped
            .iter()
            .chain(kept_files.iter())
            .map(|file| entry("symbol", file, false, true))
            .collect::<Vec<_>>();

        let (kept, skipped_scope, skipped_tests) = mint_candidates(&definitions);

        assert_eq!(
            kept.iter()
                .map(|entry| entry.file.as_str())
                .collect::<Vec<_>>(),
            kept_files
        );
        assert_eq!(skipped_scope, 0);
        assert_eq!(skipped_tests, skipped.len());
    }

    #[test]
    fn syntactic_entries_remain_lazy_only() {
        let mut syntactic = entry(
            "ts:rust:src/fallback.rs:fn:private_helper",
            "src/fallback.rs",
            false,
            false,
        );
        syntactic.producer = "tree-sitter".to_string();
        syntactic.normalized_symbol = syntactic.symbol.clone();

        let (kept, skipped_scope, skipped_tests) = mint_candidates(&[syntactic]);
        assert!(kept.is_empty());
        assert_eq!(skipped_scope, 1);
        assert_eq!(skipped_tests, 0);
    }

    #[test]
    fn collision_dedupe_keeps_first_file_then_symbol() {
        let mut a = entry(
            "rust-analyzer cargo moosedev 0.6.3 b/z().",
            "src/b.rs",
            false,
            true,
        );
        let mut b = entry(
            "rust-analyzer cargo moosedev 0.6.3 a/y().",
            "src/a.rs",
            false,
            true,
        );
        let mut c = entry(
            "rust-analyzer cargo moosedev 0.6.3 a/x().",
            "src/a.rs",
            false,
            true,
        );
        for entry in [&mut a, &mut b, &mut c] {
            entry.normalized_symbol = "same-symbol".to_string();
        }

        let (kept, collisions) = dedupe_collisions(vec![a, b, c.clone()]);

        assert_eq!(kept, vec![c]);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].normalized_symbol, "same-symbol");
        assert_eq!(collisions[0].kept_file, "src/a.rs");
        assert_eq!(
            collisions[0].dropped,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
    }

    #[test]
    fn collision_dedupe_preserves_unique_entries() {
        let a = entry(
            "rust-analyzer cargo moosedev 0.6.3 a/x().",
            "src/a.rs",
            false,
            true,
        );
        let b = entry(
            "rust-analyzer cargo moosedev 0.6.3 b/y().",
            "src/b.rs",
            false,
            true,
        );

        let (kept, collisions) = dedupe_collisions(vec![b.clone(), a.clone()]);

        assert_eq!(kept, vec![a, b]);
        assert!(collisions.is_empty());
    }
}
