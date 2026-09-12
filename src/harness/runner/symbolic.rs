//! Symbolic intent policy. The coding model answers only `harness_action` and
//! one final `harness_capture_note`; purpose, obligations, associations and
//! capture typing are derived by the daemon from the approved plan, the
//! resolved definition scopes and the graph. Everything here is reached by a
//! one-line dispatch placed before the existing policy logic, so the other
//! policies' prompts, schemas, events and errors are untouched.
use super::intent_v2::changed_files;
use super::intent_v2::{digest_json, ApprovedChangeScope, ApprovedDefinitionScope};
use super::model::{observation_preview, Action, NoopEdit};
use super::{ContextResponse, IntentPolicy, Mode, Phase, Progress, Runner};
use crate::harness::daemon::intent::{
    IntentBinding, IntentLinkRequest, IntentResolveRequest, IntentResolveResponse,
};
use crate::harness::protocol::{
    AssociatePage, AssociateRequest, CaptureRequest, CaptureTypeRequest, CaptureTypeResponse,
    CheckOutcome, IntentIndexStatus, IntentRefreshPolicy, KnowledgeProposal, TypedDisposition,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Edits outside the plan files replan autonomously this many times per task;
/// the next one parks for human guidance.
pub const MAX_SCOPE_ESCAPES: usize = 3;

/// Durable symbolic-policy state. Obligations are re-derived at every plan
/// approval; the counters bound autonomous recoveries for the whole task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolicState {
    /// The approved plan summary stands in for a selected purpose record.
    pub purpose_summary: String,
    /// Plan file -> direct dossier records of its resolved definitions.
    pub obligations: BTreeMap<String, Vec<String>>,
    pub obligations_digest: String,
    pub knowledge_revision: String,
    #[serde(default)]
    pub scope_escapes: usize,
    #[serde(default)]
    pub noop_continuations: usize,
    /// The association derived for the current edit batch; cleared by each
    /// applied edit so a later batch is derived afresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association: Option<SymbolicAssociation>,
    /// Every required-check outcome in order, feeding the symbolic lesson.
    #[serde(default)]
    pub check_history: Vec<CheckOutcome>,
    /// The one final capture note and its typing; cleared by each applied edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_note: Option<CaptureNoteState>,
}

/// `asked` (note journaled, typing not yet durable) -> `typed` (daemon typing
/// stored; the capture request is rebuilt from it on resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureNoteState {
    pub operation_id: String,
    pub capture_operation_id: String,
    pub note_event: usize,
    pub note: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CaptureTypeResponse>,
}

#[derive(Debug, Deserialize)]
struct NoteAnswer {
    note: String,
}

/// One derived association batch: the request, the daemon's page and the
/// review it entered. `derived` -> `awaiting_review` -> `resolved`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolicAssociation {
    pub request: AssociateRequest,
    pub page: AssociatePage,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_operation_id: Option<String>,
}

impl Runner {
    pub(super) fn uses_symbolic_intent(&self) -> bool {
        self.task.intent_policy == IntentPolicy::Symbolic
    }

    /// Derive the approved change scope from the plan alone: obligations are
    /// the direct dossier records of the plan files' resolved definitions and
    /// the purpose is the plan summary. No model call, one human approval.
    pub(super) async fn derive_symbolic_scope(&mut self, context: &ContextResponse) -> Result<()> {
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let resolved: IntentResolveResponse = self
            .post(
                "intent/resolve",
                &IntentResolveRequest {
                    files: plan.files.clone(),
                    refresh_index: false,
                },
            )
            .await?;
        anyhow::ensure!(
            resolved.revision == context.revision,
            "knowledge changed while deriving obligations; approve the plan again"
        );
        let mut obligations: BTreeMap<String, BTreeSet<String>> = plan
            .files
            .iter()
            .map(|file| (file.clone(), BTreeSet::new()))
            .collect();
        let mut definition_scopes = Vec::new();
        for entity in &resolved.entities {
            let records: BTreeSet<_> = entity.dossier_records.iter().cloned().collect();
            obligations
                .entry(entity.file.clone())
                .or_default()
                .extend(records.iter().cloned());
            definition_scopes.push(ApprovedDefinitionScope {
                file: entity.file.clone(),
                symbol: entity.symbol.clone(),
                source_digest: entity.source_digest.clone(),
                purpose_iris: records.into_iter().collect(),
            });
        }
        let obligations: BTreeMap<String, Vec<String>> = obligations
            .into_iter()
            .map(|(file, records)| (file, records.into_iter().collect()))
            .collect();
        let obligation_iris: Vec<String> = obligations
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let obligations_digest = digest_json(&obligations)?;
        let plan_digest = digest_json(&json!({
            "summary": plan.summary,
            "files": plan.files,
            "checks": plan.checks,
            "snapshots": self.task.snapshots,
        }))?;
        self.task.approved_change_scope = Some(ApprovedChangeScope {
            version: 2,
            plan_digest,
            knowledge_revision: context.revision.clone(),
            files: self.task.snapshots.clone(),
            purpose_iris: vec![],
            obligation_iris: obligation_iris.clone(),
            selected_records: vec![],
            definition_scopes,
            checks: plan.checks.clone(),
            approval_cycle: self.task.intent_cycle.clone().unwrap_or_default(),
            purpose_summary: Some(plan.summary.clone()),
        });
        let state = self.task.symbolic.get_or_insert_with(Default::default);
        state.purpose_summary = plan.summary.clone();
        state.obligations = obligations;
        state.obligations_digest = obligations_digest.clone();
        state.knowledge_revision = context.revision.clone();
        let detail = format!(
            "{} files, {} governing records, {} definitions, revision {}, digest {obligations_digest}",
            plan.files.len(),
            obligation_iris.len(),
            resolved.entities.len(),
            context.revision,
        );
        self.intent_event("obligations_derived", &detail);
        if !resolved.unresolved.is_empty() {
            self.intent_event(
                "obligations_unresolved",
                &format!("no index evidence for {}", resolved.unresolved.join(", ")),
            );
        }
        Ok(())
    }
}

impl Runner {
    /// Before permission checks: an edit outside the approved plan files
    /// becomes the ordinary replan transition naming the file, bounded per
    /// task. Returns `None` once parked for guidance.
    pub(super) fn symbolic_intercept(&mut self, action: Action) -> Result<Option<Action>> {
        if !self.uses_symbolic_intent() || self.task.mode != Mode::Auto {
            return Ok(Some(action));
        }
        let file = match &action {
            Action::Edit { file, .. }
            | Action::Replace { file, .. }
            | Action::Write { file, .. } => file.clone(),
            _ => return Ok(Some(action)),
        };
        let plan_files = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        if plan_files.contains(&file) {
            return Ok(Some(action));
        }
        let state = self.task.symbolic.get_or_insert_with(Default::default);
        state.scope_escapes += 1;
        let escapes = state.scope_escapes;
        let scope = plan_files.join(", ");
        if escapes > MAX_SCOPE_ESCAPES {
            self.candidate_accepted();
            let message = format!(
                "Edit to {file} is outside the approved plan files [{scope}] and this task's {MAX_SCOPE_ESCAPES} autonomous replans are used. Provide guidance or a new plan; pending work is preserved."
            );
            self.intent_event(
                "scope_escape_exhausted",
                &format!("{file}: escape {escapes}, bound {MAX_SCOPE_ESCAPES}"),
            );
            self.task.phase = Phase::AwaitingInput;
            self.task.last_response = message.clone();
            self.event(message.clone());
            if let Some(progress) = &self.progress {
                let _ = progress.send(Progress::Status(message));
            }
            return Ok(None);
        }
        self.intent_event(
            "scope_escape_replan",
            &format!("{file}: escape {escapes} of {MAX_SCOPE_ESCAPES}"),
        );
        self.event(format!(
            "Scope escape: the model proposed an edit to {file} outside the plan files [{scope}]; replanning ({escapes} of {MAX_SCOPE_ESCAPES})."
        ));
        Ok(Some(Action::Replan {
            reason: format!(
                "Edit to {file} is outside the approved plan files [{scope}]; replan with every file the change needs."
            ),
        }))
    }

    /// The first no-op edit of a task means the source already matches: run
    /// the required checks instead of spending the repair budget.
    pub(super) fn symbolic_noop_continuation(&mut self, error: &anyhow::Error) -> Option<Action> {
        if !self.uses_symbolic_intent() || !error.is::<NoopEdit>() {
            return None;
        }
        let state = self.task.symbolic.get_or_insert_with(Default::default);
        if state.noop_continuations >= 1 {
            return None;
        }
        state.noop_continuations += 1;
        self.intent_event(
            "noop_edit_continuation",
            "no-op edit treated as finish; running required checks",
        );
        self.event(
            "No-op edit: the source already matches the proposal; running required checks instead of repairing."
                .to_string(),
        );
        Some(Action::Finish {
            summary: "The source already satisfies the requested change; running required checks."
                .into(),
        })
    }
}

impl Runner {
    /// Post-edit associations without a model: derive bindings from the diff
    /// and the obligations, journal each, then reuse the existing link review.
    pub(super) async fn prepare_symbolic_associations(&mut self) -> Result<bool> {
        if self.task.edits.is_empty() {
            return Ok(false);
        }
        let existing = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.association.clone());
        let association = match existing {
            Some(association) => association,
            None => {
                let governing = self
                    .task
                    .symbolic
                    .as_ref()
                    .map(|state| state.obligations.clone())
                    .unwrap_or_default();
                let request = AssociateRequest {
                    files: changed_files(&self.task.edits)?,
                    governing,
                    refresh_policy: IntentRefreshPolicy::SupportedFrozen,
                    knowledge_revision: self.task.knowledge_revision.clone(),
                };
                let page: AssociatePage = self.post("intent/associate", &request).await?;
                anyhow::ensure!(
                    page.knowledge_revision == self.task.knowledge_revision,
                    "knowledge changed during association derivation; retry"
                );
                for binding in &page.bindings {
                    let detail = format!(
                        "{} {} -{}-> {} ({:?} basis, {})",
                        binding.file,
                        binding.name.as_deref().unwrap_or(&binding.symbol),
                        binding.predicate,
                        binding.record_iri,
                        binding.basis,
                        binding.candidate_digest
                    );
                    self.intent_event("association_derived", &detail);
                }
                for file in &page.ungoverned {
                    self.intent_event("association_none", file);
                }
                if !page.skipped.is_empty() {
                    let mut counts = std::collections::BTreeMap::new();
                    for item in &page.skipped {
                        *counts.entry(format!("{:?}", item.reason)).or_insert(0usize) += 1;
                    }
                    self.intent_event("association_skipped", &format!("{counts:?}"));
                }
                if !page.unresolved.is_empty() || page.index.status != IntentIndexStatus::Current {
                    // Journaled, never parked: a stale or missing index costs
                    // links, not the task.
                    self.intent_event(
                        "association_unresolved",
                        &format!(
                            "index {:?}; {}",
                            page.index.status,
                            serde_json::to_string(&page.unresolved)?
                        ),
                    );
                }
                let status = if page.bindings.is_empty() {
                    "resolved"
                } else {
                    "derived"
                };
                let association = SymbolicAssociation {
                    request,
                    page,
                    status: status.into(),
                    link_operation_id: None,
                };
                self.task
                    .symbolic
                    .get_or_insert_with(Default::default)
                    .association = Some(association.clone());
                self.persist()?;
                association
            }
        };
        match association.status.as_str() {
            "resolved" => Ok(false),
            "awaiting_review" => self.prepare_intent_links(false).await,
            "derived" => {
                let mut bindings: Vec<IntentBinding> = Vec::new();
                for derived in &association.page.bindings {
                    let binding = IntentBinding {
                        record_iri: derived.record_iri.clone(),
                        file: derived.file.clone(),
                        symbol: Some(derived.symbol.clone()),
                        planned_name: None,
                        source_digest: Some(derived.source_digest.clone()),
                    };
                    if !bindings.contains(&binding) {
                        bindings.push(binding);
                    }
                }
                let request = IntentLinkRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    revision: self.task.knowledge_revision.clone(),
                    bindings,
                };
                let operation_id = request.operation_id.clone();
                self.task.pending_intent_links = Some(request);
                let association = self
                    .task
                    .symbolic
                    .as_mut()
                    .and_then(|state| state.association.as_mut())
                    .context("symbolic association state missing")?;
                association.link_operation_id = Some(operation_id);
                association.status = "awaiting_review".into();
                self.persist()?;
                self.prepare_intent_links(false).await
            }
            other => bail!("unknown symbolic association status {other}"),
        }
    }

    pub(super) fn symbolic_association_matches(&self, operation_id: &str) -> bool {
        self.task
            .symbolic
            .as_ref()
            .and_then(|state| state.association.as_ref())
            .is_some_and(|association| {
                association.link_operation_id.as_deref() == Some(operation_id)
            })
    }

    pub(super) fn symbolic_associations_resolved(&self) -> bool {
        self.task
            .symbolic
            .as_ref()
            .and_then(|state| state.association.as_ref())
            .is_some_and(|association| association.status == "resolved")
    }

    pub(super) fn resolve_symbolic_association(&mut self, operation_id: &str) {
        if let Some(association) = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.association.as_mut())
            .filter(|association| association.link_operation_id.as_deref() == Some(operation_id))
        {
            association.link_operation_id = None;
            association.status = "resolved".into();
        }
    }
}

impl Runner {
    pub(super) fn record_symbolic_check(&mut self, command: &str, success: bool) {
        if !self.uses_symbolic_intent() {
            return;
        }
        let after_edit = !self.task.edits.is_empty();
        self.task
            .symbolic
            .get_or_insert_with(Default::default)
            .check_history
            .push(CheckOutcome {
                command: command.to_string(),
                success,
                after_edit,
            });
    }

    /// Symbolic capture: intermediate checkpoints only journal; the final
    /// checkpoint asks the model one plain question, has the daemon type and
    /// reconcile the answer, and hands typed proposals to the ordinary
    /// capture submission and review. Returns true when `capture_request` is
    /// set and the caller should submit it.
    pub(super) async fn symbolic_capture_page(&mut self) -> Result<bool> {
        let checkpoint_end = *self
            .task
            .capture_checkpoint_end
            .get_or_insert(self.task.events.len());
        if !self.task.final_capture {
            let events = checkpoint_end.saturating_sub(self.task.capture_cursor);
            self.event(format!(
                "Capture checkpoint deferred to the final note ({events} events)."
            ));
            self.intent_event("capture_deferred", &format!("{events} events"));
            return self.finish_symbolic_capture_page(checkpoint_end, vec![]);
        }
        if self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.capture_note.as_ref())
            .is_none()
        {
            let prompt = self.capture_note_prompt();
            let schema = json!({"type":"object","additionalProperties":false,"required":["note"],"properties":{"note":{"type":"string","maxLength":4000}}});
            let answer: NoteAnswer = self
                .model_json(&prompt, "harness_capture_note", schema)
                .await?;
            self.candidate_accepted();
            let note = answer.note.trim().to_string();
            self.event(format!("Capture note: {note}"));
            let note_event = self.task.events.len() - 1;
            self.intent_event(
                "capture_note",
                &format!("{} bytes, event {note_event}", note.len()),
            );
            self.task
                .symbolic
                .get_or_insert_with(Default::default)
                .capture_note = Some(CaptureNoteState {
                operation_id: uuid::Uuid::new_v4().to_string(),
                capture_operation_id: uuid::Uuid::new_v4().to_string(),
                note_event,
                note,
                status: "asked".into(),
                response: None,
            });
            self.persist()?;
        }
        let state = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.capture_note.clone())
            .context("symbolic capture note state missing")?;
        let response = match (state.status.as_str(), state.response) {
            ("typed", Some(response)) => response,
            _ => {
                let plan = self.task.plan.as_ref().context("no plan")?.clone();
                let request = CaptureTypeRequest {
                    owner_id: self.task.id.clone(),
                    operation_id: state.operation_id.clone(),
                    note: state.note.clone(),
                    note_evidence: vec![format!("Event {}: capture note", state.note_event)],
                    plan_summary: plan.summary.clone(),
                    plan_files: plan.files.clone(),
                    changed_files: self.changed_file_names(),
                    check_history: self
                        .task
                        .symbolic
                        .as_ref()
                        .map(|state| state.check_history.clone())
                        .unwrap_or_default(),
                    knowledge_revision: self.task.knowledge_revision.clone(),
                };
                let response: CaptureTypeResponse = self.post("capture/type", &request).await?;
                anyhow::ensure!(
                    response.revision == self.task.knowledge_revision,
                    "knowledge changed during capture typing; retry with current context"
                );
                self.intent_event(
                    "capture_typed",
                    &format!(
                        "{:?}, {} proposals{}",
                        response.typing_mode,
                        response.proposals.len(),
                        response
                            .typing_note
                            .as_deref()
                            .map(|note| format!("; {note}"))
                            .unwrap_or_default()
                    ),
                );
                for typed in &response.proposals {
                    let (kind, detail) = match &typed.disposition {
                        TypedDisposition::Restates {
                            candidate_iri,
                            confidence,
                            ..
                        } => (
                            "reconciled_restates",
                            format!(
                                "{} restates {candidate_iri} ({confidence})",
                                typed.proposal.title
                            ),
                        ),
                        TypedDisposition::Refines {
                            candidate_iri,
                            confidence,
                            ..
                        } => (
                            "reconciled_refines",
                            format!(
                                "{} refines {candidate_iri} ({confidence})",
                                typed.proposal.title
                            ),
                        ),
                        TypedDisposition::Distinct { .. } => (
                            "reconciled_distinct",
                            format!("{} {}", typed.proposal.kind, typed.proposal.title),
                        ),
                    };
                    self.intent_event(kind, &detail);
                }
                let note = self
                    .task
                    .symbolic
                    .as_mut()
                    .and_then(|state| state.capture_note.as_mut())
                    .context("symbolic capture note state missing")?;
                note.status = "typed".into();
                note.response = Some(response.clone());
                self.persist()?;
                response
            }
        };
        let restated = response
            .proposals
            .iter()
            .filter(|typed| matches!(typed.disposition, TypedDisposition::Restates { .. }))
            .count();
        let proposals: Vec<KnowledgeProposal> = response
            .proposals
            .iter()
            .filter(|typed| !matches!(typed.disposition, TypedDisposition::Restates { .. }))
            .map(|typed| typed.proposal.clone())
            .collect();
        let reason = format!(
            "Symbolic capture typing ({:?}): {} proposals, {restated} restated existing knowledge.",
            response.typing_mode,
            proposals.len()
        );
        self.task.capture_reason = Some(reason.clone());
        self.event(format!("Capture assessment: {reason}"));
        if proposals.is_empty() {
            return self.finish_symbolic_capture_page(checkpoint_end, vec![]);
        }
        self.task.capture_end = Some(checkpoint_end);
        self.task.capture_end_offset = 0;
        self.task.capture_request = Some(CaptureRequest {
            operation_id: state.capture_operation_id,
            proposals,
        });
        self.persist()?;
        Ok(true)
    }

    /// Consume the checkpoint without a submission, then continue exactly as
    /// the empty-proposals branch of the model-driven capture does.
    fn finish_symbolic_capture_page(
        &mut self,
        checkpoint_end: usize,
        _proposals: Vec<KnowledgeProposal>,
    ) -> Result<bool> {
        self.task.capture_end = Some(checkpoint_end);
        self.task.capture_end_offset = 0;
        self.commit_capture_page();
        self.candidate_accepted();
        self.task.phase = if self.capture_reviews_block_progress() {
            Phase::AwaitingReview
        } else if self.task.capture_due {
            self.capture_work_phase()
        } else if !self.task.batch_capture || self.task.final_capture {
            Phase::AwaitingReview
        } else {
            self.task.after_review
        };
        self.persist()?;
        Ok(false)
    }

    fn changed_file_names(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for edit in &self.task.edits {
            if !files.contains(&edit.file) {
                files.push(edit.file.clone());
            }
        }
        files
    }

    fn capture_note_prompt(&self) -> String {
        let plan = self.task.plan.as_ref();
        let checks: Vec<String> = self
            .task
            .symbolic
            .as_ref()
            .map(|state| {
                state
                    .check_history
                    .iter()
                    .map(|c| {
                        format!(
                            "{} {}{}",
                            c.command,
                            if c.success { "passed" } else { "failed" },
                            if c.after_edit {
                                " after the edit"
                            } else {
                                " before any edit"
                            }
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let recent: Vec<String> = self
            .task
            .events
            .iter()
            .rev()
            .take(6)
            .map(|event| observation_preview(&event.message, 600))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!(
            "The coding work is done and its required checks passed. Answer one plain question in prose, no JSON structure beyond the single note field: what should a future engineer know about this change that the diff alone does not say? Name the decision you made and why, any rule you discovered, and anything that surprised you. Say \"nothing beyond the diff\" if there is nothing durable. Do not restate the objective.\n\nObjective: {}\nApproved plan: {}\nFiles edited: {}\nChecks: {}\n\nRecent journal:\n{}",
            self.task.objective,
            plan.map(|p| p.summary.as_str()).unwrap_or(""),
            self.changed_file_names().join(", "),
            if checks.is_empty() { "none".to_string() } else { checks.join("; ") },
            recent.join("\n---\n"),
        )
    }
}
