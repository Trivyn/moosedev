//! Capture checkpoints, durable capture submission, and human capture review.
//! Intermediate checkpoints only journal; the final checkpoint asks the model
//! one plain note and hands the daemon's typed proposals to review.
use super::model::InvalidModelOutput;
use super::{bounded, HttpFailure, Mode, Phase, ReviewItem, Runner};
use crate::harness::protocol::{
    CaptureRequest, CaptureResponse, CaptureV2Request, CaptureV2Response, CheckpointResponse,
    ReviewRequest,
};
use anyhow::{Context, Result};

impl Runner {
    async fn resolve_capture(
        &mut self,
        request: &CaptureRequest,
        accept: bool,
    ) -> Result<CheckpointResponse> {
        let expected = if accept
            && self.task.final_capture
            && self.task.approved_revision.as_deref() == Some(&self.task.knowledge_revision)
            && !request.has_governing()
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
        self.update_knowledge_revision(checkpoint.revision.clone());
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

    /// Consume every event up to the frozen checkpoint end.
    pub(super) fn commit_capture_page(&mut self) {
        self.task.capture_cursor = self
            .task
            .capture_end
            .take()
            .unwrap_or(self.task.capture_cursor);
        let end = self
            .task
            .capture_checkpoint_end
            .unwrap_or(self.task.capture_cursor);
        self.task.capture_due = self.task.capture_cursor < end;
        if !self.task.capture_due {
            self.task.capture_checkpoint_end = None;
            self.task.capture_due = self
                .task
                .events
                .iter()
                .skip(end)
                .any(|e| e.message.starts_with("Human response:"));
        }
    }

    /// Consume the checkpoint without a submission and choose the next phase.
    pub(super) fn advance_after_capture_page(&mut self, checkpoint_end: usize) -> Result<()> {
        self.task.capture_end = Some(checkpoint_end);
        self.commit_capture_page();
        self.candidate_accepted();
        self.task.phase = if self.has_governing_reviews() {
            Phase::AwaitingReview
        } else if self.task.capture_due {
            self.capture_work_phase()
        } else if !self.task.batch_capture || self.task.final_capture {
            Phase::AwaitingReview
        } else {
            self.task.after_review
        };
        self.persist()
    }

    pub(super) async fn capture(&mut self) -> Result<()> {
        loop {
            match self.capture_inner().await {
                Ok(()) => {
                    return Ok(());
                }
                Err(error) => {
                    if self.repair_candidate(&error)? {
                        continue;
                    }
                    self.persist()?;
                    return Err(if error.is::<InvalidModelOutput>() {
                        error
                    } else {
                        error.context("Capture remains pending. Retry with /continue (headless step/run) after fixing the reported service or storage problem.")
                    });
                }
            }
        }
    }

    pub(super) async fn capture_inner(&mut self) -> Result<()> {
        if self.task.capture_request.is_none() && !self.symbolic_capture_page().await? {
            return Ok(());
        }
        let request = self
            .task
            .capture_request
            .as_ref()
            .context("missing capture request")?
            .clone();
        let submission = CaptureV2Request {
            operation_id: request.operation_id.clone(),
            owner_id: self.task.id.clone(),
            proposals: request.proposals.clone(),
        };
        let response: CaptureResponse = match self.post("capture/v2", &submission).await {
            Ok(CaptureV2Response::Captured { capture }) => capture,
            Ok(CaptureV2Response::Collision { collisions }) => {
                return self
                    .retype_capture_note(&format!("title collisions: {collisions:?}"))
                    .await;
            }
            Err(error)
                if error
                    .downcast_ref::<HttpFailure>()
                    .is_some_and(|e| e.status == 400) =>
            {
                // A definite pre-persistence rejection: the graph moved between
                // typing and capture. Retype the same note under fresh ids.
                return self.retype_capture_note(&format!("{error}")).await;
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
            let governing = request.has_governing();
            self.task.reviews.push(ReviewItem {
                intent_links: None,
                request,
                response,
                reason: self.task.capture_reason.clone().unwrap_or_default(),
            });
            self.task.capture_request = None;
            self.task.pending_capture = None;
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
        // Commit the review and its finished candidate budget together.
        // A restart must never charge the next checkpoint for this one's repairs.
        self.candidate_accepted();
        self.persist()
    }

    pub async fn review_operation(&mut self, id: &str, accept: bool) -> Result<()> {
        self.review_operation_with_interaction(id, accept, true)
            .await
    }

    async fn review_operation_with_interaction(
        &mut self,
        id: &str,
        accept: bool,
        emit_interaction: bool,
    ) -> Result<()> {
        let position = self
            .task
            .reviews
            .iter()
            .position(|r| r.request.operation_id == id)
            .context("unknown pending review")?;
        if self.task.reviews[position].intent_links.is_some() {
            return self
                .review_intent_links(position, accept, emit_interaction)
                .await;
        }
        anyhow::ensure!(self.task.batch_capture, "interactive review is not enabled");
        let request = self.task.reviews[position].request.clone();
        let result = self.resolve_capture(&request, accept).await?;
        anyhow::ensure!(
            result.durable && result.conforms && result.pending.is_empty(),
            "knowledge review is not durably resolved"
        );
        let item = self.task.reviews.remove(position);
        if emit_interaction {
            self.emit_review_interaction(accept, id);
        }
        self.emit_capture_review_events(id, &item.response, accept);
        self.event(format!(
            "Human {} captured knowledge.\n{}",
            if accept { "accepted" } else { "rejected" },
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

    pub(super) fn has_governing_reviews(&self) -> bool {
        self.task
            .reviews
            .iter()
            .any(|review| review.intent_links.is_some() || review.request.has_governing())
    }

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
                self.review_operation_with_interaction(&id, accept, false)
                    .await?;
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
        self.emit_review_interaction(accept, &request.operation_id);
        let pending = self
            .task
            .pending_capture
            .clone()
            .context("missing persisted capture response")?;
        self.emit_capture_review_events(&request.operation_id, &pending, accept);
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
    ) {
        let disposition = if accept { "accepted" } else { "rejected" };
        for proposal in &response.proposals {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::protocol::{
        CaptureTypeRequest, CaptureTypeResponse, ContextResponse, KnowledgeProposal,
        ProposalOrigin, ReconcileThresholds, TypedDisposition, TypedProposal, TypingMode,
    };
    use crate::llm::{LlmConfig, StructuredOutputMode};
    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::{json, Value};
    use std::{path::PathBuf, sync::Arc};

    struct Project(PathBuf);
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn context(State(root): State<Arc<PathBuf>>) -> Json<ContextResponse> {
        Json(ContextResponse {
            capture_contracts: vec![2],
            intent_contracts: vec![2],
            project_root: root.to_string_lossy().into_owned(),
            revision: "fixture".into(),
            context: String::new(),
            files: vec![],
        })
    }
    async fn model(Json(body): Json<Value>) -> Json<Value> {
        let name = body["response_format"]["json_schema"]["name"]
            .as_str()
            .unwrap();
        let answer = if name == "harness_response_probe" {
            json!({"status":"ok"})
        } else {
            assert_eq!(name, "harness_capture_note");
            json!({"note":"Keep the observed behavior."})
        };
        Json(json!({"choices":[{"message":{"content":answer.to_string()},"finish_reason":"stop"}]}))
    }
    async fn capture_type(Json(request): Json<CaptureTypeRequest>) -> Json<CaptureTypeResponse> {
        Json(CaptureTypeResponse {
            revision: "fixture".into(),
            typing_mode: TypingMode::SymbolicOnly,
            typing_note: None,
            thresholds: ReconcileThresholds::default(),
            proposals: vec![TypedProposal {
                proposal: KnowledgeProposal {
                    kind: "Lesson".into(),
                    title: "Preserve behavior".into(),
                    description: request.note,
                    evidence: request.note_evidence,
                    files: vec![],
                    components: vec![],
                    requirement: None,
                    supersedes: None,
                    retracts: None,
                    reconciled: vec![],
                },
                origin: ProposalOrigin::SymbolicDecision,
                disposition: TypedDisposition::Distinct {
                    nearest_iri: None,
                    score: None,
                    receipt_operation_id: format!("{}-r0", request.operation_id),
                },
                resolved_by: "symbolic".into(),
            }],
        })
    }
    async fn capture_v2() -> Json<Value> {
        Json(
            json!({"status":"captured","capture":{"proposals":[{"iri":"urn:captured","title":"Preserve behavior","kind":"Lesson","links":[]}]}}),
        )
    }

    /// The note, its typing and the submitted review commit together with the
    /// repair budget reset; a restart must not re-charge the finished note.
    #[tokio::test]
    async fn capture_note_commits_with_its_repair_reset_atomically() {
        let project = Project(
            std::env::temp_dir().join(format!("moosedev-capture-commit-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&project.0).unwrap();
        let root = project.0.canonicalize().unwrap();
        let router = Router::new()
            .route("/api/v1/harness/context", post(context))
            .route("/api/v1/harness/capture/type", post(capture_type))
            .route("/api/v1/harness/capture/v2", post(capture_v2))
            .route("/v1/chat/completions", post(model))
            .with_state(Arc::new(root.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let daemon = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut runner = Runner::create(root.clone(), daemon.clone(), "Preserve behavior".into())
            .await
            .unwrap();
        runner.configure(
            LlmConfig {
                base_url: format!("{daemon}/v1"),
                api_key: "fixture".into(),
                model: "scripted".into(),
                configured: true,
                context_window_tokens: 32768,
                structured_output: StructuredOutputMode::Required,
            },
            None,
        );
        runner.task.batch_capture = true;
        runner.task.plan = Some(super::super::Plan {
            summary: "Preserve behavior".into(),
            files: vec![],
            checks: vec!["true".into()],
        });
        runner.task.mode = Mode::Auto;
        runner.task.final_capture = true;
        runner.task.capture_due = true;
        runner.task.recovery = Some(super::super::RepairState {
            id: "previous-note-decision".into(),
            purpose: "harness_capture_note".into(),
            attempts: 2,
            diagnostic: "Return one note field".into(),
            status: super::super::RecoveryStatus::Paused,
        });
        // Stop exactly at the inner durable acknowledgment boundary, before
        // the outer capture wrapper can perform any additional persistence.
        runner.capture_inner().await.unwrap();
        let stored: super::super::Task =
            serde_json::from_slice(&std::fs::read(&runner.journal).unwrap()).unwrap();
        let note = stored
            .symbolic
            .as_ref()
            .unwrap()
            .capture_note
            .as_ref()
            .unwrap();
        assert_eq!(note.status, "typed");
        assert!(
            stored.recovery.is_none(),
            "completed note retained its exhausted budget"
        );
        assert_eq!(stored.model_requests.last().unwrap()["attempt"], 3);
        assert_eq!(stored.reviews.len(), 1);
        assert_eq!(stored.phase, Phase::AwaitingReview);
        let id = runner.task.id.clone();
        drop(runner);
        let mut resumed = Runner::load(root, daemon, &id).unwrap();
        resumed.begin_candidate("harness_capture_note").unwrap();
        assert_eq!(resumed.task.recovery.as_ref().unwrap().attempts, 1);
        assert_ne!(
            resumed.task.recovery.as_ref().unwrap().id,
            "previous-note-decision"
        );
        server.abort();
    }
}
