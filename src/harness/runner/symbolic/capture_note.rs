//! The one final capture note: asked of the model once, typed by the daemon,
//! retyped under fresh ids when the daemon rejects the typing, and
//! invalidated when the evidence it was typed against changes.
use super::super::model::observation_preview;
use super::super::scope::changed_files;
use super::super::{Phase, Runner};
use super::{CaptureNoteState, NoteAnswer, CAPTURE_NOTE_QUESTION, MAX_RETYPES};
use crate::harness::protocol::{
    CaptureRequest, CaptureTypeRequest, CaptureTypeResponse, KnowledgeProposal, TypedDisposition,
};
use anyhow::{bail, Context, Result};
use serde_json::json;

impl Runner {
    /// Intermediate checkpoints only journal; the final checkpoint asks the
    /// model one plain question, has the daemon type and reconcile the answer,
    /// and hands typed proposals to the ordinary capture submission and
    /// review. Returns true when `capture_request` is set and the caller
    /// should submit it.
    pub(in crate::harness::runner) async fn symbolic_capture_page(&mut self) -> Result<bool> {
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
            self.advance_after_capture_page(checkpoint_end)?;
            return Ok(false);
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
            self.symbolic_state_mut().capture_note = Some(CaptureNoteState {
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
        if state.status == "captured" {
            // Accepting that capture can change knowledge and send the task
            // back through approval and verification; the repeated final
            // checkpoint has nothing new to capture.
            let reason =
                "The final note was captured and reviewed earlier in this task; nothing new to capture.";
            self.task.capture_reason = Some(reason.into());
            self.event(format!("Capture assessment: {reason}"));
            self.advance_after_capture_page(checkpoint_end)?;
            return Ok(false);
        }
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
                    .symbolic_state_mut()
                    .capture_note
                    .as_mut()
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
            self.advance_after_capture_page(checkpoint_end)?;
            return Ok(false);
        }
        self.task.capture_end = Some(checkpoint_end);
        self.task.capture_request = Some(CaptureRequest {
            operation_id: state.capture_operation_id,
            proposals,
            changed: changed_files(&self.task.edits)?,
        });
        self.persist()?;
        Ok(true)
    }

    /// A definite pre-persistence rejection of the typed proposals: keep the
    /// note, discard the typing, and let the daemon type it again under fresh
    /// ids (it qualifies colliding titles on the next call). Bounded per task.
    pub(in crate::harness::runner) async fn retype_capture_note(
        &mut self,
        reason: &str,
    ) -> Result<()> {
        // The rejected request is gone either way; an exhausted budget parks
        // with the note and the checkpoint intact.
        self.task.capture_request = None;
        self.task.capture_end = None;
        self.task.capture_due = true;
        let state = self.symbolic_state_mut();
        if state.capture_note.is_none() {
            bail!("capture rejected before persistence without a note to retype: {reason}");
        }
        state.retypes += 1;
        let retypes = state.retypes;
        if retypes > MAX_RETYPES {
            let message = format!(
                "The daemon rejected the typed capture {MAX_RETYPES} times ({reason}). Provide guidance; the note and the checkpoint are preserved."
            );
            self.intent_event(
                "capture_retype_exhausted",
                &format!("retype {retypes}, bound {MAX_RETYPES}"),
            );
            self.task.phase = Phase::AwaitingInput;
            self.task.last_response = message.clone();
            self.event(message);
            return self.persist();
        }
        self.discard_capture_typing();
        self.intent_event(
            "capture_retyped",
            &format!("{retypes} of {MAX_RETYPES}: {reason}"),
        );
        self.event(format!(
            "Capture rejected before persistence; retyping the same note under fresh identities ({retypes} of {MAX_RETYPES}): {reason}"
        ));
        self.persist()
    }

    /// Typing stored before source or accepted knowledge changed is stale:
    /// keep the note, let the daemon type it again at the current revision.
    /// A captured note is left alone: its proposals are already in the graph.
    /// No model call and no retype budget; the change was not a rejection.
    pub(in crate::harness::runner) fn invalidate_capture_typing(&mut self, reason: &str) {
        let typed = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.capture_note.as_ref())
            .is_some_and(|note| note.status == "typed");
        if typed {
            self.discard_capture_typing();
            self.intent_event("capture_note_invalidated", reason);
        }
    }

    /// Drop the stored typing and any request built from it; the note text
    /// and its journal position survive under fresh operation identities.
    fn discard_capture_typing(&mut self) {
        self.task.capture_request = None;
        self.task.capture_end = None;
        if let Some(note) = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.capture_note.as_mut())
        {
            note.status = "asked".into();
            note.response = None;
            note.operation_id = uuid::Uuid::new_v4().to_string();
            note.capture_operation_id = uuid::Uuid::new_v4().to_string();
        }
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
            "{CAPTURE_NOTE_QUESTION}\n\nObjective: {}\nApproved plan: {}\nFiles edited: {}\nChecks: {}\n\nRecent journal:\n{}",
            self.task.objective,
            plan.map(|p| p.summary.as_str()).unwrap_or(""),
            self.changed_file_names().join(", "),
            if checks.is_empty() { "none".to_string() } else { checks.join("; ") },
            recent.join("\n---\n"),
        )
    }
}
