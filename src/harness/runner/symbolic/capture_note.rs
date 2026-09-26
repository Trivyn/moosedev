//! The one final capture note: asked of the model once, typed by the daemon,
//! retyped under fresh ids when the daemon rejects the typing, and
//! invalidated when the evidence it was typed against changes.
use super::super::model::observation_preview;
use super::super::plan_view::{plan_view, PLAN_VIEW_BYTES};
use super::super::scope::changed_files;
use super::super::source::failed_command_output;
use super::super::{ApprovedPlan, Phase, Runner};
use super::{CaptureNoteState, NoteAnswer, CAPTURE_NOTE_QUESTION, MAX_RETYPES};
use crate::harness::protocol::{
    CaptureRequest, CaptureTypeRequest, CaptureTypeResponse, KnowledgeProposal, RestatedCandidate,
    SupportEvent, TypedDisposition,
};
use anyhow::{bail, Context, Result};

/// Where the whole of a plan shown in part can be read, for a reader that
/// cannot page the journal.
const JOURNAL_ROUTE: &str = "the whole plan is kept in the task journal";
/// The smallest share of the note's plan room one approved plan is given.
const MIN_PLAN_SHARE: usize = 800;
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
                    // The daemon bounds it where it writes it, keeping the
                    // first paragraph its reconciliation key reads.
                    plan_summary: plan.summary.clone(),
                    changed_files: self.changed_file_names(),
                    check_history: self
                        .task
                        .symbolic
                        .as_ref()
                        .map(|state| state.check_history.clone())
                        .unwrap_or_default(),
                    knowledge_revision: self.task.knowledge_revision.clone(),
                    // The obligations the human approved this plan against, not
                    // a fresh derivation: the edge capture draws must point at
                    // what was actually on the table at approval.
                    obligation_iris: self
                        .task
                        .approved_change_scope
                        .as_ref()
                        .map(|scope| scope.obligation_iris.clone())
                        .unwrap_or_default(),
                    obligations_digest: self
                        .task
                        .symbolic
                        .as_ref()
                        .map(|state| state.obligations_digest.clone())
                        .unwrap_or_default(),
                    governing_labels: self
                        .task
                        .knowledge_context
                        .as_ref()
                        .map(|context| {
                            context
                                .governing_rules
                                .iter()
                                .map(|rule| rule.label.clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                    addressed_rules: self.addressed_rules(),
                    support_events: self.support_events(),
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
                for dropped in &response.dropped {
                    self.intent_event(
                        "capture_dropped",
                        &format!("{} \"{}\": {}", dropped.kind, dropped.title, dropped.reason),
                    );
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
        // A restatement that names a changed file still links its existing
        // record to that change, through the same capture and review.
        let edited = self.changed_file_names();
        let mut restated_records: Vec<RestatedCandidate> = Vec::new();
        for typed in &response.proposals {
            if let TypedDisposition::Restates {
                candidate_iri,
                receipt_operation_id,
                ..
            } = &typed.disposition
            {
                if typed
                    .proposal
                    .files
                    .iter()
                    .any(|file| edited.contains(file))
                    && !restated_records
                        .iter()
                        .any(|known| &known.candidate_iri == candidate_iri)
                {
                    restated_records.push(RestatedCandidate {
                        candidate_iri: candidate_iri.clone(),
                        receipt_operation_id: receipt_operation_id.clone(),
                        files: typed.proposal.files.clone(),
                    });
                }
            }
        }
        let reason = format!(
            "Symbolic capture typing ({:?}): {} proposals, {restated} restated existing knowledge{}.",
            response.typing_mode,
            proposals.len(),
            if response.dropped.is_empty() {
                String::new()
            } else {
                format!(", {} refused", response.dropped.len())
            }
        );
        self.task.capture_reason = Some(reason.clone());
        self.event(format!("Capture assessment: {reason}"));
        if proposals.is_empty() && restated_records.is_empty() {
            // Nothing to review, but completion still needs the human to
            // confirm that no durable knowledge changed; say so where the
            // status line and the journal will show it.
            let reason = if response
                .typing_note
                .as_deref()
                .is_some_and(|note| note.contains("declares nothing durable"))
            {
                "The model reported nothing beyond the diff; /no-knowledge confirms it and completes the task."
            } else {
                "Typing proposed no knowledge; /no-knowledge confirms it and completes the task."
            };
            self.task.capture_reason = Some(reason.to_string());
            self.event(reason);
            self.advance_after_capture_page(checkpoint_end)?;
            return Ok(false);
        }
        self.task.capture_end = Some(checkpoint_end);
        self.task.capture_request = Some(CaptureRequest {
            operation_id: state.capture_operation_id,
            proposals,
            changed: changed_files(&self.task.edits)?,
            restated: restated_records,
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
        format!(
            "{CAPTURE_NOTE_QUESTION}\n\nObjective: {}\nFiles edited: {}\nChecks: {}\n\n{}",
            self.task.objective,
            self.changed_file_names().join(", "),
            if checks.is_empty() {
                "none".to_string()
            } else {
                checks.join("; ")
            },
            self.capture_note_work(),
        )
    }

    /// Every plan approved in this task with the edits made under it, oldest
    /// first. A replan replaces `task.plan`, and the earlier plan's work is
    /// often the decision worth recording, so the note is asked about all of
    /// it. Each plan summary is shown within its share of the plan view,
    /// and edit previews shrink to fit what is left.
    fn capture_note_work(&self) -> String {
        const BUDGET: usize = 8_000;
        let current;
        let plans: &[ApprovedPlan] = if self.task.approved_plans.is_empty() {
            // A task journaled before approved plans were kept.
            current = self
                .task
                .plan
                .iter()
                .map(|plan| ApprovedPlan {
                    summary: plan.summary.clone(),
                    files: plan.files.clone(),
                    addresses: plan.addresses.clone(),
                    rules_in_view: Vec::new(),
                    edit_start: 0,
                })
                .collect::<Vec<_>>();
            &current
        } else {
            &self.task.approved_plans
        };
        // Each plan as the note is shown it: within its share of the
        // summary room, focused on the files its edits changed.
        // Room enough for each plan's intent and closing line, even when
        // many plans together pass the shared view size.
        let share = (PLAN_VIEW_BYTES / plans.len().max(1)).max(MIN_PLAN_SHARE);
        let views: Vec<String> = plans
            .iter()
            .enumerate()
            .map(|(index, plan)| {
                let end = plans
                    .get(index + 1)
                    .map_or(self.task.edits.len(), |next| next.edit_start)
                    .min(self.task.edits.len());
                let edited: Vec<String> = self.task.edits[plan.edit_start.min(end)..end]
                    .iter()
                    .map(|edit| edit.file.clone())
                    .collect();
                plan_view(&plan.summary, &edited, share, JOURNAL_ROUTE)
            })
            .collect();
        let summaries: usize = views.iter().map(|view| view.len() + 32).sum();
        let edit_count = self.task.edits.len().max(1);
        let per_edit = (BUDGET.saturating_sub(summaries) / edit_count).clamp(0, 600);
        let mut out = String::new();
        for (index, plan) in plans.iter().enumerate() {
            let end = plans
                .get(index + 1)
                .map_or(self.task.edits.len(), |next| next.edit_start)
                .min(self.task.edits.len());
            out.push_str(&format!(
                "Approved plan {} of {}:\n{}\n",
                index + 1,
                plans.len(),
                views[index].trim()
            ));
            let start = plan.edit_start.min(end);
            for edit in &self.task.edits[start..end] {
                let after = edit.after.as_deref().unwrap_or("[deleted]");
                if per_edit < 80 {
                    out.push_str(&format!("- edited {}\n", edit.file));
                } else {
                    out.push_str(&format!(
                        "- edited {}:\n{}\n",
                        edit.file,
                        observation_preview(after, per_edit)
                    ));
                }
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    /// Every rule an approved plan of this task said it implements, once each
    /// and in the order first named. A plan replaced before any edit was made
    /// under it implemented nothing, so its rules are not counted.
    fn addressed_rules(&self) -> Vec<String> {
        let mut rules: Vec<String> = Vec::new();
        let plans = &self.task.approved_plans;
        let named = if plans.is_empty() {
            self.task
                .plan
                .iter()
                .flat_map(|plan| plan.addresses.iter())
                .collect::<Vec<_>>()
        } else {
            plans
                .iter()
                .enumerate()
                .filter(|(index, plan)| {
                    let end = plans
                        .get(index + 1)
                        .map_or(self.task.edits.len(), |next| next.edit_start);
                    plan.edit_start < end.min(self.task.edits.len())
                })
                .flat_map(|(_, plan)| plan.addresses.iter())
                .collect()
        };
        for iri in named {
            if !rules.contains(iri) {
                rules.push(iri.clone());
            }
        }
        rules
    }

    /// Journal events where the project or the human pushed back: what a
    /// note-typed Lesson must rest on. Only a failed command (the project's
    /// own build and tests) and a human message after plan approval count.
    /// Repairs, scope escapes, held edits and replans are the harness's own
    /// mechanics; a lesson about them belongs to the harness, not the project
    /// (badciv dc6a7586 minted "Plan Scope Adherence" from a scope escape).
    /// Oldest first, the most recent 20.
    fn support_events(&self) -> Vec<SupportEvent> {
        const MAX: usize = 20;
        let approved_at = self
            .task
            .events
            .iter()
            .position(|event| event.message.starts_with("Human approved the plan"));
        let mut found = Vec::new();
        for (index, event) in self.task.events.iter().enumerate() {
            let message = event.message.as_str();
            let kind = if message.starts_with("Command: ") {
                if failed_command_output(message).is_none() {
                    continue;
                }
                "command_failed"
            } else if message.starts_with("Human response: ")
                && approved_at.is_some_and(|approved| index > approved)
            {
                "human_steer"
            } else {
                continue;
            };
            found.push(SupportEvent {
                event: index,
                kind: kind.into(),
                summary: one_line(message, 200),
            });
        }
        let skip = found.len().saturating_sub(MAX);
        found.split_off(skip)
    }
}

/// `text` on one line (line breaks become " | "), cut at a character
/// boundary to at most `max` bytes.
fn one_line(text: &str, max: usize) -> String {
    let mut line = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if line.len() > max {
        let mut cut = max.saturating_sub(3);
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    line
}
