//! Programmatic memory boundary for the harness. The model never calls these
//! routes: the runner asks for context, submits typed captures (`capture/v2`),
//! has a human review them, and reads checkpoints; `intent`, `associate` and
//! `capture_type` derive associations and typed proposals symbolically.
use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::harness::digest::sha256_hex;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Quad, Term};
use serde::{Deserialize, Serialize};

use super::protocol::*;
use crate::api::error::ApiError;
use crate::graph::{self, AppState, CaptureStamp, RecordInput, PROJECT_KG_GRAPH_IRI};
use crate::policy::{self, PolicyDecision, PolicyEvent};

pub mod associate;
pub mod capture_type;
pub mod intent;
pub mod intent_candidates;
mod journal;
pub mod reconcile_score;
pub mod reconciliation;
mod revision;

use journal::{journal_path, load, lock_operations, save_operation, validate_id};

const AUTHOR: &str = "moosedev-harness";
const REVIEWER: &str = "moosedev-harness-human";

#[derive(Serialize, Deserialize)]
struct Operation {
    request: CaptureRequest,
    owner_id: String,
    timestamp: String,
    entries: Vec<Entry>,
    review: Option<bool>,
    captured: bool,
    reviewed: bool,
    #[serde(default)]
    review_base_revision: Option<String>,
    #[serde(default)]
    review_result_revision: Option<String>,
    #[serde(default)]
    review_claims: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    response: CapturedProposal,
    class: String,
    relations: Vec<(String, String)>,
    rationale: Option<String>,
    // Freeze anchors when preparing the operation; retries must not re-resolve
    // against a different index generation.
    anchors: Vec<(String, String)>,
    /// Receipt-backed derived relations, annotated with confidence after the
    /// record is written.
    #[serde(default)]
    reconciled: Vec<ReconciledRelation>,
}

pub async fn context(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ContextRequest>,
) -> Result<Json<ContextResponse>, ApiError> {
    Ok(Json(context_snapshot(&state, &request)?))
}

pub fn context_snapshot(
    state: &AppState,
    request: &ContextRequest,
) -> anyhow::Result<ContextResponse> {
    let generation = state.project_write_generation();
    anyhow::ensure!(!request.topic.trim().is_empty(), "context topic is empty");
    anyhow::ensure!(
        request.files.len() <= 100,
        "at most 100 files per context request"
    );
    state.try_ensure_enriched()?;
    let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
    let records = graph::relevant_context_snapshot(state, Some(&request.topic), 12, false)?;
    let mut context = String::from("Recall: get_relevant_context(no topic, limit=100) inventory, then topic recall (limit=12).\nThe broad inventory is bounded and contains names only; retrieve more context when scope expands. Attached file dossiers remain complete.\n\nCurrent knowledge inventory:\n");
    for record in inventory {
        context.push_str(&format!(
            "[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
    }
    context.push_str("\nTopic evidence (complete claims; up to six relationships per record):\n");
    for record in records {
        context.push_str(&format!(
            "\n[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
        for property in record.properties.iter().filter(|property| {
            property.is_literal
                && !matches!(
                    property.predicate.as_str(),
                    "hasTitle" | "label" | "hasTimestamp" | "hasAuthor" | "hasLifecycleStatus"
                )
        }) {
            context.push_str(&format!("{}: {}\n", property.predicate, property.value));
        }
        let mut links: Vec<_> = record
            .properties
            .iter()
            .filter(|property| !property.is_literal)
            .collect();
        links.sort_by(|a, b| {
            graph::edge_priority(&a.predicate)
                .cmp(&graph::edge_priority(&b.predicate))
                .then_with(|| a.predicate.cmp(&b.predicate))
                .then_with(|| a.value.cmp(&b.value))
        });
        for link in links.iter().take(6) {
            context.push_str(&format!("{}: {}\n", link.predicate, link.value));
        }
        if links.len() > 6 {
            context.push_str(&format!(
                "{} further relationships omitted; retrieve them if relevant.\n",
                links.len() - 6
            ));
        }
    }
    let root = state.project_root();
    let mut files = Vec::new();
    for file in &request.files {
        validate_path(file)?;
        let push = policy::evaluate(
            state,
            &root,
            &PolicyEvent::EntityTouched {
                file: file.clone(),
                line: None,
                col: None,
            },
        )?;
        let dossier = match push {
            PolicyDecision::Inject {
                dossier_markdown, ..
            } => dossier_markdown,
            _ => "No recorded entity knowledge is linked to this file. Topic recall still applies."
                .into(),
        };
        let policy = policy::evaluate(
            state,
            &root,
            &PolicyEvent::EditProposed {
                file: file.clone(),
                line: None,
                col: None,
                anchor: None,
            },
        )?;
        files.push(FileContext {
            file: file.clone(),
            dossier,
            policy,
        });
    }
    let revision = accepted_revision(state)?;
    anyhow::ensure!(
        generation == state.project_write_generation(),
        "project knowledge changed while assembling context; retry retrieval"
    );
    Ok(ContextResponse {
        project_root: root.to_string_lossy().into_owned(),
        revision,
        context,
        files,
        capture_contracts: vec![2],
        intent_contracts: vec![2],
    })
}

/// The current knowledge records offered to the link path: the bounded
/// inventory plus the topic recall for change obligations.
pub(super) fn current_record_targets(state: &AppState) -> anyhow::Result<Vec<CaptureTarget>> {
    let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
    let topical =
        graph::relevant_context_snapshot(state, Some("change purpose obligations"), 12, false)?;
    let mut targets: Vec<CaptureTarget> = Vec::new();
    for record in inventory.iter().chain(&topical) {
        if is_record_kind(&record.kind)
            && current_status(state, &record.iri)
                .is_some_and(|status| graph::in_working_set(&status))
            && !targets.iter().any(|target| target.iri == record.iri)
        {
            targets.push(CaptureTarget {
                iri: record.iri.clone(),
                label: record.label.clone(),
                kind: record.kind.clone(),
            });
        }
    }
    Ok(targets)
}

/// Ignore unratified subjects AND inferred incoming links to those subjects.
/// A proposed capture must not invalidate the approval that led to its creation.
pub fn accepted_revision(state: &AppState) -> anyhow::Result<String> {
    accepted_revision_masked(state, &HashSet::new())
}

fn accepted_revision_masked(state: &AppState, masked: &HashSet<String>) -> anyhow::Result<String> {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let quads = state
        .store
        .quads_for_pattern(None, None, None, Some(graph))
        .collect::<Result<Vec<_>, _>>()?;
    let mut excluded: HashSet<String> = quads
        .iter()
        .filter_map(|q| {
            if q.predicate.as_str() != state.capture.status {
                return None;
            }
            match &q.object {
                Term::Literal(status) if !graph::in_working_set(status.value()) => {
                    Some(q.subject.to_string())
                }
                _ => None,
            }
        })
        .collect();
    excluded.extend(masked.iter().cloned());
    let mut canonical: Vec<String> = quads
        .iter()
        .filter(|q| {
            !excluded.contains(&q.subject.to_string()) && !excluded.contains(&q.object.to_string())
        })
        .map(ToString::to_string)
        .collect();
    canonical.sort();
    Ok(sha256_hex(canonical.join("\n")))
}

pub async fn capture_v2(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureV2Request>,
) -> Result<Json<CaptureV2Response>, ApiError> {
    match capture_v2_operation(&state, request) {
        Ok(response) => Ok(Json(response)),
        Err(error) if error.is::<CaptureInputError>() => {
            Err(ApiError::bad_request(error.to_string()))
        }
        Err(error) => Err(error.into()),
    }
}

// Only errors before an operation journal exists permit a caller to replace
// its proposal. Errors after persistence require retrying the identical ID.
#[derive(Debug)]
struct CaptureInputError(String);
impl std::fmt::Display for CaptureInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CaptureInputError {}

/// Capture that must succeed outright: a title collision is an error here.
pub fn capture_operation(
    state: &AppState,
    request: CaptureV2Request,
) -> anyhow::Result<CaptureResponse> {
    match capture_v2_operation(state, request)? {
        CaptureV2Response::Captured { capture } => Ok(capture),
        CaptureV2Response::Collision { collisions } => {
            anyhow::bail!(
                "capture title collides with current or pending knowledge: {collisions:?}"
            )
        }
    }
}

pub fn capture_v2_operation(
    state: &AppState,
    request: CaptureV2Request,
) -> anyhow::Result<CaptureV2Response> {
    validate_id(&request.owner_id, "owner_id")?;
    let _guard = lock_operations()?;
    let path = journal_path(state, &request.operation_id, "json")?;
    let capture_request = CaptureRequest {
        operation_id: request.operation_id.clone(),
        proposals: request.proposals.clone(),
    };
    if let Some(mut stored) = load::<Operation>(&path)? {
        anyhow::ensure!(
            serde_json::to_value(&stored.request)? == serde_json::to_value(&capture_request)?,
            "operation_id was already used for a different capture"
        );
        anyhow::ensure!(
            stored.owner_id == request.owner_id,
            "operation_id was retried with different ownership"
        );
        finish_capture(state, &path, &mut stored)?;
        durable_flush(state)?;
        return Ok(CaptureV2Response::Captured {
            capture: CaptureResponse {
                proposals: stored
                    .entries
                    .into_iter()
                    .map(|entry| entry.response)
                    .collect(),
            },
        });
    }

    // Bind collision discovery and operation preparation to one graph
    // snapshot. Persist the resulting intent before any graph write.
    let proposal_guard = state.lock_proposal_writes()?;
    let mut collisions = Vec::new();
    for (proposal_index, proposal) in request.proposals.iter().enumerate() {
        let candidate_iris = graph::resolve_record_exact_all(state, &proposal.title)
            .into_iter()
            .filter_map(|(iri, _)| {
                (proposal.supersedes.as_ref() != Some(&iri)
                    && current_status(state, &iri)
                        .is_some_and(|status| graph::is_current_or_proposed(&status)))
                .then_some(iri)
            })
            .collect::<Vec<_>>();
        if !candidate_iris.is_empty() {
            collisions.push(CaptureCollision {
                proposal_index,
                candidate_iris,
            });
        }
    }
    if !collisions.is_empty() {
        return Ok(CaptureV2Response::Collision { collisions });
    }
    let mut operation = prepare(state, capture_request, request.owner_id)
        .map_err(|error| CaptureInputError(error.to_string()))?;
    save_operation(&path, &operation)?;
    drop(proposal_guard);
    finish_capture(state, &path, &mut operation)?;
    durable_flush(state)?;
    Ok(CaptureV2Response::Captured {
        capture: CaptureResponse {
            proposals: operation
                .entries
                .into_iter()
                .map(|entry| entry.response)
                .collect(),
        },
    })
}

fn prepare(
    state: &AppState,
    request: CaptureRequest,
    owner_id: String,
) -> anyhow::Result<Operation> {
    anyhow::ensure!(
        request.proposals.len() <= 32,
        "at most 32 proposals per checkpoint"
    );
    let components = graph::load_components(state)?;
    let substrate = state.substrate();
    let mut entries = Vec::new();
    let mut retired_targets = HashSet::new();
    let mut titles = HashSet::new();
    for proposal in &request.proposals {
        anyhow::ensure!(is_record_kind(&proposal.kind), "unsupported capture kind");
        anyhow::ensure!(
            !proposal.title.trim().is_empty() && !proposal.description.trim().is_empty(),
            "capture needs a title and description"
        );
        anyhow::ensure!(
            !proposal.evidence.is_empty() && proposal.evidence.iter().all(|e| !e.trim().is_empty()),
            "capture needs contemporaneous evidence"
        );
        anyhow::ensure!(
            proposal.supersedes.is_none() || proposal.retracts.is_none(),
            "one proposal cannot both supersede and retract"
        );
        let input = record_input(state, proposal)?;
        let title_key = proposal
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        anyhow::ensure!(
            titles.insert(title_key),
            "duplicate proposal title in capture batch"
        );
        for (existing, _) in graph::resolve_record_exact_all(state, &proposal.title) {
            if proposal.supersedes.as_ref() == Some(&existing) {
                continue;
            }
            let status = current_status(state, &existing).unwrap_or_default();
            anyhow::ensure!(
                !graph::in_working_set(&status) && status != "proposed",
                "capture duplicates existing record {existing}; reuse or explicitly supersede it"
            );
        }
        if let Some(target) = proposal.supersedes.as_ref().or(proposal.retracts.as_ref()) {
            anyhow::ensure!(
                retired_targets.insert(target.clone()),
                "duplicate lifecycle target in batch"
            );
            let class = graph::require_information_record(state, &NamedNode::new(target)?)?;
            anyhow::ensure!(
                graph::in_working_set(&current_status(state, target).unwrap_or_default()),
                "lifecycle target must be current accepted knowledge"
            );
            if proposal.supersedes.is_some() {
                anyhow::ensure!(
                    class == input.class_iri,
                    "supersession must preserve record kind"
                );
            }
        }
        let mut relations: Vec<(String, String)> = proposal
            .components
            .iter()
            .map(|c| ("concerns".into(), c.clone()))
            .collect();
        if let Some(requirement) = &proposal.requirement {
            relations.push(("isMotivatedBy".into(), requirement.clone()));
        }
        for reconciled in &proposal.reconciled {
            anyhow::ensure!(
                reconciled.predicate == "refines",
                "only refines relations can be derived from a reconciliation receipt"
            );
            let receipt = reconcile_score::load_receipt(state, &reconciled.receipt_operation_id)?
                .ok_or_else(|| {
                anyhow::anyhow!("reconciliation receipt missing for derived relation")
            })?;
            anyhow::ensure!(
                receipt.owner_id == owner_id
                    && receipt.disposition == "refines"
                    && receipt.candidate_iri.as_deref() == Some(reconciled.target_iri.as_str())
                    && (receipt.confidence - reconciled.confidence).abs() < 1e-9,
                "derived relation does not match its reconciliation receipt"
            );
            let target_class =
                graph::require_information_record(state, &NamedNode::new(&reconciled.target_iri)?)?;
            anyhow::ensure!(
                target_class == input.class_iri
                    && graph::in_working_set(
                        &current_status(state, &reconciled.target_iri).unwrap_or_default()
                    ),
                "refines target must be current accepted knowledge of the same kind"
            );
            relations.push(("refines".into(), reconciled.target_iri.clone()));
        }
        let mut anchors = Vec::new();
        let mut unanchored = Vec::new();
        for file in &proposal.files {
            validate_path(file)?;
            if let Some(component) = graph::best_component_for_path(file, &components) {
                if let Some(iri) = &component.iri {
                    relations.push(("concerns".into(), iri.clone()));
                }
            }
            let module = substrate.as_ref().and_then(|s| {
                s.definitions_in_file(file)
                    .into_iter()
                    .filter(|d| {
                        d.entry.is_module && d.entry.display_name.as_deref() != Some("tests")
                    })
                    .min_by_key(|d| d.entry.symbol.matches('/').count())
            });
            match module {
                Some(def) => anchors.push((def.entry.symbol.clone(), file.clone())),
                None => unanchored.push(file.clone()),
            }
        }
        let (relations, _) = graph::plan_relation_args(state, &input, &relations)?;
        entries.push(Entry {
            response: CapturedProposal {
                iri: graph::mint_instance_iri(&proposal.kind),
                title: proposal.title.clone(),
                kind: proposal.kind.clone(),
                links: vec![],
                unanchored,
            },
            class: input.class_iri,
            relations,
            anchors,
            rationale: proposal
                .supersedes
                .as_ref()
                .map(|_| graph::mint_instance_iri("Rationale")),
            reconciled: proposal.reconciled.clone(),
        });
    }
    Ok(Operation {
        request,
        owner_id,
        entries,
        timestamp: Utc::now().to_rfc3339(),
        review: None,
        captured: false,
        reviewed: false,
        review_base_revision: None,
        review_result_revision: None,
        review_claims: None,
    })
}

fn record_input(state: &AppState, proposal: &KnowledgeProposal) -> anyhow::Result<RecordInput> {
    let mut description = format!(
        "{}\n\nEvidence:\n{}",
        proposal.description,
        proposal
            .evidence
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if !proposal.files.is_empty() {
        description.push_str(&format!(
            "\n\nFiles identified: {}",
            proposal.files.join(", ")
        ));
    }
    Ok(RecordInput {
        class_iri: state.resolve_class(&proposal.kind)?,
        class_local: proposal.kind.clone(),
        properties: vec![
            (moose::RDFS_LABEL.into(), proposal.title.clone()),
            (state.capture.title.clone(), proposal.title.clone()),
            (state.capture.description.clone(), description),
            (state.capture.status.clone(), "proposed".into()),
        ],
    })
}

fn finish_capture(state: &AppState, path: &Path, operation: &mut Operation) -> anyhow::Result<()> {
    if operation.captured {
        return Ok(());
    }
    let _guard = state.lock_proposal_writes()?;
    let stamp = CaptureStamp {
        capture: &state.capture,
        author: AUTHOR,
        timestamp: &operation.timestamp,
        status: "proposed",
    };
    let mut quads = Vec::new();
    for (entry, proposal) in operation.entries.iter().zip(&operation.request.proposals) {
        // Stable identities were persisted BEFORE the transaction. A lost
        // response therefore never mints another record, even after restart.
        if current_status(state, &entry.response.iri).is_some() {
            continue;
        }
        let input = record_input(state, proposal)?;
        let mut relations = entry.relations.clone();
        if let (Some(old), Some(rationale)) = (&proposal.supersedes, &entry.rationale) {
            relations.push((state.resolve_object_property("supersedes")?, old.clone()));
            relations.push((
                state.resolve_object_property("hasRationale")?,
                rationale.clone(),
            ));
            quads.extend(graph::capture_instance_quads(
                &state.store,
                rationale,
                &state.resolve_class("Rationale")?,
                &[
                    (
                        moose::RDFS_LABEL.into(),
                        format!("Rationale: {}", proposal.title),
                    ),
                    (
                        state.capture.title.clone(),
                        format!("Rationale: {}", proposal.title),
                    ),
                    (
                        state.capture.description.clone(),
                        proposal.description.clone(),
                    ),
                ],
                &[],
                &stamp,
            )?);
        }
        quads.extend(graph::capture_instance_quads(
            &state.store,
            &entry.response.iri,
            &entry.class,
            &input.properties,
            &relations,
            &stamp,
        )?);
    }
    if !quads.is_empty() {
        let mut transaction = state.store.start_transaction()?;
        transaction.extend(quads.iter().map(Quad::as_ref));
        transaction.commit()?;
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
        state.note_project_write();
    }
    for entry in &operation.entries {
        for reconciled in &entry.reconciled {
            graph::relate_with_confidence_unlocked(
                state,
                &entry.response.iri,
                &reconciled.predicate,
                &reconciled.target_iri,
                reconciled.confidence,
            )?;
        }
    }
    for (entry, proposal) in operation
        .entries
        .iter_mut()
        .zip(&operation.request.proposals)
    {
        for (symbol, file) in &entry.anchors {
            let normalized = crate::code::substrate::symbols::normalize_symbol(symbol)
                .ok_or_else(|| anyhow::anyhow!("invalid frozen substrate anchor"))?;
            let existing = graph::list_proposals(state, None)?
                .into_iter()
                .find(|p| p.subject_iri == entry.response.iri && p.target_symbol == normalized);
            let iri = match existing {
                Some(p) => p.iri,
                None => graph::propose_link_unlocked(
                    state,
                    &entry.response.iri,
                    graph::link_predicate_for_kind(&proposal.kind),
                    symbol,
                    file,
                    "file identified in harness capture evidence",
                    AUTHOR,
                    Utc::now(),
                )?,
            };
            if !entry.response.links.contains(&iri) {
                entry.response.links.push(iri);
            }
        }
    }
    state.note_project_write();
    durable_flush(state)?;
    operation.captured = true;
    save_operation(path, operation)
}

pub async fn review(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ReviewRequest>,
) -> Result<(HeaderMap, Json<CheckpointResponse>), ApiError> {
    let expected = headers
        .get("x-moosedev-expected-revision")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid expected revision"))?;
    let (checkpoint, attestation) = review_operation_checked(&state, &request, expected)?;
    let mut response_headers = HeaderMap::new();
    if let Some((base, result)) = attestation {
        response_headers.insert(
            "x-moosedev-review-base-revision",
            base.parse().map_err(anyhow::Error::from)?,
        );
        response_headers.insert(
            "x-moosedev-review-result-revision",
            result.parse().map_err(anyhow::Error::from)?,
        );
    }
    Ok((response_headers, Json(checkpoint)))
}

pub fn review_operation(
    state: &AppState,
    request: &ReviewRequest,
) -> anyhow::Result<CheckpointResponse> {
    Ok(review_operation_checked(state, request, None)?.0)
}

type ReviewResult = (CheckpointResponse, Option<(String, String)>);

fn review_operation_checked(
    state: &AppState,
    request: &ReviewRequest,
    expected_revision: Option<&str>,
) -> anyhow::Result<ReviewResult> {
    let _guard = lock_operations()?;
    let path = journal_path(state, &request.operation_id, "json")?;
    let mut operation: Operation = serde_json::from_slice(&std::fs::read(&path)?)?;
    finish_capture(state, &path, &mut operation)?;
    let proposal_guard = state.lock_proposal_writes()?;
    state.try_ensure_enriched()?;
    anyhow::ensure!(
        operation
            .review
            .is_none_or(|accepted| accepted == request.accept),
        "this capture operation already has a different review decision"
    );
    // Validate the entire review before advancing any member. The human saw
    // the journaled claims; an externally edited pending record is a different
    // proposal and needs a fresh review rather than accepting unseen content.
    if request.accept && !operation.reviewed {
        for (entry, proposal) in operation.entries.iter().zip(&operation.request.proposals) {
            let input = record_input(state, proposal)?;
            anyhow::ensure!(
                graph::require_information_record(state, &NamedNode::new(&entry.response.iri)?)?
                    == entry.class,
                "proposal kind changed after capture"
            );
            for (predicate, expected) in &input.properties {
                // A previous attempt may already have ratified this member.
                // Lifecycle consistency is checked by resolve_proposal below.
                if predicate == &state.capture.status {
                    continue;
                }
                let actual: Vec<_> = state
                    .store
                    .quads_for_pattern(
                        Some(NamedNodeRef::new(&entry.response.iri)?.into()),
                        Some(NamedNodeRef::new(predicate)?),
                        None,
                        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                            PROJECT_KG_GRAPH_IRI,
                        )?)),
                    )
                    .collect::<Result<Vec<_>, _>>()?;
                anyhow::ensure!(
                    actual.len() == 1
                        && matches!(&actual[0].object, Term::Literal(value) if value.value() == expected),
                    "proposal content changed after capture; a fresh review is required"
                );
            }
            let mut expected_relations = entry.relations.clone();
            if let (Some(target), Some(rationale)) = (&proposal.supersedes, &entry.rationale) {
                expected_relations
                    .push((state.resolve_object_property("supersedes")?, target.clone()));
                expected_relations.push((
                    state.resolve_object_property("hasRationale")?,
                    rationale.clone(),
                ));
            }
            let links = graph::list_proposals(state, None)?;
            // An interrupted review may already have materialized a journaled
            // link. Permit only that exact accepted link and its frozen symbol.
            for iri in &entry.response.links {
                if let Some(link) = links.iter().find(|link| &link.iri == iri) {
                    if operation.review == Some(true) && link.status == "accepted" {
                        let target = graph::entity_for_symbol(
                            state,
                            &graph::CodeTerms::resolve(state)?,
                            &link.target_symbol,
                        )?
                        .ok_or_else(|| anyhow::anyhow!("accepted code link target disappeared"))?;
                        expected_relations.push((
                            state.resolve_object_property(&link.predicate_local)?,
                            target,
                        ));
                    }
                }
            }
            // Check every domain relation, including fields absent from the
            // claim. Added constrains/violates/etc. are as unseen as supersedes.
            let mut predicates: BTreeSet<String> = state
                .arch_vocab
                .object_properties
                .iter()
                .chain(state.code_vocab.object_properties.iter())
                .map(|property| property.iri.clone())
                .collect();
            predicates.extend(
                expected_relations
                    .iter()
                    .map(|(predicate, _)| predicate.clone()),
            );
            for predicate in predicates {
                let expected: HashSet<Term> = expected_relations
                    .iter()
                    .filter(|(p, _)| p == &predicate)
                    .map(|(_, target)| NamedNode::new(target).map(Term::NamedNode))
                    .collect::<Result<_, _>>()?;
                let actual: HashSet<Term> = state
                    .store
                    .quads_for_pattern(
                        Some(NamedNodeRef::new(&entry.response.iri)?.into()),
                        Some(NamedNodeRef::new(&predicate)?),
                        None,
                        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                            PROJECT_KG_GRAPH_IRI,
                        )?)),
                    )
                    .map(|quad| quad.map(|q| q.object))
                    .collect::<Result<_, _>>()?;
                anyhow::ensure!(
                    actual == expected,
                    "proposal relation changed after capture"
                );
            }
            preflight_resolution(state, &entry.response.iri, true)?;
            if let Some(target) = proposal.retracts.as_ref().or(proposal.supersedes.as_ref()) {
                graph::require_information_record(state, &NamedNode::new(target)?)
                    .map_err(|error| anyhow::anyhow!("lifecycle target disappeared after capture: {error}; reject this operation or restore the target"))?;
                let status = current_status(state, target).unwrap_or_default();
                let already_resolved = current_status(state, &entry.response.iri).as_deref()
                    == Some("accepted")
                    && status
                        == if proposal.retracts.is_some() {
                            "deprecated"
                        } else {
                            "superseded"
                        };
                anyhow::ensure!(graph::in_working_set(&status) || already_resolved,
                    "lifecycle target changed after capture; reject this operation or restore the target");
            }
            for iri in &entry.response.links {
                preflight_resolution(state, iri, true)?;
                let link = links
                    .iter()
                    .find(|p| &p.iri == iri)
                    .ok_or_else(|| anyhow::anyhow!("queued code link disappeared after capture"))?;
                let expected_predicate = graph::link_predicate_for_kind(&proposal.kind);
                anyhow::ensure!(
                    link.subject_iri == entry.response.iri
                        && link.predicate_local == expected_predicate
                        && entry.anchors.iter().any(|(symbol, file)| {
                            crate::code::substrate::symbols::normalize_symbol(symbol).as_deref()
                                == Some(link.target_symbol.as_str())
                                && file == &link.target_path
                        }),
                    "queued code link changed after capture"
                );
            }
        }
    }
    if !operation.reviewed && !request.accept {
        for entry in &operation.entries {
            for iri in std::iter::once(&entry.response.iri).chain(&entry.response.links) {
                preflight_resolution(state, iri, false)?;
            }
        }
    }
    let subjects: HashSet<String> = operation
        .entries
        .iter()
        .flat_map(|entry| std::iter::once(&entry.response.iri).chain(&entry.response.links))
        .map(|iri| NamedNode::new(iri).map(|node| node.to_string()))
        .collect::<Result<_, _>>()?;
    if let Some(expected) = expected_revision.filter(|_| operation.review.is_none()) {
        let actual = accepted_revision(state)?;
        anyhow::ensure!(
            actual == expected,
            "knowledge changed before review; refresh approval before accepting"
        );
        if request.accept && !operation.request.has_governing() {
            operation.review_base_revision = Some(actual);
            operation.review_claims = Some(review_claims(state, &subjects)?);
        }
    }
    operation.review = Some(request.accept);
    save_operation(&path, &operation)?;
    if !operation.reviewed {
        if !request.accept {
            let mut members: Vec<String> = operation
                .entries
                .iter()
                .flat_map(|entry| std::iter::once(&entry.response.iri).chain(&entry.response.links))
                .cloned()
                .collect();
            members.extend(
                operation
                    .entries
                    .iter()
                    .filter_map(|entry| entry.rationale.as_ref())
                    .filter(|iri| current_status(state, iri).as_deref() == Some("proposed"))
                    .cloned(),
            );
            graph::reject_frozen_proposals_unlocked(state, &members, REVIEWER)?;
        }
        for (entry, proposal) in operation.entries.iter().zip(&operation.request.proposals) {
            resolve_proposal(state, &entry.response.iri, request.accept)?;
            for iri in &entry.response.links {
                resolve_proposal(state, iri, request.accept)?;
            }
            if request.accept {
                if let Some(target) = &proposal.retracts {
                    match current_status(state, target).as_deref() {
                        Some("deprecated") => {} // The prior attempt committed its lifecycle transaction.
                        status if status.is_none_or(graph::in_working_set) => {
                            graph::retract_decision_unlocked(
                                state,
                                target,
                                &proposal.description,
                                REVIEWER,
                                Utc::now(),
                            )?;
                            state.note_project_write();
                        }
                        _ => anyhow::bail!("retraction target changed during review"),
                    }
                }
            }
        }
        durable_flush(state)?;
        operation.reviewed = true;
        save_operation(&path, &operation)?;
    }
    drop(proposal_guard);
    let generation = state.project_write_generation();
    let checkpoint = checkpoint_snapshot(state, Some(&request.operation_id))?;
    // Do not infer freshness from two separate revision reads. Mask only the
    // previously unratified subjects, and prove the remaining graph stayed
    // identical AND the owned claims changed only in lifecycle status. New
    // code nodes or any other unproven mutation conservatively fail this proof.
    if operation.review_result_revision.is_none() {
        if let (Some(base), Some(claims)) =
            (&operation.review_base_revision, &operation.review_claims)
        {
            if accepted_revision_masked(state, &subjects)? == *base
                && review_claims(state, &subjects)? == *claims
                && generation == state.project_write_generation()
            {
                operation.review_result_revision = Some(checkpoint.revision.clone());
                save_operation(&path, &operation)?;
            }
        }
    }
    let attestation = operation
        .review_base_revision
        .zip(operation.review_result_revision);
    Ok((checkpoint, attestation))
}

fn review_claims(state: &AppState, subjects: &HashSet<String>) -> anyhow::Result<Vec<String>> {
    let mut claims = state
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
        .filter(|quad| {
            (subjects.contains(&quad.subject.to_string())
                || subjects.contains(&quad.object.to_string()))
                && quad.predicate.as_str() != state.capture.status
        })
        .map(|quad| quad.to_string())
        .collect::<Vec<_>>();
    claims.sort();
    Ok(claims)
}

fn preflight_resolution(state: &AppState, iri: &str, accept: bool) -> anyhow::Result<()> {
    let expected = if accept { "accepted" } else { "rejected" };
    anyhow::ensure!(
        current_status(state, iri)
            .as_deref()
            .is_some_and(|status| status == "proposed" || status == expected),
        "proposal {iri} was resolved differently outside this operation"
    );
    Ok(())
}

fn resolve_proposal(state: &AppState, iri: &str, accept: bool) -> anyhow::Result<()> {
    let expected = if accept { "accepted" } else { "rejected" };
    match current_status(state, iri).as_deref() {
        Some("proposed") => {
            if accept {
                graph::accept_proposal_unlocked(state, iri, REVIEWER)?;
            } else {
                graph::reject_proposal_unlocked(state, iri, REVIEWER)?;
            }
        }
        Some(status) if status == expected => {}
        _ => anyhow::bail!("proposal {iri} was resolved differently outside this operation"),
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CheckpointQuery {
    pub operation_id: Option<String>,
}

pub async fn checkpoint(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    let _guard = lock_operations()?;
    // A legacy browser can issue an Origin-less GET without Fetch Metadata.
    // Reading status must never enrich the graph or publish the canonical file.
    Ok(Json(checkpoint_status(
        &state,
        query.operation_id.as_deref(),
        false,
    )?))
}

pub async fn publish_checkpoint(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    let _guard = lock_operations()?;
    Ok(Json(checkpoint_snapshot(
        &state,
        query.operation_id.as_deref(),
    )?))
}

pub fn checkpoint_snapshot(
    state: &AppState,
    operation_id: Option<&str>,
) -> anyhow::Result<CheckpointResponse> {
    checkpoint_status(state, operation_id, true)
}

fn checkpoint_status(
    state: &AppState,
    operation_id: Option<&str>,
    publish: bool,
) -> anyhow::Result<CheckpointResponse> {
    let generation = state.project_write_generation();
    let mut pending = BTreeSet::new();
    if let Some(id) = operation_id {
        let operation: Operation =
            serde_json::from_slice(&std::fs::read(journal_path(state, id, "json")?)?)?;
        if !operation.captured || !operation.reviewed {
            pending.insert(format!("operation:{id}"));
        }
        for entry in operation.entries {
            for iri in std::iter::once(entry.response.iri).chain(entry.response.links) {
                // A completed local journal is not proof that the canonical
                // graph still contains its writes (for example after a branch
                // switch). Missing or conflicting records remain obligations.
                let status = current_status(state, &iri);
                let resolved = matches!(
                    (operation.review, status.as_deref()),
                    (Some(true), Some("accepted" | "superseded" | "deprecated"))
                        | (Some(false), Some("rejected"))
                );
                if !resolved {
                    pending.insert(iri);
                }
            }
        }
    }
    if publish {
        state.try_ensure_enriched()?;
    }
    let report = crate::validation::validate_project(state)?;
    if publish {
        durable_flush(state)?;
    }
    let revision = accepted_revision(state)?;
    anyhow::ensure!(
        generation == state.project_write_generation(),
        "project knowledge changed during checkpoint validation; retry checkpoint"
    );
    Ok(CheckpointResponse {
        conforms: report.conforms(),
        // A GET reports current state, not evidence of successful publication.
        durable: publish,
        revision,
        pending: pending.into_iter().collect(),
    })
}

fn current_status(state: &AppState, iri: &str) -> Option<String> {
    graph::first_literal(&state.store, iri, &state.capture.status)
}

fn validate_path(file: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !file.is_empty()
            && Path::new(file)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "expected a repository-relative file path"
    );
    Ok(())
}

fn durable_flush(state: &AppState) -> anyhow::Result<()> {
    state.store.flush()?;
    crate::canonical::write_through(&state.store, &state.data_dir)?;
    std::fs::File::open(crate::canonical::canonical_path(&state.data_dir))?.sync_all()?;
    std::fs::File::open(crate::canonical::stamp_path(&state.data_dir))?.sync_all()?;
    std::fs::File::open(&state.data_dir)?.sync_all()?;
    Ok(())
}
