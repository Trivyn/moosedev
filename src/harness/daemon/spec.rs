//! Explicit spec approval. Preparation is graph-read-only and freezes every
//! identity and lifecycle effect in the operation journal. Approval rechecks
//! both source and accepted-knowledge revisions before one graph transaction.
use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::Json;
use chrono::Utc;
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use serde::{Deserialize, Serialize};

use super::*;
use crate::harness::digest::sha256_hex;

const SPEC_AUTHOR: &str = "moosedev-harness-spec-approval";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpecOperation {
    request: SpecPrepareRequest,
    preview: SpecPrepareResponse,
    approval_iri: String,
    timestamp: String,
    /// Preallocated rationale IRIs, keyed by the subject whose lifecycle
    /// changes (a predecessor record or the previous approval marker).
    rationales: HashMap<String, String>,
    #[serde(default)]
    response: Option<SpecApproveResponse>,
}

pub async fn prepare(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SpecPrepareRequest>,
) -> Result<Json<SpecPrepareResponse>, ApiError> {
    Ok(Json(prepare_operation(&state, request)?))
}

pub async fn current(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SpecCurrentRequest>,
) -> Result<Json<SpecCurrentResponse>, ApiError> {
    Ok(Json(current_operation(&state, request)?))
}

/// The current approval of a spec path with its records as drafts. Read-only.
/// Preparing an unchanged source from these drafts is deterministic: the
/// claim text round-trips, so every entry reuses its record instead of the
/// extraction sensor rewording it into a supersession.
pub fn current_operation(
    state: &AppState,
    request: SpecCurrentRequest,
) -> anyhow::Result<SpecCurrentResponse> {
    let Some(marker) = current_approval_for_path(state, &request.path)? else {
        return Ok(SpecCurrentResponse {
            path: request.path,
            approval_iri: None,
            source_sha256: None,
            drafts: vec![],
        });
    };
    let mut drafts = marker_targets(state, &marker.iri)?
        .into_iter()
        .map(draft_of)
        .collect::<anyhow::Result<Vec<_>>>()?;
    drafts.sort_by(|a, b| (&a.kind, &a.title).cmp(&(&b.kind, &b.title)));
    Ok(SpecCurrentResponse {
        path: request.path,
        approval_iri: Some(marker.iri),
        source_sha256: marker.source_sha256,
        drafts,
    })
}

/// Invert `claim`: an approved record's description is the draft text
/// followed by its evidence block. The round trip is checked, never assumed.
fn draft_of(record: ExistingRecord) -> anyhow::Result<SpecRecordDraft> {
    use anyhow::Context as _;
    let (description, evidence) = record
        .description
        .rsplit_once("\n\nEvidence:\n")
        .with_context(|| format!("approved record {} carries no evidence block", record.iri))?;
    let evidence = evidence
        .lines()
        .map(|line| {
            line.strip_prefix("- ").map(str::to_owned).with_context(|| {
                format!(
                    "approved record {} has a malformed evidence line",
                    record.iri
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let draft = SpecRecordDraft {
        kind: record.kind,
        title: record.title,
        description: description.to_owned(),
        evidence,
    };
    anyhow::ensure!(
        claim(&draft) == record.description,
        "approved record {} does not round-trip through the draft shape",
        record.iri
    );
    Ok(draft)
}

pub async fn approve(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SpecApproveRequest>,
) -> Result<Json<SpecApproveResponse>, ApiError> {
    let response = approve_operation(&state, request)?;
    // Keep the paraphrase-tolerant retrieval channel coherent with the
    // symbolic transaction. This is idempotent and best-effort, as on the MCP
    // record path; the accepted graph remains primary if embeddings fail.
    for iri in response
        .records
        .iter()
        .map(|record| record.iri.as_str())
        .chain(std::iter::once(response.approval_iri.as_str()))
    {
        if let Err(error) = state.index_record(iri).await {
            tracing::warn!("dense index failed for spec-approved record {iri}: {error}");
        }
    }
    Ok(Json(response))
}

pub fn prepare_operation(
    state: &AppState,
    request: SpecPrepareRequest,
) -> anyhow::Result<SpecPrepareResponse> {
    validate_id(&request.owner_id, "owner_id")?;
    validate_source(state, &request.path, &request.source_sha256)?;
    validate_drafts(&request.path, &request.drafts, state)?;
    let component = plan_component(state, &request)?;

    let _operations = lock_operations()?;
    let path = journal_path(state, &request.operation_id, "spec.json")?;
    if let Some(stored) = load::<SpecOperation>(&path)? {
        anyhow::ensure!(
            stored.request == request,
            "operation_id was already used for a different spec preparation"
        );
        return Ok(stored.preview);
    }
    revision::ensure_unchanged(
        state,
        &request.knowledge_revision,
        "knowledge changed before spec preparation; refresh the preview",
    )?;

    let previous = current_approval_for_path(state, &request.path)?;
    let previous_owned = previous
        .as_ref()
        .map(|marker| marker_targets(state, &marker.iri))
        .transpose()?
        .unwrap_or_default();
    let previous_by_key: HashMap<(String, String), ExistingRecord> = previous_owned
        .iter()
        .cloned()
        .map(|record| ((record.kind.clone(), normalize(&record.title)), record))
        .collect();

    let mut entries = Vec::with_capacity(request.drafts.len());
    let mut retained_previous = HashSet::new();
    let mut claimed_targets = HashSet::new();
    for draft in &request.drafts {
        let key = (draft.kind.clone(), normalize(&draft.title));
        let claim = claim(draft);
        let disposition = if let Some(old) = previous_by_key.get(&key) {
            if old.description == claim {
                retained_previous.insert(old.iri.clone());
                SpecDisposition::Reuse {
                    iri: old.iri.clone(),
                }
            } else if referenced_by_other_current_approval(
                state,
                &old.iri,
                previous.as_ref().map(|marker| marker.iri.as_str()),
            )? {
                // This approval cannot globally retire knowledge another
                // current spec still owns. It gets its own changed record and
                // the old one is surfaced below as RETAIN_SHARED.
                SpecDisposition::New {
                    iri: graph::mint_instance_iri(&draft.kind),
                }
            } else {
                retained_previous.insert(old.iri.clone());
                SpecDisposition::Supersede {
                    iri: graph::mint_instance_iri(&draft.kind),
                    previous_iri: old.iri.clone(),
                }
            }
        } else {
            let same_title = current_records_with_title(state, &draft.kind, &draft.title)?;
            if let Some(existing) = same_title.iter().find(|record| record.description == claim) {
                SpecDisposition::Reuse {
                    iri: existing.iri.clone(),
                }
            } else {
                anyhow::ensure!(
                    same_title.is_empty(),
                    "spec record title {:?} already names different accepted knowledge outside this spec; qualify the title",
                    draft.title
                );
                let proposal = KnowledgeProposal {
                    kind: draft.kind.clone(),
                    title: draft.title.clone(),
                    description: draft.description.clone(),
                    evidence: draft.evidence.clone(),
                    files: vec![],
                    components: vec![],
                    requirement: None,
                    supersedes: None,
                    retracts: None,
                    learned_from: None,
                    reconciled: vec![],
                };
                match reconcile_score::score_proposal(
                    state,
                    &request.owner_id,
                    &proposal,
                    ReconcileThresholds::from_env()?,
                )?
                .disposition
                {
                    reconcile_score::ScoredDisposition::Restates { candidate_iri, .. } => {
                        SpecDisposition::Reuse { iri: candidate_iri }
                    }
                    _ => SpecDisposition::New {
                        iri: graph::mint_instance_iri(&draft.kind),
                    },
                }
            }
        };
        anyhow::ensure!(
            claimed_targets.insert(disposition.active_iri().to_string()),
            "two spec drafts reconcile to the same accepted record"
        );
        if previous_owned
            .iter()
            .any(|record| record.iri == disposition.active_iri())
        {
            retained_previous.insert(disposition.active_iri().to_string());
        }
        let existing = match &disposition {
            SpecDisposition::New { .. } => None,
            SpecDisposition::Reuse { iri } => {
                Some(existing_snapshot(current_record(state, iri)?.ok_or_else(
                    || anyhow::anyhow!("reused spec record disappeared during preparation"),
                )?))
            }
            SpecDisposition::Supersede { previous_iri, .. } => Some(existing_snapshot(
                previous_owned
                    .iter()
                    .find(|record| &record.iri == previous_iri)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("superseded spec record disappeared"))?,
            )),
        };
        entries.push(SpecPreviewEntry {
            draft: draft.clone(),
            disposition,
            existing,
        });
    }

    let mut retirements = Vec::new();
    for old in previous_owned
        .iter()
        .filter(|record| !retained_previous.contains(&record.iri))
    {
        let shared = referenced_by_other_current_approval(
            state,
            &old.iri,
            previous.as_ref().map(|marker| marker.iri.as_str()),
        )?;
        retirements.push(SpecRetirement {
            iri: old.iri.clone(),
            kind: old.kind.clone(),
            title: old.title.clone(),
            description: old.description.clone(),
            disposition: if shared {
                SpecRetirementDisposition::RetainShared
            } else {
                SpecRetirementDisposition::Retract
            },
        });
    }
    retirements.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.title.cmp(&b.title)));

    let records_unchanged = previous.as_ref().is_some_and(|marker| {
        marker.source_sha256.as_deref() == Some(request.source_sha256.as_str())
            && retirements.is_empty()
            && entries.iter().all(|entry| {
                matches!(&entry.disposition, SpecDisposition::Reuse { iri } if previous_owned.iter().any(|old| &old.iri == iri))
            })
            && entries.len() == previous_owned.len()
    });
    if previous
        .as_ref()
        .is_some_and(|marker| marker.source_sha256.as_deref() == Some(&request.source_sha256))
    {
        anyhow::ensure!(
            records_unchanged,
            "the same spec source digest produced a different record preview; keep the prior approval or change the spec"
        );
    }
    // An unchanged batch still needs an approval when it is being anchored to
    // a component for the first time, or the component gains paths.
    let component_pending = match &component {
        None => false,
        Some(plan) => {
            let concerns = state.resolve_object_property("concerns")?;
            plan.new
                || !plan.added.is_empty()
                || entries.iter().try_fold(false, |pending, entry| {
                    anyhow::Ok(
                        pending
                            || !direct_objects(state, entry.disposition.active_iri(), &concerns)?
                                .contains(&plan.iri),
                    )
                })?
        }
    };
    let already_approved = records_unchanged && !component_pending;

    let preview = SpecPrepareResponse {
        operation_id: request.operation_id.clone(),
        owner_id: request.owner_id.clone(),
        path: request.path.clone(),
        source_sha256: request.source_sha256.clone(),
        knowledge_revision: request.knowledge_revision.clone(),
        entries,
        retirements,
        component,
        previous_approval_iri: previous.as_ref().map(|marker| marker.iri.clone()),
        already_approved,
    };
    let mut rationales = HashMap::new();
    for entry in &preview.entries {
        if let SpecDisposition::Supersede { previous_iri, .. } = &entry.disposition {
            rationales.insert(previous_iri.clone(), graph::mint_instance_iri("Rationale"));
        }
    }
    for retirement in &preview.retirements {
        if retirement.disposition == SpecRetirementDisposition::Retract {
            rationales.insert(
                retirement.iri.clone(),
                graph::mint_instance_iri("Rationale"),
            );
        }
    }
    if let Some(previous_iri) = &preview.previous_approval_iri {
        if !preview.already_approved {
            rationales.insert(previous_iri.clone(), graph::mint_instance_iri("Rationale"));
        }
    }
    let operation = SpecOperation {
        request,
        preview: preview.clone(),
        approval_iri: if preview.already_approved {
            preview
                .previous_approval_iri
                .clone()
                .expect("unchanged approval")
        } else {
            graph::mint_instance_iri("ArchitecturalDecision")
        },
        timestamp: Utc::now().to_rfc3339(),
        rationales,
        response: None,
    };
    // This is the only preparation write: an operation journal outside the KG.
    save_operation(&path, &operation)?;
    Ok(preview)
}

pub fn approve_operation(
    state: &AppState,
    request: SpecApproveRequest,
) -> anyhow::Result<SpecApproveResponse> {
    validate_id(&request.owner_id, "owner_id")?;
    let _operations = lock_operations()?;
    let path = journal_path(state, &request.operation_id, "spec.json")?;
    let mut operation: SpecOperation = load(&path)?
        .ok_or_else(|| anyhow::anyhow!("no prepared spec approval for operation_id"))?;
    anyhow::ensure!(
        operation.request.owner_id == request.owner_id,
        "spec approval was prepared by a different owner"
    );
    let base_revision = operation.request.knowledge_revision.clone();

    let _proposal_guard = state.lock_proposal_writes()?;
    if operation.response.is_some() {
        if operation.preview.already_approved {
            verify_unchanged_approval(state, &operation)?;
        } else {
            anyhow::ensure!(
                current_status(state, &operation.approval_iri).as_deref() == Some("accepted"),
                "completed spec approval is missing from the current graph"
            );
            verify_committed_operation(state, &operation)?;
        }
        durable_flush(state)?;
        let checkpoint = checkpoint_snapshot(state, None)?;
        anyhow::ensure!(
            checkpoint.conforms && checkpoint.durable,
            "completed spec approval no longer has a conforming durable checkpoint"
        );
        let response = approval_response(&operation, base_revision, checkpoint);
        operation.response = Some(response.clone());
        save_operation(&path, &operation)?;
        return Ok(response);
    }
    // A crash can happen after the atomic graph commit but before the response
    // reaches the journal. The preallocated accepted marker proves that exact
    // commit; do not reject its retry merely because the commit changed the
    // revision it was intentionally based on.
    let committed = !operation.preview.already_approved
        && current_status(state, &operation.approval_iri).as_deref() == Some("accepted");
    if committed {
        verify_committed_operation(state, &operation)?;
    } else {
        validate_source(
            state,
            &operation.request.path,
            &operation.request.source_sha256,
        )?;
        // Preflight without enrichment: publishing a checkpoint here could
        // materialize inferred edges and change the very accepted revision
        // the read-only preview was bound to. The post-commit checkpoint is
        // the sole publisher for this transition.
        durable_flush(state)?;
        let preflight_revision = accepted_revision(state)?;
        let preflight_report = crate::validation::validate_project(state)?;
        anyhow::ensure!(
            preflight_report.conforms()
                && preflight_revision == operation.request.knowledge_revision,
            "knowledge changed or ceased to conform after the spec preview; prepare it again"
        );
        validate_planned_relations(state, &operation)?;
    }
    if !operation.preview.already_approved && !committed {
        preflight_approval(state, &operation)?;
        commit_approval(state, &operation)?;
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
        state.note_project_write();
    }
    durable_flush(state)?;
    let checkpoint = checkpoint_snapshot(state, None)?;
    anyhow::ensure!(
        checkpoint.conforms && checkpoint.durable,
        "spec approval did not produce a conforming durable checkpoint"
    );
    let response = approval_response(&operation, base_revision, checkpoint);
    operation.response = Some(response.clone());
    save_operation(&path, &operation)?;
    Ok(response)
}

fn approval_response(
    operation: &SpecOperation,
    base_revision: String,
    checkpoint: CheckpointResponse,
) -> SpecApproveResponse {
    let records = operation
        .preview
        .entries
        .iter()
        .map(|entry| SpecApprovedRecord {
            kind: entry.draft.kind.clone(),
            title: entry.draft.title.clone(),
            iri: entry.disposition.active_iri().to_string(),
            disposition: entry.disposition.clone(),
        })
        .collect();
    SpecApproveResponse {
        operation_id: operation.request.operation_id.clone(),
        path: operation.request.path.clone(),
        source_sha256: operation.request.source_sha256.clone(),
        base_revision,
        result_revision: checkpoint.revision.clone(),
        records,
        retirements: operation.preview.retirements.clone(),
        approval_iri: operation.approval_iri.clone(),
        checkpoint,
    }
}

fn verify_committed_operation(state: &AppState, operation: &SpecOperation) -> anyhow::Result<()> {
    let marker_title = marker_title(operation);
    let marker_description = marker_description(operation);
    let marker_class =
        graph::require_information_record(state, &NamedNode::new(&operation.approval_iri)?)?;
    anyhow::ensure!(
        graph::local_name(&marker_class) == "ArchitecturalDecision"
            && graph::first_literal(&state.store, &operation.approval_iri, &state.capture.title)
                .as_deref()
                == Some(marker_title.as_str())
            && graph::first_literal(
                &state.store,
                &operation.approval_iri,
                &state.capture.description
            )
            .as_deref()
                == Some(marker_description.as_str()),
        "accepted spec approval marker does not match its durable operation"
    );

    let motivated_by = state.resolve_object_property("isMotivatedBy")?;
    let actual_targets = direct_objects(state, &operation.approval_iri, &motivated_by)?;
    let expected_targets: HashSet<String> = operation
        .preview
        .entries
        .iter()
        .map(|entry| entry.disposition.active_iri().to_string())
        .collect();
    anyhow::ensure!(
        actual_targets == expected_targets,
        "accepted spec approval ownership differs from its durable preview"
    );

    let supersedes = state.resolve_object_property("supersedes")?;
    if let Some(previous) = &operation.preview.previous_approval_iri {
        anyhow::ensure!(
            current_status(state, previous).as_deref() == Some("superseded")
                && direct_objects(state, &operation.approval_iri, &supersedes)?
                    == HashSet::from([previous.clone()]),
            "previous spec approval lifecycle does not match the committed operation"
        );
    }
    for entry in &operation.preview.entries {
        let iri = entry.disposition.active_iri();
        anyhow::ensure!(
            current_status(state, iri).as_deref() == Some("accepted"),
            "committed spec record is no longer accepted"
        );
        if let Some(expected) = &entry.existing {
            let actual = existing_record(state, &expected.iri, expected.kind.clone())?;
            anyhow::ensure!(
                actual.iri == expected.iri
                    && actual.kind == expected.kind
                    && actual.title == expected.title
                    && actual.description == expected.description,
                "existing spec record differs from the claim shown at approval"
            );
        }
        if !matches!(entry.disposition, SpecDisposition::Reuse { .. }) {
            let class = graph::require_information_record(state, &NamedNode::new(iri)?)?;
            anyhow::ensure!(
                graph::local_name(&class) == entry.draft.kind
                    && graph::first_literal(&state.store, iri, &state.capture.title).as_deref()
                        == Some(entry.draft.title.as_str())
                    && graph::first_literal(&state.store, iri, &state.capture.description)
                        .as_deref()
                        == Some(claim(&entry.draft).as_str()),
                "committed spec record content differs from its durable preview"
            );
        }
        if let SpecDisposition::Supersede { previous_iri, .. } = &entry.disposition {
            anyhow::ensure!(
                current_status(state, previous_iri).as_deref() == Some("superseded")
                    && direct_objects(state, iri, &supersedes)?
                        == HashSet::from([previous_iri.clone()]),
                "committed spec supersession differs from its durable preview"
            );
        }
    }
    for retirement in &operation.preview.retirements {
        let expected = match retirement.disposition {
            SpecRetirementDisposition::Retract => "deprecated",
            SpecRetirementDisposition::RetainShared => "accepted",
        };
        anyhow::ensure!(
            current_status(state, &retirement.iri).as_deref() == Some(expected),
            "committed spec retirement differs from its durable preview"
        );
    }
    if let Some(plan) = &operation.preview.component {
        let concerns = state.resolve_object_property("concerns")?;
        let covered = graph::load_components(state)?
            .into_iter()
            .find(|component| component.iri.as_deref() == Some(plan.iri.as_str()))
            .map(|component| component.covers_paths)
            .unwrap_or_default();
        anyhow::ensure!(
            plan.covers.iter().all(|path| covered.contains(path))
                && operation
                    .preview
                    .entries
                    .iter()
                    .try_fold(true, |all, entry| {
                        anyhow::Ok(
                            all && direct_objects(
                                state,
                                entry.disposition.active_iri(),
                                &concerns,
                            )?
                            .contains(&plan.iri),
                        )
                    })?,
            "committed spec component differs from its durable preview"
        );
    }
    Ok(())
}

fn verify_unchanged_approval(state: &AppState, operation: &SpecOperation) -> anyhow::Result<()> {
    anyhow::ensure!(
        operation.preview.already_approved
            && current_status(state, &operation.approval_iri).as_deref() == Some("accepted"),
        "unchanged spec approval is no longer current"
    );
    let description = graph::first_literal(
        &state.store,
        &operation.approval_iri,
        &state.capture.description,
    )
    .unwrap_or_default();
    anyhow::ensure!(
        description
            .lines()
            .any(|line| line == format!("spec-approval: {}", operation.request.path))
            && description
                .lines()
                .any(|line| line == format!("spec-sha256: {}", operation.request.source_sha256)),
        "unchanged spec approval marker differs from its durable operation"
    );
    let motivated_by = state.resolve_object_property("isMotivatedBy")?;
    let actual_targets = direct_objects(state, &operation.approval_iri, &motivated_by)?;
    let expected_targets: HashSet<String> = operation
        .preview
        .entries
        .iter()
        .map(|entry| entry.disposition.active_iri().to_string())
        .collect();
    anyhow::ensure!(
        actual_targets == expected_targets
            && operation.preview.entries.iter().all(|entry| {
                matches!(entry.disposition, SpecDisposition::Reuse { .. })
                    && current_status(state, entry.disposition.active_iri()).as_deref()
                        == Some("accepted")
            }),
        "unchanged spec approval records differ from its durable preview"
    );
    Ok(())
}

fn marker_title(operation: &SpecOperation) -> String {
    format!(
        "Spec {} approved for implementation",
        operation.request.path
    )
}

fn marker_description(operation: &SpecOperation) -> String {
    let active = operation
        .preview
        .entries
        .iter()
        .map(|entry| format!("- {}", entry.disposition.active_iri()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Approved specification.\n\nApprover: harness human\nApproved at: {}\nActive records:\n{}\n\nspec-approval: {}\nspec-sha256: {}",
        operation.timestamp,
        active,
        operation.request.path,
        operation.request.source_sha256,
    )
}

fn direct_objects(
    state: &AppState,
    subject: &str,
    predicate: &str,
) -> anyhow::Result<HashSet<String>> {
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(subject)?.into()),
            Some(NamedNodeRef::new(predicate)?),
            None,
            Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                PROJECT_KG_GRAPH_IRI,
            )?)),
        )
        .filter_map(|quad| match quad {
            Ok(Quad {
                object: Term::NamedNode(object),
                ..
            }) => Some(Ok(object.as_str().to_string())),
            Ok(_) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

fn validate_planned_relations(state: &AppState, operation: &SpecOperation) -> anyhow::Result<()> {
    let decision = state.resolve_class("ArchitecturalDecision")?;
    let rationale = state.resolve_class("Rationale")?;
    let motivated_by = state.resolve_object_property("isMotivatedBy")?;
    let supersedes = state.resolve_object_property("supersedes")?;
    let superseded_by = state.resolve_object_property("isSupersededBy")?;
    let has_rationale = state.resolve_object_property("hasRationale")?;

    let component = operation
        .preview
        .component
        .as_ref()
        .map(|_| {
            anyhow::Ok((
                state.resolve_object_property("concerns")?,
                state.resolve_class("SystemComponent")?,
            ))
        })
        .transpose()?;

    for entry in &operation.preview.entries {
        let class = state.resolve_class(&entry.draft.kind)?;
        ensure_legal_relation(state, &decision, &motivated_by, &class)?;
        if let Some((concerns, component_class)) = &component {
            ensure_legal_relation(state, &class, concerns, component_class)?;
        }
        if matches!(entry.disposition, SpecDisposition::Supersede { .. }) {
            ensure_legal_relation(state, &class, &supersedes, &class)?;
            ensure_legal_relation(state, &class, &has_rationale, &rationale)?;
            ensure_legal_relation(state, &class, &superseded_by, &class)?;
        }
    }
    for retirement in &operation.preview.retirements {
        if retirement.disposition == SpecRetirementDisposition::Retract {
            ensure_legal_relation(
                state,
                &state.resolve_class(&retirement.kind)?,
                &has_rationale,
                &rationale,
            )?;
        }
    }
    if operation.preview.previous_approval_iri.is_some() && !operation.preview.already_approved {
        ensure_legal_relation(state, &decision, &supersedes, &decision)?;
        ensure_legal_relation(state, &decision, &has_rationale, &rationale)?;
        ensure_legal_relation(state, &decision, &superseded_by, &decision)?;
    }
    Ok(())
}

fn ensure_legal_relation(
    state: &AppState,
    subject_class: &str,
    predicate: &str,
    object_class: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        state
            .catalogue
            .legal_predicates(&state.store, subject_class, object_class)
            .iter()
            .any(|edge| {
                edge.predicate_iri == predicate && edge.direction == graph::EdgeDirection::Forward
            }),
        "planned spec relation is not legal for its endpoint classes"
    );
    Ok(())
}

fn preflight_approval(state: &AppState, operation: &SpecOperation) -> anyhow::Result<()> {
    for entry in &operation.preview.entries {
        let expected_class = state.resolve_class(&entry.draft.kind)?;
        match &entry.disposition {
            SpecDisposition::New { iri } => {
                anyhow::ensure!(
                    current_status(state, iri).is_none(),
                    "prepared spec record IRI already exists"
                );
            }
            SpecDisposition::Reuse { iri } => {
                anyhow::ensure!(
                    current_status(state, iri).as_deref() == Some("accepted")
                        && graph::require_information_record(state, &NamedNode::new(iri)?)?
                            == expected_class,
                    "reused spec record is no longer accepted"
                );
            }
            SpecDisposition::Supersede { iri, previous_iri } => {
                anyhow::ensure!(
                    current_status(state, iri).is_none(),
                    "prepared replacement IRI already exists"
                );
                anyhow::ensure!(
                    current_status(state, previous_iri).as_deref() == Some("accepted"),
                    "supersession target is no longer accepted"
                );
                anyhow::ensure!(
                    graph::require_information_record(state, &NamedNode::new(previous_iri)?)?
                        == expected_class,
                    "supersession target changed kind"
                );
            }
        }
    }
    for retirement in &operation.preview.retirements {
        if retirement.disposition == SpecRetirementDisposition::Retract {
            anyhow::ensure!(
                current_status(state, &retirement.iri).as_deref() == Some("accepted"),
                "retraction target is no longer accepted"
            );
        }
    }
    if let Some(previous) = &operation.preview.previous_approval_iri {
        anyhow::ensure!(
            current_status(state, previous).as_deref() == Some("accepted"),
            "previous spec approval is no longer current"
        );
    }
    if let Some(plan) = &operation.preview.component {
        let minted = graph::load_components(state)?
            .into_iter()
            .any(|component| component.iri.as_deref() == Some(plan.iri.as_str()));
        anyhow::ensure!(
            minted != plan.new,
            "the spec's component changed since the preview; prepare it again"
        );
    }
    Ok(())
}

fn commit_approval(state: &AppState, operation: &SpecOperation) -> anyhow::Result<()> {
    let stamp = CaptureStamp {
        capture: &state.capture,
        author: SPEC_AUTHOR,
        timestamp: &operation.timestamp,
        status: "accepted",
    };
    let mut insertions = Vec::new();
    let mut removals = Vec::new();
    let mut status_changes = Vec::new();
    let supersedes = state.resolve_object_property("supersedes")?;
    let superseded_by = state.resolve_object_property("isSupersededBy")?;
    let has_rationale = state.resolve_object_property("hasRationale")?;
    let motivated_by = state.resolve_object_property("isMotivatedBy")?;
    let rationale_class = state.resolve_class("Rationale")?;

    for entry in &operation.preview.entries {
        match &entry.disposition {
            SpecDisposition::Reuse { .. } => {}
            SpecDisposition::New { iri } => {
                insertions.extend(record_quads(state, iri, &entry.draft, &[], &stamp)?)
            }
            SpecDisposition::Supersede { iri, previous_iri } => {
                let rationale = operation
                    .rationales
                    .get(previous_iri)
                    .expect("frozen rationale");
                insertions.extend(rationale_quads(
                    state,
                    rationale,
                    &rationale_class,
                    &format!("Rationale: {}", entry.draft.title),
                    &format!(
                        "The approved specification changed the claim previously recorded as {}.",
                        entry.draft.title
                    ),
                    &stamp,
                )?);
                insertions.extend(record_quads(
                    state,
                    iri,
                    &entry.draft,
                    &[
                        (supersedes.clone(), previous_iri.clone()),
                        (has_rationale.clone(), rationale.clone()),
                    ],
                    &stamp,
                )?);
                insertions.push(object_quad(previous_iri, &superseded_by, iri)?);
                plan_status_change(
                    state,
                    previous_iri,
                    "superseded",
                    &mut removals,
                    &mut status_changes,
                )?;
            }
        }
    }

    for retirement in &operation.preview.retirements {
        if retirement.disposition != SpecRetirementDisposition::Retract {
            continue;
        }
        let rationale = operation
            .rationales
            .get(&retirement.iri)
            .expect("frozen rationale");
        insertions.extend(rationale_quads(
            state,
            rationale,
            &rationale_class,
            &format!("Rationale: retract {}", retirement.title),
            &format!(
                "The record is no longer present in the approved specification {}.",
                operation.request.path
            ),
            &stamp,
        )?);
        insertions.push(object_quad(&retirement.iri, &has_rationale, rationale)?);
        plan_status_change(
            state,
            &retirement.iri,
            "deprecated",
            &mut removals,
            &mut status_changes,
        )?;
    }

    let marker_title = marker_title(operation);
    let active_iris: Vec<String> = operation
        .preview
        .entries
        .iter()
        .map(|entry| entry.disposition.active_iri().to_string())
        .collect();
    if let Some(plan) = &operation.preview.component {
        insertions.extend(component_quads(state, plan, &active_iris, &stamp)?);
    }
    let marker_description = marker_description(operation);
    let marker_class = state.resolve_class("ArchitecturalDecision")?;
    let marker_props = vec![
        (moose::RDFS_LABEL.to_string(), marker_title.clone()),
        (state.capture.title.clone(), marker_title.clone()),
        (state.capture.description.clone(), marker_description),
    ];
    let mut marker_edges: Vec<(String, String)> = active_iris
        .iter()
        .map(|iri| (motivated_by.clone(), iri.clone()))
        .collect();
    if let Some(previous) = &operation.preview.previous_approval_iri {
        let rationale = operation
            .rationales
            .get(previous)
            .expect("frozen marker rationale");
        let reason = match &operation.preview.component {
            Some(plan)
                if previous_marker_sha256(state, previous)?.as_deref()
                    == Some(operation.request.source_sha256.as_str()) =>
            {
                format!(
                    "The approval now anchors the specification's records to component {}; the source digest is unchanged.",
                    plan.name
                )
            }
            _ => "A changed source digest replaced the previous approval of this specification."
                .to_string(),
        };
        insertions.extend(rationale_quads(
            state,
            rationale,
            &rationale_class,
            &format!("Rationale: {marker_title}"),
            &reason,
            &stamp,
        )?);
        marker_edges.push((supersedes.clone(), previous.clone()));
        marker_edges.push((has_rationale.clone(), rationale.clone()));
        insertions.push(object_quad(
            previous,
            &superseded_by,
            &operation.approval_iri,
        )?);
        plan_status_change(
            state,
            previous,
            "superseded",
            &mut removals,
            &mut status_changes,
        )?;
    }
    insertions.extend(graph::capture_instance_quads(
        &state.store,
        &operation.approval_iri,
        &marker_class,
        &marker_props,
        &marker_edges,
        &stamp,
    )?);
    insertions.extend(status_changes);

    let mut transaction = state.store.start_transaction()?;
    for quad in &removals {
        transaction.remove(quad.as_ref());
    }
    transaction.extend(insertions.iter().map(Quad::as_ref));
    transaction.commit()?;
    Ok(())
}

fn record_quads(
    state: &AppState,
    iri: &str,
    draft: &SpecRecordDraft,
    relations: &[(String, String)],
    stamp: &CaptureStamp<'_>,
) -> anyhow::Result<Vec<Quad>> {
    let class = state.resolve_class(&draft.kind)?;
    let properties = vec![
        (moose::RDFS_LABEL.to_string(), draft.title.clone()),
        (state.capture.title.clone(), draft.title.clone()),
        (state.capture.description.clone(), claim(draft)),
    ];
    graph::capture_instance_quads(&state.store, iri, &class, &properties, relations, stamp)
}

fn rationale_quads(
    state: &AppState,
    iri: &str,
    class: &str,
    title: &str,
    description: &str,
    stamp: &CaptureStamp<'_>,
) -> anyhow::Result<Vec<Quad>> {
    graph::capture_instance_quads(
        &state.store,
        iri,
        class,
        &[
            (moose::RDFS_LABEL.to_string(), title.to_string()),
            (state.capture.title.clone(), title.to_string()),
            (state.capture.description.clone(), description.to_string()),
        ],
        &[],
        stamp,
    )
}

/// The component the approval anchors its records to, from the paths the
/// human named: an existing component with that name gains any new paths,
/// otherwise one is minted. A directory keeps or gains its trailing `/` and
/// need not exist yet (a spec may describe a crate still to be created); a
/// file path is exact; a path that does not exist yet and has no trailing
/// slash is a directory unless its last segment has an extension; `.` covers
/// the whole project.
fn plan_component(
    state: &AppState,
    request: &SpecPrepareRequest,
) -> anyhow::Result<Option<SpecComponentPlan>> {
    if request.covers.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        request.covers.len() <= 16,
        "a specification covers at most 16 paths"
    );
    let root = state.project_root().canonicalize()?;
    let mut covers: Vec<String> = Vec::new();
    for raw in &request.covers {
        let path = if raw == graph::COVERS_WHOLE_PROJECT {
            raw.clone()
        } else {
            let directory = raw.ends_with('/');
            let trimmed = raw.trim_end_matches('/');
            validate_path(trimmed)?;
            anyhow::ensure!(
                !trimmed.is_empty()
                    && trimmed.len() <= 4096
                    && !trimmed.chars().any(char::is_control),
                "covered path is empty, too long or contains control characters"
            );
            match root.join(trimmed).canonicalize() {
                Ok(target) => {
                    anyhow::ensure!(
                        target.starts_with(&root),
                        "covered path escapes the project"
                    );
                    if target.is_dir() {
                        format!("{trimmed}/")
                    } else {
                        anyhow::ensure!(
                            !directory,
                            "covered path {raw} is a file, not a directory"
                        );
                        trimmed.to_string()
                    }
                }
                // A spec often describes a crate or file still to be created.
                // Without a trailing slash, a last segment with no extension
                // is a directory (`crates/map`) and one with an extension an
                // exact file (`docs/map.md`); the preview shows which.
                Err(_)
                    if directory
                        || !trimmed.rsplit('/').next().unwrap_or(trimmed).contains('.') =>
                {
                    format!("{trimmed}/")
                }
                Err(_) => trimmed.to_string(),
            }
        };
        if !covers.contains(&path) {
            covers.push(path);
        }
    }
    let name = match covers[0].as_str() {
        graph::COVERS_WHOLE_PROJECT => Path::new(&request.path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("project")
            .to_string(),
        first => first
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(first)
            .to_string(),
    };
    let components = graph::load_components(state)?;
    let existing = components
        .iter()
        .find(|component| component.name.eq_ignore_ascii_case(&name));
    Ok(Some(match existing {
        Some(component) => {
            let added: Vec<String> = covers
                .iter()
                .filter(|path| !component.covers_paths.contains(*path))
                .cloned()
                .collect();
            let mut all: Vec<String> = component.covers_paths.iter().cloned().collect();
            all.extend(added.iter().cloned());
            SpecComponentPlan {
                iri: component
                    .iri
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("component {name} has no IRI"))?,
                name: component.name.clone(),
                new: false,
                covers: all,
                added,
            }
        }
        None => SpecComponentPlan {
            iri: graph::mint_instance_iri("SystemComponent"),
            name,
            new: true,
            covers: covers.clone(),
            added: covers,
        },
    }))
}

/// The quads that mint or extend the component and attach every active
/// record to it with `concerns`.
fn component_quads(
    state: &AppState,
    plan: &SpecComponentPlan,
    active_iris: &[String],
    stamp: &CaptureStamp<'_>,
) -> anyhow::Result<Vec<Quad>> {
    let covers_path = graph::datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let concerns = state.resolve_object_property("concerns")?;
    let graph_name = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let mut quads = Vec::new();
    if plan.new {
        let class = state.resolve_class("SystemComponent")?;
        let mut properties = vec![
            (moose::RDFS_LABEL.to_string(), plan.name.clone()),
            (state.capture.title.clone(), plan.name.clone()),
        ];
        properties.extend(
            plan.covers
                .iter()
                .map(|path| (covers_path.clone(), path.clone())),
        );
        quads.extend(graph::capture_instance_quads(
            &state.store,
            &plan.iri,
            &class,
            &properties,
            &[],
            stamp,
        )?);
    } else {
        for path in &plan.added {
            quads.push(Quad::new(
                NamedNode::new(&plan.iri)?,
                NamedNode::new(&covers_path)?,
                Literal::new_simple_literal(path.as_str()),
                graph_name.clone(),
            ));
        }
    }
    for iri in active_iris {
        quads.push(object_quad(iri, &concerns, &plan.iri)?);
    }
    Ok(quads)
}

/// The `spec-sha256` line of an approval marker.
fn previous_marker_sha256(state: &AppState, marker_iri: &str) -> anyhow::Result<Option<String>> {
    Ok(
        graph::first_literal(&state.store, marker_iri, &state.capture.description)
            .as_deref()
            .and_then(|description| {
                description
                    .lines()
                    .find_map(|line| line.strip_prefix("spec-sha256: "))
                    .map(str::to_string)
            }),
    )
}

fn object_quad(subject: &str, predicate: &str, object: &str) -> anyhow::Result<Quad> {
    Ok(Quad::new(
        NamedNode::new(subject)?,
        NamedNode::new(predicate)?,
        NamedNode::new(object)?,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    ))
}

fn plan_status_change(
    state: &AppState,
    iri: &str,
    status: &str,
    removals: &mut Vec<Quad>,
    insertions: &mut Vec<Quad>,
) -> anyhow::Result<()> {
    removals.extend(
        state
            .store
            .quads_for_pattern(
                Some(NamedNodeRef::new(iri)?.into()),
                Some(NamedNodeRef::new(&state.capture.status)?),
                None,
                Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                    PROJECT_KG_GRAPH_IRI,
                )?)),
            )
            .collect::<Result<Vec<_>, _>>()?,
    );
    insertions.push(Quad::new(
        NamedNode::new(iri)?,
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal(status),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    ));
    Ok(())
}

#[derive(Clone)]
struct ExistingRecord {
    iri: String,
    kind: String,
    title: String,
    description: String,
}

fn existing_snapshot(record: ExistingRecord) -> SpecExistingRecord {
    SpecExistingRecord {
        iri: record.iri,
        kind: record.kind,
        title: record.title,
        description: record.description,
    }
}

fn current_record(state: &AppState, iri: &str) -> anyhow::Result<Option<ExistingRecord>> {
    if current_status(state, iri).as_deref() != Some("accepted") {
        return Ok(None);
    }
    let class = graph::require_information_record(state, &NamedNode::new(iri)?)?;
    Ok(Some(existing_record(
        state,
        iri,
        graph::local_name(&class).to_string(),
    )?))
}

struct ApprovalMarker {
    iri: String,
    source_sha256: Option<String>,
}

/// Every approved specification against its file: a digest that no longer
/// matches, or a file that is gone, marks the approval's records as possibly
/// stale until the spec is approved again.
pub(super) fn approved_spec_statuses(state: &AppState) -> anyhow::Result<Vec<ApprovedSpecStatus>> {
    let root = state.project_root();
    let mut statuses = Vec::new();
    for (path, marker) in current_approval_markers(state)? {
        let current = std::fs::read(root.join(&path)).ok().map(sha256_hex);
        statuses.push(ApprovedSpecStatus {
            stale: current.is_none() || current != marker.source_sha256,
            record_count: marker_targets(state, &marker.iri)?.len(),
            path,
        });
    }
    statuses.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(statuses)
}

fn current_approval_for_path(
    state: &AppState,
    path: &str,
) -> anyhow::Result<Option<ApprovalMarker>> {
    let mut found: Vec<ApprovalMarker> = current_approval_markers(state)?
        .into_iter()
        .filter(|(marked, _)| marked == path)
        .map(|(_, marker)| marker)
        .collect();
    anyhow::ensure!(
        found.len() <= 1,
        "multiple current approval markers exist for this spec path"
    );
    Ok(found.pop())
}

/// Every accepted approval marker with the spec path it carries.
fn current_approval_markers(state: &AppState) -> anyhow::Result<Vec<(String, ApprovalMarker)>> {
    let mut found = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(NamedNodeRef::new(&state.capture.description)?),
        None,
        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
            PROJECT_KG_GRAPH_IRI,
        )?)),
    ) {
        let quad = quad?;
        let Term::Literal(description) = quad.object else {
            continue;
        };
        let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        let Some(path) = description
            .value()
            .lines()
            .find_map(|line| line.strip_prefix("spec-approval: "))
        else {
            continue;
        };
        if current_status(state, subject.as_str()).as_deref() != Some("accepted") {
            continue;
        }
        let class = graph::require_information_record(state, &subject)?;
        if graph::local_name(&class) != "ArchitecturalDecision" {
            continue;
        }
        found.push((
            path.to_string(),
            ApprovalMarker {
                iri: subject.as_str().to_string(),
                source_sha256: description
                    .value()
                    .lines()
                    .find_map(|line| line.strip_prefix("spec-sha256: ").map(str::to_string)),
            },
        ));
    }
    Ok(found)
}

fn marker_targets(state: &AppState, marker: &str) -> anyhow::Result<Vec<ExistingRecord>> {
    let predicate = state.resolve_object_property("isMotivatedBy")?;
    let mut records = Vec::new();
    for quad in state.store.quads_for_pattern(
        Some(NamedNodeRef::new(marker)?.into()),
        Some(NamedNodeRef::new(&predicate)?),
        None,
        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
            PROJECT_KG_GRAPH_IRI,
        )?)),
    ) {
        let quad = quad?;
        let Term::NamedNode(target) = quad.object else {
            continue;
        };
        if current_status(state, target.as_str()).as_deref() != Some("accepted") {
            continue;
        }
        let class = graph::require_information_record(state, &target)?;
        let kind = graph::local_name(&class).to_string();
        if !matches!(kind.as_str(), "Requirement" | "Constraint") {
            continue;
        }
        records.push(existing_record(state, target.as_str(), kind));
    }
    records.into_iter().collect()
}

fn existing_record(state: &AppState, iri: &str, kind: String) -> anyhow::Result<ExistingRecord> {
    Ok(ExistingRecord {
        iri: iri.to_string(),
        kind,
        title: graph::first_literal(&state.store, iri, &state.capture.title).unwrap_or_default(),
        description: graph::first_literal(&state.store, iri, &state.capture.description)
            .unwrap_or_default(),
    })
}

fn current_records_with_title(
    state: &AppState,
    kind: &str,
    title: &str,
) -> anyhow::Result<Vec<ExistingRecord>> {
    graph::resolve_record_exact_all(state, title)
        .into_iter()
        .filter_map(|(iri, class)| {
            (graph::local_name(&class) == kind
                && current_status(state, &iri).as_deref() == Some("accepted"))
            .then(|| existing_record(state, &iri, kind.to_string()))
        })
        .collect()
}

fn referenced_by_other_current_approval(
    state: &AppState,
    target: &str,
    excluded_marker: Option<&str>,
) -> anyhow::Result<bool> {
    let predicate = state.resolve_object_property("isMotivatedBy")?;
    for quad in state.store.quads_for_pattern(
        None,
        Some(NamedNodeRef::new(&predicate)?),
        Some(NamedNodeRef::new(target)?.into()),
        Some(GraphNameRef::NamedNode(NamedNodeRef::new(
            PROJECT_KG_GRAPH_IRI,
        )?)),
    ) {
        let quad = quad?;
        let oxigraph::model::NamedOrBlankNode::NamedNode(subject_node) = quad.subject else {
            continue;
        };
        let subject = subject_node.as_str();
        if Some(subject) == excluded_marker
            || current_status(state, subject).as_deref() != Some("accepted")
        {
            continue;
        }
        let description = graph::first_literal(&state.store, subject, &state.capture.description)
            .unwrap_or_default();
        if description
            .lines()
            .any(|line| line.starts_with("spec-approval: "))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_source(state: &AppState, path: &str, expected: &str) -> anyhow::Result<()> {
    validate_path(path)?;
    anyhow::ensure!(
        path.len() <= 4096 && !path.chars().any(char::is_control),
        "spec path is too long or contains control characters"
    );
    anyhow::ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "source_sha256 must be lower-case SHA-256 hex"
    );
    let root = state.project_root().canonicalize()?;
    let source = root.join(path).canonicalize()?;
    anyhow::ensure!(
        source.starts_with(&root) && source.is_file(),
        "spec path escapes the project or is not a file"
    );
    anyhow::ensure!(
        source.metadata()?.len() <= 2 * 1024 * 1024,
        "spec exceeds the 2 MiB workspace file limit"
    );
    let bytes = std::fs::read(source)?;
    anyhow::ensure!(
        std::str::from_utf8(&bytes).is_ok(),
        "spec must be UTF-8 text"
    );
    anyhow::ensure!(
        sha256_hex(bytes) == expected,
        "spec source changed; prepare a fresh preview"
    );
    Ok(())
}

fn validate_drafts(path: &str, drafts: &[SpecRecordDraft], state: &AppState) -> anyhow::Result<()> {
    anyhow::ensure!(
        !drafts.is_empty() && drafts.len() <= MAX_SPEC_RECORDS,
        "spec approval needs 1..={MAX_SPEC_RECORDS} records"
    );
    let source = std::fs::read_to_string(state.project_root().join(path))?;
    let line_count = source.lines().count().max(1);
    let mut titles = HashSet::new();
    for draft in drafts {
        check_spec_draft(draft).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            titles.insert((draft.kind.clone(), normalize(&draft.title))),
            "duplicate spec record title and kind"
        );
        for evidence in &draft.evidence {
            validate_evidence(path, evidence, line_count)?;
        }
    }
    Ok(())
}

fn validate_evidence(path: &str, evidence: &str, line_count: usize) -> anyhow::Result<()> {
    let range = evidence
        .strip_prefix(&format!("{path}:"))
        .ok_or_else(|| anyhow::anyhow!("spec evidence must use {path}:line or {path}:start-end"))?;
    let (start, end) = range.split_once('-').unwrap_or((range, range));
    let start: usize = start.parse()?;
    let end: usize = end.parse()?;
    let canonical = if start == end {
        format!("{path}:{start}")
    } else {
        format!("{path}:{start}-{end}")
    };
    anyhow::ensure!(
        evidence == canonical && start >= 1 && start <= end && end <= line_count,
        "spec evidence line range is outside the source"
    );
    Ok(())
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn claim(draft: &SpecRecordDraft) -> String {
    format!(
        "{}\n\nEvidence:\n{}",
        draft.description,
        draft
            .evidence
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmConfig, StructuredOutputMode, DEFAULT_LLM_CONTEXT_WINDOW_TOKENS};
    use std::path::Path;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("moosedev-spec-approval-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("docs")).unwrap();
            Self(root)
        }

        fn state(&self) -> AppState {
            AppState::bootstrap_with_llm_config(
                &self.0.join(".moosedev"),
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
                LlmConfig {
                    base_url: "http://127.0.0.1:1/v1".into(),
                    api_key: "test".into(),
                    model: "unused".into(),
                    configured: false,
                    context_window_tokens: DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
                    structured_output: StructuredOutputMode::Auto,
                    timeouts: Default::default(),
                },
            )
            .unwrap()
        }

        fn write_spec(&self, text: &str) -> String {
            std::fs::write(self.0.join("docs/spec.md"), text).unwrap();
            sha256_hex(text)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn draft(kind: &str, title: &str, description: &str, line: usize) -> SpecRecordDraft {
        SpecRecordDraft {
            kind: kind.into(),
            title: title.into(),
            description: description.into(),
            evidence: vec![format!("docs/spec.md:{line}")],
        }
    }

    fn prepare_request(
        id: &str,
        hash: String,
        revision: String,
        drafts: Vec<SpecRecordDraft>,
    ) -> SpecPrepareRequest {
        SpecPrepareRequest {
            operation_id: id.into(),
            owner_id: "spec-test".into(),
            path: "docs/spec.md".into(),
            source_sha256: hash,
            knowledge_revision: revision,
            drafts,
            covers: vec![],
        }
    }

    #[test]
    fn evidence_ranges_are_path_bound_and_one_based() {
        assert!(validate_evidence("docs/spec.md", "docs/spec.md:2-4", 4).is_ok());
        assert!(validate_evidence("docs/spec.md", "other.md:2", 4).is_err());
        assert!(validate_evidence("docs/spec.md", "docs/spec.md:0", 4).is_err());
        assert!(validate_evidence("docs/spec.md", "docs/spec.md:01", 4).is_err());
        assert!(validate_evidence("docs/spec.md", "docs/spec.md:2-2", 4).is_err());
        assert!(validate_evidence("docs/spec.md", "docs/spec.md:4-5", 4).is_err());
    }

    #[test]
    fn claim_has_capture_compatible_evidence_shape() {
        assert_eq!(
            claim(&SpecRecordDraft {
                kind: "Requirement".into(),
                title: "Multiline input".into(),
                description: "The harness supports multiline input.".into(),
                evidence: vec!["docs/spec.md:8-9".into()],
            }),
            "The harness supports multiline input.\n\nEvidence:\n- docs/spec.md:8-9"
        );
    }

    #[test]
    fn an_unchanged_approved_source_prepares_again_from_its_own_records() {
        let fixture = Fixture::new();
        let hash =
            fixture.write_spec("# Map crate\nParse SCRATCHMAP 1 files.\nNo rusqlite dependency.\n");
        let state = fixture.state();
        let none = current_operation(
            &state,
            SpecCurrentRequest {
                path: "docs/spec.md".into(),
            },
        )
        .unwrap();
        assert_eq!(none.approval_iri, None);
        assert!(none.drafts.is_empty());

        let revision = accepted_revision(&state).unwrap();
        let drafts = vec![
            draft(
                "Requirement",
                "Parse maps",
                "The crate parses SCRATCHMAP 1 files.",
                2,
            ),
            draft(
                "Constraint",
                "No SQLite",
                "The crate has no rusqlite dependency.",
                3,
            ),
        ];
        prepare_operation(
            &state,
            prepare_request("spec-cur-1", hash.clone(), revision, drafts.clone()),
        )
        .unwrap();
        let approved = approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-cur-1".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();

        let current = current_operation(
            &state,
            SpecCurrentRequest {
                path: "docs/spec.md".into(),
            },
        )
        .unwrap();
        assert_eq!(
            current.approval_iri.as_deref(),
            Some(approved.approval_iri.as_str())
        );
        assert_eq!(current.source_sha256.as_deref(), Some(hash.as_str()));
        let mut expected = drafts.clone();
        expected.sort_by(|a, b| (&a.kind, &a.title).cmp(&(&b.kind, &b.title)));
        assert_eq!(
            current.drafts, expected,
            "drafts round-trip through the stored claim"
        );

        // Preparing the same source from those drafts, now with a covered
        // path, reuses every record: no supersession from rewording.
        let revision = accepted_revision(&state).unwrap();
        let mut request = prepare_request("spec-cur-2", hash, revision, current.drafts);
        request.covers = vec!["badciv-map/".into()];
        let preview = prepare_operation(&state, request).unwrap();
        assert!(!preview.already_approved, "the component is new");
        assert!(preview
            .entries
            .iter()
            .all(|entry| matches!(entry.disposition, SpecDisposition::Reuse { .. })));
        assert!(preview.retirements.is_empty());
    }

    #[test]
    fn a_covered_path_not_created_yet_is_read_by_its_shape() {
        let fixture = Fixture::new();
        let hash = fixture.write_spec("# Map crate\nParse SCRATCHMAP 1 files.\n");
        let state = fixture.state();
        let revision = accepted_revision(&state).unwrap();
        let drafts = vec![draft(
            "Requirement",
            "Parse SCRATCHMAP 1",
            "The crate parses SCRATCHMAP 1 files.",
            2,
        )];
        let mut request = prepare_request("spec-shape", hash, revision, drafts);
        request.covers = vec!["crates/badciv-map".into(), "docs/map-format.md".into()];
        let plan = plan_component(&state, &request).unwrap().unwrap();
        assert_eq!(
            plan.covers,
            vec![
                "crates/badciv-map/".to_string(),
                "docs/map-format.md".into()
            ]
        );
        assert_eq!(plan.name, "badciv-map");
    }

    #[test]
    fn approval_anchors_its_records_to_the_covering_component() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("badciv-map/src")).unwrap();
        let hash =
            fixture.write_spec("# Map crate\nParse SCRATCHMAP 1 files.\nNo rusqlite dependency.\n");
        let state = fixture.state();
        let revision = accepted_revision(&state).unwrap();
        let drafts = vec![
            draft(
                "Requirement",
                "Parse SCRATCHMAP 1",
                "The crate parses SCRATCHMAP 1 files.",
                2,
            ),
            draft(
                "Constraint",
                "No rusqlite",
                "The crate must not depend on rusqlite.",
                3,
            ),
        ];
        let mut request = prepare_request("spec-map", hash.clone(), revision, drafts.clone());
        // A directory not created yet keeps its trailing slash; an existing one
        // gains it; the component is named after the first path.
        request.covers = vec!["badciv-map".into(), "badciv-sim/".into()];
        let preview = prepare_operation(&state, request).unwrap();
        let plan = preview.component.clone().unwrap();
        assert!(plan.new);
        assert_eq!(plan.name, "badciv-map");
        assert_eq!(
            plan.covers,
            vec!["badciv-map/".to_string(), "badciv-sim/".into()]
        );
        assert_eq!(plan.added, plan.covers);
        assert!(
            graph::load_components(&state).unwrap().is_empty(),
            "prepare writes nothing"
        );

        let approved = approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-map".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        assert!(approved.checkpoint.conforms);
        let components = graph::load_components(&state).unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].iri.as_deref(), Some(plan.iri.as_str()));
        assert_eq!(components[0].name, "badciv-map");
        assert_eq!(
            components[0]
                .covers_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["badciv-map/".to_string(), "badciv-sim/".into()]
        );
        let concerns = state.resolve_object_property("concerns").unwrap();
        for record in &approved.records {
            assert_eq!(
                direct_objects(&state, &record.iri, &concerns).unwrap(),
                HashSet::from([plan.iri.clone()]),
                "{}",
                record.title
            );
        }
        assert_eq!(
            graph::best_component_for_path("badciv-map/src/parse.rs", &components)
                .unwrap()
                .name,
            "badciv-map"
        );
        assert!(crate::validation::validate_project(&state)
            .unwrap()
            .conforms());

        // The same spec with one more covered path: records unchanged, but the
        // component gains the path, so the batch is approved again rather than
        // reported as already approved.
        let revision = accepted_revision(&state).unwrap();
        let mut request = prepare_request("spec-map-2", hash.clone(), revision, drafts.clone());
        request.covers = vec!["badciv-map/".into(), "docs/spec.md".into()];
        let preview = prepare_operation(&state, request).unwrap();
        assert!(!preview.already_approved);
        assert!(preview
            .entries
            .iter()
            .all(|entry| matches!(entry.disposition, SpecDisposition::Reuse { .. })));
        let plan = preview.component.clone().unwrap();
        assert!(!plan.new);
        assert_eq!(plan.added, vec!["docs/spec.md".to_string()]);
        let again = approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-map-2".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        assert_ne!(again.approval_iri, approved.approval_iri);
        assert_eq!(
            current_status(&state, &approved.approval_iri).as_deref(),
            Some("superseded")
        );
        let components = graph::load_components(&state).unwrap();
        assert_eq!(components.len(), 1);
        assert!(components[0].covers_paths.contains("docs/spec.md"));
        assert!(components[0].covers_paths.contains("badciv-sim/"));

        // Nothing new for the component either: already approved.
        let revision = accepted_revision(&state).unwrap();
        let mut request = prepare_request("spec-map-3", hash, revision, drafts);
        request.covers = vec!["badciv-map/".into()];
        let preview = prepare_operation(&state, request).unwrap();
        assert!(preview.already_approved);
        assert!(crate::validation::validate_project(&state)
            .unwrap()
            .conforms());
    }

    #[test]
    fn context_reports_an_approved_spec_whose_file_changed() {
        let fixture = Fixture::new();
        let hash = fixture.write_spec("# Spec\nThe harness accepts specs.\n");
        let state = fixture.state();
        let revision = accepted_revision(&state).unwrap();
        let request = prepare_request(
            "spec-drift",
            hash,
            revision,
            vec![draft(
                "Requirement",
                "Approve explicit specs",
                "The harness accepts an explicit spec approval.",
                2,
            )],
        );
        prepare_operation(&state, request).unwrap();
        approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-drift".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        let context = |state: &AppState| {
            crate::harness::daemon::context_snapshot(
                state,
                &ContextRequest {
                    topic: "specs".into(),
                    files: vec![],
                    evidence_only: false,
                    max_bytes: None,
                },
            )
            .unwrap()
        };
        let fresh = context(&state);
        assert_eq!(
            fresh.approved_specs,
            vec![ApprovedSpecStatus {
                path: "docs/spec.md".into(),
                stale: false,
                record_count: 1,
            }]
        );
        assert!(!fresh.context.contains("changed since its approval"));

        fixture.write_spec("# Spec\nThe harness accepts specs and more.\n");
        let drifted = context(&state);
        assert!(drifted.approved_specs[0].stale);
        assert!(drifted.context.contains(
            "Approved spec docs/spec.md changed since its approval (1 record(s) may be stale); run /approve-spec docs/spec.md to reconcile them."
        ), "{}", drifted.context);

        std::fs::remove_file(fixture.0.join("docs/spec.md")).unwrap();
        assert!(context(&state).approved_specs[0].stale);
    }

    #[test]
    fn prepare_is_read_only_and_approval_is_accepted_and_replay_safe() {
        let fixture = Fixture::new();
        let hash = fixture.write_spec("# Spec\nThe harness accepts specs.\nSpecs remain local.\n");
        let state = fixture.state();
        let before = accepted_revision(&state).unwrap();
        let request = prepare_request(
            "spec-first",
            hash,
            before.clone(),
            vec![
                draft(
                    "Requirement",
                    "Approve explicit specs",
                    "The harness accepts an explicit spec approval.",
                    2,
                ),
                draft(
                    "Constraint",
                    "Keep spec approval local",
                    "Spec approval remains local to the repository.",
                    3,
                ),
            ],
        );
        let preview = prepare_operation(&state, request).unwrap();
        assert_eq!(accepted_revision(&state).unwrap(), before);
        assert!(preview
            .entries
            .iter()
            .all(|entry| matches!(entry.disposition, SpecDisposition::New { .. })));

        let approve = SpecApproveRequest {
            operation_id: "spec-first".into(),
            owner_id: "spec-test".into(),
        };
        let first = approve_operation(&state, approve.clone()).unwrap();
        assert!(first.checkpoint.conforms);
        assert_ne!(first.result_revision, before);
        assert_eq!(
            current_status(&state, &first.approval_iri).as_deref(),
            Some("accepted")
        );
        assert!(first
            .records
            .iter()
            .all(|record| { current_status(&state, &record.iri).as_deref() == Some("accepted") }));

        // Simulate interruption after the graph commit but before the response
        // was journaled. The frozen marker identity makes the retry replay.
        let journal = journal_path(&state, "spec-first", "spec.json").unwrap();
        let mut interrupted: SpecOperation = load(&journal).unwrap().unwrap();
        interrupted.response = None;
        save_operation(&journal, &interrupted).unwrap();
        fixture.write_spec("# Changed after the approval committed\n");
        let replay = approve_operation(&state, approve).unwrap();
        assert_eq!(replay.approval_iri, first.approval_iri);
        assert_eq!(replay.result_revision, first.result_revision);

        let mut removals = Vec::new();
        let mut insertions = Vec::new();
        plan_status_change(
            &state,
            &first.approval_iri,
            "superseded",
            &mut removals,
            &mut insertions,
        )
        .unwrap();
        let mut transaction = state.store.start_transaction().unwrap();
        for quad in &removals {
            transaction.remove(quad.as_ref());
        }
        transaction.extend(insertions.iter().map(Quad::as_ref));
        transaction.commit().unwrap();
        state.note_project_write();
        assert!(approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-first".into(),
                owner_id: "spec-test".into(),
            }
        )
        .is_err());
    }

    #[test]
    fn changed_spec_previews_and_atomically_applies_supersession_and_retraction() {
        let fixture = Fixture::new();
        let first_hash = fixture.write_spec("# Spec\nApprove specs.\nStay local.\n");
        let state = fixture.state();
        let first_preview = prepare_operation(
            &state,
            prepare_request(
                "spec-v1",
                first_hash,
                accepted_revision(&state).unwrap(),
                vec![
                    draft(
                        "Requirement",
                        "Approve specs",
                        "Approve specs explicitly.",
                        2,
                    ),
                    draft("Constraint", "Stay local", "Spec approval stays local.", 3),
                ],
            ),
        )
        .unwrap();
        approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-v1".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        let old_requirement = first_preview.entries[0]
            .disposition
            .active_iri()
            .to_string();
        let old_constraint = first_preview.entries[1]
            .disposition
            .active_iri()
            .to_string();

        let second_hash = fixture.write_spec("# Spec\nApprove reviewed specs.\n");
        let second = prepare_operation(
            &state,
            prepare_request(
                "spec-v2",
                second_hash,
                accepted_revision(&state).unwrap(),
                vec![draft(
                    "Requirement",
                    "Approve specs",
                    "Approve specs only after displaying the extracted records.",
                    2,
                )],
            ),
        )
        .unwrap();
        assert!(matches!(
            &second.entries[0].disposition,
            SpecDisposition::Supersede { previous_iri, .. } if previous_iri == &old_requirement
        ));
        assert_eq!(second.retirements.len(), 1);
        assert_eq!(second.retirements[0].iri, old_constraint);
        assert_eq!(
            second.retirements[0].disposition,
            SpecRetirementDisposition::Retract
        );

        let approved = approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-v2".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        assert!(approved.checkpoint.conforms);
        assert_eq!(
            current_status(&state, &old_requirement).as_deref(),
            Some("superseded")
        );
        assert_eq!(
            current_status(&state, &old_constraint).as_deref(),
            Some("deprecated")
        );
        assert_eq!(
            current_status(&state, &approved.records[0].iri).as_deref(),
            Some("accepted")
        );
    }

    #[test]
    fn preparation_reuses_a_high_confidence_symbolic_restatement() {
        let fixture = Fixture::new();
        let hash = fixture.write_spec("# Spec\nExplicit spec approval records remain durable.\n");
        let state = fixture.state();
        let draft = draft(
            "Requirement",
            "Explicit spec approval records remain durable",
            "Approved spec records remain durable across retries.",
            2,
        );
        let existing_title = "Spec approval records remain durable";
        let existing = graph::record_instance(
            &state,
            &RecordInput {
                class_iri: state.resolve_class("Requirement").unwrap(),
                class_local: "Requirement".into(),
                properties: vec![
                    (moose::RDFS_LABEL.into(), existing_title.into()),
                    (state.capture.title.clone(), existing_title.into()),
                    (state.capture.description.clone(), claim(&draft)),
                ],
            },
            "test-human",
            Utc::now(),
        )
        .unwrap();
        state.note_project_write();

        let preview = prepare_operation(
            &state,
            prepare_request(
                "spec-symbolic-reuse",
                hash,
                accepted_revision(&state).unwrap(),
                vec![draft],
            ),
        )
        .unwrap();
        assert!(matches!(
            &preview.entries[0].disposition,
            SpecDisposition::Reuse { iri } if iri == &existing
        ));
    }

    #[test]
    fn changed_claim_does_not_supersede_a_record_owned_by_another_spec() {
        let fixture = Fixture::new();
        let first_hash = fixture.write_spec("# Spec A\nShared rule.\n");
        let state = fixture.state();
        let first = prepare_operation(
            &state,
            prepare_request(
                "spec-shared-a1",
                first_hash,
                accepted_revision(&state).unwrap(),
                vec![draft(
                    "Requirement",
                    "Shared rule",
                    "The original shared rule applies.",
                    2,
                )],
            ),
        )
        .unwrap();
        approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-shared-a1".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        let shared = first.entries[0].disposition.active_iri().to_string();

        let marker_title = "Spec docs/spec-b.md approved for implementation";
        graph::record_instance_with_relation_args(
            &state,
            &RecordInput {
                class_iri: state.resolve_class("ArchitecturalDecision").unwrap(),
                class_local: "ArchitecturalDecision".into(),
                properties: vec![
                    (moose::RDFS_LABEL.into(), marker_title.into()),
                    (state.capture.title.clone(), marker_title.into()),
                    (
                        state.capture.description.clone(),
                        "Approved specification.\n\nspec-approval: docs/spec-b.md\nspec-sha256: fixture"
                            .into(),
                    ),
                ],
            },
            &[("isMotivatedBy".into(), shared.clone())],
            "test-human",
            Utc::now(),
        )
        .unwrap();
        state.note_project_write();

        let second_hash = fixture.write_spec("# Spec A\nChanged shared rule.\n");
        let second = prepare_operation(
            &state,
            prepare_request(
                "spec-shared-a2",
                second_hash,
                accepted_revision(&state).unwrap(),
                vec![draft(
                    "Requirement",
                    "Shared rule",
                    "Spec A now requires a different shared rule.",
                    2,
                )],
            ),
        )
        .unwrap();
        assert!(matches!(
            second.entries[0].disposition,
            SpecDisposition::New { .. }
        ));
        assert_eq!(second.retirements.len(), 1);
        assert_eq!(second.retirements[0].iri, shared);
        assert_eq!(
            second.retirements[0].disposition,
            SpecRetirementDisposition::RetainShared
        );

        approve_operation(
            &state,
            SpecApproveRequest {
                operation_id: "spec-shared-a2".into(),
                owner_id: "spec-test".into(),
            },
        )
        .unwrap();
        assert_eq!(
            current_status(&state, &second.retirements[0].iri).as_deref(),
            Some("accepted")
        );
    }
}
