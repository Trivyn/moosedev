//! Typed capture (`capture/v2`): journal the operation, write the proposed
//! records and their derived relations, and replay a completed operation from
//! its journal on retry.
use super::*;

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
        changed: request.changed.clone(),
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
        review_unminted_symbols: Vec::new(),
    })
}

pub(super) fn record_input(
    state: &AppState,
    proposal: &KnowledgeProposal,
) -> anyhow::Result<RecordInput> {
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

pub(super) fn finish_capture(
    state: &AppState,
    path: &Path,
    operation: &mut Operation,
) -> anyhow::Result<()> {
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
