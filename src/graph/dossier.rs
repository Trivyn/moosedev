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
use super::lifecycle::in_working_set;
use super::proposals::{judgments_for_entity, JudgmentSummary};
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
    /// Optional captured claim describing the record.
    pub description: Option<String>,
    /// Workbench deep link when the HTTP UI has published an address.
    pub workbench_url: Option<String>,
    /// Lifecycle status; superseded records remain visible as labeled history.
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
    /// True when this entity is backed by a tree-sitter syntactic identity.
    pub syntactic_anchor: bool,
    /// Role/criticality judgments (proposed = advisory, accepted = ratified).
    pub judgments: Vec<JudgmentSummary>,
    /// Observation digest lines from the churn sidecar (derived, not knowledge).
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
    let syntactic_anchor = first_literal(&state.store, &entity_iri, &terms.has_substrate_symbol)
        .is_some_and(|symbol| symbol.starts_with("ts:"));
    let realizes = first_realized_component(state, &terms, &entity_iri)?;

    let direct_records = direct_records_for_entity(state, &entity_iri)?;
    // Silence-rule amendment (of AD 8f20452a): judgments count as direct
    // knowledge — a ratified core role with zero records is exactly the
    // hotspot a hover must surface. Observations alone never break silence
    // (measurements are not knowledge).
    let judgments = judgments_for_entity(state, &entity_iri)?;
    if direct_records.is_empty() && judgments.is_empty() {
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

    // Observation digest: derived churn/authorship for the defining file.
    let mut observations = Vec::new();
    if let (Some(substrate), Some(file)) = (state.substrate(), defined_in.as_deref()) {
        if let Some(churn) = substrate.churn_for_file(file) {
            let window = substrate.churn_window_months().unwrap_or(24);
            observations.push(format!(
                "churn: {} commits/{window}mo · last {} · {} author(s) · top {:.0}%",
                churn.commits,
                churn.last_commit.get(..10).unwrap_or(&churn.last_commit),
                churn.distinct_authors,
                churn.top_author_share * 100.0
            ));
        }
    }

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
        syntactic_anchor,
        judgments,
        observations,
    }))
}

/// Return all dossier-visible knowledge records directly linked to one CodeEntity.
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
    let marker = if dossier.syntactic_anchor {
        " [syntactic anchor]"
    } else {
        ""
    };
    let mut out = format!(
        "### {} ({}){}\n",
        dossier.display_name, dossier.kind, marker
    );
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

    if !dossier.judgments.is_empty() {
        out.push_str("\n**Judgments**\n");
        for judgment in &dossier.judgments {
            render_judgment_line(&mut out, judgment);
        }
        // §5.3 item 5: a ratified core/critical entity with no linked
        // rationale is a comprehension-debt hotspot with an address.
        let core_or_critical = dossier.judgments.iter().any(|j| {
            j.status == "accepted"
                && matches!(
                    j.target_local.as_str(),
                    "core-algorithm" | "domain-logic" | "high"
                )
        });
        if core_or_critical && dossier.direct_records.is_empty() {
            out.push_str("⚠ core entity — no linked rationale\n");
        }
    }

    if !dossier.direct_records.is_empty() {
        out.push_str("\n**Records**\n");
        for record in &dossier.direct_records {
            render_record_line(&mut out, record);
        }
    }
    if let Some((_, label)) = &dossier.realizes {
        if !dossier.component_records.is_empty() {
            out.push_str(&format!("\n**Via component {label}**\n"));
            for record in &dossier.component_records {
                render_record_line(&mut out, record);
            }
        }
    }
    if !dossier.observations.is_empty() {
        out.push_str("\n**Observations**\n");
        for observation in &dossier.observations {
            out.push_str(&format!("- {observation}\n"));
        }
    }
    if dossier.substrate_stale {
        out.push_str(
            "\nwarning: substrate is stale; positions may have drifted, re-run `moosedev index`.\n",
        );
    }
    out
}

/// One judgment line: ratified plain with provenance, proposed visually
/// distinct with confidence + disposition (spec §5.3: provisional judgments
/// render as proposals).
fn render_judgment_line(out: &mut String, judgment: &JudgmentSummary) {
    let axis = match judgment.predicate_local.as_str() {
        "playsRole" => "role",
        "hasCriticality" => "criticality",
        other => other,
    };
    if judgment.status == "accepted" {
        let date = judgment.timestamp.get(..10).unwrap_or(&judgment.timestamp);
        out.push_str(&format!(
            "- {axis}: {} — proposed by {}, ratified {date}\n",
            judgment.target_local, judgment.author
        ));
    } else {
        let confidence = judgment.confidence.as_deref().unwrap_or("?");
        let escalation = judgment.escalation.as_deref().unwrap_or("pending");
        out.push_str(&format!(
            "- {axis}: {}? (proposed, {confidence}, {escalation})\n",
            judgment.target_local
        ));
    }
}

fn render_record_line(out: &mut String, record: &RecordSummary) {
    let title = match &record.workbench_url {
        Some(url) => format!("[{}]({url})", record.title),
        None => record.title.clone(),
    };
    out.push_str(&format!(
        "- [{}] {} - {}, {} (via {})\n",
        record.kind, title, record.status, record.timestamp, record.predicate_local
    ));
}

/// Convert the caller's selector into an existing CodeEntity IRI without minting.
/// Crate-visible so the policy engine can resolve gate candidates without the
/// dossier's "no linked records → silence" behavior.
pub(crate) fn resolve_target_entity(
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

/// Build a display summary for a record, skipping dangling and dossier-hidden nodes.
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
    // Proposed records live only in the ratification inbox; rejected and
    // deprecated records are hidden history. Superseded records remain visible
    // as labeled history so a dossier preserves the rationale's evolution.
    if !is_dossier_visible(&status) {
        return None;
    }
    Some(RecordSummary {
        iri: record_iri.to_string(),
        kind: local_name(&kind_iri).to_string(),
        title,
        description: first_literal(&state.store, record_iri, &state.capture.description),
        workbench_url: workbench_record_url(state, record_iri, local_name(&kind_iri)),
        status,
        timestamp: first_literal(&state.store, record_iri, &state.capture.timestamp)
            .unwrap_or_default(),
        predicate_local: predicate_local.to_string(),
    })
}

/// Dossier lifecycle policy shared by direct records, inherited component
/// records, hover rendering, and why-coverage through [`summarize_record`].
fn is_dossier_visible(status: &str) -> bool {
    in_working_set(status) || status.eq_ignore_ascii_case("superseded")
}

/// Return a fresh workbench deep link for a record when the HTTP UI is
/// serving in THIS daemon run.
///
/// The in-process address is the only trusted source: the `http.addr` file
/// lingers after a crash, so after an HTTP-disabled or failed restart it can
/// point at a dead port or an unrelated local service. (The file stays for
/// cross-process `--status`/`ui` discovery.)
pub(crate) fn workbench_record_url(
    state: &AppState,
    record_iri: &str,
    kind: &str,
) -> Option<String> {
    let addr = state.http_addr()?;
    let route = match kind {
        "ArchitecturalDecision" => "adrs",
        "Requirement" => "requirements",
        "Lesson" => "lessons",
        "Constraint" => "constraints",
        _ => "record",
    };
    Some(format!(
        "http://{addr}/#/{route}/{}",
        local_name(record_iri)
    ))
}

/// Workbench deep link for a code entity. The `#/record/<uuid>` route resolves
/// any typed project-graph subject by UUID, CodeEntities included, so entity
/// links need no dedicated route.
pub(crate) fn workbench_entity_url(state: &AppState, entity_iri: &str) -> Option<String> {
    workbench_record_url(state, entity_iri, "CodeEntity")
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
