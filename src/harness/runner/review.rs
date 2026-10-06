//! Human capture review: resolving one card or a whole batch against the
//! daemon, journaling each disposition, and choosing the phase that follows.
use super::approval::Disposition;
use super::{bounded, HttpFailure, Phase, Runner};
use crate::harness::protocol::{
    CaptureRequest, CaptureResponse, CheckpointResponse, ReviewRequest,
};
use anyhow::{Context, Result};

impl Runner {
    /// The daemon decision for a capture the human accepted or rejected: an
    /// accept carries the proposals the human dropped, and dropping every
    /// proposal of a capture with nothing restated is a rejection.
    fn capture_decision(&self, request: &CaptureRequest, accept: bool) -> (bool, Vec<usize>) {
        let dropped = self
            .task
            .review_drops
            .get(&request.operation_id)
            .cloned()
            .unwrap_or_default();
        if !accept {
            return (false, Vec::new());
        }
        if !request.proposals.is_empty()
            && dropped.len() == request.proposals.len()
            && request.restated.is_empty()
        {
            return (false, Vec::new());
        }
        (true, dropped)
    }

    pub(super) async fn resolve_capture(
        &mut self,
        request: &CaptureRequest,
        accept: bool,
        rejected: &[usize],
    ) -> Result<CheckpointResponse> {
        let attestable = accept && self.own_final_capture(request);
        if attestable {
            // Never send a stale expected revision: the daemon rejects the
            // whole acceptance when accepted knowledge moved since the last read.
            self.refresh(&[]).await?;
        }
        let expected = if attestable
            && self.task.approved_revision.as_deref() == Some(&self.task.knowledge_revision)
        {
            self.task.approved_revision.clone()
        } else {
            None
        };
        let mut call = self
            .http
            .post(format!("{}/api/v1/harness/review", self.daemon))
            .json(&ReviewRequest {
                operation_id: request.operation_id.clone(),
                accept,
                rejected: rejected.to_vec(),
            });
        if let Some(revision) = &expected {
            call = call.header("x-moosedev-expected-revision", revision);
        }
        let response = call.send().await?;
        let status = response.status();
        let base = response
            .headers()
            .get("x-moosedev-review-base-revision")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let result = response
            .headers()
            .get("x-moosedev-review-result-revision")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await?;
        if !status.is_success() {
            return Err(HttpFailure {
                status: status.as_u16(),
                message: format!("review: {}", bounded(&text, 4000)),
            }
            .into());
        }
        let checkpoint: CheckpointResponse = serde_json::from_str(&text)?;
        if checkpoint.durable
            && checkpoint.conforms
            && checkpoint.pending.is_empty()
            && expected.is_some()
            && base == expected
            && result.as_deref() == Some(&checkpoint.revision)
        {
            // The daemon proved that this operation alone caused the revision
            // transition. An unproven or externally changed graph stays stale.
            self.task.approved_revision = Some(checkpoint.revision.clone());
            if let Some(scope) = self.task.approved_change_scope.as_mut() {
                scope.knowledge_revision = checkpoint.revision.clone();
            }
            self.intent_event(
                "final_review_attested",
                &format!(
                    "{}: {} -> {}",
                    request.operation_id,
                    expected.as_deref().unwrap_or_default(),
                    checkpoint.revision
                ),
            );
        }
        self.update_knowledge_revision(checkpoint.revision.clone());
        Ok(checkpoint)
    }

    /// Mark proposal `number` (1-based, as the review card numbers it) of a
    /// pending capture dropped or kept. `review` (1-based) picks the card when
    /// more than one capture awaits review. Returns the proposal's title.
    pub fn set_proposal_dropped(
        &mut self,
        review: Option<usize>,
        number: usize,
        dropped: bool,
    ) -> Result<String> {
        let captures: Vec<&CaptureRequest> = match review {
            Some(review) => vec![
                &self
                    .task
                    .reviews
                    .get(
                        review
                            .checked_sub(1)
                            .context("Review numbers start at 1.")?,
                    )
                    .filter(|item| item.intent_links.is_none())
                    .context("No capture review with that number.")?
                    .request,
            ],
            None => self
                .task
                .reviews
                .iter()
                .filter(|item| item.intent_links.is_none())
                .map(|item| &item.request)
                .chain(self.task.capture_request.iter())
                .collect(),
        };
        let request = match captures.as_slice() {
            [request] => (*request).clone(),
            [] => anyhow::bail!("no captured proposals await review"),
            _ => anyhow::bail!(
                "several captures await review; name one as <review>.<proposal>, for example /drop 1.{number}"
            ),
        };
        let index = number
            .checked_sub(1)
            .context("Proposal numbers start at 1.")?;
        let title = request
            .proposals
            .get(index)
            .context("No proposal with that number in this capture.")?
            .title
            .clone();
        let drops = self
            .task
            .review_drops
            .entry(request.operation_id.clone())
            .or_default();
        drops.retain(|known| *known != index);
        if dropped {
            drops.push(index);
            drops.sort_unstable();
        }
        if drops.is_empty() {
            self.task.review_drops.remove(&request.operation_id);
        }
        self.intent_event(
            if dropped {
                "proposal_dropped"
            } else {
                "proposal_kept"
            },
            &format!("{number} {title} in {}", request.operation_id),
        );
        self.persist()?;
        Ok(title)
    }

    /// One human decision on one card; journaled as one interaction.
    pub async fn review_operation(&mut self, id: &str, accept: bool) -> Result<()> {
        self.settle_review(id, accept, true).await
    }

    /// Resolve one pending card. `journal_interaction` is false when the card
    /// is settled as part of a batch whose single interaction is journaled by
    /// the caller: the study counts human decisions, not cards.
    async fn settle_review(
        &mut self,
        id: &str,
        accept: bool,
        journal_interaction: bool,
    ) -> Result<()> {
        let position = self
            .task
            .reviews
            .iter()
            .position(|r| r.request.operation_id == id)
            .context("unknown pending review")?;
        if self.task.reviews[position].intent_links.is_some() {
            return self
                .review_intent_links(position, accept, journal_interaction)
                .await;
        }
        self.resolve_capture_card(position, accept, journal_interaction)
            .await?;
        if self.task.reviews.is_empty() && self.task.phase == Phase::AwaitingReview {
            return self.continue_after_review().await;
        }
        self.persist()
    }

    /// Resolve the capture card at `position` with the daemon and journal
    /// its disposition, leaving the phase to the caller.
    async fn resolve_capture_card(
        &mut self,
        position: usize,
        accept: bool,
        journal_interaction: bool,
    ) -> Result<()> {
        anyhow::ensure!(self.task.batch_capture, "interactive review is not enabled");
        let request = self.task.reviews[position].request.clone();
        let id = request.operation_id.clone();
        let (accept, rejected) = self.capture_decision(&request, accept);
        let result = self.resolve_capture(&request, accept, &rejected).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        let item = self.task.reviews.remove(position);
        self.task.review_drops.remove(&id);
        if journal_interaction {
            self.emit_review_interaction(accept, &id);
        }
        self.emit_capture_review_events(&id, &item.response, accept, &rejected);
        self.event(format!(
            "{}{}.\n{}",
            capture_reviewed(accept),
            dropped_titles(&item.request, &rejected),
            serde_json::to_string(&item.request)?
        ));
        Ok(())
    }

    /// The phase after every pending review is resolved: an outstanding
    /// checkpoint first, completion after the final one, else the journaled
    /// continuation.
    pub(super) async fn continue_after_review(&mut self) -> Result<()> {
        if self.task.capture_due {
            self.task.phase = self.capture_work_phase();
            return self.persist();
        }
        if self.task.incomplete_capture {
            self.task.review_continuation = None;
            self.persist()?;
            return self.finish_incomplete().await;
        }
        if self.task.final_capture {
            self.task.review_continuation = None;
            self.task.phase = Phase::Verifying;
            self.persist()?;
            return self.finish().await;
        }
        self.task.phase = self
            .task
            .review_continuation
            .take()
            .unwrap_or(self.task.after_review);
        self.persist()
    }

    /// The task's own final note, captured after every required check passed,
    /// with nothing else pending: the only acceptance whose revision change
    /// the daemon may attest. A supersession or retraction always re-gates.
    fn own_final_capture(&self, request: &CaptureRequest) -> bool {
        self.task.final_capture
            && !request.has_lifecycle_change()
            && self.task.pending_edit.is_none()
            && self.task.intent.is_none()
            && self.task.pending_intent_links.is_none()
            && self.required_checks_passed()
            && self
                .task
                .symbolic
                .as_ref()
                .and_then(|state| state.capture_note.as_ref())
                .is_some_and(|note| {
                    note.status == "captured" && note.capture_operation_id == request.operation_id
                })
    }

    pub(super) fn has_governing_reviews(&self) -> bool {
        self.task
            .reviews
            .iter()
            .any(|review| review.intent_links.is_some() || review.request.has_governing())
    }

    /// Headless review: every pending card takes the same disposition.
    pub async fn review(&mut self, accept: bool) -> Result<()> {
        if self.task.batch_capture || self.task.reviews.iter().any(|r| r.intent_links.is_some()) {
            anyhow::ensure!(
                !self.task.reviews.is_empty(),
                "no captured proposals awaiting review"
            );
            let ids: Vec<_> = self
                .task
                .reviews
                .iter()
                .map(|r| r.request.operation_id.clone())
                .collect();
            self.intent_event(
                "review_interaction",
                &format!(
                    "{} batch [{}]",
                    if accept { "accepted" } else { "rejected" },
                    ids.join(",")
                ),
            );
            self.persist()?;
            for id in ids {
                self.settle_review(&id, accept, false).await?;
            }
            return Ok(());
        }
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingReview && self.task.pending_capture.is_some(),
            "no captured proposals awaiting review"
        );
        self.resolve_pending_capture(accept).await?;
        // Ratification can change governing knowledge; fresh_approval checks it before work.
        self.continue_after_review().await
    }

    /// Resolve the one pending capture of a task without batch review and
    /// journal its disposition, leaving the phase to the caller.
    async fn resolve_pending_capture(&mut self, accept: bool) -> Result<()> {
        let request = self
            .task
            .capture_request
            .clone()
            .context("missing capture operation")?;
        let (accept, rejected) = self.capture_decision(&request, accept);
        let result = self.resolve_capture(&request, accept, &rejected).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        self.task.review_drops.remove(&request.operation_id);
        self.event(format!(
            "{}{}.\n{}",
            capture_reviewed(accept),
            dropped_titles(&request, &rejected),
            serde_json::to_string(&self.task.capture_request)?
        ));
        self.emit_review_interaction(accept, &request.operation_id);
        let pending = self
            .task
            .pending_capture
            .clone()
            .context("missing persisted capture response")?;
        self.emit_capture_review_events(&request.operation_id, &pending, accept, &rejected);
        self.task.pending_capture = None;
        self.task.capture_request = None;
        self.commit_capture_page();
        Ok(())
    }

    /// The human sends the work back at the final capture review instead of
    /// letting a rejection complete the task (badciv P5: step 2 had to be
    /// restarted as a new task). The pending capture is rejected as `/reject`
    /// records it. The note is judged against the approved plan as a park
    /// answer is: one that changes the plan (it opens with a refusal, turns
    /// the work around, or names a file or rule the plan leaves out) returns
    /// the task to Plan with the note as guidance, as a steering message
    /// does. Otherwise the task returns to Working under the approval it had,
    /// with the note as guidance, and its next finish verifies and asks for
    /// the final note again. `fresh_approval` still guards the next step.
    pub async fn rework(&mut self, note: String) -> Result<()> {
        anyhow::ensure!(
            self.task.at_final_review(),
            "nothing to send back: rework applies at the final capture review"
        );
        anyhow::ensure!(
            !note.trim().is_empty() && note.len() <= 16_000,
            "say what is left to do: the note must contain 1..16000 bytes"
        );
        anyhow::ensure!(
            self.task.reviews.iter().all(|r| r.intent_links.is_none()),
            "resolve the association review first"
        );
        let disposition = self.judge_message(&note, true).await;
        let ids: Vec<String> = self
            .task
            .reviews
            .iter()
            .map(|r| r.request.operation_id.clone())
            .collect();
        if !ids.is_empty() {
            self.intent_event(
                "review_interaction",
                &format!("rejected batch [{}]", ids.join(",")),
            );
            self.persist()?;
            for _ in &ids {
                self.resolve_capture_card(0, false, false).await?;
            }
        } else if self.task.pending_capture.is_some() {
            self.resolve_pending_capture(false).await?;
        }
        // Sent back at an incomplete end, the task goes on as a sent-back
        // finished one does.
        self.progressed();
        self.task.incomplete_capture = false;
        self.event(format!("Human sent the work back at review: {note}"));
        self.task.review_continuation = None;
        if let Some(state) = self.task.symbolic.as_mut() {
            state.auto_verification = None;
            // The next final checkpoint asks for a note about the reworked
            // change instead of reporting the rejected one as captured.
            state.capture_note = None;
            // A human choice to verify with planned files missing answered
            // the finish before review; the next finish is gated again.
            state.unfinished_accepted_at = None;
        }
        if let Disposition::Replan(reason) = &disposition {
            self.intent_event("review_rework", &format!("replan: {reason}"));
            self.event(format!("Returning to Plan: {reason}"));
            // The review is resolved: the steering path returns to Plan.
            self.task.phase = Phase::Planning;
            return self.return_to_plan_with_guidance(None, note).await;
        }
        self.intent_event("review_rework", &bounded(&note, 400));
        self.end_unchanged_window();
        self.forget_failure();
        self.task.knowledge_turn_sequence = self.task.knowledge_turn_sequence.saturating_add(1);
        self.task.final_capture = false;
        self.task.completion_pending = false;
        self.task.check_results.clear();
        self.task.guidance = note.clone();
        self.task.last_response = format!("The human sent the work back at review: {note}");
        self.task.recovery = None;
        self.task.handed_back = false;
        self.task.plan_stands_park = false;
        self.task.turn_finished = false;
        self.task.steps = 0;
        self.task.after_review = Phase::Working;
        self.task.phase = Phase::Working;
        self.persist()
    }

    pub async fn confirm_no_knowledge(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingReview
                && self.task.pending_capture.is_none()
                && self.task.capture_request.is_none()
                && self.task.reviews.is_empty(),
            "no no-change review pending"
        );
        self.event(NO_KNOWLEDGE_CONFIRMED);
        if self.task.batch_capture && !self.task.capture_due {
            self.task.capture_due = self
                .task
                .events
                .iter()
                .skip(self.task.capture_cursor)
                .any(|e| e.message.starts_with("Human response:"));
        }
        if self.task.incomplete_capture && !self.task.capture_due {
            return self.finish_incomplete().await;
        }
        if self.task.final_capture && !self.task.capture_due {
            return self.finish().await;
        }
        self.continue_after_review().await
    }
}

impl Runner {
    pub(super) fn emit_review_interaction(&mut self, accept: bool, operation_id: &str) {
        self.intent_event(
            "review_interaction",
            &format!(
                "{} {}",
                if accept { "accepted" } else { "rejected" },
                operation_id
            ),
        );
    }

    fn emit_capture_review_events(
        &mut self,
        operation_id: &str,
        response: &CaptureResponse,
        accept: bool,
        rejected: &[usize],
    ) {
        for (index, proposal) in response.proposals.iter().enumerate() {
            let disposition = if accept && !rejected.contains(&index) {
                "accepted"
            } else {
                "rejected"
            };
            self.intent_event(
                "record_review",
                &format!("{disposition} {} in {operation_id}", proposal.iri),
            );
            for link in &proposal.links {
                self.intent_event(
                    "link_review",
                    &format!("{disposition} {link} in {operation_id}"),
                );
            }
        }
        let disposition = if accept { "accepted" } else { "rejected" };
        for link in response
            .restated
            .iter()
            .flat_map(|restated| &restated.links)
        {
            self.intent_event(
                "link_review",
                &format!("{disposition} {link} in {operation_id}"),
            );
        }
    }
}

/// "; dropped: A; B" for the proposals rejected within an accept.
fn dropped_titles(request: &CaptureRequest, rejected: &[usize]) -> String {
    let titles: Vec<&str> = rejected
        .iter()
        .filter_map(|index| request.proposals.get(*index))
        .map(|proposal| proposal.title.as_str())
        .collect();
    if titles.is_empty() {
        String::new()
    } else {
        format!("; dropped: {}", titles.join("; "))
    }
}

/// The journal line of a confirmation that a capture checkpoint changed no
/// durable knowledge. Headless runs journal one at every checkpoint.
const NO_KNOWLEDGE_CONFIRMED: &str =
    "Human confirmed that no durable knowledge changed at this checkpoint.";

/// The journal prefixes of a capture review decision, accepted and rejected.
const CAPTURE_REVIEWED: [&str; 2] = [
    "Human accepted captured knowledge",
    "Human rejected captured knowledge",
];

/// The journal prefix of the human's decision on a capture.
fn capture_reviewed(accept: bool) -> &'static str {
    CAPTURE_REVIEWED[usize::from(!accept)]
}

/// Whether a journal event is the human changing what the model works
/// with: guidance, an answer, an approval, a grant or a denial. A capture
/// checkpoint's confirmation or review decision settles knowledge, not the
/// work, so it is neutral: a window of looking or of unchanged commands runs
/// on across it. badciv run 14 (c7abc2d0) looped 15 times through inspect,
/// `ls`, read and `cargo test` without a refusal, because every headless
/// checkpoint journaled "Human confirmed…" and ended the window.
/// Whether a journal event restarts the loop guards' windows: human
/// progress, or the harness's own recovery from a model-stuck stop, which
/// stands in for the answer a human would have given ([`super::recover`]).
pub(super) fn is_progress(message: &str) -> bool {
    is_human_progress(message) || message.starts_with(super::recover::HARNESS_RECOVERY)
}

pub(super) fn is_human_progress(message: &str) -> bool {
    message.starts_with("Human ")
        && message != NO_KNOWLEDGE_CONFIRMED
        && !CAPTURE_REVIEWED
            .iter()
            .any(|prefix| message.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_confirmations_and_reviews_are_not_human_progress() {
        for neutral in [
            "Human confirmed that no durable knowledge changed at this checkpoint.",
            "Human accepted captured knowledge.\n{}",
            "Human accepted captured knowledge; dropped: A; B.\n{}",
            "Human rejected captured knowledge.\n{}",
        ] {
            assert!(!is_human_progress(neutral), "{neutral}");
        }
        for progress in [
            "Human response: use the fixture",
            "Human approved the plan and entered Auto.",
            "Human sent the work back at review: write the test",
            "Human approved task-scoped permission p1 for exact pending command c.",
            "Human denied permission request: p1",
            "Human chose for open choice 1: A",
            "Human added a.rs to the approved plan.",
        ] {
            assert!(is_human_progress(progress), "{progress}");
        }
        assert!(!is_human_progress("Applied edit to a.rs"));
        assert_eq!(capture_reviewed(true), "Human accepted captured knowledge");
        assert_eq!(capture_reviewed(false), "Human rejected captured knowledge");
    }
}
