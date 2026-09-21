//! Human decisions on a task: plan and edit approval, review requests,
//! steering messages, returning to Plan, and answering a question.
use super::*;

impl Runner {
    pub async fn approve_plan(&mut self) -> Result<()> {
        self.intent_event("plan_approval_attempt", "human plan approval requested");
        self.persist()?;
        let result = self.approve_plan_inner().await;
        if let Err(error) = &result {
            self.intent_event("plan_blocked", &error.to_string());
            self.persist()?;
        }
        result
    }

    async fn approve_plan_inner(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.has_governing_reviews(),
            "review new governing knowledge before approving execution"
        );
        anyhow::ensure!(
            self.task.pending_spec.is_none(),
            "approve or revise the pending specification before approving execution"
        );
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingPlan && !self.task.capture_due,
            "no plan awaiting approval"
        );
        let files = self.task.plan.as_ref().context("no plan")?.files.clone();
        let previous = self.task.knowledge_revision.clone();
        let context = self.refresh(&files).await?;
        let snapshots = self.snapshot(&files)?;
        if previous != context.revision || snapshots != self.task.snapshots {
            self.task.snapshots = snapshots;
            self.task.approved_change_scope = None;
            self.event(
                "Evidence changed before approval; inspect refreshed context and approve again.",
            );
            self.persist()?;
            bail!("plan evidence changed; renewed approval required");
        }
        if self.prepare_intent_links().await? {
            self.intent_event("plan_blocked", "intent associations await human review");
            return self.persist();
        }
        self.derive_symbolic_scope(&context).await?;
        let state = self.symbolic_state_mut();
        state.unchanged_since_approval = true;
        state.cycle_replan_continuations = 0;
        state.plan_grounded = false;
        self.task.approved_revision = Some(context.revision);
        self.task.completion_pending = false;
        self.task.mode = Mode::Auto;
        self.task.phase = Phase::Working;
        self.task.turn_finished = false;
        self.task.check_results.clear();
        self.intent_event("plan_approved", "approved current source and knowledge");
        self.end_intent_cycle("approved");
        self.event("Human approved the plan and entered Auto.");
        self.persist()
    }

    pub async fn approve_policy(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.has_governing_reviews(),
            "review new governing knowledge before approving execution"
        );
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingPolicy,
            "no edit awaiting approval"
        );
        let edit = self.task.pending_edit.clone().context("no pending edit")?;
        if !self.fresh_approval().await? {
            bail!("plan evidence changed before edit approval");
        }
        let context = self.refresh(std::slice::from_ref(&edit.file)).await?;
        anyhow::ensure!(
            context.revision == edit.revision,
            "governing knowledge changed before edit approval"
        );
        if let PolicyDecision::Gate {
            disposition: GateDisposition::Deny | GateDisposition::RequirePlan,
            reason,
            ..
        } = &context.files[0].policy
        {
            bail!("edit cannot be approved: {reason}");
        }
        self.event(format!(
            "Human approved the exact pending edit to {}.",
            edit.file
        ));
        self.apply_edit(edit)
    }

    /// Explicit review is available between turns as well as at completion.
    pub fn request_review(&mut self) -> Result<()> {
        anyhow::ensure!(self.task.batch_capture, "interactive review is not enabled");
        anyhow::ensure!(
            !self.task.cleanup_pending,
            "cancelled task cleanup is pending; retry cancel or resume before opening review"
        );
        anyhow::ensure!(
            !self.task.completion_pending,
            "knowledge review is already resolved; use /continue to retry completion"
        );
        anyhow::ensure!(
            self.task.capture_request.is_none() && !self.task.capture_due,
            "finish the outstanding capture assessment first"
        );
        if self.task.phase != Phase::AwaitingReview {
            self.task.review_continuation = Some(self.task.phase);
            self.task.phase = Phase::AwaitingReview;
        }
        self.persist()
    }

    /// Human steering is evidence, never implicit approval of a plan or edit.
    pub async fn submit_message(&mut self, text: String) -> Result<()> {
        self.submit_message_inner(None, text).await
    }

    pub async fn submit_message_once(&mut self, id: &str, text: String) -> Result<()> {
        if self.task.delivered_messages.iter().any(|known| known == id) {
            return Ok(());
        }
        uuid::Uuid::parse_str(id).context("invalid message ID")?;
        self.submit_message_inner(Some(id), text).await
    }

    async fn submit_message_inner(&mut self, id: Option<&str>, text: String) -> Result<()> {
        anyhow::ensure!(
            self.task.phase != Phase::Complete,
            "start a new task after completion"
        );
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= 16_000,
            "message must contain 1..16000 bytes"
        );
        if self.task.phase == Phase::Cancelled {
            self.resume().await?;
        }
        if self.task.intent.is_some() {
            self.reconcile()?;
            if self.task.intent.is_some() {
                anyhow::ensure!(
                    self.task.phase == Phase::AwaitingInput,
                    "reconcile interrupted action first"
                );
                // Record identity with the same journal write as the acknowledgment.
                if let Some(id) = id {
                    self.task.delivered_messages.push(id.to_owned());
                }
                self.task.guidance = text.clone();
                self.answer(text).await?;
                self.task.turn_finished = false;
                return self.persist();
            }
        }
        self.abandon_pending_intent("new human guidance").await?;
        if self.task.pending_spec.take().is_some() {
            self.event(
                "Discarded the pending spec preview because the human supplied new guidance.",
            );
        }
        self.event(format!("Human response: {text}"));
        if let Some(id) = id {
            self.task.delivered_messages.push(id.to_owned());
        }
        self.task.knowledge_turn_sequence = self.task.knowledge_turn_sequence.saturating_add(1);
        self.task.guidance = text.clone();
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.turn_finished = false;
        self.task.steps = 0;
        self.end_intent_cycle("new human guidance");
        self.task.approved_revision = None;
        self.discard_pending_edit("new human guidance invalidated the proposed edit")?;
        self.task.completion_pending = false;
        self.task.check_results.clear();
        self.task.final_capture = false;
        self.task.mode = Mode::Plan;
        self.task.after_review = Phase::Planning;
        self.task.read_files.clear();
        self.task.source.clear();
        if let Some(state) = self.task.symbolic.as_mut() {
            state.read_snapshots.clear();
        }
        if self.task.phase == Phase::AwaitingReview {
            self.task.review_continuation = Some(Phase::Planning);
        } else {
            self.task.phase = Phase::Planning;
        }
        // Existing uncertain capture requests remain frozen for idempotent retry.
        self.persist()
    }

    pub(super) fn discard_pending_edit(&mut self, reason: &str) -> Result<()> {
        if let Some(edit) = self.task.pending_edit.take() {
            self.event(format!(
                "Discarded pending policy edit: {reason}.\n{}",
                serde_json::to_string(&edit)?
            ));
        }
        Ok(())
    }

    pub async fn mode_plan(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.task.cleanup_pending,
            "cancelled task cleanup is pending; retry cancel or resume before replanning"
        );
        anyhow::ensure!(
            self.task.phase != Phase::Complete
                && self.task.pending_capture.is_none()
                && self.task.capture_request.is_none(),
            "resolve pending knowledge review before replanning"
        );
        self.abandon_pending_intent("human returned to Plan")
            .await?;
        self.task.mode = Mode::Plan;
        self.task.phase = Phase::Planning;
        if self.task.pending_spec.take().is_some() {
            self.event("Discarded the pending spec preview because the human returned to Plan.");
        }
        self.task.approved_revision = None;
        self.discard_pending_edit("human returned the task to Plan")?;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        self.task.check_results.clear();
        self.task.after_review = Phase::Planning;
        self.task.read_files.clear();
        self.task.source.clear();
        if let Some(state) = self.task.symbolic.as_mut() {
            state.read_snapshots.clear();
        }
        self.task.steps = 0;
        self.end_intent_cycle("human replan");
        self.event("Human returned the task to Plan.");
        self.persist()
    }

    pub async fn answer(&mut self, text: String) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingInput && !text.trim().is_empty(),
            "no question awaiting an answer"
        );
        self.event(format!("Human response: {text}"));
        self.end_unchanged_window();
        self.task.knowledge_turn_sequence = self.task.knowledge_turn_sequence.saturating_add(1);
        self.task.guidance = text.clone();
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.steps = 0;
        if self.task.intent.take().is_some() {
            self.task.mode = Mode::Plan;
            self.task.approved_revision = None;
            self.task.check_results.clear();
        }
        self.task.phase = if self.task.mode == Mode::Plan {
            Phase::Planning
        } else {
            Phase::Working
        };
        self.task.capture_due = true;
        self.task.after_review = self.task.phase;
        self.persist()
    }
}
