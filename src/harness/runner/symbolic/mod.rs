//! The harness's own decisions. The coding model answers only `harness_action`
//! and one final `harness_capture_note`; obligations, associations and capture
//! typing are derived by the daemon from the approved plan, the resolved
//! definition scopes and the graph.
mod associate;
mod capture_note;
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
    pub(in crate::harness::runner) fn record_symbolic_check(
        &mut self,
        command: &str,
        success: bool,
    ) {
        let after_edit = !self.task.edits.is_empty();
        self.symbolic_state_mut().check_history.push(CheckOutcome {
            command: command.to_string(),
            success,
            after_edit,
        });
    }
}
