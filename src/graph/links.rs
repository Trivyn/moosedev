//! Link suggestion and under-linked-record advisory.
//! Suggestions reuse retrieval plus the relation catalogue but never write edges.

use moose::traits::LlmClient;
use moose::types::LlmAssistLevel;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef};
use oxigraph::sparql::QueryResults;

use crate::ontology;

use super::capture::require_information_record;
use super::context::{edge_priority, first_literal};
use super::relations::{EdgeDirection, LegalEdge};
use super::state::AppState;
use super::util::{
    iri_value, local_name, run_sparql, RDF_FIRST, RDF_REST, SH_OR, SH_PATH, SH_TARGET_CLASS,
};
use super::PROJECT_KG_GRAPH_IRI;

// ============================================================================
// Link suggester — symbolic-first, suggest-only (invariants #1, #4, #6)
//
// Candidate generation is the hybrid retriever; legality is the SHACL relation
// catalogue; the LLM is at most a gated tiebreaker. Nothing here writes: it
// returns ranked legal candidates the agent confirms via `relate`/inline
// relations. Co-located with `relevant_context` because it reuses the same
// retrieval, neighbor, and catalogue primitives.
// ============================================================================

/// Lifecycle object properties owned by `supersede`/`retract` (and their inverses)
/// — legal between any record pair, but never *suggested*: they record decision
/// evolution, not an abductive semantic link.
const LIFECYCLE_PREDICATES: &[&str] = &[
    "supersedes",
    "isSupersededBy",
    "hasRationale",
    "isRationaleFor",
];

/// A candidate link from the suggester: a legal, currently-unasserted edge to a
/// similar record. Suggest-only — the agent confirms it through the validated
/// `relate` path (or inline relations); [`LinkSuggestion::confirm`] yields the
/// exact `relate` arguments.
#[derive(Debug, Clone)]
pub struct LinkSuggestion {
    pub predicate_local: String,
    /// The edge's subject IRI (orientation already resolved from the direction).
    pub subject_iri: String,
    /// The edge's object IRI.
    pub object_iri: String,
    /// Title + class of each endpoint, stored in subject→object order so a rendered
    /// suggestion reads in the *same* direction as the `relate` it will create —
    /// even for an Inverse-direction predicate, where the seed record is the object.
    /// (Rendering only the candidate with a leading arrow misled readers into seeing
    /// the reverse, illegal orientation.)
    pub subject_title: String,
    pub subject_kind: String,
    pub object_title: String,
    pub object_kind: String,
    /// Relevance-derived rank score (higher = stronger).
    pub score: f32,
}

impl LinkSuggestion {
    /// Exact `(subject_iri, predicate_local, object_iri)` arguments for
    /// [`relate`] that assert this suggested edge.
    pub fn confirm(&self) -> (String, String, String) {
        (
            self.subject_iri.clone(),
            self.predicate_local.clone(),
            self.object_iri.clone(),
        )
    }
}

/// A record that the shapes say SHOULD carry a link it currently lacks.
#[derive(Debug, Clone)]
pub struct UnderLinked {
    pub iri: String,
    pub class_local: String,
    pub missing_predicate: String,
}

/// Whether the gated LLM predicate tiebreak runs (default OFF — symbolic-first).
fn llm_tiebreak_enabled(level: LlmAssistLevel) -> bool {
    matches!(
        level,
        LlmAssistLevel::AssistedValidation | LlmAssistLevel::FallbackExecutor
    )
}

/// True if any object-property edge (excluding `rdf:type`) already connects the two
/// records in the project graph, in either direction — so the suggester never
/// re-proposes an existing link.
fn record_pair_linked(state: &AppState, a: &str, b: &str) -> bool {
    object_edge_exists(state, a, b) || object_edge_exists(state, b, a)
}

fn object_edge_exists(state: &AppState, subject: &str, object: &str) -> bool {
    let (Ok(s), Ok(o)) = (NamedNodeRef::new(subject), NamedNodeRef::new(object)) else {
        return false;
    };
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(s.into()),
            None,
            Some(o.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .any(|q| q.predicate.as_str() != moose::RDF_TYPE)
}

/// Choose the best legal *semantic* predicate for an ordered class pair (lifecycle
/// predicates excluded). Symbolic order is priority-tier then name; a gated LLM
/// tiebreak (default OFF) may reorder among the already-legal options when more
/// than one fits — it can never introduce a predicate. `None` ⇒ no semantic
/// predicate is legal, so prefer not-suggesting (anti-confabulation).
async fn pick_predicate(
    state: &AppState,
    iri: &str,
    hit_iri: &str,
    legal: &[LegalEdge],
    prioritize: Option<&str>,
) -> Option<LegalEdge> {
    let mut semantic: Vec<LegalEdge> = legal
        .iter()
        .filter(|e| !LIFECYCLE_PREDICATES.contains(&e.predicate_local.as_str()))
        .cloned()
        .collect();
    if semantic.is_empty() {
        return None;
    }
    // Gap-targeting: when the caller is filling a specific missing predicate and it
    // is legal for this candidate, use it — so a scan for a record "missing
    // isMotivatedBy" surfaces isMotivatedBy candidates, not just the most
    // lexically-similar legal link.
    if let Some(want) = prioritize {
        if let Some(edge) = semantic.iter().find(|e| e.predicate_local == want) {
            return Some(edge.clone());
        }
    }
    semantic.sort_by(|a, b| {
        edge_priority(&a.predicate_local)
            .cmp(&edge_priority(&b.predicate_local))
            .then_with(|| a.predicate_local.cmp(&b.predicate_local))
    });
    if semantic.len() > 1 && llm_tiebreak_enabled(state.engine_config.llm_assist_level) {
        if let Some(chosen) = llm_pick_predicate(state, iri, hit_iri, &semantic).await {
            return Some(chosen);
        }
    }
    semantic.into_iter().next()
}

/// Gated LLM tiebreak: ask the in-process sensor which single legal predicate (if
/// any) best holds between the two records. Reorders only among `candidates`; a
/// miss/"none"/error returns `None` so the caller keeps the symbolic top.
async fn llm_pick_predicate(
    state: &AppState,
    iri: &str,
    hit_iri: &str,
    candidates: &[LegalEdge],
) -> Option<LegalEdge> {
    let a = first_literal(&state.store, iri, moose::RDFS_LABEL)?;
    let a_desc = first_literal(&state.store, iri, &state.capture.description).unwrap_or_default();
    let b = first_literal(&state.store, hit_iri, moose::RDFS_LABEL)?;
    let b_desc =
        first_literal(&state.store, hit_iri, &state.capture.description).unwrap_or_default();
    let options: Vec<&str> = candidates
        .iter()
        .map(|e| e.predicate_local.as_str())
        .collect();
    let prompt = format!(
        "Two software-project records:\n\
         A: \"{a}\" — {a_desc}\n\
         B: \"{b}\" — {b_desc}\n\n\
         Which ONE of these typed relationships best holds between A and B, if any?\n\
         Options: {}.\n\
         Reply with exactly one option name, or \"none\" if no relationship clearly holds.",
        options.join(", ")
    );
    let reply = state
        .llm
        .chat_completion(&state.model, &prompt, None)
        .await
        .ok()?
        .trim()
        .to_lowercase();
    candidates
        .iter()
        .find(|e| reply.contains(&e.predicate_local.to_lowercase()))
        .cloned()
}

/// Rank legal, currently-unasserted links from `iri` to records similar to it.
/// Symbolic candidate generation (hybrid retrieval) + symbolic legality (the SHACL
/// catalogue); self, already-linked pairs, and candidates with no legal semantic
/// predicate are dropped (prefer not-suggesting). Suggest-only — writes nothing.
pub async fn suggest_links_for_record(
    state: &AppState,
    iri: &str,
    top_n: usize,
    floor: Option<f32>,
    prioritize: Option<&str>,
) -> Vec<LinkSuggestion> {
    let Ok(subject) = NamedNode::new(iri) else {
        return Vec::new();
    };
    let Ok(class_iri) = require_information_record(state, &subject) else {
        return Vec::new();
    };
    let seed_text = state.record_embed_text(iri);
    if seed_text.trim().is_empty() {
        return Vec::new();
    }

    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [PROJECT_KG_GRAPH_IRI.to_string()];
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    // Over-fetch so the legality / already-linked filters still leave enough.
    let want = (top_n * 4).max(10);
    let ranked: Vec<(usize, String, String)> = state
        .entity_index
        .search_records_hybrid(
            &seed_text,
            &class_iris,
            &state.store,
            &data_graphs,
            &text_fields,
            want,
            &state.instance_store,
            floor,
        )
        .into_iter()
        .enumerate()
        .map(|(rank, h)| (rank, h.iri, h.class_iri))
        .collect();

    let mut suggestions: Vec<LinkSuggestion> = Vec::new();
    for (rank, hit_iri, hit_class) in ranked {
        if hit_iri == iri || record_pair_linked(state, iri, &hit_iri) {
            continue;
        }
        let legal = state
            .catalogue
            .legal_predicates(&state.store, &class_iri, &hit_class);
        let Some(edge) = pick_predicate(state, iri, &hit_iri, &legal, prioritize).await else {
            continue;
        };
        // Describe each endpoint as (iri, title, kind), then orient by direction so
        // subject/object match the legal edge — the seed record is the *object* for
        // an Inverse predicate (e.g. Lesson -learnedFrom-> this decision).
        let describe = |node: &str, class: &str| {
            (
                node.to_string(),
                first_literal(&state.store, node, moose::RDFS_LABEL)
                    .unwrap_or_else(|| node.to_string()),
                local_name(class).to_string(),
            )
        };
        let current = describe(iri, &class_iri);
        let hit = describe(&hit_iri, &hit_class);
        let (subject, object) = match edge.direction {
            EdgeDirection::Forward => (current, hit),
            EdgeDirection::Inverse => (hit, current),
        };
        suggestions.push(LinkSuggestion {
            predicate_local: edge.predicate_local,
            subject_iri: subject.0,
            object_iri: object.0,
            subject_title: subject.1,
            subject_kind: subject.2,
            object_title: object.1,
            object_kind: object.2,
            score: 1.0 / (1.0 + rank as f32),
        });
    }
    // Prioritized predicate (the record's missing link) first, then typed-edge
    // priority, then similarity, then IRI for determinism.
    let prio_rank = |p: &str| -> u8 {
        match prioritize {
            Some(want) if want == p => 0,
            _ => 1,
        }
    };
    suggestions.sort_by(|a, b| {
        prio_rank(&a.predicate_local)
            .cmp(&prio_rank(&b.predicate_local))
            .then(edge_priority(&a.predicate_local).cmp(&edge_priority(&b.predicate_local)))
            .then(
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| a.object_iri.cmp(&b.object_iri))
    });
    suggestions.truncate(top_n);
    suggestions
}

/// The `sh:or` "should-have-a-link" requirements: each NodeShape with an `sh:or`
/// maps its `sh:targetClass` to the branch predicates a conforming record SHOULD
/// carry at least one of. The declarative source of truth for the link advisory.
fn shacl_or_link_requirements(state: &AppState) -> Vec<(String, Vec<String>)> {
    let sparql = format!(
        r#"
SELECT DISTINCT ?targetClass ?predicate
WHERE {{
  VALUES ?shapeGraph {{ <{}> <{}> }}
  GRAPH ?shapeGraph {{
    ?shape <{}> ?targetClass ;
           <{}>/<{}>*/<{}> ?branch .
    ?branch <{}> ?predicate .
  }}
}}"#,
        ontology::SE_SHAPES_GRAPH_IRI,
        ontology::ARCH_SHAPES_GRAPH_IRI,
        SH_TARGET_CLASS,
        SH_OR,
        RDF_REST,
        RDF_FIRST,
        SH_PATH,
    );
    let Ok(QueryResults::Solutions(solutions)) = run_sparql(&state.store, &sparql) else {
        return Vec::new();
    };
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for sol in solutions.flatten() {
        let (Some(target_class), Some(predicate)) = (
            iri_value(sol.get("targetClass")),
            iri_value(sol.get("predicate")),
        ) else {
            continue;
        };
        if let Some((_, preds)) = groups.iter_mut().find(|(c, _)| c == &target_class) {
            if !preds.contains(&predicate) {
                preds.push(predicate);
            }
        } else {
            groups.push((target_class, vec![predicate]));
        }
    }
    groups
}

/// Records the shapes say SHOULD carry a link (an `sh:or` branch predicate) but
/// currently lack every one of those predicates. Drives the non-blocking validate
/// advisory and the `suggest_links` scan. Bounded by `max_records`.
pub fn under_linked_records(state: &AppState, max_records: usize) -> Vec<UnderLinked> {
    let mut out: Vec<UnderLinked> = Vec::new();
    for (target_class, predicates) in shacl_or_link_requirements(state) {
        let not_exists: String = predicates
            .iter()
            .enumerate()
            .map(|(i, p)| format!("    FILTER NOT EXISTS {{ ?node <{p}> ?v{i} }}\n"))
            .collect();
        // Records are typed directly as these leaf classes, so an exact `rdf:type`
        // match in the project graph suffices (no cross-graph subclass path).
        // Exclude superseded/deprecated records: like every other read, the advisory
        // concerns the current working set, not history.
        let sparql = format!(
            "SELECT DISTINCT ?node\nWHERE {{\n  GRAPH <{}> {{\n    ?node <{}> <{}> .\n{}    FILTER NOT EXISTS {{ ?node <{}> ?st . FILTER(STR(?st) = \"superseded\" || STR(?st) = \"deprecated\") }}\n  }}\n}}",
            PROJECT_KG_GRAPH_IRI,
            moose::RDF_TYPE,
            target_class,
            not_exists,
            state.capture.status
        );
        let Ok(QueryResults::Solutions(solutions)) = run_sparql(&state.store, &sparql) else {
            continue;
        };
        for sol in solutions.flatten() {
            if let Some(node) = iri_value(sol.get("node")) {
                out.push(UnderLinked {
                    iri: node,
                    class_local: local_name(&target_class).to_string(),
                    missing_predicate: local_name(&predicates[0]).to_string(),
                });
                if out.len() >= max_records {
                    return out;
                }
            }
        }
    }
    out
}
