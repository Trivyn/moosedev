//! Read-only CodeEntity dossier queries.
//!
//! This module is deliberately a pure read path: it resolves an already-minted
//! CodeEntity, collects records directly linked to it, and returns `None` when
//! no direct knowledge exists so future hover surfaces stay quiet.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};

use crate::code::substrate::symbols::normalize_symbol;
use crate::code::substrate::Position;

use super::capture::asserted_project_types;
use super::code_entities::{entities_by_symbol, CodeTerms};
use super::context::first_literal;
use super::state::AppState;
use super::util::local_name;
use super::PROJECT_KG_GRAPH_IRI;

/// One dossier target: a 1-based file position, a SCIP symbol (raw or
/// normalized), or the entity IRI itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DossierTarget {
    /// Resolve the symbol at a 1-based source position through the loaded substrate.
    Position { file: String, line: u32, col: u32 },
    /// Resolve a raw or version-normalized SCIP symbol through minted entities.
    Symbol(String),
    /// Address an already-minted CodeEntity directly.
    Iri(String),
}

/// One knowledge record shown in a dossier, annotated with the canonical link
/// predicate that made it relevant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    /// IRI of the linked knowledge record.
    pub iri: String,
    /// Local name of the record's asserted class.
    pub kind: String,
    /// Human-readable title, falling back to label/local IRI when needed.
    pub title: String,
    /// Lifecycle status; superseded records remain visible, deprecated records do not.
    pub status: String,
    /// RFC3339 capture timestamp, or empty when the record lacks one.
    pub timestamp: String,
    /// Canonical predicate local name, regardless of which direction was asserted.
    pub predicate_local: String,
}

/// Read model for all project knowledge directly attached to a CodeEntity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dossier {
    /// IRI of the CodeEntity this dossier describes.
    pub entity_iri: String,
    /// Code-layer kind literal, such as `Function`, or `Unknown`.
    pub kind: String,
    /// Display name from code metadata, then `rdfs:label`, then IRI local name.
    pub display_name: String,
    /// Logical path from the SCIP symbol, when available.
    pub logical_path: Option<String>,
    /// Repo-relative definition path, when known.
    pub defined_in: Option<String>,
    /// Realized component IRI and label, when the entity maps to a component.
    pub realizes: Option<(String, String)>,
    /// Records directly linked to the CodeEntity; empty means no dossier is returned.
    pub direct_records: Vec<RecordSummary>,
    /// Records linked through the realized component; rendered only as secondary context.
    pub component_records: Vec<RecordSummary>,
    /// True when the loaded substrate was built from a different commit.
    pub substrate_stale: bool,
    /// v2.1 seams; always empty in v2.0.
    pub judgments: Vec<String>,
    pub observations: Vec<String>,
}

/// Return the read-only dossier for a target, or `None` when no directly linked
/// knowledge exists. Caller mistakes such as zero positions or invalid IRIs are
/// errors; absence of KG knowledge is silence.
pub fn get_entity_dossier(
    state: &AppState,
    target: &DossierTarget,
) -> anyhow::Result<Option<Dossier>> {
    let terms = CodeTerms::resolve(state)?;
    let Some(entity_iri) = resolve_target_entity(state, &terms, target)? else {
        return Ok(None);
    };

    let entity = NamedNode::new(&entity_iri)
        .with_context(|| format!("invalid CodeEntity IRI {entity_iri}"))?;
    if !asserted_project_types(state, &entity)
        .iter()
        .any(|class| class == &terms.code_entity_class)
    {
        return Ok(None);
    }

    let kind = first_literal(&state.store, &entity_iri, &terms.has_entity_kind)
        .unwrap_or_else(|| "Unknown".to_string());
    let display_name = first_literal(&state.store, &entity_iri, &terms.has_code_name)
        .or_else(|| first_literal(&state.store, &entity_iri, moose::RDFS_LABEL))
        .unwrap_or_else(|| local_name(&entity_iri).to_string());
    let logical_path = first_literal(&state.store, &entity_iri, &terms.has_logical_path);
    let defined_in = first_literal(&state.store, &entity_iri, &terms.defined_in_path);
    let realizes = first_realized_component(state, &terms, &entity_iri)?;

    let direct_records = direct_records_for_entity(state, &entity_iri)?;
    if direct_records.is_empty() {
        return Ok(None);
    }

    let pairs = LinkPairs::resolve(state)?;
    let direct_iris = direct_records
        .iter()
        .map(|record| record.iri.as_str())
        .collect::<BTreeSet<_>>();
    let mut component_records = match realizes.as_ref() {
        Some((component_iri, _)) => collect_records(state, &pairs.concerns, component_iri)?
            .into_iter()
            .filter(|record| !direct_iris.contains(record.iri.as_str()))
            .collect(),
        None => Vec::new(),
    };
    sort_records(&mut component_records);

    Ok(Some(Dossier {
        entity_iri,
        kind,
        display_name,
        logical_path,
        defined_in,
        realizes,
        direct_records,
        component_records,
        substrate_stale: state.substrate().map(|s| s.is_stale()).unwrap_or(false),
        judgments: Vec::new(),
        observations: Vec::new(),
    }))
}

/// Return all non-deprecated knowledge records directly linked to one CodeEntity.
pub(crate) fn direct_records_for_entity(
    state: &AppState,
    entity_iri: &str,
) -> anyhow::Result<Vec<RecordSummary>> {
    let pairs = LinkPairs::resolve(state)?;
    let mut direct_records = collect_records(state, &pairs.all, entity_iri)?;
    sort_records(&mut direct_records);
    Ok(direct_records)
}

/// Render a stable Markdown view suitable for MCP and future hover surfaces.
pub fn render_markdown(dossier: &Dossier) -> String {
    let mut out = format!("### {} ({})\n", dossier.display_name, dossier.kind);
    if dossier.logical_path.is_some() || dossier.defined_in.is_some() {
        match (&dossier.logical_path, &dossier.defined_in) {
            (Some(path), Some(defined)) => {
                out.push_str(&format!("`{path}` - defined in `{defined}`\n"));
            }
            (Some(path), None) => out.push_str(&format!("`{path}`\n")),
            (None, Some(defined)) => out.push_str(&format!("Defined in `{defined}`\n")),
            (None, None) => {}
        }
    }
    if let Some((_, label)) = &dossier.realizes {
        out.push_str(&format!("Realizes component: {label}\n"));
    }
    out.push_str("\n**Records**\n");
    for record in &dossier.direct_records {
        out.push_str(&format!(
            "- [{}] {} - {}, {} (via {})\n",
            record.kind, record.title, record.status, record.timestamp, record.predicate_local
        ));
    }
    if let Some((_, label)) = &dossier.realizes {
        if !dossier.component_records.is_empty() {
            out.push_str(&format!("\n**Via component {label}**\n"));
            for record in &dossier.component_records {
                out.push_str(&format!(
                    "- [{}] {} - {}, {}\n",
                    record.kind, record.title, record.status, record.timestamp
                ));
            }
        }
    }
    if dossier.substrate_stale {
        out.push_str(
            "\nwarning: substrate is stale; positions may have drifted, re-run `moosedev index`.\n",
        );
    }
    out
}

/// Convert the caller's selector into an existing CodeEntity IRI without minting.
fn resolve_target_entity(
    state: &AppState,
    terms: &CodeTerms,
    target: &DossierTarget,
) -> anyhow::Result<Option<String>> {
    match target {
        DossierTarget::Position { file, line, col } => {
            if *line == 0 || *col == 0 {
                anyhow::bail!("code positions are 1-based; line and col must be greater than 0");
            }
            let Some(substrate) = state.substrate() else {
                return Ok(None);
            };
            let Some(resolution) = substrate.resolve(
                file,
                Position {
                    line: line - 1,
                    col: col - 1,
                },
            ) else {
                return Ok(None);
            };
            if resolution.is_local {
                return Ok(None);
            }
            let Some(symbol) = normalize_symbol(&resolution.symbol) else {
                return Ok(None);
            };
            Ok(entities_by_symbol(state, terms)?.get(&symbol).cloned())
        }
        DossierTarget::Symbol(symbol) => {
            let normalized = normalize_symbol(symbol).unwrap_or_else(|| symbol.clone());
            Ok(entities_by_symbol(state, terms)?.get(&normalized).cloned())
        }
        DossierTarget::Iri(iri) => {
            NamedNode::new(iri).with_context(|| format!("invalid entity IRI {iri}"))?;
            Ok(Some(iri.clone()))
        }
    }
}

/// Return the first component reached by `realizes`, with a display label.
fn first_realized_component(
    state: &AppState,
    terms: &CodeTerms,
    entity_iri: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(entity_iri)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let Term::NamedNode(component) = q.object else {
            continue;
        };
        let iri = component.as_str().to_string();
        let label = first_literal(&state.store, &iri, moose::RDFS_LABEL)
            .unwrap_or_else(|| local_name(&iri).to_string());
        return Ok(Some((iri, label)));
    }
    Ok(None)
}

/// Canonical predicate plus inverse predicate used to find records regardless of
/// which direction was asserted in the project graph.
#[derive(Debug, Clone)]
struct PredicatePair {
    canonical_local: &'static str,
    canonical_iri: String,
    inverse_iri: String,
    direction: CanonicalDirection,
}

/// Orientation of the canonical predicate relative to the CodeEntity target.
#[derive(Debug, Clone, Copy)]
enum CanonicalDirection {
    RecordToEntity,
    EntityToRecord,
}

/// Resolved predicate sets for direct records and component secondary records.
struct LinkPairs {
    concerns: Vec<PredicatePair>,
    all: Vec<PredicatePair>,
}

impl LinkPairs {
    /// Resolve the object properties by local name, keeping ontology namespaces
    /// out of the read logic.
    fn resolve(state: &AppState) -> anyhow::Result<Self> {
        let specs = [
            (
                "concerns",
                "isConcernedBy",
                CanonicalDirection::RecordToEntity,
            ),
            (
                "constrains",
                "isConstrainedBy",
                CanonicalDirection::RecordToEntity,
            ),
            (
                "satisfies",
                "isSatisfiedBy",
                CanonicalDirection::EntityToRecord,
            ),
            (
                "embodies",
                "isEmbodiedBy",
                CanonicalDirection::EntityToRecord,
            ),
            (
                "violates",
                "isViolatedBy",
                CanonicalDirection::EntityToRecord,
            ),
        ];
        let all = specs
            .into_iter()
            .map(|(canonical_local, inverse_local, direction)| {
                Ok(PredicatePair {
                    canonical_local,
                    canonical_iri: state.resolve_object_property(canonical_local)?,
                    inverse_iri: state.resolve_object_property(inverse_local)?,
                    direction,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            concerns: all[..1].to_vec(),
            all,
        })
    }
}

/// Collect summaries for all records linked to one target by any supplied pair.
fn collect_records(
    state: &AppState,
    pairs: &[PredicatePair],
    target_iri: &str,
) -> anyhow::Result<Vec<RecordSummary>> {
    let mut records = BTreeMap::new();
    for pair in pairs {
        for record_iri in linked_records(state, pair, target_iri)? {
            if let Some(summary) = summarize_record(state, &record_iri, pair.canonical_local) {
                records.insert((record_iri, pair.canonical_local), summary);
            }
        }
    }
    Ok(records.into_values().collect())
}

/// Return candidate record IRIs linked through the canonical or inverse edge.
fn linked_records(
    state: &AppState,
    pair: &PredicatePair,
    target_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let target = NamedNodeRef::new(target_iri)?;
    let mut out = BTreeSet::new();
    match pair.direction {
        CanonicalDirection::RecordToEntity => {
            collect_subjects(state, &pair.canonical_iri, target, graph, &mut out)?;
            collect_objects(state, target, &pair.inverse_iri, graph, &mut out)?;
        }
        CanonicalDirection::EntityToRecord => {
            collect_objects(state, target, &pair.canonical_iri, graph, &mut out)?;
            collect_subjects(state, &pair.inverse_iri, target, graph, &mut out)?;
        }
    }
    Ok(out)
}

/// Add named subjects from `(?subject, predicate, object)` matches.
fn collect_subjects(
    state: &AppState,
    predicate_iri: &str,
    object: NamedNodeRef<'_>,
    graph: NamedNodeRef<'_>,
    out: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let predicate = NamedNodeRef::new(predicate_iri)?;
    for q in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = q.subject {
            out.insert(subject.as_str().to_string());
        }
    }
    Ok(())
}

/// Add named objects from `(subject, predicate, ?object)` matches.
fn collect_objects(
    state: &AppState,
    subject: NamedNodeRef<'_>,
    predicate_iri: &str,
    graph: NamedNodeRef<'_>,
    out: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let predicate = NamedNodeRef::new(predicate_iri)?;
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let Term::NamedNode(object) = q.object {
            out.insert(object.as_str().to_string());
        }
    }
    Ok(())
}

/// Build a display summary for a record, skipping dangling and deprecated nodes.
fn summarize_record(
    state: &AppState,
    record_iri: &str,
    predicate_local: &str,
) -> Option<RecordSummary> {
    let subject = NamedNode::new(record_iri).ok()?;
    let kind_iri = asserted_project_types(state, &subject).into_iter().next()?;
    let title = first_literal(&state.store, record_iri, &state.capture.title)
        .or_else(|| first_literal(&state.store, record_iri, moose::RDFS_LABEL))
        .unwrap_or_else(|| local_name(record_iri).to_string());
    let status = first_literal(&state.store, record_iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    if status.eq_ignore_ascii_case("deprecated") {
        return None;
    }
    Some(RecordSummary {
        iri: record_iri.to_string(),
        kind: local_name(&kind_iri).to_string(),
        title,
        status,
        timestamp: first_literal(&state.store, record_iri, &state.capture.timestamp)
            .unwrap_or_default(),
        predicate_local: predicate_local.to_string(),
    })
}

/// Keep dossier output stable and useful: constraints first, then decisions,
/// then lessons, with newer records before older records inside each group.
fn sort_records(records: &mut [RecordSummary]) {
    records.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.iri.cmp(&b.iri))
    });
}

/// Rank the record classes that are most important in hover-sized context.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "Constraint" => 0,
        "ArchitecturalDecision" => 1,
        "Lesson" => 2,
        _ => 3,
    }
}
