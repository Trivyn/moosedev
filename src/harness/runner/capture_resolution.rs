//! Durable semantic reconciliation for the version-2 capture contract.
use super::{bounded, HttpFailure, ReviewItem, Runner};
use crate::harness::protocol::{CaptureRequest, CaptureResponse, KnowledgeProposal};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResolution {
    pub original_proposal: KnowledgeProposal,
    pub evidence_page: Value,
    pub candidate_pages: Vec<Value>,
    pub operation_id: String,
    #[serde(default)]
    pub attempts: usize,
    #[serde(default)]
    pub rejected_candidate_iris: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<ResolutionDisposition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureBatch {
    pub proposals: Vec<KnowledgeProposal>,
    pub evidence_page: Value,
    pub index: usize,
    pub outgoing: Vec<KnowledgeProposal>,
    pub reconciliation_operations: Vec<String>,
    #[serde(default)]
    pub member_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRevision {
    pub reconciliation_operation_id: String,
    pub origin_operation_id: String,
    pub replacement: KnowledgeProposal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionDisposition {
    pub choice: String,
    pub rationale: String,
    pub candidate_iri: String,
    pub candidate_title: String,
    pub candidate_claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_proposal: Option<KnowledgeProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionReview {
    pub operation_id: String,
    pub candidate_iri: String,
    pub candidate_title: String,
    #[serde(default)]
    pub original_kind: String,
    #[serde(default)]
    pub candidate_kind: String,
    pub original_claim: String,
    pub existing_claim: String,
    pub rationale: String,
    pub reuse_unchanged: bool,
    pub recommendation_source: String,
    pub original_proposal: KnowledgeProposal,
    pub evidence_page: Value,
    pub candidate_pages: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Judgment {
    disposition: String,
    candidate_id: String,
    rationale: String,
    #[serde(default)]
    revised_title: Option<String>,
    #[serde(default)]
    revised_description: Option<String>,
}

impl Runner {
    pub(super) fn capture_v2(&self) -> bool {
        self.task.capture_contract >= 2
    }

    pub(super) async fn reconcile_proposals(
        &mut self,
        proposals: Vec<KnowledgeProposal>,
        evidence_page: Value,
    ) -> Result<Vec<KnowledgeProposal>> {
        if self.task.capture_batch.is_none() {
            let member_operation_ids = (0..proposals.len())
                .map(|_| uuid::Uuid::new_v4().to_string())
                .collect();
            self.task.capture_batch = Some(CaptureBatch {
                proposals,
                evidence_page,
                index: 0,
                outgoing: vec![],
                reconciliation_operations: vec![],
                member_operation_ids,
            });
            self.persist()?;
        }
        while self.task.capture_batch.as_ref().unwrap().index
            < self.task.capture_batch.as_ref().unwrap().proposals.len()
        {
            let batch = self.task.capture_batch.as_ref().unwrap();
            let proposal = batch.proposals[batch.index].clone();
            let evidence = batch.evidence_page.clone();
            let operation = batch.member_operation_ids[batch.index].clone();
            let (resolved, has_receipt) = self
                .reconcile_proposal(proposal, evidence, operation.clone())
                .await?;
            let batch = self.task.capture_batch.as_mut().unwrap();
            if let Some(proposal) = resolved {
                batch.outgoing.push(proposal);
            }
            if has_receipt && !batch.reconciliation_operations.contains(&operation) {
                batch.reconciliation_operations.push(operation);
            }
            batch.index += 1;
            self.task.capture_resolution = None;
            self.persist()?;
        }
        Ok(self.task.capture_batch.as_ref().unwrap().outgoing.clone())
    }

    async fn reconcile_proposal(
        &mut self,
        proposal: KnowledgeProposal,
        evidence_page: Value,
        operation_id: String,
    ) -> Result<(Option<KnowledgeProposal>, bool)> {
        if self.task.capture_resolution.is_none() {
            self.task.capture_resolution = Some(CaptureResolution {
                original_proposal: proposal.clone(),
                evidence_page,
                candidate_pages: vec![],
                operation_id,
                attempts: 0,
                rejected_candidate_iris: vec![],
                disposition: None,
            });
            self.persist()?;
        }
        if self
            .task
            .capture_resolution
            .as_ref()
            .unwrap()
            .candidate_pages
            .is_empty()
        {
            let body = json!({"owner_id":self.task.id,"proposal":proposal,"topic":proposal.title,"cursor":null,"limit":5});
            let page: Value = self.post("capture/candidates", &body).await?;
            self.task
                .capture_resolution
                .as_mut()
                .unwrap()
                .candidate_pages
                .push(page);
            self.persist()?;
        }
        let state = self.task.capture_resolution.as_ref().unwrap().clone();
        let candidates = merged_candidates(&state.candidate_pages);
        if candidates.is_empty() {
            self.task.capture_resolution = None;
            self.persist()?;
            return Ok((Some(proposal), false));
        }
        anyhow::ensure!(state.attempts < 3, "capture reconciliation failed validation after three attempts; provide human guidance before retrying");
        let indexed: Vec<Value> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"id":format!("c{i}"),"candidate":c}))
            .collect();
        let prompt = format!("Choose the semantic relationship between the original proposed knowledge and one supplied existing candidate. Choose reuse_unchanged only when the claim and every intended relationship are already present unchanged and the human has not rejected that pairing. Kinds may differ because equivalence is semantic, but a kind mismatch is material and the rationale must address it explicitly. Choose revise_proposal only for an owned pending candidate. Choose distinct_knowledge when both claims must remain distinct. For an exact-title collision, return a distinct revised title; otherwise the original title may remain. Candidate retrieval is not proof. Never append, drop, or rewrite assertions or evidence silently.\nHuman-rejected reuse candidates: {}\nOriginal proposal kind: {}\nOriginal proposal:\n{}\nOriginal evidence page:\n{}\nCandidates (each includes its explicit kind, complete claims, lifecycle, ownership, relationships and legal relation catalogue):\n{}", serde_json::to_string(&state.rejected_candidate_iris)?, proposal.kind, serde_json::to_string(&proposal)?, serde_json::to_string(&state.evidence_page)?, serde_json::to_string(&indexed)?);
        let schema = resolution_schema(&indexed);
        let required = prompt.len() + serde_json::to_string(&schema)?.len() + 64;
        if required > self.prompt_budget()? {
            if candidates.len() > 1 {
                self.task
                    .capture_resolution
                    .as_mut()
                    .unwrap()
                    .candidate_pages[0]["candidates"]
                    .as_array_mut()
                    .unwrap()
                    .truncate(1);
                self.persist()?;
                return Box::pin(self.reconcile_proposal(
                    proposal,
                    state.evidence_page,
                    state.operation_id,
                ))
                .await;
            }
            let candidate = &candidates[0];
            anyhow::ensure!(candidate["status"] == "accepted" || candidate["owned_by_requester"] == true,
                "the complete external pending candidate exceeds the model context and cannot be imported or accepted automatically");
            let candidate_iri = candidate["iri"]
                .as_str()
                .context("candidate omitted iri")?
                .to_owned();
            if state.rejected_candidate_iris.contains(&candidate_iri) {
                // The human already declined this pairing; asking again with
                // a new card is the loop, not a review. Keep the observation
                // unresolved and move on.
                self.intent_event(
                    "reuse_unresolved",
                    &format!(
                        "{} rejected oversized candidate {candidate_iri}",
                        state.operation_id
                    ),
                );
                self.event(format!(
                    "Observation retained unresolved: the only reuse candidate {candidate_iri} exceeds the model context and was already rejected."
                ));
                self.task.capture_resolution = None;
                self.persist()?;
                return Ok((None, false));
            }
            let candidate_title = candidate["title"].as_str().unwrap_or_default().to_owned();
            let rationale = "The complete candidate exceeds the configured model context; human semantic determination is required from the complete review card.".to_string();
            let request = json!({"operation_id":state.operation_id,"owner_id":self.task.id,"proposal":proposal,"candidate_iri":candidate_iri,"candidate_digest":candidate["assertion_digest"],"candidate_revision":state.candidate_pages[0]["revision"],"disposition":"reuse_unchanged","rationale":rationale,"replacement_proposal":null});
            let receipt: crate::harness::protocol::ReconcileCaptureResponse =
                self.post("capture/reconcile", &request).await?;
            anyhow::ensure!(
                receipt.operation_id == state.operation_id
                    && receipt.candidate_iri == candidate_iri,
                "daemon returned a mismatched capture reconciliation receipt"
            );
            let review = ResolutionReview {
                operation_id: state.operation_id.clone(),
                candidate_iri,
                candidate_title,
                original_kind: proposal.kind.clone(),
                candidate_kind: candidate["kind"].as_str().unwrap_or_default().to_owned(),
                original_claim: proposal.description.clone(),
                existing_claim: candidate_claim(candidate),
                rationale,
                reuse_unchanged: true,
                recommendation_source: "human_required".into(),
                original_proposal: proposal.clone(),
                evidence_page: state.evidence_page.clone(),
                candidate_pages: state.candidate_pages.clone(),
            };
            self.task.reviews.push(ReviewItem { intent_links: None, capture_resolution: Some(review), request: CaptureRequest { operation_id: state.operation_id, proposals: vec![] }, response: CaptureResponse { proposals: vec![] }, reason: "The full candidate is too large for model judgment. Accept only if the proposed claim is represented unchanged; reject to keep the observation unresolved.".into() });
            self.intent_event(
                "reuse_candidate",
                "oversized candidate requires human semantic determination",
            );
            self.persist()?;
            return Ok((None, false));
        }
        // A persisted disposition means the model already judged this
        // operation and the daemon call was interrupted: replay it verbatim so
        // the daemon sees the identical reconciliation request.
        let (disposition, candidate) = if let Some(disposition) = state.disposition.clone() {
            let candidate = candidates
                .iter()
                .find(|candidate| {
                    candidate["iri"].as_str() == Some(disposition.candidate_iri.as_str())
                })
                .context("persisted reconciliation disposition names an unknown candidate")?;
            self.event(format!(
                "Replaying the persisted reconciliation disposition for {} after an interrupted daemon call.",
                state.operation_id
            ));
            (disposition, candidate)
        } else {
            let judgment: Judgment = self
                .model_json(&prompt, "harness_capture_resolution", schema)
                .await?;
            self.task.capture_resolution.as_mut().unwrap().attempts += 1;
            self.persist()?;
            anyhow::ensure!(
                !judgment.rationale.trim().is_empty(),
                "capture reconciliation rationale is required"
            );
            let index = judgment
                .candidate_id
                .strip_prefix('c')
                .and_then(|v| v.parse::<usize>().ok())
                .context("unknown capture candidate")?;
            let candidate = candidates.get(index).context("unknown capture candidate")?;
            anyhow::ensure!(
                matches!(
                    judgment.disposition.as_str(),
                    "reuse_unchanged" | "revise_proposal" | "distinct_knowledge"
                ),
                "unknown capture disposition"
            );
            let exact_title = candidate["exact_title"] == true;
            let needs_revision = judgment.disposition == "revise_proposal"
                || (judgment.disposition == "distinct_knowledge" && exact_title);
            if needs_revision {
                anyhow::ensure!(
                    judgment
                        .revised_title
                        .as_ref()
                        .is_some_and(|v| !v.trim().is_empty())
                        && judgment
                            .revised_description
                            .as_ref()
                            .is_some_and(|v| !v.trim().is_empty()),
                    "revised title and description are required"
                );
            }
            let revised = if needs_revision {
                let mut revised = proposal.clone();
                revised.title = judgment.revised_title.clone().unwrap();
                revised.description = judgment.revised_description.clone().unwrap();
                Some(revised)
            } else if judgment.disposition == "distinct_knowledge" {
                Some(proposal.clone())
            } else {
                None
            };
            let candidate_iri = candidate["iri"]
                .as_str()
                .context("candidate omitted iri")?
                .to_owned();
            anyhow::ensure!(
                judgment.disposition != "reuse_unchanged"
                    || !state.rejected_candidate_iris.contains(&candidate_iri),
                "human rejected reuse of this candidate; choose a distinct legal disposition"
            );
            let candidate_title = candidate["title"].as_str().unwrap_or_default().to_owned();
            if judgment.disposition == "distinct_knowledge" && exact_title {
                let revised = revised.as_ref().unwrap();
                anyhow::ensure!(
                    revised.title.trim().to_lowercase() != candidate_title.trim().to_lowercase(),
                    "reconciled replacement title must be distinct from the candidate title"
                );
            }
            if judgment.disposition == "revise_proposal" {
                anyhow::ensure!(
                    candidate["status"] == "proposed" && candidate["owned_by_requester"] == true,
                    "revise_proposal is legal only for an owned pending candidate"
                );
            }
            let candidate_claim = candidate_claim(candidate);
            let disposition = ResolutionDisposition {
                choice: judgment.disposition.clone(),
                rationale: judgment.rationale.clone(),
                candidate_iri: candidate_iri.clone(),
                candidate_title: candidate_title.clone(),
                candidate_claim: candidate_claim.clone(),
                revised_proposal: revised.clone(),
            };
            self.task.capture_resolution.as_mut().unwrap().disposition = Some(disposition.clone());
            self.persist()?;
            (disposition, candidate)
        };
        let candidate_iri = disposition.candidate_iri.clone();
        let candidate_title = disposition.candidate_title.clone();
        let candidate_claim = disposition.candidate_claim.clone();
        let request = json!({"operation_id":state.operation_id,"owner_id":self.task.id,"proposal":proposal,"candidate_iri":candidate_iri,"candidate_digest":candidate["assertion_digest"],"candidate_revision":state.candidate_pages[0]["revision"],"disposition":disposition.choice.clone(),"rationale":disposition.rationale.clone(),"replacement_proposal":disposition.revised_proposal.clone()});
        let response: crate::harness::protocol::ReconcileCaptureResponse =
            self.post("capture/reconcile", &request).await?;
        anyhow::ensure!(
            response.operation_id == state.operation_id && response.candidate_iri == candidate_iri,
            "daemon returned a mismatched capture reconciliation receipt"
        );
        match disposition.choice.as_str() {
            "reuse_unchanged" => {
                let originating = response.pending_capture_operation.as_deref();
                let review = ResolutionReview {
                    operation_id: state.operation_id.clone(),
                    candidate_iri: disposition.candidate_iri,
                    candidate_title: disposition.candidate_title,
                    original_kind: proposal.kind.clone(),
                    candidate_kind: candidate["kind"].as_str().unwrap_or_default().to_owned(),
                    original_claim: proposal.description.clone(),
                    existing_claim: candidate_claim,
                    rationale: disposition.rationale,
                    reuse_unchanged: true,
                    recommendation_source: "model".into(),
                    original_proposal: proposal.clone(),
                    evidence_page: state.evidence_page.clone(),
                    candidate_pages: state.candidate_pages.clone(),
                };
                let _owned_pending_review = originating.and_then(|id| {
                    self.task
                        .reviews
                        .iter()
                        .find(|r| r.request.operation_id == id)
                });
                if !self.task.reviews.iter().any(|item| {
                    item.capture_resolution
                        .as_ref()
                        .is_some_and(|item| item.operation_id == state.operation_id)
                }) {
                    self.task.reviews.push(ReviewItem { intent_links: None, capture_resolution: Some(review), request: CaptureRequest { operation_id: state.operation_id, proposals: vec![] }, response: CaptureResponse { proposals: vec![] }, reason: "Confirm that the proposed claim is already represented unchanged; rejected additions and differences remain visible below.".into() });
                }
                self.intent_event(
                    "reuse_candidate",
                    &format!("{} matches {}", proposal.title, candidate_title),
                );
                Ok((None, false))
            }
            "revise_proposal" => {
                let origin_operation_id = response
                    .pending_capture_operation
                    .context("owned pending revision omitted its originating capture operation")?;
                anyhow::ensure!(
                    self.task
                        .reviews
                        .iter()
                        .any(|review| review.request.operation_id == origin_operation_id
                            && review.capture_resolution.is_none()),
                    "originating owned capture batch is not available for whole-operation review"
                );
                self.task.pending_revision = Some(PendingRevision {
                    reconciliation_operation_id: state.operation_id,
                    origin_operation_id: origin_operation_id.clone(),
                    replacement: disposition
                        .revised_proposal
                        .context("missing durable revised proposal")?,
                });
                self.task.phase = super::Phase::AwaitingReview;
                self.event(format!("Revision is paused until the complete originating capture operation {origin_operation_id} is rejected."));
                self.persist()?;
                Ok((None, true))
            }
            _ => Ok((disposition.revised_proposal, true)),
        }
    }

    pub(super) async fn review_capture_resolution(
        &mut self,
        position: usize,
        accept: bool,
        emit_interaction: bool,
    ) -> Result<()> {
        let review = self.task.reviews[position]
            .capture_resolution
            .clone()
            .context("missing capture resolution review")?;
        let response = self
            .http
            .post(format!(
                "{}/api/v1/harness/capture/reconcile/review",
                self.daemon
            ))
            .json(&json!({"operation_id":review.operation_id,"accept":accept}))
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(HttpFailure {
                status: status.as_u16(),
                message: format!("capture reconciliation review: {}", bounded(&text, 4000)),
            }
            .into());
        }
        let receipt: crate::harness::protocol::ReconcileCaptureResponse =
            serde_json::from_str(&text)?;
        anyhow::ensure!(
            receipt.operation_id == review.operation_id
                && receipt.candidate_iri == review.candidate_iri
                && receipt.review == Some(accept),
            "daemon returned a mismatched reconciliation review receipt"
        );
        self.task.reviews.remove(position);
        if emit_interaction {
            self.emit_review_interaction(accept, &review.operation_id);
        }
        self.intent_event(
            "reuse_review",
            &format!(
                "{} {}",
                if accept { "accepted" } else { "rejected" },
                review.operation_id
            ),
        );
        self.event(format!(
            "Human {} semantic reuse assessment {}.",
            if accept { "accepted" } else { "rejected" },
            review.operation_id
        ));
        if !accept && review.recommendation_source == "human_required" {
            // The card promised "reject to keep the observation unresolved".
            // The proposal was never outgoing and the batch already moved past
            // it, so there is nothing to restart: re-reconciling would meet
            // the same oversized candidate and mint an unbounded chain of
            // fresh cards. Record the disposition and continue.
            self.intent_event("reuse_unresolved", &review.operation_id);
            self.event(format!(
                "Observation retained unresolved after the human rejected the oversized reuse candidate {}.",
                review.candidate_iri
            ));
        } else if !accept {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let proposal = review.original_proposal.clone();
            let evidence_page = review.evidence_page.clone();
            self.task.capture_batch = Some(CaptureBatch {
                proposals: vec![proposal.clone()],
                evidence_page: evidence_page.clone(),
                index: 0,
                outgoing: vec![],
                reconciliation_operations: vec![],
                member_operation_ids: vec![operation_id.clone()],
            });
            // Restart from an empty page set: the judged pages carry the
            // revision the review was taken at, and any sibling review in the
            // same batch has already moved the project revision past it. The
            // human-facing review item keeps its own copy of what was judged.
            self.task.capture_resolution = Some(CaptureResolution {
                original_proposal: proposal,
                evidence_page,
                candidate_pages: vec![],
                operation_id,
                attempts: 0,
                rejected_candidate_iris: vec![review.candidate_iri.clone()],
                disposition: None,
            });
            self.task.capture_due = true;
            self.task.capture_end = None;
            self.task.phase = self.capture_work_phase();
            return self.persist();
        }
        if self.awaiting_v2_missing_capture() {
            if !self.task.reviews.is_empty() {
                self.task.phase = super::Phase::AwaitingReview;
                return self.persist();
            }
            if self.task.capture_due {
                self.task.phase = self.capture_work_phase();
                return self.persist();
            }
            self.persist()?;
            if self.finish_v2_missing_capture_if_quiescent().await? {
                return Ok(());
            }
        }
        if self.task.reviews.is_empty() && self.task.phase == super::Phase::AwaitingReview {
            if self.task.final_capture {
                return self.finish().await;
            }
            self.task.phase = self
                .task
                .review_continuation
                .take()
                .unwrap_or(self.task.after_review);
        }
        self.persist()
    }
}

fn candidate_claim(candidate: &Value) -> String {
    serde_json::to_string_pretty(candidate).unwrap_or_else(|_| candidate.to_string())
}

fn merged_candidates(pages: &[Value]) -> Vec<Value> {
    let mut merged: Vec<Value> = vec![];
    for candidate in pages
        .iter()
        .flat_map(|page| page["candidates"].as_array().into_iter().flatten())
    {
        if let Some(existing) = merged
            .iter_mut()
            .find(|item| item["iri"] == candidate["iri"])
        {
            for field in ["literals", "relations", "legal_relations"] {
                if let (Some(target), Some(values)) =
                    (existing[field].as_array_mut(), candidate[field].as_array())
                {
                    for value in values {
                        if !target.contains(value) {
                            target.push(value.clone());
                        }
                    }
                }
            }
            existing["assertions_complete"] = candidate["assertions_complete"].clone();
            existing["next_assertion_cursor"] = candidate["next_assertion_cursor"].clone();
        } else {
            merged.push(candidate.clone());
        }
    }
    merged
}

fn resolution_schema(candidates: &[Value]) -> Value {
    let ids: Vec<_> = candidates.iter().filter_map(|c| c["id"].as_str()).collect();
    json!({"type":"object","additionalProperties":false,"required":["disposition","candidate_id","rationale","revised_title","revised_description"],"properties":{"disposition":{"type":"string","enum":["reuse_unchanged","revise_proposal","distinct_knowledge"]},"candidate_id":{"type":"string","enum":ids},"rationale":{"type":"string"},"revised_title":{"type":["string","null"]},"revised_description":{"type":["string","null"]}}})
}
