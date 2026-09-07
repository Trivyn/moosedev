//! Evidence paging, durable knowledge capture, and human capture review.
use super::model::{observation_preview, InvalidModelOutput};
use super::{bounded, Event, HttpFailure, Mode, Phase, ReviewItem, Runner};
use crate::harness::protocol::{
    CaptureRequest, CaptureResponse, CheckpointResponse, KnowledgeProposal, ReviewRequest,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// Spend a JSON-encoded byte budget, splitting even a single large event.
/// The returned position is committed only after assessment and durable capture.
fn evidence_page(
    events: &[Event],
    mut index: usize,
    mut offset: usize,
    end: usize,
    objective: &str,
    budget: usize,
) -> Result<(Vec<String>, usize, usize)> {
    let mut evidence = vec![format!("User objective: {objective}")];
    anyhow::ensure!(serde_json::to_string(&evidence)?.len() + 256 <= budget,
        "governing capture context leaves no evidence-page space; increase the configured model window and retry (this checkpoint's file set is frozen; capture remains pending)");
    while index < end {
        let event = &events[index].message;
        anyhow::ensure!(
            offset <= event.len() && event.is_char_boundary(offset),
            "invalid capture evidence cursor"
        );
        if offset == event.len() {
            index += 1;
            offset = 0;
            continue;
        }
        let header = format!("Event {index}, byte {offset}:\n");
        let available =
            budget.saturating_sub(serde_json::to_string(&evidence)?.len() + header.len() + 10);
        let mut low = 0;
        let mut high = (event.len() - offset).min(available);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let mut size = middle;
            while !event.is_char_boundary(offset + size) {
                size -= 1;
            }
            let candidate = format!("{header}{}", &event[offset..offset + size]);
            let encoded = serde_json::to_string(&candidate)?.len();
            if serde_json::to_string(&evidence)?.len() + encoded < budget {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        while !event.is_char_boundary(offset + low) {
            low -= 1;
        }
        if low == 0 {
            break;
        }
        evidence.push(format!("{header}{}", &event[offset..offset + low]));
        offset += low;
        if offset < event.len() {
            break;
        }
        index += 1;
        offset = 0;
    }
    anyhow::ensure!(
        index == end || evidence.len() > 1,
        "capture page could not make progress"
    );
    Ok((evidence, index, offset))
}

impl Runner {
    async fn resolve_capture(
        &mut self,
        request: &CaptureRequest,
        accept: bool,
    ) -> Result<CheckpointResponse> {
        let expected = if accept
            && self.task.final_capture
            && self.task.approved_revision.as_deref() == Some(&self.task.knowledge_revision)
            && !has_governing_proposals(request)
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
        }
        self.task.knowledge_revision = checkpoint.revision.clone();
        Ok(checkpoint)
    }

    pub(super) fn capture_work_phase(&self) -> Phase {
        if self.task.mode == Mode::Plan {
            Phase::Planning
        } else if self.task.final_capture {
            Phase::Verifying
        } else {
            Phase::Working
        }
    }

    pub(super) fn commit_capture_page(&mut self) {
        self.task.capture_cursor = self
            .task
            .capture_end
            .take()
            .unwrap_or(self.task.capture_cursor);
        self.task.capture_offset = self.task.capture_end_offset;
        self.task.capture_end_offset = 0;
        let end = self
            .task
            .capture_checkpoint_end
            .unwrap_or(self.task.capture_cursor);
        self.task.capture_due = self.task.capture_cursor < end;
        if !self.task.capture_due {
            self.task.capture_checkpoint_end = None;
            self.task.capture_files.clear();
            self.task.capture_due = self
                .task
                .events
                .iter()
                .skip(end)
                .any(|e| e.message.starts_with("Human response:"));
        }
    }

    pub(super) async fn capture(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.capture_repairs < 3,
            "capture failed three times; provide guidance before retrying"
        );
        let result = self.capture_inner().await;
        if result
            .as_ref()
            .is_err_and(|error| error.is::<InvalidModelOutput>())
        {
            self.task.capture_repairs += 1;
            if self.task.capture_repairs >= 3 {
                self.task.phase = Phase::AwaitingInput;
                self.task.last_response = "Capture failed three times. Review the validation evidence and provide guidance before retrying; capture remains pending.".into();
            }
        } else if result.is_ok() {
            self.task.capture_repairs = 0;
        }
        self.persist()?;
        result.map_err(|error| {
            if error.is::<InvalidModelOutput>() {
                error
            } else {
                error.context("Capture remains pending. Retry with /continue (headless step/run) after fixing the reported service or storage problem.")
            }
        })
    }

    async fn capture_inner(&mut self) -> Result<()> {
        if self.task.capture_request.is_none() {
            let mut files = self
                .task
                .plan
                .as_ref()
                .map(|p| p.files.clone())
                .unwrap_or_default();
            for file in &self.task.read_files {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
            if self.task.capture_checkpoint_end.is_some() {
                files = self.task.capture_files.clone();
            } else {
                self.task.capture_files = files.clone();
            }
            let context = self.refresh(&files).await?;
            let checkpoint_end = *self
                .task
                .capture_checkpoint_end
                .get_or_insert(self.task.events.len());
            let mut prompt = format!("You are the knowledge-capture sensor for a coding task. The harness requires this review independently of your coding actions. Extract only durable decisions, requirements, constraints, lessons, patterns, antipatterns supported by the supplied contemporaneous evidence. Do not invent rationale. Compare existing knowledge first; identify supersedes/retracts only with existing IRIs. Return proposals (possibly empty) and a reason. Each proposal needs kind,title,description,evidence (quote supporting evidence),files,components,requirement (existing IRI or null),supersedes (existing IRI or null),retracts (existing IRI or null). Never accept knowledge.\nObjective: {}\nExisting knowledge:\n{}\n", self.task.objective, context.context);
            prompt.push_str(&format!("\nCurrent human guidance (instructions; quote facts only from the evidence page): {}\n",self.task.guidance));
            if let Some(feedback) = self.task.events.iter().rev().find(|e| {
                e.message.starts_with("Step rejected or interrupted:")
                    || e.message
                        .starts_with("Capture rejected before persistence;")
            }) {
                prompt.push_str(&format!(
                    "Last validation feedback (not evidence): {}\n",
                    observation_preview(&feedback.message, 1000)
                ));
            }
            if self.task.batch_capture {
                let pending: Vec<_> = self
                    .task
                    .reviews
                    .iter()
                    .flat_map(|r| r.request.proposals.iter())
                    .map(|p| format!("{}: {}", p.kind, p.title))
                    .collect();
                prompt.push_str("\nAlready-pending proposal titles (not authority; full proposals remain in the journal):\n");
                prompt.push_str(&observation_preview(&pending.join("\n"), 4000));
            }
            prompt.push_str("\nUse only existing component identifiers from supplied knowledge; use an empty components array when none exists. A progress update, routine edit, or repeated task request alone does not warrant a durable record. Evidence is paged verbatim from the journal; this page may be only part of a large observation. Do not infer a new requirement from code or command output alone. The harness processes all pages before clearing capture.\nEvidence:\n");
            let budget = self.prompt_budget()?.saturating_sub(
                prompt.len()
                    + "\nRequired JSON schema:\n".len()
                    + serde_json::to_string(&capture_schema())?.len(),
            );
            let (evidence, page_end, page_offset) = evidence_page(
                &self.task.events,
                self.task.capture_cursor,
                self.task.capture_offset,
                checkpoint_end,
                &self.task.objective,
                budget,
            )?;
            prompt.push_str(&serde_json::to_string(&evidence)?);
            let assessment: Assessment = self
                .model_json(&prompt, "harness_capture", capture_schema())
                .await?;
            let mut proposals = assessment.proposals;
            (|| -> Result<()> {
                anyhow::ensure!(
                    !assessment.reason.trim().is_empty() && proposals.len() <= 20,
                    "capture review requires a reason and at most 20 proposals"
                );
                for proposal in &mut proposals {
                    anyhow::ensure!(
                        !proposal.evidence.is_empty()
                            && proposal.evidence.iter().all(|quote| !quote.is_empty()
                                && evidence.iter().any(|event| event.contains(quote))),
                        "capture evidence must quote actual task events"
                    );
                    anyhow::ensure!(
                        proposal.files.iter().all(|f| files.contains(f)),
                        "capture linked an unobserved file"
                    );
                    // Bind evidence to the durable task journal, independently of generated metadata.
                    proposal
                        .evidence
                        .push(format!("Task {}: {}", self.task.id, self.task.objective));
                }
                Ok(())
            })()
            .context(InvalidModelOutput)?;
            self.task.capture_reason = Some(assessment.reason.clone());
            self.task.capture_end = Some(page_end);
            self.task.capture_end_offset = page_offset;
            self.event(format!("Capture assessment: {}", assessment.reason));
            if proposals.is_empty() {
                self.commit_capture_page();
                self.task.phase = if self.task.capture_due {
                    self.capture_work_phase()
                } else if !self.task.batch_capture || self.task.final_capture {
                    Phase::AwaitingReview
                } else {
                    self.task.after_review
                };
                self.persist()?;
                return Ok(());
            }
            self.task.capture_request = Some(CaptureRequest {
                operation_id: uuid::Uuid::new_v4().to_string(),
                proposals,
            });
            self.persist()?;
        }
        let request = self
            .task
            .capture_request
            .as_ref()
            .context("missing capture request")?
            .clone();
        let response: CaptureResponse = match self.post("capture", &request).await {
            Ok(response) => response,
            Err(error)
                if error
                    .downcast_ref::<HttpFailure>()
                    .is_some_and(|e| e.status == 400) =>
            {
                self.task.capture_request = None;
                self.task.capture_end = None;
                self.task.capture_due = true;
                self.event(format!("Capture rejected before persistence; revise the proposal using this validation result: {error}"));
                self.persist()?;
                return Err(error.context(InvalidModelOutput));
            }
            Err(error) => return Err(error),
        };
        anyhow::ensure!(
            response.proposals.len() == request.proposals.len(),
            "daemon omitted capture proposals"
        );
        if !self.task.capture_operations.contains(&request.operation_id) {
            self.task
                .capture_operations
                .push(request.operation_id.clone());
        }
        self.task.capture_due = false;
        if self.task.batch_capture {
            self.commit_capture_page();
            let governing = has_governing_proposals(&request);
            self.task.reviews.push(ReviewItem {
                request,
                response,
                reason: self.task.capture_reason.clone().unwrap_or_default(),
            });
            self.task.capture_request = None;
            self.task.pending_capture = None;
            self.task.capture_repairs = 0;
            let continuation = if self.task.capture_due {
                self.capture_work_phase()
            } else {
                self.task.after_review
            };
            self.task.phase = if governing || (self.task.final_capture && !self.task.capture_due) {
                Phase::AwaitingReview
            } else {
                continuation
            };
            if governing {
                self.task.review_continuation = Some(continuation);
                self.event("New requirements, constraints, or changed knowledge need review before further work.");
            }
        } else {
            self.task.pending_capture = Some(response);
            self.task.phase = Phase::AwaitingReview;
        }
        self.persist()
    }

    pub async fn review_operation(&mut self, id: &str, accept: bool) -> Result<()> {
        anyhow::ensure!(self.task.batch_capture, "interactive review is not enabled");
        let position = self
            .task
            .reviews
            .iter()
            .position(|r| r.request.operation_id == id)
            .context("unknown pending review")?;
        let request = self.task.reviews[position].request.clone();
        let result = self.resolve_capture(&request, accept).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        let item = self.task.reviews.remove(position);
        self.event(format!(
            "Human {} captured knowledge.\n{}",
            if accept { "accepted" } else { "rejected" },
            serde_json::to_string(&item.request)?
        ));
        if self.task.reviews.is_empty() && self.task.phase == Phase::AwaitingReview {
            if self.task.capture_due {
                self.task.phase = self.capture_work_phase();
                return self.persist();
            }
            if self.task.final_capture {
                return self.finish().await;
            }
            self.task.phase = self
                .task
                .review_continuation
                .take()
                .unwrap_or(self.task.after_review);
            if self.task.phase == Phase::AwaitingPlan {
                // The next approval card must describe the graph after the
                // human's review, not force a redundant stale-revision retry.
                let files = self
                    .task
                    .plan
                    .as_ref()
                    .map(|p| p.files.clone())
                    .unwrap_or_default();
                self.refresh(&files).await?;
            }
        }
        self.persist()
    }

    pub(super) fn has_governing_reviews(&self) -> bool {
        self.task
            .reviews
            .iter()
            .any(|review| has_governing_proposals(&review.request))
    }

    pub async fn review(&mut self, accept: bool) -> Result<()> {
        if self.task.batch_capture {
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
            for id in ids {
                self.review_operation(&id, accept).await?;
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
        let result = self.resolve_capture(&request, accept).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        self.event(format!(
            "Human {} captured knowledge.\n{}",
            if accept { "accepted" } else { "rejected" },
            serde_json::to_string(&self.task.capture_request)?
        ));
        self.task.pending_capture = None;
        self.task.capture_request = None;
        self.commit_capture_page();
        self.task.capture_repairs = 0;
        if self.task.capture_due {
            self.task.phase = self.capture_work_phase();
            return self.persist();
        }
        if self.task.final_capture {
            return self.finish().await;
        }
        self.task.phase = self.task.after_review;
        // Ratification can change governing knowledge; fresh_approval checks it before work.
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
        self.event("Human confirmed that no durable knowledge changed at this checkpoint.");
        if self.task.batch_capture && !self.task.capture_due {
            self.task.capture_due = self
                .task
                .events
                .iter()
                .skip(self.task.capture_cursor)
                .any(|e| e.message.starts_with("Human response:"));
        }
        self.task.capture_repairs = 0;
        if self.task.capture_due {
            self.task.phase = self.capture_work_phase();
            self.persist()
        } else if self.task.final_capture {
            self.finish().await
        } else {
            self.task.phase = self
                .task
                .review_continuation
                .take()
                .unwrap_or(self.task.after_review);
            self.persist()
        }
    }
}

fn has_governing_proposals(request: &CaptureRequest) -> bool {
    request.proposals.iter().any(|proposal| {
        matches!(proposal.kind.as_str(), "Requirement" | "Constraint")
            || proposal.supersedes.is_some()
            || proposal.retracts.is_some()
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Assessment {
    proposals: Vec<KnowledgeProposal>,
    reason: String,
}

fn capture_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["proposals","reason"],"properties":{"reason":{"type":"string"},"proposals":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["kind","title","description","evidence","files","components","requirement","supersedes","retracts"],"properties":{"kind":{"type":"string","enum":["ArchitecturalDecision","Requirement","Constraint","Lesson","Pattern","AntiPattern"]},"title":{"type":"string"},"description":{"type":"string"},"evidence":{"type":"array","items":{"type":"string"}},"files":{"type":"array","items":{"type":"string"}},"components":{"type":"array","items":{"type":"string"}},"requirement":{"type":["string","null"]},"supersedes":{"type":["string","null"]},"retracts":{"type":["string","null"]}}}}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_pages_cover_large_escaped_events_without_skipping_bytes() {
        let events = vec![
            Event {
                message: "Read file: \"λ😀\\\n".repeat(8000),
            },
            Event {
                message: "Important final requirement".into(),
            },
        ];
        let mut position = (0, 0);
        let mut reconstructed = vec![String::new(); events.len()];
        let mut pages = 0;
        while position.0 < events.len() {
            let (page, index, offset) = evidence_page(
                &events,
                position.0,
                position.1,
                events.len(),
                "Test objective",
                4096,
            )
            .unwrap();
            assert!(serde_json::to_string(&page).unwrap().len() <= 4096);
            let mut cursor = position.0;
            for fragment in page.iter().skip(1) {
                let (_, body) = fragment.split_once('\n').unwrap();
                reconstructed[cursor].push_str(body);
                if reconstructed[cursor].len() == events[cursor].message.len() {
                    cursor += 1;
                }
            }
            assert!((index, offset) > position);
            position = (index, offset);
            pages += 1;
            assert!(pages < 1000);
        }
        assert!(pages > 2);
        assert_eq!(
            reconstructed,
            events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        );
    }
}
