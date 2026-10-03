//! Durable, bounded correction of rejected sensor output before side effects.
use super::{
    model::{InvalidModelOutput, NoopEdit},
    Mode, Phase, Progress, Runner,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub(super) const MAX_CANDIDATES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Generating,
    Retrying,
    AwaitingGuidance,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairState {
    #[serde(default)]
    pub id: String,
    pub purpose: String,
    pub attempts: usize,
    pub diagnostic: String,
    pub status: RecoveryStatus,
    /// Set when the model repeated the same rejected no-op while planned files
    /// did not exist: the next attempt may only write one of these files, read
    /// or ask. Cleared with the whole state when a candidate is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrowed: Option<Vec<String>>,
}

/// The repair lever. A model at temperature 0 re-given the same prompt plus
/// one correction line gives the same answer: badciv c83c10f8's a4b sent the
/// byte-identical whole-file write of lib.rs until the repairs ran out. After
/// an identical rejected repeat the harness narrows what may be sent, or stops
/// spending attempts. `MOOSEDEV_HARNESS_NARROW_REPAIR=off` removes the lever.
fn narrowing_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_NARROW_REPAIR").map_or(true, |value| value.trim() != "off")
}

/// The action a journaled response proposed: the `action` object of a
/// conversational answer, or the bare action.
fn proposed_action(response: &serde_json::Value) -> serde_json::Value {
    response
        .as_str()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .map(|value| match value.get("action") {
            Some(action) if action.is_object() => action.clone(),
            _ => value,
        })
        .unwrap_or(serde_json::Value::Null)
}

impl Runner {
    /// Charge before generation so a restart cannot silently replenish attempts.
    pub(super) fn begin_candidate(&mut self, purpose: &str) -> Result<()> {
        if self
            .task
            .recovery
            .as_ref()
            .is_none_or(|r| r.purpose != purpose)
        {
            self.task.recovery = Some(RepairState {
                id: uuid::Uuid::new_v4().to_string(),
                purpose: purpose.into(),
                attempts: 0,
                diagnostic: String::new(),
                status: RecoveryStatus::Generating,
                narrowed: None,
            });
        }
        let repair = self.task.recovery.as_mut().unwrap();
        if repair.attempts >= MAX_CANDIDATES {
            repair.status = RecoveryStatus::AwaitingGuidance;
            let action = repair.purpose == "harness_action";
            self.task.phase = Phase::AwaitingInput;
            if action {
                self.park_under_approved_plan();
            }
            self.persist()?;
            anyhow::bail!("model output failed validation after three attempts; provide human guidance before retrying");
        }
        repair.attempts += 1;
        repair.status = RecoveryStatus::Generating;
        self.persist()
    }

    /// Provider/service failures produced no candidate; they do not spend repairs.
    pub(super) fn candidate_unavailable(&mut self) {
        if let Some(repair) = &mut self.task.recovery {
            repair.attempts = repair.attempts.saturating_sub(1);
            repair.status = RecoveryStatus::Paused;
        }
    }

    pub(super) fn candidate_accepted(&mut self) {
        self.task.recovery = None;
    }

    /// Whether the last two candidates of this decision proposed the same
    /// action.
    fn repeated_rejection(&self) -> bool {
        let Some(repair) = self.task.recovery.as_ref() else {
            return false;
        };
        let answers: Vec<serde_json::Value> = self
            .task
            .model_requests
            .iter()
            .rev()
            .filter(|request| {
                // An interrupted stream refunded its attempt: it was never a
                // rejected candidate, whatever text it left.
                request["purpose"] == repair.purpose.as_str()
                    && request["decision_id"] == repair.id.as_str()
                    && request["interrupted"] != true
            })
            .take(2)
            .map(|request| proposed_action(&request["response"]))
            .collect();
        answers.len() == 2 && !answers[0].is_null() && answers[0] == answers[1]
    }

    /// The files a narrowed repair may write, if any. Only approved work is
    /// narrowed: a task returned to Plan must be able to plan.
    pub(super) fn narrowed_files(&self) -> Option<&[String]> {
        if self.task.mode != Mode::Auto || self.task.phase != Phase::Working {
            return None;
        }
        self.task
            .recovery
            .as_ref()
            .and_then(|repair| repair.narrowed.as_deref())
    }

    /// Returns true only for an invalid candidate that can safely be regenerated.
    pub(super) fn repair_candidate(&mut self, error: &anyhow::Error) -> Result<bool> {
        if !error.is::<InvalidModelOutput>() {
            if let Some(repair) = &mut self.task.recovery {
                if repair.status != RecoveryStatus::AwaitingGuidance {
                    repair.status = RecoveryStatus::Paused;
                }
            }
            return Ok(false);
        }
        let repeated = narrowing_enabled()
            && self
                .task
                .recovery
                .as_ref()
                .is_some_and(|repair| repair.purpose == "harness_action")
            && self.repeated_rejection();
        // The failure that stands, if any, described for a park.
        let stalled = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.stalled_failure.as_ref())
            .map(|stall| stall.signature.clone());
        let unwritten = if repeated && error.downcast_ref::<NoopEdit>().is_some() {
            self.unwritten_planned_files()
        } else {
            Vec::new()
        };
        let repair = self
            .task
            .recovery
            .as_mut()
            .context("missing model repair state")?;
        if repair.status == RecoveryStatus::AwaitingGuidance {
            return Ok(false);
        }
        repair.diagnostic = super::bounded(&format!("{error:#}"), 700);
        // An identical no-op with planned files missing: offer only a write to
        // one of them. Any other identical repeat, or one already narrowed,
        // would come back again: stop spending attempts on it.
        let narrow = repeated && !unwritten.is_empty() && repair.narrowed.is_none();
        let repeat_parked = repeated && !narrow;
        if narrow {
            repair.narrowed = Some(unwritten.clone());
        }
        let exhausted = repair.attempts >= MAX_CANDIDATES || repeat_parked;
        repair.status = if exhausted {
            RecoveryStatus::AwaitingGuidance
        } else {
            RecoveryStatus::Retrying
        };
        let stage = match repair.purpose.as_str() {
            "harness_action" => "action",
            "harness_capture_note" => "capture note",
            other => other,
        };
        // A repeated replace of an earlier version's text while a failure
        // stands is the source going back and forth, not a malformed action:
        // the park says so.
        let back_and_forth = repeat_parked
            && repair.diagnostic.contains(super::actions::STALE_TEXT)
            && stalled.is_some();
        let message = if back_and_forth {
            format!(
                "[Harness: the edits are going back and forth on {}: the same replace of an earlier version's text came twice. Guidance is needed: say which side is wrong, the test or the code it exercises, or /plan to change the approach.] {}",
                stalled.as_deref().unwrap_or_default(),
                repair.diagnostic
            )
        } else if repeat_parked {
            format!("{stage} repeated the same rejected candidate; the same prompt would only produce it again. Provide human guidance before retrying; pending work is preserved. {}", repair.diagnostic)
        } else if exhausted {
            format!("{stage} failed validation after three attempts. Provide human guidance before retrying; pending work is preserved. {}", repair.diagnostic)
        } else if narrow {
            format!(
                "Correcting {stage}, attempt {} of {MAX_CANDIDATES}, narrowed: the same rejected candidate came twice, so only a write to a planned file that does not exist yet ({}), a read or a question is offered: {}",
                repair.attempts + 1,
                unwritten.join(", "),
                repair.diagnostic
            )
        } else {
            format!(
                "Correcting {stage}, attempt {} of {MAX_CANDIDATES}: {}",
                repair.attempts + 1,
                repair.diagnostic
            )
        };
        if exhausted {
            self.task.phase = Phase::AwaitingInput;
            self.task.last_response = message.clone();
            let purpose = repair.purpose.clone();
            // A rejected action leaves the approved plan standing: a hint
            // answering it continues the plan. A capture-note park is not
            // about the plan's work and keeps its guidance path.
            if purpose == "harness_action" {
                self.park_under_approved_plan();
            }
            self.intent_event(
                if repeat_parked {
                    "repair_repeat_parked"
                } else {
                    "repair_exhausted"
                },
                &purpose,
            );
        }
        if narrow {
            self.intent_event("repair_narrowed", &unwritten.join(", "));
        }
        self.event(message.clone());
        if let Some(progress) = &self.progress {
            let _ = progress.send(Progress::Status(message));
        }
        self.persist()?;
        Ok(!exhausted)
    }
}
