//! Human capture review: resolving one card or a whole batch against the
//! daemon, journaling each disposition, and choosing the phase that follows.
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
        anyhow::ensure!(self.task.batch_capture, "interactive review is not enabled");
        let request = self.task.reviews[position].request.clone();
        let (accept, rejected) = self.capture_decision(&request, accept);
        let result = self.resolve_capture(&request, accept, &rejected).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        let item = self.task.reviews.remove(position);
        self.task.review_drops.remove(id);
        if journal_interaction {
            self.emit_review_interaction(accept, id);
        }
        self.emit_capture_review_events(id, &item.response, accept, &rejected);
        self.event(format!(
            "Human {} captured knowledge{}.\n{}",
            if accept { "accepted" } else { "rejected" },
            dropped_titles(&item.request, &rejected),
            serde_json::to_string(&item.request)?
        ));
        if self.task.reviews.is_empty() && self.task.phase == Phase::AwaitingReview {
            return self.continue_after_review().await;
        }
        self.persist()
    }

    /// The phase after every pending review is resolved: an outstanding
    /// checkpoint first, completion after the final one, else the journaled
    /// continuation.
    pub(super) async fn continue_after_review(&mut self) -> Result<()> {
        if self.task.capture_due {
            self.task.phase = self.capture_work_phase();
            return self.persist();
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
            "Human {} captured knowledge{}.\n{}",
            if accept { "accepted" } else { "rejected" },
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
        // Ratification can change governing knowledge; fresh_approval checks it before work.
        self.continue_after_review().await
    }

    pub async fn confirm_no_knowledge(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingReview
                && self.task.pending_capture.is_none()
                && self.task.capture_request.is_none()
                && self.task.reviews.is_empty(),
            "no no-change review pending"
        );
        self.event("Human confirmed that no durable knowledge changed at this checkpoint.");
        if self.task.batch_capture && !self.task.capture_due {
            self.task.capture_due = self
                .task
                .events
                .iter()
                .skip(self.task.capture_cursor)
                .any(|e| e.message.starts_with("Human response:"));
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
