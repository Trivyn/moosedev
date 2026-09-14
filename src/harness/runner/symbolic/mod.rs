//! The harness's own decisions. The coding model answers only `harness_action`
//! and one final `harness_capture_note`; obligations, associations and capture
//! typing are derived by the daemon from the approved plan, the resolved
//! definition scopes and the graph.
mod associate;
mod capture_note;
mod coverage;
mod grounding;
mod scope;
mod state;

use super::actions::Step;
use super::model::{Action, NoopEdit};
use super::{Mode, PendingEdit, Phase, Progress, Runner};
use crate::harness::protocol::CheckOutcome;
use anyhow::Result;
pub use state::{
    CaptureNoteState, SymbolicAssociation, SymbolicState, MAX_RETYPES, MAX_SCOPE_ESCAPES,
};
use state::{NoteAnswer, CAPTURE_NOTE_QUESTION};

impl Runner {
    pub(super) fn symbolic_state_mut(&mut self) -> &mut SymbolicState {
        self.task.symbolic.get_or_insert_with(Default::default)
    }

    /// Every applied edit starts a new association batch and a new note.
    pub(in crate::harness::runner) fn clear_symbolic_batch_state(&mut self, edit: &PendingEdit) {
        if let Some(scope) = self.task.approved_change_scope.as_mut() {
            scope
                .files
                .insert(edit.file.clone(), super::fingerprint(&edit.after));
            if let Some(after_digest) = super::fingerprint(&edit.after) {
                for definition in scope
                    .definition_scopes
                    .iter_mut()
                    .filter(|definition| definition.file == edit.file)
                {
                    definition.source_digest = after_digest.clone();
                }
            }
        }
        if let Some(state) = self.task.symbolic.as_mut() {
            state.association = None;
            state.capture_note = None;
        }
    }
}

impl Runner {
    /// Before permission checks: an edit outside the approved plan files
    /// becomes the ordinary replan transition naming the file, bounded per
    /// task. Returns `None` once parked for guidance.
    pub(in crate::harness::runner) fn symbolic_intercept(
        &mut self,
        action: Action,
    ) -> Result<Option<Action>> {
        if self.task.mode != Mode::Auto {
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
        let state = self.symbolic_state_mut();
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
    pub(in crate::harness::runner) fn symbolic_noop_continuation(
        &mut self,
        error: &anyhow::Error,
    ) -> Option<Step> {
        if !error.is::<NoopEdit>() {
            return None;
        }
        let state = self.symbolic_state_mut();
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
        Some(Step::Finish {
            summary: "The source already satisfies the requested change; running required checks."
                .into(),
        })
    }
}

impl Runner {
    /// An applied edit, a command, a required-check result or a human answer
    /// is new evidence: a replan after it is a real replan.
    pub(in crate::harness::runner) fn end_unchanged_window(&mut self) {
        if let Some(state) = self.task.symbolic.as_mut() {
            state.unchanged_since_approval = false;
        }
    }

    /// The model's own replan changes nothing while the approved plan still
    /// governs and nothing new has arrived since approval.
    pub(in crate::harness::runner) fn replan_changes_nothing(&self) -> bool {
        self.task.mode == Mode::Auto
            && self.task.phase == Phase::Working
            && self.task.approved_revision.is_some()
            && self.task.approved_change_scope.is_some()
            && self.task.pending_edit.is_none()
            && self
                .task
                .symbolic
                .as_ref()
                .is_some_and(|state| state.unchanged_since_approval)
    }

    /// Continue the approved plan instead of reopening planning and approval.
    pub(in crate::harness::runner) fn symbolic_replan_continuation(&mut self, reason: &str) {
        let state = self.symbolic_state_mut();
        state.replan_continuations += 1;
        let count = state.replan_continuations;
        let files = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.join(", "))
            .unwrap_or_default();
        self.intent_event("replan_continuation", &format!("{count}: {reason}"));
        self.event(format!(
            "Replan continued the approved plan ({count}): {reason}"
        ));
        self.task.last_response = format!(
            "Replan not needed: nothing has changed since the plan was approved (no edit, command, check result or human answer since approval), so the approved plan still governs. Make the change it describes in {files}, or finish to run the required checks. An edit to a file outside the plan returns the task to planning automatically."
        );
    }

    /// A replan while already planning changes nothing.
    pub(in crate::harness::runner) fn symbolic_replan_noop(&mut self, reason: &str) {
        self.intent_event("replan_noop", reason);
        self.event(format!("Replan while planning changes nothing: {reason}"));
        self.task.last_response = "Already planning; replan changes nothing here. Propose the plan with plan(summary, files, checks), or read, search or ask first.".into();
    }

    pub(in crate::harness::runner) fn record_symbolic_check(
        &mut self,
        command: &str,
        success: bool,
    ) {
        self.end_unchanged_window();
        let after_edit = !self.task.edits.is_empty();
        self.symbolic_state_mut().check_history.push(CheckOutcome {
            command: command.to_string(),
            success,
            after_edit,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{context_router, serve, Project};
    use super::*;

    #[tokio::test]
    async fn record_symbolic_check_ends_the_unchanged_window() {
        let project = Project::new("unchanged-window");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Keep the approved plan".into())
            .await
            .unwrap();
        for success in [true, false] {
            runner.symbolic_state_mut().unchanged_since_approval = true;
            runner.record_symbolic_check("true", success);
            assert!(
                !runner
                    .task
                    .symbolic
                    .as_ref()
                    .unwrap()
                    .unchanged_since_approval,
                "a check result (success {success}) is new evidence"
            );
        }
        server.abort();
    }
}
