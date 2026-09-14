//! Intent events and the reviewed code-link path: a derived association batch
//! is frozen as an `intent/link` operation, reviewed by a human, and its
//! disposition journaled. Graph identity and link validation stay in the daemon.
use super::*;
use crate::harness::daemon::intent::IntentLinkResponse;
use std::collections::BTreeSet;

/// The review item kind under which derived associations are presented.
pub(super) const LINK_REVIEW_KIND: &str = "DerivedAssociation";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEvent {
    pub id: String,
    pub cycle: Option<String>,
    pub kind: String,
    pub detail: String,
}

impl Runner {
    pub(super) fn update_knowledge_revision(&mut self, revision: String) {
        if !self.task.knowledge_revision.is_empty() && self.task.knowledge_revision != revision {
            self.intent_event(
                "knowledge_revision_changed",
                &format!("{} -> {}", self.task.knowledge_revision, revision),
            );
        }
        self.task.knowledge_revision = revision;
    }

    pub(super) fn intent_event(&mut self, kind: &str, detail: &str) {
        self.task.intent_events.push(IntentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            cycle: self.task.intent_cycle.clone(),
            kind: kind.to_string(),
            detail: bounded(detail, 2000),
        });
    }
    pub(super) fn start_intent_cycle(&mut self) {
        if self.task.intent_cycle.is_none() {
            self.task.intent_cycle = Some(uuid::Uuid::new_v4().to_string());
            self.intent_event("cycle_started", "entered plan review");
        }
    }
    pub(super) fn end_intent_cycle(&mut self, reason: &str) {
        if self.task.intent_cycle.is_some() {
            self.intent_event("cycle_ended", reason);
            self.task.intent_cycle = None;
        }
    }

    pub(super) fn action_schema(&self) -> Value {
        if self.task.batch_capture {
            conversational_schema(self.task.mode)
        } else {
            action_schema()
        }
    }

    /// Post the frozen link request and turn its receipt into one review item.
    /// Returns true when the task now waits on that review (or resolved the
    /// batch without links); false when there was nothing pending.
    pub(super) async fn prepare_intent_links(&mut self) -> Result<bool> {
        let Some(request) = self.task.pending_intent_links.clone() else {
            return Ok(false);
        };
        let response: IntentLinkResponse = self.post("intent/link", &request).await?;
        if !response.unresolved.is_empty() {
            anyhow::ensure!(response.links.is_empty() && response.resolved.is_empty(),
                "daemon must reject unresolved intent batches atomically; use /plan to abandon this operation");
            // Journaled, never parked: an unprovable binding costs the link,
            // not the task. The association batch is closed without links.
            self.task.pending_intent_links = None;
            self.intent_event("unresolved_binding", &response.unresolved.join("; "));
            self.resolve_symbolic_association(&request.operation_id);
            self.event(format!(
                "Derived associations could not be bound to current indexed source and were dropped: {}",
                response.unresolved.join("; ")
            ));
            self.persist()?;
            return Ok(false);
        }
        anyhow::ensure!(
            response.resolved == request.bindings,
            "daemon intent response did not preserve the exact requested binding order"
        );
        let unique_links: BTreeSet<_> = response.links.iter().collect();
        anyhow::ensure!(
            response.links.len() == request.bindings.len()
                && unique_links.len() == response.links.len(),
            "daemon intent response must identify one unique link per requested binding"
        );
        // Reuse the existing review UI for precise associations. Existing
        // dossier links were skipped; daemon retries deduplicate new proposals.
        self.task.reviews.push(ReviewItem {
            intent_links: Some(request.clone()),
            request: CaptureRequest {
                operation_id: request.operation_id.clone(),
                proposals: vec![],
                changed: vec![],
            },
            response: CaptureResponse {
                proposals: vec![CapturedProposal {
                    iri: request.operation_id.clone(),
                    title: "Proposed code-to-knowledge associations".into(),
                    kind: LINK_REVIEW_KIND.into(),
                    links: response.links,
                    unanchored: vec![],
                    anchors: vec![],
                    anchor_notes: vec![],
                }],
            },
            reason: "Review the relevance of each derived association; a link records relevance, not proof of implementation.".into(),
        });
        self.task.pending_intent_links = None;
        self.task.phase = Phase::AwaitingReview;
        self.task.mode = Mode::Plan;
        self.task.approved_revision = None;
        // Set only by human steering during the review; an unsteered review
        // resumes verification.
        self.task.review_continuation = None;
        self.task.after_review = Phase::AwaitingPlan;
        self.task.final_capture = false;
        self.start_intent_cycle();
        self.persist()?;
        Ok(true)
    }

    pub(super) async fn abandon_pending_intent(&mut self, reason: &str) -> Result<()> {
        let Some(request) = self.task.pending_intent_links.clone() else {
            return Ok(());
        };
        // Resolve uncertainty at the owner of the durable graph operation.
        // A rejection tombstone prevents an in-flight request resurfacing later.
        let response: CheckpointResponse = self
            .post(
                "intent/abandon",
                &ReviewRequest {
                    operation_id: request.operation_id.clone(),
                    accept: false,
                },
            )
            .await?;
        anyhow::ensure!(
            response.durable && response.conforms && response.pending.is_empty(),
            "intent operation could not be durably abandoned; retry after the daemon recovers"
        );
        self.task.pending_intent_links = None;
        // The abandoned batch must be derived afresh at the next finish, or
        // the task could never satisfy its association gate again.
        if let Some(state) = self.task.symbolic.as_mut() {
            state.association = None;
        }
        self.intent_event(
            "intent_abandoned",
            &format!("{}: {}", request.operation_id, reason),
        );
        self.update_knowledge_revision(response.revision);
        self.persist()
    }

    pub(super) async fn review_intent_links(
        &mut self,
        position: usize,
        accept: bool,
        journal_interaction: bool,
    ) -> Result<()> {
        let request = self.task.reviews[position]
            .intent_links
            .clone()
            .context("no intent associations")?;
        let link_iris = self.task.reviews[position]
            .response
            .proposals
            .first()
            .context("intent review omitted its persisted response")?
            .links
            .clone();
        anyhow::ensure!(
            self.task.reviews[position].response.proposals.len() == 1
                && link_iris.len() == request.bindings.len(),
            "intent review response does not identify exactly one link per binding"
        );
        let response: CheckpointResponse = self
            .post(
                "intent/review",
                &ReviewRequest {
                    operation_id: request.operation_id.clone(),
                    accept,
                },
            )
            .await?;
        anyhow::ensure!(
            response.durable && response.conforms && response.pending.is_empty(),
            "intent review is not durably resolved"
        );
        self.task.reviews.remove(position);
        if journal_interaction {
            self.emit_review_interaction(accept, &request.operation_id);
        }
        for (binding, link_iri) in request.bindings.iter().zip(&link_iris) {
            self.intent_event(
                "link_review",
                &format!(
                    "{} {} for {} {} in {}",
                    if accept { "accepted" } else { "rejected" },
                    link_iri,
                    binding.record_iri,
                    binding.symbol,
                    request.operation_id
                ),
            );
        }
        if !accept {
            self.intent_event("intent_rejected", "human rejected derived associations");
        }
        self.resolve_symbolic_association(&request.operation_id);
        self.update_knowledge_revision(response.revision);
        if self.task.reviews.is_empty() {
            match self.task.review_continuation.take() {
                // Human steering during the review already returned the task
                // to Plan; the disposition is journaled and planning resumes
                // with that guidance instead of the pre-review approval.
                Some(phase) => self.task.phase = phase,
                None => {
                    // This task's association disposition changes the graph
                    // revision but does not invalidate its approved scope or
                    // owned source chain.
                    self.task.approved_revision = Some(self.task.knowledge_revision.clone());
                    if let Some(scope) = self.task.approved_change_scope.as_mut() {
                        scope.knowledge_revision = self.task.knowledge_revision.clone();
                    }
                    self.task.mode = Mode::Auto;
                    self.task.phase = Phase::Verifying;
                }
            }
            let files = self.task.plan.as_ref().context("no plan")?.files.clone();
            self.task.intent_refresh_pending = files;
        }
        // The daemon receipt, dispositions, and continuation are one durable
        // local commit. Context refresh may fail and is retried independently.
        self.persist()?;
        self.resume_intent_refresh().await
    }

    pub(super) async fn resume_intent_refresh(&mut self) -> Result<()> {
        if self.task.intent_refresh_pending.is_empty() {
            return Ok(());
        }
        let files = self.task.intent_refresh_pending.clone();
        self.refresh(&files).await?;
        self.task.intent_refresh_pending.clear();
        self.persist()
    }
}
