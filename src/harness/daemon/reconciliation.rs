//! Snapshot-bound candidate retrieval for `reconcile_score`: the records a
//! fresh proposal may restate or refine, with their complete assertions.
//! Retrieval nominates; it never asserts semantic equivalence.

use std::collections::{BTreeSet, HashMap};

use crate::harness::digest::{sha256_hex, sha256_json};
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};

use super::journal::validate_id;
use super::{current_status, Operation};
use crate::graph::{self, AppState, EdgeDirection, PROJECT_KG_GRAPH_IRI};
use crate::harness::protocol::*;

const MAX_CANDIDATES: usize = 20;
const MAX_EXACT_CANDIDATES: usize = 100;
const MAX_OPERATION_JOURNALS: usize = 10_000;
const MAX_ASSERTIONS_PER_CANDIDATE: usize = 256;
const MAX_ASSERTION_BYTES: usize = 32 * 1024;
const MAX_CANDIDATE_ASSERTION_BYTES: usize = 64 * 1024;
const MAX_CANDIDATE_PAGE_BYTES: usize = 96 * 1024;

pub fn candidate_page(
    state: &AppState,
    request: &CaptureCandidateRequest,
) -> anyhow::Result<CaptureCandidatePage> {
    validate_id(&request.owner_id, "owner_id")?;
    anyhow::ensure!(
        !request.proposal.title.trim().is_empty(),
        "candidate lookup needs a proposal title"
    );
    let limit = request.limit.unwrap_or(8).clamp(1, MAX_CANDIDATES);
    let generation = state.project_write_generation();
    let revision = project_assertion_revision(state)?;
    let offset = match request.cursor.as_deref() {
        None => 0,
        Some(cursor) => {
            let (offset, bound_revision) = cursor
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("invalid candidate cursor"))?;
            anyhow::ensure!(
                bound_revision == revision,
                "candidate cursor belongs to a stale snapshot"
            );
            offset
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("invalid candidate cursor"))?
        }
    };
    let exact: BTreeSet<String> = graph::resolve_record_exact_all(state, &request.proposal.title)
        .into_iter()
        .map(|(iri, _)| iri)
        .collect();
    anyhow::ensure!(
        exact.len() <= MAX_EXACT_CANDIDATES,
        "more than 100 records share this exact title; resolve the duplicate set outside the harness"
    );
    let topic = request
        .topic
        .as_deref()
        .filter(|topic| !topic.trim().is_empty())
        .unwrap_or(&request.proposal.title);
    let mut iris: Vec<String> = exact.iter().cloned().collect();
    for item in graph::relevant_context_snapshot(state, Some(topic), 100, true)? {
        if !iris.contains(&item.iri) {
            iris.push(item.iri);
        }
    }
    let origins = capture_origins(state)?;
    let proposal_class = state.resolve_class(&request.proposal.kind)?;
    let mut all = Vec::with_capacity(iris.len());
    for iri in iris {
        let Ok(class) = graph::require_information_record(state, &NamedNode::new(&iri)?) else {
            continue;
        };
        let (title, literals, relations, assertion_digest) = candidate_assertions(state, &iri)?;
        let origin = origins.get(&iri).cloned();
        let legal_relations = state
            .catalogue
            .legal_predicates(&state.store, &proposal_class, &class)
            .into_iter()
            .map(|edge| LegalRelationChoice {
                predicate: edge.predicate_local,
                direction: match edge.direction {
                    EdgeDirection::Forward => LegalRelationDirection::Forward,
                    EdgeDirection::Inverse => LegalRelationDirection::Inverse,
                },
            })
            .collect();
        all.push(CaptureCandidate {
            iri: iri.clone(),
            title,
            kind: graph::local_name(&class).to_string(),
            status: current_status(state, &iri).unwrap_or_else(|| "unknown".into()),
            assertion_digest,
            literals,
            relations,
            owned_by_requester: origin
                .as_ref()
                .is_some_and(|origin| origin.owner_id == request.owner_id),
            origin,
            exact_title: exact.contains(&iri),
            legal_relations,
        });
    }
    let mut end = offset.min(all.len());
    let mut page_bytes = 0usize;
    while end < all.len() && end.saturating_sub(offset) < limit {
        let candidate_bytes = serde_json::to_vec(&all[end])?.len();
        if end > offset && page_bytes.saturating_add(candidate_bytes) > MAX_CANDIDATE_PAGE_BYTES {
            break;
        }
        page_bytes = page_bytes.saturating_add(candidate_bytes);
        end += 1;
    }
    let candidates = all.get(offset..end).unwrap_or_default().to_vec();
    let next_cursor = (end < all.len()).then(|| format!("{end}:{revision}"));
    anyhow::ensure!(
        generation == state.project_write_generation()
            && revision == project_assertion_revision(state)?,
        "project knowledge changed during candidate lookup; retry"
    );
    Ok(CaptureCandidatePage {
        revision,
        proposal_digest: sha256_json(&request.proposal)?,
        candidates,
        next_cursor,
    })
}

pub(super) fn candidate_assertions(
    state: &AppState,
    iri: &str,
) -> anyhow::Result<(
    String,
    Vec<CandidateLiteral>,
    Vec<CandidateRelation>,
    String,
)> {
    graph::require_information_record(state, &NamedNode::new(iri)?)?;
    let subject = NamedNodeRef::new(iri)?;
    let graph_name = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let mut canonical = Vec::new();
    let mut canonical_bytes = 0usize;
    let mut assertion_count = 0usize;
    let mut literals = Vec::new();
    let mut relations = Vec::new();
    let mut title = String::new();
    for quad in state
        .store
        .quads_for_pattern(Some(subject.into()), None, None, Some(graph_name))
    {
        let quad = quad?;
        let assertion = quad.to_string();
        anyhow::ensure!(
            assertion.len() <= MAX_ASSERTION_BYTES,
            "candidate contains an assertion larger than 32 KiB; inspect it outside capture reconciliation"
        );
        assertion_count = assertion_count.saturating_add(1);
        canonical_bytes = canonical_bytes.saturating_add(assertion.len());
        anyhow::ensure!(
            assertion_count <= MAX_ASSERTIONS_PER_CANDIDATE
                && canonical_bytes <= MAX_CANDIDATE_ASSERTION_BYTES,
            "candidate assertions exceed the bounded reconciliation payload; narrow or resolve this record outside the harness"
        );
        canonical.push(assertion);
        match &quad.object {
            Term::Literal(value) => {
                let predicate = graph::local_name(quad.predicate.as_str()).to_string();
                if quad.predicate.as_str() == moose::RDFS_LABEL {
                    title = value.value().to_string();
                }
                if quad.predicate.as_str() != moose::RDFS_LABEL {
                    literals.push(CandidateLiteral {
                        predicate,
                        value: value.value().to_string(),
                        datatype: Some(value.datatype().as_str().to_string()),
                        language: value.language().map(str::to_string),
                    });
                }
            }
            Term::NamedNode(target) if quad.predicate.as_str() != moose::RDF_TYPE => {
                relations.push(CandidateRelation {
                    predicate: graph::local_name(quad.predicate.as_str()).to_string(),
                    target_iri: target.as_str().to_string(),
                    incoming: false,
                });
            }
            _ => {}
        }
    }
    for quad in state
        .store
        .quads_for_pattern(None, None, Some(subject.into()), Some(graph_name))
    {
        let quad = quad?;
        let assertion = quad.to_string();
        anyhow::ensure!(
            assertion.len() <= MAX_ASSERTION_BYTES,
            "candidate contains an assertion larger than 32 KiB; inspect it outside capture reconciliation"
        );
        assertion_count = assertion_count.saturating_add(1);
        canonical_bytes = canonical_bytes.saturating_add(assertion.len());
        anyhow::ensure!(
            assertion_count <= MAX_ASSERTIONS_PER_CANDIDATE
                && canonical_bytes <= MAX_CANDIDATE_ASSERTION_BYTES,
            "candidate assertions exceed the bounded reconciliation payload; narrow or resolve this record outside the harness"
        );
        canonical.push(assertion);
        relations.push(CandidateRelation {
            predicate: graph::local_name(quad.predicate.as_str()).to_string(),
            target_iri: quad.subject.to_string(),
            incoming: true,
        });
    }
    literals.sort_by(|a, b| (&a.predicate, &a.value).cmp(&(&b.predicate, &b.value)));
    relations.sort_by(|a, b| {
        (&a.predicate, &a.target_iri, a.incoming).cmp(&(&b.predicate, &b.target_iri, b.incoming))
    });
    canonical.sort();
    Ok((title, literals, relations, sha256_hex(canonical.join("\n"))))
}

fn capture_origins(state: &AppState) -> anyhow::Result<HashMap<String, CandidateOrigin>> {
    let mut origins = HashMap::new();
    let directory = state.data_dir.join("harness/operations");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(origins);
    };
    let mut scanned = 0usize;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.trim_end_matches(".json").contains('.') {
            continue;
        }
        scanned = scanned.saturating_add(1);
        anyhow::ensure!(
            scanned <= MAX_OPERATION_JOURNALS,
            "capture ownership journal count exceeds the bounded reconciliation scan"
        );
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let operation: Operation = serde_json::from_slice(&std::fs::read(&path)?)?;
        for entry in operation.entries {
            origins.insert(
                entry.response.iri,
                CandidateOrigin {
                    operation_id: operation.request.operation_id.clone(),
                    owner_id: operation.owner_id.clone(),
                },
            );
        }
    }
    Ok(origins)
}

fn project_assertion_revision(state: &AppState) -> anyhow::Result<String> {
    let mut assertions = state
        .store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                PROJECT_KG_GRAPH_IRI,
            )?)),
        )
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|quad| quad.to_string())
        .collect::<Vec<_>>();
    assertions.sort();
    Ok(sha256_hex(assertions.join("\n")))
}
