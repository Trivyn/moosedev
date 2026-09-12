//! Capture typing: one plain-prose note from the coding model,
//! plus the plan, the diff and the check history, become typed proposals.
//! Two proposals are purely symbolic (a decision for the change, a lesson for
//! a check that failed then passed); a bounded LLM sensor may add more when
//! the daemon has one. Every proposal is then reconciled symbolically against
//! the graph and receives a durable receipt. No graph writes happen here.
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use moose::types::LlmAssistLevel;
use serde::Deserialize;
use serde_json::json;

use super::current_status;
use super::journal::{journal_path, load, save_operation, validate_id};
use super::reconcile_score::{record_receipt, score_proposal, ScoreReceipt, ScoredDisposition};
use super::revision::ensure_unchanged;
use crate::api::error::ApiError;
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const MAX_SENSOR_PROPOSALS: usize = 5;
const MAX_PROPOSALS: usize = 16;
const MAX_TITLE_CHARS: usize = 100;

pub async fn capture_type(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureTypeRequest>,
) -> Result<Json<CaptureTypeResponse>, ApiError> {
    Ok(Json(capture_type_operation(&state, request).await?))
}

#[derive(Debug, Deserialize)]
struct SensorTyping {
    proposals: Vec<SensorProposal>,
    #[allow(dead_code)]
    reason: String,
}
#[derive(Debug, Deserialize)]
struct SensorProposal {
    kind: String,
    title: String,
    description: String,
}

fn cap_title(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_TITLE_CHARS {
        return text;
    }
    let capped: String = text.chars().take(MAX_TITLE_CHARS - 3).collect();
    format!("{}…", capped.trim_end())
}

fn normalized(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn base_proposal(
    kind: &str,
    title: String,
    description: String,
    evidence: Vec<String>,
    files: Vec<String>,
) -> KnowledgeProposal {
    KnowledgeProposal {
        kind: kind.into(),
        title,
        description,
        evidence,
        files,
        components: vec![],
        requirement: None,
        supersedes: None,
        retracts: None,
        reconciled: vec![],
    }
}

/// Idempotent by operation id: a replay returns the stored typing.
pub async fn capture_type_operation(
    state: &AppState,
    request: CaptureTypeRequest,
) -> anyhow::Result<CaptureTypeResponse> {
    validate_id(&request.owner_id, "owner_id")?;
    let path = journal_path(state, &request.operation_id, "type.json")?;
    if let Some(stored) = load::<StoredTyping>(&path)? {
        anyhow::ensure!(
            stored.request == request,
            "operation_id was already used for a different capture typing"
        );
        return Ok(stored.response);
    }
    // The sensor runs outside the operation lock; see `journal` for the race
    // this admits on same-id retries.
    let response = type_note(state, &request).await?;
    save_operation(
        &path,
        &StoredTyping {
            request,
            response: response.clone(),
        },
    )?;
    Ok(response)
}

/// Types one note against the current graph; the caller journals the result.
async fn type_note(
    state: &AppState,
    request: &CaptureTypeRequest,
) -> anyhow::Result<CaptureTypeResponse> {
    ensure_unchanged(
        state,
        &request.knowledge_revision,
        "knowledge changed before capture typing; refresh context and retry",
    )?;
    let revision = request.knowledge_revision.clone();
    anyhow::ensure!(
        request.note.len() <= 16_000 && request.note_evidence.len() <= 64,
        "capture note or its evidence exceeds the typing bound"
    );
    let thresholds = ReconcileThresholds::from_env()?;
    let note = request.note.trim();
    let mut raw: Vec<(KnowledgeProposal, ProposalOrigin)> = Vec::new();
    let mut evidence = request.note_evidence.clone();
    if evidence.is_empty() {
        evidence.push(format!("approved plan: {}", request.plan_summary));
    }
    if !request.changed_files.is_empty() {
        let mut description = String::new();
        if !note.is_empty() {
            description.push_str(note);
            description.push_str("\n\n");
        }
        description.push_str(&format!("Approved plan: {}", request.plan_summary.trim()));
        description.push_str(&format!(
            "\n\nFiles changed: {}.",
            request.changed_files.join(", ")
        ));
        let mut decision_evidence = evidence.clone();
        let plan_line = format!("plan approved: {}", request.plan_summary.trim());
        if !decision_evidence.contains(&plan_line) {
            decision_evidence.push(plan_line);
        }
        raw.push((
            base_proposal(
                "ArchitecturalDecision",
                cap_title(&request.plan_summary),
                description,
                decision_evidence,
                request.changed_files.clone(),
            ),
            ProposalOrigin::SymbolicDecision,
        ));
    }
    // A check that failed, then passed after an edit, is a deterministic lesson.
    let mut lesson_commands: Vec<String> = Vec::new();
    for (index, outcome) in request.check_history.iter().enumerate() {
        if outcome.success || lesson_commands.contains(&outcome.command) {
            continue;
        }
        let recovered = request.check_history[index + 1..]
            .iter()
            .any(|later| later.command == outcome.command && later.success && later.after_edit);
        if recovered {
            lesson_commands.push(outcome.command.clone());
        }
    }
    for command in &lesson_commands {
        let mut description =
            format!("The required check `{command}` failed before the change and passed after it.");
        if !note.is_empty() {
            description.push_str(&format!("\n\nModel note: {note}"));
        }
        let mut lesson_evidence = vec![
            format!("check failed: {command}"),
            format!("check passed after edit: {command}"),
        ];
        lesson_evidence.extend(evidence.iter().cloned());
        raw.push((
            base_proposal(
                "Lesson",
                cap_title(&format!(
                    "Check {command} failed before the edit and passed after it"
                )),
                description,
                lesson_evidence,
                request.changed_files.clone(),
            ),
            ProposalOrigin::SymbolicLesson,
        ));
    }
    let sensor_enabled = state.llm_configured
        && state.engine_config.llm_assist_level != LlmAssistLevel::PureSymbolic
        && !note.is_empty();
    let (typing_mode, mut typing_note) = if sensor_enabled {
        (TypingMode::Sensor, None)
    } else {
        (
            TypingMode::SymbolicOnly,
            Some(if note.is_empty() {
                "empty note; symbolic typing only".to_string()
            } else {
                "no daemon LLM sensor configured; symbolic typing only".to_string()
            }),
        )
    };
    if sensor_enabled {
        match sensor_typing(state, request).await {
            Ok(typed) => {
                let known: Vec<String> = raw.iter().map(|(p, _)| normalized(&p.title)).collect();
                for proposal in typed.proposals.into_iter().take(MAX_SENSOR_PROPOSALS) {
                    if !is_record_kind(&proposal.kind)
                        || proposal.title.trim().is_empty()
                        || proposal.description.trim().is_empty()
                        || known.contains(&normalized(&proposal.title))
                    {
                        continue;
                    }
                    raw.push((
                        base_proposal(
                            &proposal.kind,
                            cap_title(&proposal.title),
                            proposal.description.trim().to_string(),
                            evidence.clone(),
                            request.changed_files.clone(),
                        ),
                        ProposalOrigin::LlmSensor,
                    ));
                }
            }
            Err(error) => {
                typing_note = Some(format!(
                    "sensor typing failed; symbolic proposals only: {error}"
                ));
            }
        }
    }
    raw.truncate(MAX_PROPOSALS);
    let tiebreak_enabled = sensor_enabled
        && state.engine_config.llm_assist_level == LlmAssistLevel::SensorWithFallback;
    let mut proposals = Vec::new();
    for (index, (mut proposal, origin)) in raw.into_iter().enumerate() {
        let scored = score_proposal(state, &request.owner_id, &proposal, thresholds)?;
        let receipt_id = format!("{}-r{index}", request.operation_id);
        let mut resolved_by = "symbolic".to_string();
        let disposition = match scored.disposition.clone() {
            ScoredDisposition::Tiebreak {
                candidate_iri,
                score,
                boundary,
            } => {
                let candidate = scored
                    .candidates
                    .iter()
                    .find(|candidate| candidate.iri == candidate_iri)
                    .cloned();
                let verdict = if tiebreak_enabled {
                    sensor_tiebreak(
                        state,
                        &proposal,
                        candidate.as_ref().map(|c| c.title.as_str()),
                    )
                    .await
                } else {
                    None
                };
                match verdict.as_deref() {
                    Some("same") => {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Restates {
                            candidate_iri,
                            score,
                        }
                    }
                    Some("narrower")
                        if boundary == "refines"
                            || candidate.as_ref().is_some_and(|c| c.longer) =>
                    {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Refines {
                            candidate_iri,
                            score,
                            containment: candidate.map(|c| c.containment).unwrap_or(0.0),
                        }
                    }
                    Some(_) => {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Distinct { nearest: candidate }
                    }
                    None => ScoredDisposition::Distinct { nearest: candidate },
                }
            }
            other => other,
        };
        let candidate_digest = |iri: &str| {
            scored
                .candidates
                .iter()
                .find(|candidate| candidate.iri == iri)
                .map(|candidate| candidate.assertion_digest.clone())
        };
        let typed = match disposition {
            ScoredDisposition::Restates {
                candidate_iri,
                score,
            } => {
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "restates".into(),
                        candidate_iri: Some(candidate_iri.clone()),
                        candidate_digest: candidate_digest(&candidate_iri),
                        score,
                        confidence: score,
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                TypedDisposition::Restates {
                    candidate_iri,
                    score,
                    confidence: score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Refines {
                candidate_iri,
                score,
                containment,
            } => {
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "refines".into(),
                        candidate_iri: Some(candidate_iri.clone()),
                        candidate_digest: candidate_digest(&candidate_iri),
                        score,
                        confidence: score,
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                proposal.reconciled = vec![ReconciledRelation {
                    predicate: "refines".into(),
                    target_iri: candidate_iri.clone(),
                    confidence: score,
                    receipt_operation_id: receipt_id.clone(),
                }];
                TypedDisposition::Refines {
                    candidate_iri,
                    score,
                    containment,
                    confidence: score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Distinct { nearest } => {
                let nearest_iri = nearest.as_ref().map(|c| c.iri.clone());
                let score = nearest.as_ref().map(|c| c.score);
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "distinct".into(),
                        candidate_iri: nearest_iri.clone(),
                        candidate_digest: nearest_iri.as_deref().and_then(candidate_digest),
                        score: score.unwrap_or(0.0),
                        confidence: 1.0 - score.unwrap_or(0.0),
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                // A distinct claim under a title the graph already uses is
                // qualified so the ordinary capture path accepts it.
                if graph::resolve_record_exact_all(state, &proposal.title)
                    .into_iter()
                    .any(|(iri, _)| {
                        current_status(state, &iri)
                            .is_some_and(|status| graph::is_current_or_proposed(&status))
                    })
                {
                    let qualifier = request
                        .changed_files
                        .first()
                        .cloned()
                        .unwrap_or_else(|| request.operation_id.chars().take(8).collect());
                    proposal.title = cap_title(&format!("{} ({qualifier})", proposal.title));
                }
                TypedDisposition::Distinct {
                    nearest_iri,
                    score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Tiebreak { .. } => unreachable!("tiebreaks are resolved above"),
        };
        proposals.push(TypedProposal {
            proposal,
            origin,
            disposition: typed,
            resolved_by,
        });
    }
    ensure_unchanged(
        state,
        &revision,
        "knowledge changed during capture typing; retry",
    )?;
    Ok(CaptureTypeResponse {
        revision,
        typing_mode,
        typing_note,
        thresholds,
        proposals,
    })
}

#[derive(serde::Serialize, Deserialize)]
struct StoredTyping {
    request: CaptureTypeRequest,
    response: CaptureTypeResponse,
}

async fn sensor_typing(
    state: &AppState,
    request: &CaptureTypeRequest,
) -> anyhow::Result<SensorTyping> {
    let prompt = format!(
        "You type one engineer's note into durable software-project knowledge. Use only what the note and the listed facts state; do not invent.\n\nObjective of the approved plan: {}\nFiles changed: {}\nChecks: {}\n\nNote:\n{}\n\nReturn up to {MAX_SENSOR_PROPOSALS} proposals. Kinds: ArchitecturalDecision (a choice and why), Lesson (a non-obvious gotcha), Constraint (a hard rule), Requirement (a need), Pattern (a deliberate recurring approach), AntiPattern (something to avoid). Titles are short names under 100 characters; descriptions state the claim in one or two sentences. Return an empty list when the note carries no durable claim.",
        request.plan_summary.trim(),
        if request.changed_files.is_empty() { "none".to_string() } else { request.changed_files.join(", ") },
        request
            .check_history
            .iter()
            .map(|c| format!("{} {}", c.command, if c.success { "passed" } else { "failed" }))
            .collect::<Vec<_>>()
            .join("; "),
        request.note.trim(),
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["proposals", "reason"],
        "properties": {
            "proposals": {
                "type": "array",
                "maxItems": MAX_SENSOR_PROPOSALS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "title", "description"],
                    "properties": {
                        "kind": {"type": "string", "enum": RECORD_KINDS},
                        "title": {"type": "string", "minLength": 1, "maxLength": 200},
                        "description": {"type": "string", "minLength": 1, "maxLength": 2000}
                    }
                }
            },
            "reason": {"type": "string", "maxLength": 1000}
        }
    });
    let text = state
        .llm
        .chat_completion_json_schema_checked(
            &state.model,
            &prompt,
            None,
            "harness_capture_typing",
            schema,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(serde_json::from_str(&text)?)
}

async fn sensor_tiebreak(
    state: &AppState,
    proposal: &KnowledgeProposal,
    candidate_title: Option<&str>,
) -> Option<String> {
    let candidate_title = candidate_title?;
    let prompt = format!(
        "Two software-project claims of the same kind.\nA (new): \"{}\" — {}\nB (existing): \"{candidate_title}\"\n\nIs A the same claim as B, a narrower special case of B, or a different claim? Answer with exactly one word: same, narrower, or different.",
        proposal.title, proposal.description
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string", "enum": ["same", "narrower", "different"]}}
    });
    let text = state
        .llm
        .chat_completion_json_schema_checked(
            &state.model,
            &prompt,
            None,
            "harness_reconcile_tiebreak",
            schema,
        )
        .await
        .ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["verdict"].as_str().map(str::to_string)
}
