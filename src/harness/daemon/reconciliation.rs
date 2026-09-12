//! Durable, model-independent reconciliation of capture candidates.
//!
//! Candidate retrieval may nominate an existing record. It never asserts
//! semantic equivalence: that disposition is journaled separately and, for
//! reuse, remains an explicit human review obligation.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    current_status, operation_path, save_operation, validate_owner_id, Operation, OPERATIONS,
};
use crate::api::error::ApiError;
use crate::graph::{self, AppState, EdgeDirection, PROJECT_KG_GRAPH_IRI};
use crate::harness::protocol::*;

const MAX_CANDIDATES: usize = 20;
const MAX_EXACT_CANDIDATES: usize = 100;
const MAX_OPERATION_JOURNALS: usize = 10_000;
const MAX_ASSERTIONS_PER_CANDIDATE: usize = 256;
const MAX_ASSERTION_BYTES: usize = 32 * 1024;
const MAX_CANDIDATE_ASSERTION_BYTES: usize = 64 * 1024;
const MAX_CANDIDATE_PAGE_BYTES: usize = 96 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct ReconciliationOperation {
    request: ReconcileCaptureRequest,
    response: ReconcileCaptureResponse,
    #[serde(default)]
    review: Option<bool>,
}

pub async fn candidates(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureCandidateRequest>,
) -> Result<Json<CaptureCandidatePage>, ApiError> {
    Ok(Json(candidate_page(&state, &request)?))
}

pub fn candidate_page(
    state: &AppState,
    request: &CaptureCandidateRequest,
) -> anyhow::Result<CaptureCandidatePage> {
    validate_owner_id(&request.owner_id)?;
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
        proposal_digest: json_digest(&request.proposal)?,
        candidates,
        next_cursor,
    })
}

pub async fn reconcile(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ReconcileCaptureRequest>,
) -> Result<Json<ReconcileCaptureResponse>, ApiError> {
    Ok(Json(reconcile_operation(&state, request)?))
}

pub fn reconcile_operation(
    state: &AppState,
    request: ReconcileCaptureRequest,
) -> anyhow::Result<ReconcileCaptureResponse> {
    let _guard = OPERATIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("harness operation lock poisoned"))?;
    validate_owner_id(&request.owner_id)?;
    validate_operation_id(&request.operation_id)?;
    anyhow::ensure!(
        !request.rationale.trim().is_empty(),
        "reconciliation needs a rationale"
    );
    let path = reconciliation_path(state, &request.operation_id)?;
    if path.exists() {
        let stored: ReconciliationOperation = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            serde_json::to_value(&stored.request)? == serde_json::to_value(&request)?,
            "operation_id was already used for a different reconciliation"
        );
        return Ok(stored.response);
    }
    let _proposal_guard = state.lock_proposal_writes()?;
    anyhow::ensure!(
        request.candidate_revision == project_assertion_revision(state)?,
        "candidate snapshot is stale; retrieve candidates again"
    );
    let candidate_class =
        graph::require_information_record(state, &NamedNode::new(&request.candidate_iri)?)?;
    let (_, _, _, digest) = candidate_assertions(state, &request.candidate_iri)?;
    anyhow::ensure!(
        digest == request.candidate_digest,
        "candidate assertions changed; retrieve candidates again"
    );
    let status = current_status(state, &request.candidate_iri)
        .ok_or_else(|| anyhow::anyhow!("candidate record disappeared"))?;
    let origin = capture_origins(state)?.get(&request.candidate_iri).cloned();
    match request.disposition {
        CaptureDisposition::ReuseUnchanged => anyhow::ensure!(
            request.replacement_proposal.is_none(),
            "reuse cannot replace or modify the existing assertion"
        ),
        CaptureDisposition::ReviseProposal | CaptureDisposition::DistinctKnowledge => {
            let replacement = request
                .replacement_proposal
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("this disposition needs a replacement proposal"))?;
            anyhow::ensure!(
                replacement.kind == request.proposal.kind
                    && replacement.evidence == request.proposal.evidence
                    && replacement.files == request.proposal.files
                    && replacement.components == request.proposal.components
                    && replacement.requirement == request.proposal.requirement
                    && replacement.supersedes == request.proposal.supersedes
                    && replacement.retracts == request.proposal.retracts,
                "replacement may change only title and description"
            );
            if request.disposition == CaptureDisposition::ReviseProposal {
                anyhow::ensure!(
                    graph::local_name(&candidate_class) == replacement.kind,
                    "revision must preserve the pending record kind"
                );
            }
            anyhow::ensure!(
                graph::resolve_record_exact_all(state, &replacement.title)
                    .into_iter()
                    .all(|(iri, _)| {
                        (request.disposition == CaptureDisposition::ReviseProposal
                            && iri == request.candidate_iri)
                            || current_status(state, &iri).is_none_or(|status| {
                                status != "proposed" && !graph::in_working_set(&status)
                            })
                    }),
                "replacement title still collides with current or pending knowledge"
            );
        }
    }
    let pending_capture_operation = match request.disposition {
        CaptureDisposition::ReuseUnchanged => match status.as_str() {
            "accepted" => None,
            "proposed" => {
                let owned = origin
                    .as_ref()
                    .filter(|origin| origin.owner_id == request.owner_id);
                anyhow::ensure!(
                    owned.is_some(),
                    "pending candidate belongs to another operation and cannot be reused"
                );
                owned.map(|origin| origin.operation_id.clone())
            }
            _ => anyhow::bail!("only accepted or task-owned pending knowledge can be reused"),
        },
        CaptureDisposition::ReviseProposal => {
            anyhow::ensure!(
                status == "proposed"
                    && origin
                        .as_ref()
                        .is_some_and(|origin| origin.owner_id == request.owner_id),
                "only a task-owned pending proposal can be revised"
            );
            origin.map(|origin| origin.operation_id)
        }
        CaptureDisposition::DistinctKnowledge => None,
    };
    let response = ReconcileCaptureResponse {
        operation_id: request.operation_id.clone(),
        disposition: request.disposition.clone(),
        candidate_iri: request.candidate_iri.clone(),
        requires_human_review: request.disposition == CaptureDisposition::ReuseUnchanged,
        pending_capture_operation,
        review: None,
    };
    save_operation(
        &path,
        &ReconciliationOperation {
            request,
            response: response.clone(),
            review: None,
        },
    )?;
    Ok(response)
}

pub async fn review(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ReconcileReviewRequest>,
) -> Result<Json<ReconcileCaptureResponse>, ApiError> {
    Ok(Json(review_operation(&state, &request)?))
}

pub fn review_operation(
    state: &AppState,
    request: &ReconcileReviewRequest,
) -> anyhow::Result<ReconcileCaptureResponse> {
    let _guard = OPERATIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("harness operation lock poisoned"))?;
    let path = reconciliation_path(state, &request.operation_id)?;
    let mut operation: ReconciliationOperation = serde_json::from_slice(&std::fs::read(&path)?)?;
    anyhow::ensure!(
        operation.response.requires_human_review,
        "this reconciliation has no human review obligation"
    );
    anyhow::ensure!(
        operation.review.is_none_or(|prior| prior == request.accept),
        "this reconciliation already has a different review decision"
    );
    if operation.review.is_some() {
        return Ok(operation.response);
    }
    let _proposal_guard = state.lock_proposal_writes()?;
    let (_, _, _, digest) = candidate_assertions(state, &operation.request.candidate_iri)?;
    anyhow::ensure!(
        digest == operation.request.candidate_digest,
        "candidate assertions changed before reconciliation review; retrieve and decide again"
    );
    if current_status(state, &operation.request.candidate_iri).as_deref() == Some("proposed") {
        let origin = capture_origins(state)?
            .remove(&operation.request.candidate_iri)
            .ok_or_else(|| anyhow::anyhow!("pending candidate no longer has a proven owner"))?;
        anyhow::ensure!(
            origin.owner_id == operation.request.owner_id,
            "pending candidate belongs to another harness owner"
        );
    }
    operation.review = Some(request.accept);
    operation.response.review = Some(request.accept);
    save_operation(&path, &operation)?;
    Ok(operation.response)
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
    Ok((
        title,
        literals,
        relations,
        format!("{:x}", Sha256::digest(canonical.join("\n").as_bytes())),
    ))
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
        let Some(owner_id) = operation.owner_id else {
            continue;
        };
        for entry in operation.entries {
            origins.insert(
                entry.response.iri,
                CandidateOrigin {
                    operation_id: operation.request.operation_id.clone(),
                    owner_id: owner_id.clone(),
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
    Ok(format!(
        "{:x}",
        Sha256::digest(assertions.join("\n").as_bytes())
    ))
}

pub(super) fn authorize_capture(
    state: &AppState,
    request: &CaptureV2Request,
) -> anyhow::Result<()> {
    let mut used = BTreeSet::new();
    let mut authorized_proposals = BTreeSet::new();
    for id in &request.reconciliation_operation_ids {
        anyhow::ensure!(used.insert(id), "duplicate reconciliation operation ID");
        let operation: ReconciliationOperation =
            serde_json::from_slice(&std::fs::read(reconciliation_path(state, id)?)?)?;
        anyhow::ensure!(
            operation.request.owner_id == request.owner_id,
            "reconciliation operation belongs to another harness owner"
        );
        anyhow::ensure!(
            matches!(
                operation.request.disposition,
                CaptureDisposition::ReviseProposal | CaptureDisposition::DistinctKnowledge
            ),
            "reuse reconciliation cannot authorize a new capture"
        );
        let replacement = operation
            .request
            .replacement_proposal
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("reconciliation has no replacement proposal"))?;
        let matches = request
            .proposals
            .iter()
            .enumerate()
            .filter_map(|(index, proposal)| {
                (serde_json::to_value(proposal).ok() == serde_json::to_value(replacement).ok())
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() == 1,
            "reconciliation must authorize exactly one submitted proposal"
        );
        anyhow::ensure!(
            authorized_proposals.insert(matches[0]),
            "multiple reconciliation receipts authorize the same proposal"
        );
        match operation.request.disposition {
            CaptureDisposition::DistinctKnowledge => {
                anyhow::ensure!(
                    operation.request.candidate_revision == project_assertion_revision(state)?,
                    "distinct-knowledge candidate snapshot changed before capture"
                );
                let (_, _, _, digest) =
                    candidate_assertions(state, &operation.request.candidate_iri)?;
                anyhow::ensure!(
                    digest == operation.request.candidate_digest,
                    "distinct-knowledge candidate changed before capture"
                );
            }
            CaptureDisposition::ReviseProposal => {
                let origin = operation
                    .response
                    .pending_capture_operation
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("revision has no originating capture"))?;
                let original: Operation =
                    serde_json::from_slice(&std::fs::read(operation_path(state, origin)?)?)?;
                anyhow::ensure!(
                    original.owner_id.as_deref() == Some(request.owner_id.as_str())
                        && original.review == Some(false)
                        && original.reviewed
                        && current_status(state, &operation.request.candidate_iri).as_deref()
                            == Some("rejected"),
                    "original owned capture must be wholly rejected before revision"
                );
            }
            CaptureDisposition::ReuseUnchanged => unreachable!(),
        }
    }
    Ok(())
}

fn json_digest(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn validate_operation_id(id: &str) -> anyhow::Result<()> {
    validate_owner_id(id).map_err(|_| anyhow::anyhow!("invalid operation_id"))
}

fn reconciliation_path(state: &AppState, id: &str) -> anyhow::Result<PathBuf> {
    validate_operation_id(id)?;
    Ok(state
        .data_dir
        .join("harness/operations")
        .join(format!("{id}.reconcile.json")))
}
