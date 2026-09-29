//! The harness runs the required checks itself when the approved work looks
//! done, instead of waiting for the model to call finish (offloading change 1,
//! AD d1195c3d; Constraint cd9f1a96). Small builders spent steps and repair
//! attempts deciding when to finish, arguing with gates and re-proposing edits
//! already made (badciv e3c533b4).
//!
//! Conservative by design (Lesson d6bedaef: control can cost capability). It
//! arms only on a language-server result about exactly the current source,
//! and fires only when every planned file exists and was edited since
//! approval, no stub is left and nothing is reported. A plan listing a file
//! that needs no change never fires: the model finishes as before. A model's
//! own finish is unchanged. `MOOSEDEV_HARNESS_AUTO_VERIFY=off` removes the
//! lever for study variants (Requirement e9166711).

use anyhow::Result;

use super::super::{Mode, Phase, Runner};

/// Auto-verify runs per approval cycle. Each needs a new clean edit, and the
/// model's finish still works after the limit.
pub(in crate::harness::runner) const AUTO_VERIFY_LIMIT: usize = 3;

/// What the model and the human are told when the harness runs the checks.
pub(in crate::harness::runner) const AUTO_VERIFY_NOTE: &str = "All planned files are edited and the language server reports no problems: running the required checks.";

/// How a failed check the harness ran by itself is introduced.
pub(in crate::harness::runner) const AUTO_VERIFY_FAILED: &str = "The harness ran the plan's required checks after your last edit (every planned file edited, no language-server findings). A check failed; fix what it reports. The checks run again after your next clean edit, or when you finish.";

fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_AUTO_VERIFY").map_or(true, |value| value.trim() != "off")
}

impl Runner {
    /// After an applied edit: arm when the servers reported on this edit,
    /// settled, with nothing to report. Anything else disarms.
    pub(in crate::harness::runner) fn arm_auto_verify(&mut self, fresh: bool) {
        let clean = fresh && self.diagnostics_clean();
        let at = self.task.edits.len();
        self.symbolic_state_mut().auto_verify_armed = clean.then_some(at);
    }

    /// Clear what the harness would do by itself next: an approval was
    /// withdrawn or granted.
    pub(in crate::harness::runner) fn disarm_harness_arms(&mut self) {
        if let Some(state) = self.task.symbolic.as_mut() {
            state.auto_verify_armed = None;
            state.auto_fix_armed = None;
        }
    }

    /// Whether approved work is idle, so the harness may act without the
    /// model: nothing pending, no human waited on, no handback.
    pub(in crate::harness::runner) fn harness_may_act(&self) -> bool {
        let task = &self.task;
        task.mode == Mode::Auto
            && task.phase == Phase::Working
            && task.approved_revision.is_some()
            && task.pending_edit.is_none()
            && task.pending_permission.is_none()
            && task.intent.is_none()
            && task.recovery.is_none()
            && !task.handed_back
            && !task.turn_finished
            && !task.completion_pending
            && !task.final_capture
    }

    fn diagnostics_clean(&self) -> bool {
        self.task.diagnostics.as_ref().is_some_and(|diagnostics| {
            diagnostics.settled
                && diagnostics.errors.is_empty()
                && diagnostics.warnings.is_empty()
                && diagnostics.lints.is_empty()
        })
    }

    /// Whether the harness runs the checks now. Takes the arm, so each source
    /// state gets at most one attempt.
    pub(in crate::harness::runner) fn auto_verify_due(&mut self) -> bool {
        let at = self.task.edits.len();
        let armed = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.auto_verify_armed.take());
        if armed != Some(at) || !enabled() {
            return false;
        }
        let idle = self.harness_may_act();
        let task = &self.task;
        let Some(plan) = task.plan.as_ref().filter(|plan| !plan.checks.is_empty()) else {
            return false;
        };
        if !idle || self.untested_failure().is_some() || !self.diagnostics_clean() {
            return false;
        }
        // Every planned file exists and has an edit of the model's own this
        // cycle: a fix the harness applied to a file the model has not worked
        // on yet is not that file done. This is also why auto-verify never
        // meets the unfinished-plan gate in `begin_verification`.
        let every_file_done = plan
            .files
            .iter()
            .all(|file| matches!(self.workspace.read(file), Ok(Some(_))))
            && self.planned_files_unedited().is_empty();
        if !every_file_done || !self.planned_stubs_split().1.is_empty() {
            return false;
        }
        let state = self.symbolic_state_mut();
        if state.auto_verifications >= AUTO_VERIFY_LIMIT {
            if state.auto_verifications == AUTO_VERIFY_LIMIT {
                // Counted past the limit so this is journaled once per cycle.
                state.auto_verifications += 1;
                self.intent_event(
                    "auto_verify_exhausted",
                    &format!("{AUTO_VERIFY_LIMIT} runs this approval cycle; the model finishes"),
                );
            }
            return false;
        }
        true
    }

    /// Run the required checks without a model step, through the same gates
    /// and link review as the model's finish.
    pub(in crate::harness::runner) async fn auto_finish(&mut self) -> Result<()> {
        let at = self.task.edits.len();
        let state = self.symbolic_state_mut();
        state.auto_verifications += 1;
        state.auto_verification = Some(at);
        let checks = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.checks.join("; "))
            .unwrap_or_default();
        self.intent_event(
            "auto_verify",
            &format!("after edit {at}: checks [{checks}]"),
        );
        self.event(AUTO_VERIFY_NOTE);
        self.task.last_response_observation = true;
        self.begin_verification(AUTO_VERIFY_NOTE.into()).await?;
        self.persist()
    }
}
