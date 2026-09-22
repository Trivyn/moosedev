//! Task completion, cancellation, and recovery after interruption: the
//! durable checkpoint, scratch cleanup, and reconciling an uncertain intent.
use super::*;

impl Runner {
    pub(super) async fn finish(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.reviews.is_empty()
                && self.task.capture_request.is_none()
                && !self.task.capture_due,
            "knowledge review remains unresolved"
        );
        anyhow::ensure!(
            self.task.edits.is_empty() || self.symbolic_associations_resolved(),
            "post-edit association assessment remains unresolved"
        );
        // Publish resolved review and this retryable completion intent together.
        // A failed checkpoint must not send the user back to a no-change review.
        self.task.completion_pending = true;
        self.task.phase = Phase::Verifying;
        self.persist()?;
        if !self.fresh_approval().await? {
            self.task.final_capture = false;
            self.task.completion_pending = false;
            return self.persist();
        }
        let files = self.task.plan.as_ref().context("no plan")?.files.clone();
        anyhow::ensure!(
            self.snapshot(&files)? == self.task.snapshots,
            "source changed after verification; return to planning"
        );
        anyhow::ensure!(
            self.required_checks_passed(),
            "required checks have not passed"
        );
        for id in &self.task.capture_operations {
            let checkpoint = self.checkpoint(Some(id)).await?;
            anyhow::ensure!(
                checkpoint.conforms && checkpoint.durable && checkpoint.pending.is_empty(),
                "capture operation {id} remains unresolved"
            );
        }
        let checkpoint = self.checkpoint(None).await?;
        anyhow::ensure!(
            checkpoint.conforms && checkpoint.durable,
            "graph validation or durable checkpoint failed"
        );
        anyhow::ensure!(
            self.task.approved_revision.as_deref() == Some(&checkpoint.revision),
            "knowledge changed during completion; refresh and approve the plan again"
        );
        self.update_knowledge_revision(checkpoint.revision);
        executor::cleanup_task(&self.scratch_path())?;
        self.expire_permissions();
        self.task.phase = Phase::Complete;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        // A task that changed an approved spec leaves its records to reconcile;
        // the re-approval previews the supersessions.
        let mut edited_specs: Vec<String> = self
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "spec_edited")
            .map(|event| event.detail.clone())
            .collect();
        edited_specs.dedup();
        let specs = edited_specs
            .iter()
            .map(|path| format!(" Approved spec {path} changed in this task; /approve-spec {path} reconciles its records."))
            .collect::<String>();
        self.event(format!("Complete: required checks passed, human knowledge review resolved, graph validated and durably checkpointed.{specs}"));
        self.persist()
    }

    /// Every plan check ran, in order, and passed.
    pub(super) fn required_checks_passed(&self) -> bool {
        self.task.plan.as_ref().is_some_and(|plan| {
            plan.checks.len() == self.task.check_results.len()
                && plan
                    .checks
                    .iter()
                    .zip(&self.task.check_results)
                    .all(|(command, result)| command == &result.command && result.success)
        })
    }

    pub async fn cancel(&mut self) -> Result<()> {
        self.preserve_stream();
        anyhow::ensure!(
            self.task.phase != Phase::Complete,
            "completed task cannot be cancelled"
        );
        if self.task.phase != Phase::Cancelled {
            self.task.resume_phase = self.task.phase;
            self.task.phase = Phase::Cancelled;
            self.task.cleanup_pending = true;
            self.event("Cancelled; unresolved actions, capture obligations, and scratch cleanup preserved.");
            self.persist()?;
        }
        self.retry_cancelled_cleanup()
    }

    fn retry_cancelled_cleanup(&mut self) -> Result<()> {
        if !self.task.cleanup_pending {
            return Ok(());
        }
        match executor::cleanup_task(&self.scratch_path()) {
            Ok(()) => {
                self.task.cleanup_pending = false;
                self.task.last_error = None;
                self.event("Cancelled task scratch cleanup finished.");
                self.persist()
            }
            Err(error) => {
                let message = format!("Cancellation took effect; scratch cleanup is pending: {error:#}. Press Esc or retry cancel to clean up; /continue (headless resume) retries cleanup before resuming work.");
                self.task.last_error = Some(message.clone());
                self.event(message.clone());
                self.persist()?;
                bail!(message)
            }
        }
    }

    pub(super) fn reconcile(&mut self) -> Result<()> {
        if let Some(intent) = self.task.intent.clone() {
            match intent {
                Intent::Edit(edit) => {
                    let current = self.workspace.read(&edit.file)?;
                    if current == edit.after {
                        if !self.task.edits.iter().any(|past| {
                            past.file == edit.file
                                && past.before == edit.before
                                && past.after == edit.after
                                && past.revision == edit.revision
                        }) {
                            self.task.edits.push(edit.clone());
                        }
                        self.task
                            .snapshots
                            .insert(edit.file.clone(), fingerprint(&current));
                        self.task.source.insert(edit.file.clone(), current);
                        self.end_unchanged_window();
                        self.task.intent = None;
                        self.task.capture_due = true;
                        self.task.after_review = Phase::Working;
                        self.event(format!("Recovered completed edit to {} without replay. Before: {:?}; after: {:?}", edit.file, edit.before, edit.after));
                    } else {
                        self.task.phase = Phase::AwaitingInput;
                        self.task.last_response = "Interrupted edit has not reached its expected result. Inspect the file and answer before proceeding; it will not be replayed automatically.".into();
                    }
                }
                Intent::Command(command) | Intent::PermissionedCommand { command, .. } => {
                    self.task.phase = Phase::AwaitingInput;
                    self.task.last_response = format!("Command outcome is unknown after interruption: {command}. Inspect and acknowledge before proceeding. It will not be replayed automatically.");
                }
            }
        }
        self.persist()
    }

    pub async fn resume(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase != Phase::Complete,
            "task is already complete"
        );
        self.retry_cancelled_cleanup()?;
        if self.task.phase == Phase::Cancelled {
            self.task.phase = self.task.resume_phase;
        }
        self.reconcile()?;
        self.resume_intent_refresh().await?;
        let files = self.task.read_files.clone();
        self.refresh(&files).await?;
        // Recover an outstanding capture before approval freshness can change
        // the phase: AwaitingPlan cannot advance an uncertain capture request.
        if self.task.mode == Mode::Auto
            && matches!(self.task.phase, Phase::Working | Phase::Verifying)
            && self.task.intent.is_none()
            && !self.task.capture_due
            && self.task.capture_request.is_none()
        {
            self.fresh_approval().await?;
        }
        self.event("Resumed with refreshed project knowledge.");
        self.persist()
    }
}
