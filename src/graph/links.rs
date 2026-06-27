//! Link suggestion and under-linked-record advisory.
//! Suggestions reuse retrieval plus the relation catalogue but never write edges.

use moose::traits::LlmClient;
use moose::types::LlmAssistLevel;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};

use super::capture::require_information_record;
use super::context::{edge_priority, first_literal};
use super::relations::{EdgeDirection, LegalEdge};
use super::state::AppState;
use super::util::local_name;
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

/// A record that the warning-severity shapes say SHOULD carry a link it currently
/// lacks.
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

/// Records that emitted SHACL Warning results for declarative SHOULD-link
/// constraints. Drives both the non-blocking validate advisory and the
/// `suggest_links` scan. Bounded by `max_records`.
pub fn under_linked_from_report(
    state: &AppState,
    report: &moose::shacl::ShaclReport,
    max_records: usize,
) -> Vec<UnderLinked> {
    let mut out: Vec<UnderLinked> = Vec::new();
    for warning in &report.violations {
        if warning.severity != moose::shacl::ShaclSeverity::Warning {
            continue;
        }
        let Some(path) = &warning.result_path else {
            continue;
        };
        if is_superseded_or_deprecated(state, &warning.focus_node) {
            continue;
        }
        let Some(class_local) = focus_node_class_local(state, &warning.focus_node) else {
            continue;
        };
        out.push(UnderLinked {
            iri: warning.focus_node.clone(),
            class_local,
            missing_predicate: local_name(path).to_string(),
        });
        if out.len() >= max_records {
            return out;
        }
    }
    out
}

/// Records the warning-severity shapes say SHOULD carry a link. This path runs
/// SNARL for the `suggest_links` scan; validate reuses its already-built report.
pub fn under_linked_records(state: &AppState, max_records: usize) -> Vec<UnderLinked> {
    match crate::validation::run_project_shacl(state) {
        Ok(report) => under_linked_from_report(state, &report, max_records),
        Err(_) => Vec::new(),
    }
}

fn is_superseded_or_deprecated(state: &AppState, iri: &str) -> bool {
    let (Ok(subject), Ok(status)) = (
        NamedNodeRef::new(iri),
        NamedNodeRef::new(&state.capture.status),
    ) else {
        return false;
    };
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(status),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .any(|quad| match quad.object {
            Term::Literal(literal) => {
                let status = literal.value();
                status == "superseded" || status == "deprecated"
            }
            _ => false,
        })
}

fn focus_node_class_local(state: &AppState, iri: &str) -> Option<String> {
    let subject = NamedNodeRef::new(iri).ok()?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE).ok()?;
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(rdf_type),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .find_map(|quad| match quad.object {
            Term::NamedNode(class) => Some(local_name(class.as_str()).to_string()),
            _ => None,
        })
}
