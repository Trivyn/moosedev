//! Recover, don't park (AD ad50c9cd). Only a stop the human owns waits for
//! the human: a permission, plan approval, information only the human holds.
//! When the model is stuck (a look loop, a fix loop, output that keeps failing
//! validation, the step cap), the harness recovers and keeps going. Park and
//! wait was never designed; it accumulated from local loop fixes (Lesson
//! 3efaba62), and in the automated runs 87% of parks were the model stuck,
//! each answered by a scripted nudge that carried no information.
//!
//! The ladder's two ends are built here. First the harness continues by
//! itself, as that nudge did, journaled as its own action; after
//! [`STUCK_CONTINUES`] recoveries without progress it finishes the task as
//! best it can: the required checks run, and a task whose checks fail ends
//! [`Phase::Incomplete`] with what it decided captured as unverified.
use super::{executor, Mode, Phase, Runner};
use anyhow::Result;

/// Harness continuations allowed between two progress events before the
/// best-effort finish.
pub(super) const STUCK_CONTINUES: usize = 2;
/// Opens the journal line of a harness recovery; the loop guards count it as
/// progress, as they count a human answer ([`super::review::is_progress`]).
pub(super) const HARNESS_RECOVERY: &str = "Harness recovery";

/// `MOOSEDEV_HARNESS_RECOVER=off` parks every model-stuck stop for the
/// human, as before.
pub(super) fn recover_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_RECOVER").map_or(true, |value| value.trim() != "off")
}

/// The facts of a stop message, without its request to the human.
fn facts(message: &str) -> &str {
    message
        .split(" Guidance is needed")
        .next()
        .unwrap_or(message)
        .trim()
}

impl Runner {
    /// Ends a model-stuck stop of `kind`. With recovery on, the harness
    /// recovers ([`Self::recover`]); off, it parks for the human as before,
    /// journaling `parked` and telling the human `message`.
    pub(super) fn stop_stuck(&mut self, kind: &str, parked: String, message: String) {
        if recover_enabled() {
            self.recover(kind, &message);
            return;
        }
        self.event(parked);
        self.task.last_response = message;
        self.task.phase = Phase::AwaitingInput;
        self.task.turn_finished = true;
        self.park_under_approved_plan();
    }

    /// The harness's own way out of a model-stuck stop: while fewer than
    /// [`STUCK_CONTINUES`] recoveries happened since the last progress, it
    /// continues the task itself with the stop's facts as guidance and resets
    /// what a human answer resets (but not the step count, so the step cap
    /// still bounds the task); after that it marks the task for the
    /// best-effort finish, which the next advance runs.
    pub(super) fn recover(&mut self, kind: &str, message: &str) {
        let facts = facts(message).to_string();
        let work_phase = if self.task.mode == Mode::Plan {
            Phase::Planning
        } else {
            Phase::Working
        };
        if self.task.stuck_recoveries < STUCK_CONTINUES {
            self.task.stuck_recoveries += 1;
            let count = self.task.stuck_recoveries;
            self.intent_event(
                "stuck_recovered",
                &format!("{kind} {count}/{STUCK_CONTINUES}"),
            );
            self.event(format!(
                "{HARNESS_RECOVERY} ({kind}, {count} of {STUCK_CONTINUES}): {facts}"
            ));
            let next = if self.task.mode == Mode::Plan {
                "Continue: propose the plan with what is shown, or ask the human with question if something only the human knows is missing."
            } else {
                "Continue: take the plan's next step with what is shown."
            };
            let guidance = format!("{facts}\n{next}");
            self.forget_failure();
            self.end_unchanged_window();
            self.task.recovery = None;
            self.task.handed_back = false;
            self.task.plan_stands_park = false;
            self.task.guidance = guidance.clone();
            // The observation that ended the loop stays, after the guidance:
            // a check's output, a served read.
            let previous = std::mem::take(&mut self.task.last_response);
            self.task.last_response = if previous.trim().is_empty() || previous == facts {
                guidance
            } else {
                format!("{guidance}\n\n{previous}")
            };
            self.task.phase = work_phase;
            self.task.turn_finished = false;
            return;
        }
        self.intent_event("best_effort_finish_due", kind);
        self.event(format!(
            "{HARNESS_RECOVERY} ({kind}): still stuck after {STUCK_CONTINUES} recoveries; the harness finishes the task as best it can. {facts}"
        ));
        self.task.best_effort = Some(kind.to_string());
        self.task.recovery = None;
        self.task.handed_back = false;
        self.task.plan_stands_park = false;
        self.task.phase = work_phase;
        self.task.turn_finished = false;
    }

    /// Progress since the last recovery restarts the count: an applied edit,
    /// an approved plan, a human answer. A human's word also calls off a
    /// best-effort finish that has not run yet: the model goes on with it.
    pub(super) fn progressed(&mut self) {
        self.task.stuck_recoveries = 0;
        self.task.best_effort = None;
    }

    /// The ladder's last rung. With an approved plan in Auto, the plan's
    /// required checks run as at a finish, without the gates that send the
    /// model back: all passing completes the task as usual; a failure
    /// captures what the task decided as unverified and ends it incomplete
    /// (`verify_next`). Without an approved plan there is nothing to verify,
    /// and the task ends incomplete at once.
    pub(super) async fn best_effort_finish(&mut self) -> Result<()> {
        let kind = self.task.best_effort.clone().unwrap_or_default();
        let verifiable = self.task.mode == Mode::Auto
            && self.task.approved_revision.is_some()
            && self
                .task
                .plan
                .as_ref()
                .is_some_and(|plan| !plan.checks.is_empty());
        if verifiable {
            self.intent_event("best_effort_verify", &kind);
            self.event(format!(
                "Best-effort finish ({kind}): running the plan's required checks."
            ));
            return self
                .begin_verification(format!(
                    "The harness is finishing the task as best it can after the model stayed stuck ({kind})."
                ))
                .await;
        }
        self.finish_incomplete().await
    }

    /// Ends a task the best-effort finish could not complete: the same
    /// cleanup as a completion, the terminal phase [`Phase::Incomplete`], and
    /// one journal line naming the stop and the failing checks.
    pub(super) async fn finish_incomplete(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.reviews.is_empty()
                && self.task.capture_request.is_none()
                && !self.task.capture_due,
            "the capture review must be resolved before the task ends"
        );
        anyhow::ensure!(
            self.task.intent.is_none(),
            "an action's outcome must be reconciled before the task ends"
        );
        executor::cleanup_task(&self.scratch_path())?;
        self.stop_language_servers();
        self.expire_permissions();
        let kind = self.task.best_effort.clone().unwrap_or_default();
        let failing = self.failing_checks();
        let captured = self.task.incomplete_capture;
        self.task.phase = Phase::Incomplete;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        self.task.incomplete_capture = false;
        self.intent_event("task_incomplete", &kind);
        self.event(format!(
            "Incomplete: the model stayed stuck ({kind}). {} {}",
            if failing.is_empty() {
                "No required check ran.".to_string()
            } else {
                format!("Failing required checks: {}.", failing.join("; "))
            },
            if captured {
                "What the task decided was captured as unverified."
            } else {
                "Nothing was captured: no approved plan reached its checks."
            }
        ));
        self.persist()
    }

    /// The best-effort finish's failing required check `command`: what the
    /// task decided is captured as unverified, and the task ends incomplete
    /// once the review is resolved.
    pub(super) async fn capture_incomplete(&mut self, command: &str) -> Result<()> {
        self.intent_event("best_effort_check_failed", command);
        self.event(format!(
            "Best-effort finish: required check `{command}` failed; capturing what the task decided as unverified."
        ));
        // The check ran to its end: nothing is left to reconcile.
        self.task.intent = None;
        self.task.incomplete_capture = true;
        self.task.capture_due = true;
        self.task.after_review = Phase::Verifying;
        self.capture().await
    }

    /// Leaving an end-of-task capture for new guidance: the task is no
    /// longer ending, and the note asked for that end no longer stands.
    pub(super) fn abandon_task_end(&mut self) {
        if self.task.final_capture || self.task.incomplete_capture {
            if let Some(state) = self.task.symbolic.as_mut() {
                state.capture_note = None;
            }
        }
        self.task.final_capture = false;
        self.task.incomplete_capture = false;
        self.progressed();
    }

    /// The required checks that failed in the latest verification, by
    /// command.
    pub(super) fn failing_checks(&self) -> Vec<String> {
        self.task
            .check_results
            .iter()
            .filter(|result| !result.success)
            .map(|result| format!("`{}`", result.command))
            .collect()
    }
}
