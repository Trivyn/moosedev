//! Capture checkpoints and durable capture submission. Intermediate
//! checkpoints only journal; the final checkpoint asks the model one plain
//! note and submits the daemon's typed proposals.
use super::model::InvalidModelOutput;
use super::{HttpFailure, Mode, Phase, ReviewItem, Runner};
use crate::harness::protocol::{CaptureResponse, CaptureV2Request, CaptureV2Response};
use anyhow::{Context, Result};

impl Runner {
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
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{context_router, serve, Project};
    use super::*;
    use crate::harness::protocol::{
        CaptureTypeRequest, CaptureTypeResponse, KnowledgeProposal, ProposalOrigin,
        ReconcileThresholds, TypedDisposition, TypedProposal, TypingMode,
    };
    use crate::llm::{LlmConfig, StructuredOutputMode};
    use axum::{routing::post, Json};
    use serde_json::{json, Value};

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
        let project = Project::new("capture-commit");
        let root = project.0.clone();
        let router = context_router()
            .route("/api/v1/harness/capture/type", post(capture_type))
            .route("/api/v1/harness/capture/v2", post(capture_v2))
            .route("/v1/chat/completions", post(model));
        let (daemon, server) = serve(router, &project).await;
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
