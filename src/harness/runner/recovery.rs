//! Durable, bounded correction of rejected sensor output before side effects.
use super::{model::InvalidModelOutput, Phase, Progress, Runner};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_CANDIDATES: usize = 3;

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
                attempts: if purpose == "harness_capture" {
                    self.task.capture_repairs
                } else {
                    0
                },
                diagnostic: String::new(),
                status: RecoveryStatus::Generating,
            });
        }
        let repair = self.task.recovery.as_mut().unwrap();
        if repair.attempts >= MAX_CANDIDATES {
            repair.status = RecoveryStatus::AwaitingGuidance;
            self.task.phase = Phase::AwaitingInput;
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
        self.task.capture_repairs = 0;
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
        let repair = self
            .task
            .recovery
            .as_mut()
            .context("missing model repair state")?;
        if repair.status == RecoveryStatus::AwaitingGuidance {
            return Ok(false);
        }
        repair.diagnostic = super::bounded(&format!("{error:#}"), 700);
        let exhausted = repair.attempts >= MAX_CANDIDATES;
        repair.status = if exhausted {
            RecoveryStatus::AwaitingGuidance
        } else {
            RecoveryStatus::Retrying
        };
        let stage = match repair.purpose.as_str() {
            "harness_capture" => "capture",
            "harness_action" => "action",
            "harness_purpose_selection" => "purpose selection",
            "harness_association_selection" => "association selection",
            "harness_capture_resolution" => "capture resolution",
            "harness_capture_note" => "capture note",
            other => other,
        };
        let message = if exhausted {
            format!("{stage} failed validation after three attempts. Provide human guidance before retrying; pending work is preserved. {}", repair.diagnostic)
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
            self.intent_event("repair_exhausted", &purpose);
        }
        self.event(message.clone());
        if let Some(progress) = &self.progress {
            let _ = progress.send(Progress::Status(message));
        }
        self.persist()?;
        Ok(!exhausted)
    }
}
