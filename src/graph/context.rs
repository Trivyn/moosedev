//! Relevant-context retrieval, exact record resolution, and bounded graph expansion.
//! Retrieval remains symbolic-first, with dense vectors only as a fallback seed/ranking channel.

use std::collections::HashSet;

use moose::embeddings::retrieval_embed_query;
use moose::entity_index::DEFAULT_DENSE_FLOOR;
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};
use oxigraph::store::Store;

use super::capture::{
    asserted_project_types, class_label_mirror_property_iri, require_information_record,
};
use super::lifecycle::{in_working_set, is_retired};
use super::state::AppState;
use super::util::{any_subclass_of, local_name};
use super::PROJECT_KG_GRAPH_IRI;

/// A recorded knowledge item returned as structured context.
pub struct ContextItem {
    pub iri: String,
    pub kind: String,
    pub label: String,
    pub properties: Vec<(String, String)>,
}

impl ContextItem {
    /// True when this record is outside the authoritative working set:
    /// retired (`superseded`/`deprecated`), declined (`rejected`), or not yet
    /// ratified (`proposed` — inbox material, never recall material).
    pub fn is_historical(&self) -> bool {
        self.properties
            .iter()
            .any(|(k, v)| k == "hasLifecycleStatus" && !in_working_set(v))
    }
}

/// Retrieve recorded knowledge relevant to `topic` — BM25 lexical relevance over
/// each record's title + description via moose `search_records` — or list all
/// recorded instances when `topic` is empty. Records sharing no query term are
/// excluded, so an empty result is reported honestly as "nothing relevant"
/// rather than padded with noise (invariant #6: be correct, don't sound correct).
/// Symbolic — no LLM.
pub fn relevant_context(
    state: &AppState,
    topic: Option<&str>,
    limit: usize,
    include_history: bool,
) -> anyhow::Result<Vec<ContextItem>> {
    // Materialize inferred edges if a write invalidated them, so the typed expansion
    // traverses fresh inverse/subproperty links (bidirectional walk).
    state.ensure_enriched();
    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];

    // Document text for both retrieval channels: rdfs:label (weighted) + description.
    // We search rdfs:label — every record carries it as its title — rather than hasTitle,
    // so label-only records are still found and the title text isn't double-counted. The
    // same two fields feed the dense document embedding (see `AppState::index_record`), so
    // the lexical and dense channels score the same text.
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    // A focused topic, trimmed to None when blank — drives both the seed and the
    // (topic-only) relational expansion below.
    let query = topic.map(str::trim).filter(|t| !t.is_empty());
    let subjects: Vec<(String, String)> = match query {
        Some(t) => {
            // Hybrid BM25F ⊕ dense seed: the dense channel surfaces records whose
            // meaning matches `t` with no shared term (paraphrase / vocabulary
            // mismatch) — the lexical blind spot that otherwise gates the whole
            // expansion. The confidence floor preserves the honest empty state
            // (invariant #6): an irrelevant query still seeds nothing. Soft-falls to
            // pure BM25 when the instance index is empty or the backbone is absent.
            let mut hits: Vec<(String, String)> = state
                .entity_index
                .search_records_hybrid(
                    t,
                    &class_iris,
                    &state.store,
                    &data_graphs,
                    &text_fields,
                    limit,
                    &state.instance_store,
                    dense_floor(),
                )
                .into_iter()
                .map(|h| (h.iri, h.class_iri))
                .collect();
            // Symbolic-first anchoring (invariant #1): when `t` names an existing
            // record by an exact label/title match, seed it FIRST — ahead of the
            // lexical+dense ranking — so the named record is guaranteed to expand.
            // Free-text topics that name no record fall through to the hybrid seed.
            if let Some(anchor) = resolve_topic_to_record(state, t) {
                hits.retain(|(iri, _)| iri != &anchor.0);
                hits.insert(0, anchor);
                hits.truncate(limit);
            }
            hits
        }
        None => list_instances(&state.store, &class_iris, limit),
    };

    let mut items: Vec<ContextItem> = subjects
        .into_iter()
        .map(|(iri, class_iri)| build_context_item(state, iri, class_iri))
        .collect();

    // Default to the *current* working set: hide superseded/deprecated records
    // (history is one hop away — `include_history` lists them, and each current
    // item still surfaces its `supersedes` link + rationale) AND unratified or
    // declined ones — a `proposed` record lives in the ratification inbox and a
    // `rejected` one was expressly declined; neither may reach an agent as
    // authoritative recall. Filtering after the fetch means a page can return
    // fewer than `limit` items when history exists; acceptable for v1's data
    // volumes.
    if !include_history {
        items.retain(|item| !item.is_historical());
    }

    // Bounded relational expansion (Constraint aa8b3fa3): for a focused topic, reach
    // the few linked records that COMPLETE an answer — the lexically-distant neighbor
    // BM25 alone misses — WITHOUT dumping the neighborhood (context efficiency is the
    // whole point, AD 7b824b26). Skipped for list-all and when MOOSEDEV_EXPAND_HOPS=0.
    // Candidate neighbors are RANKED (typed-edge priority, then dense topic-similarity)
    // before the EXPAND_MAX budget is spent, so the links that survive the cap are the
    // answer-completing ones rather than whatever order the store happened to yield.
    let max_hops = std::env::var("MOOSEDEV_EXPAND_HOPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(EXPAND_MAX_HOPS);
    if let Some(t) = query.filter(|_| max_hops > 0) {
        // One query embedding for the whole expansion, reused to rank each hop's
        // neighbors by topic similarity. `None` (no backbone) → ranking falls back to
        // typed-edge priority then IRI; seeding was already lexical-only in that case.
        let query_emb = retrieval_embed_query(t).ok();
        let mut seen: std::collections::HashSet<String> =
            items.iter().map(|i| i.iri.clone()).collect();
        let mut expanded: Vec<ContextItem> = Vec::new();
        let mut frontier: Vec<String> = items
            .iter()
            .take(EXPAND_FROM_TOP)
            .map(|i| i.iri.clone())
            .collect();
        for _ in 0..max_hops {
            // Gather this hop's fresh neighbors across the whole frontier, deduped,
            // then rank the pool together so the budget is spent on the best — not on
            // whichever source/edge happened to come first in store order.
            let mut candidates: Vec<(String, String, String)> = Vec::new();
            let mut pooled: std::collections::HashSet<String> = std::collections::HashSet::new();
            for src in &frontier {
                for (pred, neighbor_iri, neighbor_class) in record_neighbors(state, src) {
                    if seen.contains(&neighbor_iri) || !pooled.insert(neighbor_iri.clone()) {
                        continue; // already a seed/expanded, or already pooled this hop
                    }
                    candidates.push((pred, neighbor_iri, neighbor_class));
                }
            }
            if candidates.is_empty() {
                break;
            }
            rank_neighbors(state, query_emb.as_deref(), &mut candidates);

            let mut next: Vec<String> = Vec::new();
            let mut budget_spent = false;
            for (pred, neighbor_iri, neighbor_class) in candidates {
                if !seen.insert(neighbor_iri.clone()) {
                    continue; // ranked pool is deduped, but keep `seen` authoritative
                }
                let mut item = build_context_item(state, neighbor_iri.clone(), neighbor_class);
                if !include_history && item.is_historical() {
                    continue; // stay in the current working set
                }
                item.properties.insert(0, ("linkedVia".to_string(), pred));
                expanded.push(item);
                next.push(neighbor_iri);
                if expanded.len() >= EXPAND_MAX {
                    budget_spent = true;
                    break;
                }
            }
            if budget_spent || next.is_empty() {
                break;
            }
            frontier = next;
        }
        items.extend(expanded);
    }

    Ok(items)
}

/// A rationale record offered as a "Link decision to this entity…" candidate.
#[derive(Debug, Clone)]
pub struct LinkCandidate {
    pub iri: String,
    /// Local class name (e.g. `ArchitecturalDecision`).
    pub kind: String,
    pub title: String,
}

/// Record kinds an editor code action may offer as link candidates: the
/// rationale-bearing classes a `concerns` edge from a code entity means
/// something for.
const LINKABLE_KINDS: &[&str] = &[
    "ArchitecturalDecision",
    "Constraint",
    "Lesson",
    "Requirement",
];

/// Top candidate records for linking to a code entity, ranked by the same
/// hybrid BM25F⊕dense seed [`relevant_context`] uses — seeded with the
/// entity's display name + logical path rather than a human topic. No
/// relational expansion: the editor menu wants the few best anchors, not a
/// neighborhood. Retired and rejected records are never offered (a link to
/// them could not be ratified).
pub fn link_candidates(
    state: &AppState,
    seed: &str,
    limit: usize,
) -> anyhow::Result<Vec<LinkCandidate>> {
    link_candidates_excluding(state, seed, limit, &HashSet::new())
}

/// [`link_candidates`] with caller-specific exclusions applied before the
/// result limit. LSP callers use this for records already linked to the entity
/// or already represented by a pending proposal.
pub fn link_candidates_excluding(
    state: &AppState,
    seed: &str,
    limit: usize,
    excluded: &HashSet<String>,
) -> anyhow::Result<Vec<LinkCandidate>> {
    let seed = seed.trim();
    if seed.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let class_iris = LINKABLE_KINDS
        .iter()
        .map(|kind| state.resolve_class(kind))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    // Moose scores the complete class-scoped set internally. Supplying its
    // finite cardinality retains every ranked hit for eligibility filtering
    // without an unbounded limit sentinel.
    let ranked_limit = count_instances(&state.store, &class_iris);
    let candidates = state
        .entity_index
        .search_records_hybrid(
            seed,
            &class_iris,
            &state.store,
            &data_graphs,
            &text_fields,
            ranked_limit,
            &state.instance_store,
            dense_floor(),
        )
        .into_iter()
        .filter(|hit| !excluded.contains(&hit.iri))
        .filter(|hit| {
            // Working-set only: a proposed record cannot be linked-to yet
            // (link accept requires a ratified subject), and rejected/retired
            // records were declined or replaced.
            let status = first_literal(&state.store, &hit.iri, &state.capture.status);
            status.as_deref().is_none_or(in_working_set)
        })
        .map(|hit| LinkCandidate {
            title: first_literal(&state.store, &hit.iri, moose::RDFS_LABEL)
                .unwrap_or_else(|| local_name(&hit.iri).to_string()),
            kind: local_name(&hit.class_iri).to_string(),
            iri: hit.iri,
        })
        .take(limit)
        .collect();
    Ok(candidates)
}

/// Bounds for relational expansion in [`relevant_context`] (Constraint aa8b3fa3):
/// expansion is a budgeted reach to the few neighbors that complete an answer, never
/// a neighborhood dump — context efficiency is the whole point (AD 7b824b26).
const EXPAND_FROM_TOP: usize = 3; // expand only from the top-N most-relevant seed hits
const EXPAND_MAX_HOPS: usize = 2; // follow at most this many hops outward
const EXPAND_MAX: usize = 5; // hard cap on total appended (linked) records

/// Outbound edges from `iri` to other recorded knowledge items, as
/// `(predicate_local, neighbor_iri, neighbor_class)`. Only edges whose object is an
/// `InformationRecord` (or subclass) are returned — excluding the `prov:*` metadata
/// firehose and literal properties — and `hasRationale` is skipped because its text
/// is already inlined by [`build_context_item`].
fn record_neighbors(state: &AppState, iri: &str) -> Vec<(String, String, String)> {
    let Ok(subject) = NamedNodeRef::new(iri) else {
        return Vec::new();
    };
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out = Vec::new();
    for q in state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
    {
        if q.predicate.as_str() == moose::RDF_TYPE {
            continue;
        }
        let pred_local = local_name(q.predicate.as_str()).to_string();
        if pred_local == "hasRationale" {
            continue; // its text is already inlined by build_context_item
        }
        if let Term::NamedNode(obj) = &q.object {
            if let Ok(class) = require_information_record(state, obj) {
                out.push((pred_local, obj.as_str().to_string(), class));
            }
        }
    }
    out
}

/// List up to `limit` instances of the given classes in the project KG graph.
/// Confidence floor for the dense channel of the hybrid seed, from
/// `MOOSEDEV_DENSE_FLOOR` (an absolute cosine), defaulting to core's
/// [`DEFAULT_DENSE_FLOOR`]. The floor preserves the honest empty state (invariant
/// #6): cosine has no natural zero, so without it RRF would always promote *some*
/// nearest neighbor and manufacture a seed for an irrelevant query. Mirrors the
/// `MOOSEDEV_EXPAND_HOPS` override pattern. Always `Some` — config never disables
/// the guarantee (an unparseable value falls back to the default).
pub fn dense_floor() -> Option<f32> {
    let floor = std::env::var("MOOSEDEV_DENSE_FLOOR")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(DEFAULT_DENSE_FLOOR);
    Some(floor)
}

/// Symbolic-first anchor: resolve a free-text `topic` to an existing record by an
/// exact (normalized) match on its `rdfs:label` or `hasTitle`, returning the
/// record's `(iri, class_iri)`. Lets [`relevant_context`] seed a *named* record as
/// the top anchor before lexical+dense ranking (invariant #1 — the symbolic layer
/// is primary; dense is the open-vocabulary fallback). Returns `None` for a topic
/// that names no record. Alias (`skos:altLabel`) anchoring is a later refinement —
/// records carry none today.
fn resolve_topic_to_record(state: &AppState, topic: &str) -> Option<(String, String)> {
    let needle = normalize_match(topic);
    if needle.is_empty() {
        return None;
    }
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    for pred_iri in [moose::RDFS_LABEL, state.capture.title.as_str()] {
        let Ok(pred) = NamedNodeRef::new(pred_iri) else {
            continue;
        };
        for q in state
            .store
            .quads_for_pattern(None, Some(pred), None, Some(GraphNameRef::NamedNode(graph)))
            .flatten()
        {
            let Term::Literal(lit) = &q.object else {
                continue;
            };
            if normalize_match(lit.value()) != needle {
                continue;
            }
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                if let Ok(class) = require_information_record(state, s) {
                    return Some((s.as_str().to_string(), class));
                }
            }
        }
    }
    None
}

/// Resolve a free-text target to recorded project items by an exact (normalized)
/// match on `rdfs:label` or `hasTitle`, returning *all* distinct matches as
/// `(iri, class)`. The many-match analogue of [`resolve_topic_to_record`], so a
/// caller can tell "not found" (empty) from "ambiguous" (>1). Deduped by IRI (a
/// record matches on both its label and its title).
pub(crate) fn resolve_record_exact_all(state: &AppState, target: &str) -> Vec<(String, String)> {
    let needle = normalize_match(target);
    if needle.is_empty() {
        return Vec::new();
    }
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out: Vec<(String, String)> = Vec::new();
    for pred_iri in [moose::RDFS_LABEL, state.capture.title.as_str()] {
        let Ok(pred) = NamedNodeRef::new(pred_iri) else {
            continue;
        };
        for q in state
            .store
            .quads_for_pattern(None, Some(pred), None, Some(GraphNameRef::NamedNode(graph)))
            .flatten()
        {
            let Term::Literal(lit) = &q.object else {
                continue;
            };
            if normalize_match(lit.value()) != needle {
                continue;
            }
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                let iri = s.as_str().to_string();
                if out.iter().any(|(existing, _)| existing == &iri) {
                    continue;
                }
                if let Ok(class) = require_information_record(state, s) {
                    out.push((iri, class));
                }
            }
        }
    }
    out
}

/// Resolve a free-text target to existing typed project instances by exact
/// normalized match on `rdfs:label` or the label-mirror property for any expected
/// class. Unlike [`resolve_record_exact_all`], this accepts non-InformationRecord
/// instances when the caller's predicate shape expects them, e.g. `SystemComponent`
/// for `concerns`.
pub(crate) fn resolve_instance_exact_all(
    state: &AppState,
    target: &str,
    expected_classes: &[String],
) -> Vec<(String, String)> {
    let needle = normalize_match(target);
    if needle.is_empty() || expected_classes.is_empty() {
        return Vec::new();
    }
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut predicates = vec![moose::RDFS_LABEL.to_string()];
    for class_iri in expected_classes {
        let label_pred = class_label_mirror_property_iri(&state.store, &state.capture, class_iri);
        if !predicates.iter().any(|p| p == &label_pred) {
            predicates.push(label_pred);
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for pred_iri in predicates {
        let Ok(pred) = NamedNodeRef::new(&pred_iri) else {
            continue;
        };
        for q in state
            .store
            .quads_for_pattern(None, Some(pred), None, Some(GraphNameRef::NamedNode(graph)))
            .flatten()
        {
            let Term::Literal(lit) = &q.object else {
                continue;
            };
            if normalize_match(lit.value()) != needle {
                continue;
            }
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                let iri = s.as_str().to_string();
                if out.iter().any(|(existing, _)| existing == &iri) {
                    continue;
                }
                let asserted = asserted_project_types(state, s);
                if any_subclass_of(&state.store, &asserted, expected_classes) {
                    let matched_class = asserted
                        .iter()
                        .find(|class| {
                            any_subclass_of(
                                &state.store,
                                std::slice::from_ref(*class),
                                expected_classes,
                            )
                        })
                        .cloned()
                        .unwrap_or_else(|| asserted.first().cloned().unwrap_or_default());
                    out.push((iri, matched_class));
                }
            }
        }
    }
    out
}

/// Lightweight normalization for exact-ish anchor matching: collapse whitespace and
/// lowercase. Deliberately simpler than MOOSE's entity normalizer — anchoring wants
/// a high-precision exact match, not fuzzy recall (that is the dense channel's job).
fn normalize_match(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Rank candidate expansion neighbors in place before the `EXPAND_MAX` budget is
/// spent, so the links that survive the cap are the answer-completing ones rather
/// than whatever order `record_neighbors` yielded. Primary key: a typed-edge
/// priority tier ([`edge_priority`]); secondary: dense similarity of the neighbor to
/// the query topic (`None` query embedding → all 0.0, so ranking is edge-tier then
/// IRI); final tie-break: IRI, for determinism.
fn rank_neighbors(
    state: &AppState,
    query_emb: Option<&[f32]>,
    candidates: &mut [(String, String, String)],
) {
    let sims: std::collections::HashMap<String, f32> = match query_emb {
        Some(q) => {
            let iris: Vec<&str> = candidates.iter().map(|(_, iri, _)| iri.as_str()).collect();
            state
                .instance_store
                .score_candidates(q, &iris, None)
                .map(|scores| scores.into_iter().map(|s| (s.iri, s.cosine)).collect())
                .unwrap_or_default()
        }
        None => std::collections::HashMap::new(),
    };
    candidates.sort_by(|a, b| {
        let sa = sims.get(&a.1).copied().unwrap_or(0.0);
        let sb = sims.get(&b.1).copied().unwrap_or(0.0);
        edge_priority(&a.0)
            .cmp(&edge_priority(&b.0))
            .then(sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.1.cmp(&b.1))
    });
}

/// Object-property local names whose edges rank highest for graph-walk expansion —
/// the *why* and *what it touches* of a decision/constraint/lesson outrank
/// structural/containment edges. Host-side domain policy, kept out of MOOSE core
/// (which stays domain-neutral, invariant #11). Every name here is an object
/// property declared in the architecture/engineering SHACL shapes; the
/// `priority_edges_are_all_in_catalogue` test asserts each appears in the
/// [`RelationCatalogue`], so an ontology rename can't silently break ranking.
pub(crate) const PRIORITY_EDGES: &[&str] = &[
    "isMotivatedBy",
    "violates",
    "supersedes",
    "constrains",
    "concerns",
    "learnedFrom",
    "resultsIn",
    "weighs",
    "dependsOn",
];

/// Host-side priority tier for a typed edge (lower = expand first); see
/// [`PRIORITY_EDGES`].
pub(crate) fn edge_priority(predicate_local: &str) -> u8 {
    if PRIORITY_EDGES.contains(&predicate_local) {
        0
    } else {
        1
    }
}

pub(crate) fn list_instances(
    store: &Store,
    class_iris: &[String],
    limit: usize,
) -> Vec<(String, String)> {
    let rdf_type = NamedNodeRef::new_unchecked(moose::RDF_TYPE);
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut out = Vec::new();
    for class_iri in class_iris {
        let Ok(class) = NamedNodeRef::new(class_iri) else {
            continue;
        };
        for q in store
            .quads_for_pattern(
                None,
                Some(rdf_type),
                Some(class.into()),
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                out.push((s.as_str().to_string(), class_iri.clone()));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

fn count_instances(store: &Store, class_iris: &[String]) -> usize {
    let rdf_type = NamedNodeRef::new_unchecked(moose::RDF_TYPE);
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut subjects = HashSet::new();
    for class_iri in class_iris {
        let Ok(class) = NamedNodeRef::new(class_iri) else {
            continue;
        };
        for quad in store
            .quads_for_pattern(
                None,
                Some(rdf_type),
                Some(class.into()),
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = &quad.subject {
                subjects.insert(subject.as_str().to_string());
            }
        }
    }
    subjects.len()
}

/// Fetch an instance's label, literal properties, and relations from the project
/// KG graph. Object-valued edges (e.g. `supersedes`, `hasRationale`) are surfaced
/// as `(local-name, target-IRI)` so the lifecycle chain is visible and walkable;
/// the linked `Rationale`'s text (the *why*) is dereferenced inline, and a
/// retired record also gets a `supersededBy` back-link to what replaced it.
fn build_context_item(state: &AppState, iri: String, class_iri: String) -> ContextItem {
    let store = &state.store;
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut label = String::new();
    let mut properties: Vec<(String, String)> = Vec::new();
    let mut rationale_iri: Option<String> = None;

    if let Ok(subject) = NamedNodeRef::new(&iri) {
        for q in store
            .quads_for_pattern(
                Some(subject.into()),
                None,
                None,
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            let pred = q.predicate.as_str();
            if pred == moose::RDF_TYPE {
                continue;
            }
            match &q.object {
                Term::Literal(lit) if pred == moose::RDFS_LABEL => {
                    label = lit.value().to_string();
                }
                Term::Literal(lit) => {
                    properties.push((local_name(pred).to_string(), lit.value().to_string()));
                }
                Term::NamedNode(obj) => {
                    let pname = local_name(pred);
                    if pname == "hasRationale" {
                        rationale_iri = Some(obj.as_str().to_string());
                    }
                    properties.push((pname.to_string(), obj.as_str().to_string()));
                }
                _ => {}
            }
        }
    }

    // Surface the rationale *text* (the why), not just the link to its node.
    if let Some(rat) = &rationale_iri {
        if let Some(text) = first_literal(store, rat, &state.capture.description) {
            properties.push(("rationale".to_string(), text));
        }
    }

    // For a retired record, surface what replaced it (inverse `supersedes`).
    let is_historical = properties
        .iter()
        .any(|(k, v)| k == "hasLifecycleStatus" && is_retired(v));
    if is_historical {
        if let (Ok(subject), Ok(pred)) = (
            NamedNodeRef::new(&iri),
            state.resolve_object_property("supersedes"),
        ) {
            if let Ok(pred_ref) = NamedNodeRef::new(&pred) {
                for q in store
                    .quads_for_pattern(
                        None,
                        Some(pred_ref),
                        Some(subject.into()),
                        Some(GraphNameRef::NamedNode(graph)),
                    )
                    .flatten()
                {
                    if let oxigraph::model::NamedOrBlankNode::NamedNode(s) = &q.subject {
                        properties.push(("supersededBy".to_string(), s.as_str().to_string()));
                    }
                }
            }
        }
    }

    ContextItem {
        iri,
        kind: local_name(&class_iri).to_string(),
        label,
        properties,
    }
}

/// First literal object of `(subject, predicate, *)` in the project graph, if any.
pub(crate) fn first_literal(
    store: &Store,
    subject_iri: &str,
    predicate_iri: &str,
) -> Option<String> {
    let subject = NamedNodeRef::new(subject_iri).ok()?;
    let predicate = NamedNodeRef::new(predicate_iri).ok()?;
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .find_map(|q| match q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
}
