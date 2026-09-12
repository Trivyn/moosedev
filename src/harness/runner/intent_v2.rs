//! Incremental purpose and post-edit association state for change-level-v2.
//!
//! Daemon pages are persisted before semantic selection. Existing intent/link
//! remains the only graph-write path.
use super::*;
use crate::harness::protocol::{
    ChangedFile, HarnessSourcePosition, HarnessSourceRange, IntentCandidatePage,
    IntentCandidateRequest, IntentIndexStatus, IntentRefreshPolicy, IntentScopeBasis,
    PurposeCandidatePage, PurposeCandidateRequest, PurposeRetrieval,
};
use serde_json::json;
use std::collections::BTreeSet;

/// A controller-state invariant, as opposed to model output that failed
/// validation. The `ControllerInvariant` context lets `advance` classify the
/// failure by type rather than by message text.
macro_rules! invariant {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            return Err(anyhow::anyhow!($msg).context(super::model::ControllerInvariant));
        }
    };
}

mod probe;
pub use probe::{
    probe_intent_contracts, IntentContractProbeAttempt, IntentContractProbeReceipt,
    IntentContractProbeUsage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurposeDecision {
    pub iri: String,
    pub assertion_digest: String,
    pub role: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurposeSelectionState {
    pub version: u8,
    pub request: PurposeCandidateRequest,
    pub revision: String,
    pub plan_scope_digest: String,
    pub current_page: PurposeCandidatePage,
    #[serde(default)]
    pub pages: Vec<PurposeCandidatePage>,
    pub cursor: Option<String>,
    pub selected: Vec<PurposeDecision>,
    #[serde(default)]
    pub rejected_iris: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedChangeScope {
    pub version: u8,
    pub plan_digest: String,
    pub knowledge_revision: String,
    pub files: BTreeMap<String, Option<String>>,
    pub purpose_iris: Vec<String>,
    pub obligation_iris: Vec<String>,
    #[serde(default)]
    pub selected_records: Vec<PurposeDecision>,
    #[serde(default)]
    pub definition_scopes: Vec<ApprovedDefinitionScope>,
    pub checks: Vec<String>,
    pub approval_cycle: String,
    /// Symbolic policy only: the plan summary standing in for a purpose record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedDefinitionScope {
    pub file: String,
    pub symbol: String,
    pub source_digest: String,
    pub purpose_iris: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociationDecision {
    pub candidate_digest: String,
    pub selected_record_iris: Vec<String>,
    #[serde(default)]
    pub selected_assertion_digests: BTreeMap<String, String>,
    pub rationale: String,
    pub disposition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostEditAssociationState {
    pub version: u8,
    pub request: IntentCandidateRequest,
    pub current_page: IntentCandidatePage,
    #[serde(default)]
    pub pages: Vec<IntentCandidatePage>,
    pub cursor: Option<String>,
    pub decisions: Vec<AssociationDecision>,
    #[serde(default)]
    pub proposed_bindings: Vec<crate::harness::daemon::intent::IntentBinding>,
    #[serde(default)]
    pub rejected_bindings: Vec<crate::harness::daemon::intent::IntentBinding>,
    pub unresolved: Vec<crate::harness::protocol::IntentCandidateUnresolved>,
    pub pending_link_operation_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditScopeAssessment {
    pub id: String,
    pub file: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub edit_digest: String,
    pub knowledge_revision: String,
    pub disposition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<IntentCandidateRequest>,
    #[serde(default)]
    pub pages: Vec<IntentCandidatePage>,
    #[serde(default)]
    pub candidate_scope_digests: Vec<String>,
    #[serde(default)]
    pub delivered_governing_records: Vec<String>,
}

impl EditScopeAssessment {
    pub(crate) fn matches_edit(&self, edit: &PendingEdit) -> bool {
        self.knowledge_revision == edit.revision
            && digest_json(&json!({
                "file": edit.file,
                "before": edit.before,
                "after": edit.after,
            }))
            .is_ok_and(|digest| digest == self.edit_digest)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum PurposeChoice {
    Select {
        #[serde(rename = "record_handle", alias = "candidate_handle")]
        candidate_handle: String,
        role: String,
        rationale: String,
    },
    Skip {
        #[serde(rename = "record_handle", alias = "candidate_handle")]
        candidate_handle: String,
        rationale: String,
    },
    NextPage {
        rationale: String,
    },
    Missing {
        grounded_reason: String,
    },
    Done {
        rationale: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
enum AssociationChoice {
    Associate {
        record_handles: Vec<String>,
        rationale: String,
    },
    NoneApplies {
        rationale: String,
    },
}

pub(super) fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(hash(&serde_json::to_string(value)?))
}

fn purpose_schema(handles: &[String], has_next_page: bool, has_selected_purpose: bool) -> Value {
    let mut choices = vec![];
    if !handles.is_empty() {
        choices.push(json!({"type":"object","additionalProperties":false,"required":["decision","record_handle","role","rationale"],"properties":{"decision":{"const":"select"},"record_handle":{"enum":handles},"role":{"enum":["purpose","obligation"]},"rationale":{"type":"string","minLength":1,"maxLength":2000}}}));
        choices.push(json!({"type":"object","additionalProperties":false,"required":["decision","record_handle","rationale"],"properties":{"decision":{"const":"skip"},"record_handle":{"enum":handles},"rationale":{"type":"string","minLength":1,"maxLength":2000}}}));
    }
    if has_next_page {
        choices.push(json!({"type":"object","additionalProperties":false,"required":["decision","rationale"],"properties":{"decision":{"const":"next_page"},"rationale":{"type":"string","minLength":1,"maxLength":2000}}}));
    }
    if !has_next_page {
        choices.push(json!({"type":"object","additionalProperties":false,"required":["decision","grounded_reason"],"properties":{"decision":{"const":"missing"},"grounded_reason":{"type":"string","minLength":1,"maxLength":2000}}}));
    }
    if has_selected_purpose {
        choices.push(json!({"type":"object","additionalProperties":false,"required":["decision","rationale"],"properties":{"decision":{"const":"done"},"rationale":{"type":"string","minLength":1,"maxLength":2000}}}));
    }
    json!({"oneOf": choices})
}

fn association_schema(handles: &[String]) -> Value {
    json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["decision","record_handles","rationale"],"properties":{"decision":{"const":"associate"},"record_handles":{"type":"array","minItems":1,"maxItems":16,"items":{"enum":handles}},"rationale":{"type":"string","minLength":1,"maxLength":2000}}},
        {"type":"object","additionalProperties":false,"required":["decision","rationale"],"properties":{"decision":{"const":"none_applies"},"rationale":{"type":"string","minLength":1,"maxLength":2000}}}
    ]})
}

fn association_prompt(
    candidate: &crate::harness::protocol::PostEditCandidate,
    choices: &[crate::harness::protocol::PurposeCandidate],
) -> Result<String> {
    let mut identity = serde_json::to_value(candidate)?;
    identity
        .as_object_mut()
        .and_then(|object| object.remove("record_choices"))
        .context("post-edit candidate serialization omitted record_choices")?;
    Ok(format!(
        "For this proven current indexed post-edit entity, select only supplied current records whose relevance should be proposed for human review, or explicitly choose none_applies. Existing associations are already satisfied. A link records relevance, not implementation correctness. Do not author symbols, files, predicates, ranges, or IRIs.\nCandidate:\n{}\nRecord choices with complete claims/lifecycle/legal predicates:\n{}",
        serde_json::to_string(&identity)?,
        serde_json::to_string(choices)?,
    ))
}

fn validate_association_handles(
    handles: &[String],
    choices: &[crate::harness::protocol::PurposeCandidate],
) -> Result<Vec<String>> {
    anyhow::ensure!(
        !handles.is_empty() && handles.len() <= 16,
        "association must select 1..16 supplied record handles"
    );
    let mut seen = BTreeSet::new();
    let mut selected = Vec::with_capacity(handles.len());
    for handle in handles {
        anyhow::ensure!(
            seen.insert(handle.as_str()),
            "association selected duplicate record handle `{handle}`"
        );
        let record = choices
            .iter()
            .find(|record| record.handle == *handle)
            .with_context(|| format!("association selected unknown supplied handle `{handle}`"))?;
        selected.push(record.iri.clone());
    }
    Ok(selected)
}

impl Runner {
    pub(super) fn uses_v2_intent(&self) -> bool {
        self.task.intent_policy == IntentPolicy::ChangeLevelV2
    }

    pub(super) fn uses_postedit_associations(&self) -> bool {
        self.task.postedit_association_contract == 1
    }

    pub(super) fn awaiting_v2_missing_capture(&self) -> bool {
        self.task
            .purpose_selection
            .as_ref()
            .is_some_and(|state| state.status == "awaiting_missing_capture")
    }

    pub(super) fn schedule_v2_missing_capture(&mut self) {
        self.task.capture_due = true;
        self.task.after_review = Phase::AwaitingPlan;
        self.task.phase = self.capture_work_phase();
    }

    pub(super) fn normalize_v2_missing_capture_resume(&mut self) -> bool {
        if self.awaiting_v2_missing_capture()
            && self.task.capture_due
            && self.task.phase == Phase::AwaitingPlan
        {
            self.task.after_review = Phase::AwaitingPlan;
            self.task.phase = self.capture_work_phase();
            self.intent_event(
                "purpose_missing_resume",
                "resumed the durable missing-purpose capture checkpoint",
            );
            return true;
        }
        false
    }

    fn v2_missing_capture_quiescent(&self) -> bool {
        self.awaiting_v2_missing_capture()
            && !self.task.capture_due
            && self.task.capture_checkpoint_end.is_none()
            && self.task.capture_request.is_none()
            && self.task.capture_batch.is_none()
            && self.task.capture_resolution.is_none()
            && self.task.pending_capture.is_none()
            && self.task.pending_revision.is_none()
            && self.task.reviews.is_empty()
    }

    pub(super) async fn finish_v2_missing_capture_if_quiescent(&mut self) -> Result<bool> {
        if !self.v2_missing_capture_quiescent() {
            return Ok(false);
        }
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let files = plan.files.clone();
        self.refresh(&files).await?;
        let plan_scope_digest = digest_json(&json!({
            "summary": plan.summary,
            "files": plan.files,
            "checks": plan.checks,
            "snapshots": self.task.snapshots,
        }))?;
        let request = PurposeCandidateRequest {
            objective: self.task.objective.clone(),
            files,
            cursor: None,
            limit: Some(8),
        };
        let page: PurposeCandidatePage = self.post("intent/purpose/candidates", &request).await?;
        invariant!(
            page.revision == self.task.knowledge_revision,
            "knowledge changed while completing missing-purpose capture"
        );
        let exhausted = match page.retrieval {
            PurposeRetrieval::ExhaustedEmpty => {
                invariant!(
                    page.candidates.is_empty() && page.next_cursor.is_none(),
                    "exhausted purpose lookup returned candidates or a continuation cursor"
                );
                true
            }
            PurposeRetrieval::Page => {
                invariant!(
                    !page.candidates.is_empty(),
                    "purpose page is empty without an exhausted-empty proof"
                );
                false
            }
        };
        let cursor = page.next_cursor.clone();
        self.task.purpose_selection = Some(PurposeSelectionState {
            version: 1,
            request,
            revision: page.revision.clone(),
            plan_scope_digest,
            current_page: page.clone(),
            pages: vec![page],
            cursor,
            selected: vec![],
            rejected_iris: vec![],
            status: if exhausted {
                "awaiting_missing_capture".into()
            } else {
                "selecting".into()
            },
            attempts: 0,
        });
        self.task.approved_revision = None;
        self.task.approved_change_scope = None;
        self.task.mode = Mode::Plan;
        self.task.phase = Phase::AwaitingPlan;
        self.task.after_review = Phase::AwaitingPlan;
        if exhausted {
            self.task.phase = Phase::AwaitingInput;
            self.task.last_response = "The missing-purpose capture checkpoint is complete, but the refreshed accepted-purpose inventory is empty. Provide governing knowledge or revise the plan; no edit is authorized.".into();
            self.intent_event(
                "purpose_missing_unresolved",
                &self.task.last_response.clone(),
            );
            self.persist()?;
            return Ok(true);
        }
        self.intent_event(
            "purpose_missing_refreshed",
            "capture checkpoint drained; selecting purpose from a fresh accepted snapshot",
        );
        self.persist()?;
        self.prepare_v2_purpose().await?;
        Ok(true)
    }

    pub(super) fn validate_intent_contract(&self, context: &ContextResponse) -> Result<()> {
        if self.uses_v2_intent() || self.uses_postedit_associations() {
            anyhow::ensure!(
                context
                    .intent_contracts
                    .as_ref()
                    .is_some_and(|versions| versions.contains(&2)),
                "project daemon does not advertise intent contract v2; upgrade the daemon before using change-level-v2 or mandatory post-edit associations"
            );
        }
        anyhow::ensure!(
            !self.uses_v2_intent() || self.task.postedit_association_contract == 1,
            "change-level-v2 requires postedit_association_contract 1"
        );
        anyhow::ensure!(
            self.task.postedit_association_contract <= 1,
            "unsupported post-edit association contract"
        );
        Ok(())
    }

    pub(super) async fn prepare_v2_purpose(&mut self) -> Result<bool> {
        if !self.uses_v2_intent() {
            return Ok(true);
        }
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let plan_scope_digest = digest_json(&json!({
            "summary": plan.summary,
            "files": plan.files,
            "checks": plan.checks,
            "snapshots": self.task.snapshots,
        }))?;
        let needs_new = self
            .task
            .purpose_selection
            .as_ref()
            .is_none_or(|state| state.plan_scope_digest != plan_scope_digest);
        if needs_new {
            let request = PurposeCandidateRequest {
                objective: self.task.objective.clone(),
                files: plan.files.clone(),
                cursor: None,
                limit: Some(8),
            };
            let page: PurposeCandidatePage =
                self.post("intent/purpose/candidates", &request).await?;
            invariant!(
                page.revision == self.task.knowledge_revision,
                "knowledge changed during purpose lookup"
            );
            self.task.purpose_selection = Some(PurposeSelectionState {
                version: 1,
                request,
                revision: page.revision.clone(),
                plan_scope_digest: plan_scope_digest.clone(),
                current_page: page.clone(),
                pages: vec![page.clone()],
                cursor: page.next_cursor.clone(),
                selected: vec![],
                rejected_iris: vec![],
                status: "selecting".into(),
                attempts: 0,
            });
            self.intent_event(
                "purpose_candidates",
                "persisted initial bounded purpose page",
            );
            self.persist()?;
        }
        loop {
            let state = self.task.purpose_selection.as_ref().unwrap().clone();
            if state.status == "ready" {
                return Ok(true);
            }
            if state.status == "awaiting_missing_capture" {
                return Ok(false);
            }
            invariant!(
                state.version == 1
                    && state.revision == self.task.knowledge_revision
                    && state.plan_scope_digest == plan_scope_digest,
                "purpose selection snapshot is stale"
            );
            let available: Vec<_> = state
                .current_page
                .candidates
                .iter()
                .filter(|candidate| {
                    !state.selected.iter().any(|item| item.iri == candidate.iri)
                        && !state.rejected_iris.contains(&candidate.iri)
                })
                .cloned()
                .collect();
            if available.is_empty() {
                if let Some(cursor) = state.cursor.clone() {
                    let mut request = state.request.clone();
                    request.cursor = Some(cursor);
                    let page: PurposeCandidatePage =
                        self.post("intent/purpose/candidates", &request).await?;
                    anyhow::ensure!(
                        page.revision == state.revision,
                        "purpose page revision changed"
                    );
                    let selection = self.task.purpose_selection.as_mut().unwrap();
                    selection.request = request;
                    selection.cursor = page.next_cursor.clone();
                    selection.pages.push(page.clone());
                    selection.current_page = page;
                    selection.attempts = 0;
                    self.persist()?;
                    continue;
                }
            }
            let handles: Vec<_> = available.iter().map(|c| c.handle.clone()).collect();
            let prompt = format!(
                "Choose one of the JSON schema's currently allowed decisions. A selected accepted/current record may serve as purpose or obligation based on relevance. Retrieval is nomination, not semantic relevance. Skipped Constraints remain applicable. Use missing only to declare grounded missing governing knowledge. Use only supplied handles.\nTask objective:\n{}\nPlan:\n{}\nRetained choices:\n{}\nCandidates with complete claims/lifecycle/relations:\n{}",
                self.task.objective,
                serde_json::to_string(&plan)?,
                serde_json::to_string(&state.selected)?,
                serde_json::to_string(&available)?
            );
            let schema = purpose_schema(
                &handles,
                state.cursor.is_some(),
                state.selected.iter().any(|item| item.role == "purpose"),
            );
            if super::model::json_request_bytes(&prompt, &schema)? > self.prompt_budget()? {
                self.task.phase = Phase::AwaitingInput;
                self.task.last_response = "One complete purpose candidate exceeds the configured model context. Human purpose guidance is required; the complete page remains in the journal.".into();
                self.intent_event("purpose_unresolved", &self.task.last_response.clone());
                self.persist()?;
                return Ok(false);
            }
            loop {
                let choice: PurposeChoice = self
                    .model_json(&prompt, "harness_purpose_selection", schema.clone())
                    .await?;
                if self.apply_purpose_choice(choice, &available)? {
                    self.candidate_accepted();
                    self.persist()?;
                    break;
                }
                self.task.purpose_selection.as_mut().unwrap().attempts += 1;
                let error = anyhow::anyhow!(
                    "purpose selection is invalid for the persisted candidate page and retained choices"
                )
                .context(super::model::InvalidModelOutput);
                if !self.repair_candidate(&error)? {
                    return Err(error);
                }
            }
        }
    }

    fn apply_purpose_choice(
        &mut self,
        choice: PurposeChoice,
        available: &[crate::harness::protocol::PurposeCandidate],
    ) -> Result<bool> {
        let state = self.task.purpose_selection.as_mut().unwrap();
        match choice {
            PurposeChoice::Select {
                candidate_handle,
                role,
                rationale,
            } => {
                let Some(candidate) = available.iter().find(|c| c.handle == candidate_handle)
                else {
                    return Ok(false);
                };
                if candidate.lifecycle != "accepted" {
                    return Ok(false);
                }
                if !matches!(role.as_str(), "purpose" | "obligation") {
                    return Ok(false);
                }
                state.selected.push(PurposeDecision {
                    iri: candidate.iri.clone(),
                    assertion_digest: candidate.assertion_digest.clone(),
                    role: role.clone(),
                    rationale,
                });
                state.attempts = 0;
                self.intent_event("purpose_selected", &format!("{role} {}", candidate.iri));
            }
            PurposeChoice::Skip {
                candidate_handle,
                rationale,
            } => {
                let Some(candidate) = available.iter().find(|c| c.handle == candidate_handle)
                else {
                    return Ok(false);
                };
                state.rejected_iris.push(candidate.iri.clone());
                state.attempts = 0;
                self.intent_event(
                    "purpose_skipped",
                    &format!("{}: {rationale}", candidate.iri),
                );
            }
            PurposeChoice::NextPage { rationale } => {
                if state.cursor.is_none() {
                    return Ok(false);
                }
                state
                    .rejected_iris
                    .extend(available.iter().map(|c| c.iri.clone()));
                state.attempts = 0;
                self.intent_event("purpose_next_page", &rationale);
            }
            PurposeChoice::Missing { grounded_reason } => {
                if state.cursor.is_some() {
                    return Ok(false);
                }
                state.status = "awaiting_missing_capture".into();
                state.attempts = 0;
                self.task.intent_missing_rounds = self.task.intent_missing_rounds.saturating_add(1);
                if self.task.intent_missing_rounds >= 3 {
                    self.task.capture_due = false;
                    self.task.phase = Phase::AwaitingInput;
                    self.task.after_review = Phase::AwaitingInput;
                    self.task.last_response = format!(
                        "{grounded_reason} Three semantic purpose-selection cycles still found no applicable governing record. Human guidance is required."
                    );
                    self.intent_event(
                        "purpose_missing_rounds_exhausted",
                        "three consecutive missing-purpose cycles; human guidance required",
                    );
                } else {
                    self.task.last_response = grounded_reason.clone();
                    self.schedule_v2_missing_capture();
                }
                self.intent_event("intent_missing", &grounded_reason);
            }
            PurposeChoice::Done { rationale } => {
                if !state.selected.iter().any(|item| item.role == "purpose") {
                    return Ok(false);
                }
                state.status = "ready".into();
                state.attempts = 0;
                self.task.intent_missing_rounds = 0;
                self.intent_event("purpose_selection_ready", &rationale);
            }
        }
        Ok(true)
    }

    pub(super) async fn approve_v2_scope(&mut self) -> Result<()> {
        if !self.uses_v2_intent() {
            return Ok(());
        }
        let selection = self
            .task
            .purpose_selection
            .as_ref()
            .context("purpose selection missing")?
            .clone();
        anyhow::ensure!(
            selection.status == "ready",
            "purpose selection is unresolved"
        );
        anyhow::ensure!(
            selection
                .selected
                .iter()
                .all(|selected| selection.pages.iter().any(|page| {
                    page.revision == selection.revision
                        && page.candidates.iter().any(|candidate| {
                            candidate.iri == selected.iri
                                && candidate.assertion_digest == selected.assertion_digest
                                && candidate.lifecycle == "accepted"
                        })
                })),
            "selected purpose lifecycle or assertion digest is no longer proven"
        );
        let plan = self.task.plan.as_ref().context("no plan")?.clone();
        let selected_iris: BTreeSet<_> = selection
            .selected
            .iter()
            .map(|selected| selected.iri.as_str())
            .collect();
        let resolved: crate::harness::daemon::intent::IntentResolveResponse = self
            .post(
                "intent/resolve",
                &crate::harness::daemon::intent::IntentResolveRequest {
                    files: plan.files.clone(),
                    refresh_index: false,
                },
            )
            .await?;
        anyhow::ensure!(
            resolved.revision == selection.revision,
            "definition scope knowledge snapshot changed"
        );
        let definition_scopes = resolved
            .entities
            .into_iter()
            .filter_map(|entity| {
                let purpose_iris: Vec<_> = entity
                    .dossier_records
                    .iter()
                    .filter(|iri| selected_iris.contains(iri.as_str()))
                    .cloned()
                    .collect();
                (!purpose_iris.is_empty()).then_some(ApprovedDefinitionScope {
                    file: entity.file,
                    symbol: entity.symbol,
                    source_digest: entity.source_digest,
                    purpose_iris,
                })
            })
            .collect();
        self.task.approved_change_scope = Some(ApprovedChangeScope {
            version: 1,
            plan_digest: selection.plan_scope_digest.clone(),
            knowledge_revision: selection.revision.clone(),
            files: self.task.snapshots.clone(),
            purpose_iris: selection
                .selected
                .iter()
                .filter(|item| item.role == "purpose")
                .map(|item| item.iri.clone())
                .collect(),
            obligation_iris: selection
                .selected
                .iter()
                .filter(|item| item.role == "obligation")
                .map(|item| item.iri.clone())
                .collect(),
            selected_records: selection.selected.clone(),
            definition_scopes,
            checks: plan.checks.clone(),
            approval_cycle: self.task.intent_cycle.clone().unwrap_or_default(),
            purpose_summary: None,
        });
        Ok(())
    }

    pub(super) async fn assess_v2_edit(&mut self, edit: &PendingEdit) -> Result<bool> {
        if !self.uses_v2_intent() {
            return Ok(true);
        }
        invariant!(
            self.task.approved_change_scope.is_some(),
            "change-level-v2 scope is not approved"
        );
        let scope = self.task.approved_change_scope.as_ref().unwrap().clone();
        invariant!(
            scope.knowledge_revision == edit.revision,
            "approved knowledge changed"
        );
        invariant!(
            self.task.purpose_selection.is_some(),
            "approved purpose selection is missing"
        );
        let selection = self.task.purpose_selection.as_ref().unwrap();
        invariant!(
            selection.revision == edit.revision
                && selection.status == "ready"
                && selection.selected == scope.selected_records,
            "approved purpose assertions or roles changed"
        );
        anyhow::ensure!(
            scope.files.contains_key(&edit.file),
            "edit is outside approved file scope"
        );
        anyhow::ensure!(
            self.task.snapshots.get(&edit.file) == Some(&fingerprint(&edit.before)),
            "edit before-state does not match the owned source chain"
        );
        let edit_digest =
            digest_json(&json!({"file":edit.file,"before":edit.before,"after":edit.after}))?;
        if self.task.scope_assessments.iter().any(|assessment| {
            assessment.edit_digest == edit_digest && assessment.disposition == "approved"
        }) {
            return Ok(true);
        }
        let current_digest = fingerprint(&edit.before);
        let request = edit.before.as_ref().map(|_| IntentCandidateRequest {
            files: vec![ChangedFile {
                file: edit.file.clone(),
                before_digest: current_digest.clone(),
                after_digest: current_digest.clone(),
                changed_ranges: vec![changed_range_in_before(
                    edit.before.as_deref(),
                    edit.after.as_deref(),
                )],
            }],
            refresh_policy: IntentRefreshPolicy::SupportedFrozen,
            cursor: None,
            limit: Some(8),
        });
        self.task.scope_assessments.push(EditScopeAssessment {
            id: uuid::Uuid::new_v4().to_string(),
            file: edit.file.clone(),
            before_digest: fingerprint(&edit.before),
            after_digest: fingerprint(&edit.after),
            edit_digest: edit_digest.clone(),
            knowledge_revision: edit.revision.clone(),
            disposition: "assessing".into(),
            request: request.clone(),
            pages: vec![],
            candidate_scope_digests: vec![],
            delivered_governing_records: vec![],
        });
        self.persist()?;
        let mut pages = Vec::new();
        if let Some(mut request) = request {
            loop {
                let page: IntentCandidatePage = self.post("intent/candidates", &request).await?;
                anyhow::ensure!(
                    page.knowledge_revision == edit.revision,
                    "knowledge changed during edit scope assessment"
                );
                anyhow::ensure!(
                    page.index.status == IntentIndexStatus::Current && page.unresolved.is_empty(),
                    "current indexed definition scope is unavailable for the proposed edit"
                );
                let next = page.next_cursor.clone();
                pages.push(page);
                self.task.scope_assessments.last_mut().unwrap().pages = pages.clone();
                self.persist()?;
                let Some(cursor) = next else { break };
                request.cursor = Some(cursor);
                self.task.scope_assessments.last_mut().unwrap().request = Some(request.clone());
                self.persist()?;
            }
        }
        let approved_symbols: BTreeSet<_> = scope
            .definition_scopes
            .iter()
            .filter(|definition| {
                definition.file == edit.file
                    && Some(&definition.source_digest) == current_digest.as_ref()
            })
            .map(|definition| definition.symbol.as_str())
            .collect();
        let candidates: Vec<_> = pages.iter().flat_map(|page| &page.candidates).collect();
        let scoped = edit.before.is_none()
            || (!candidates.is_empty()
                && candidates
                    .iter()
                    .all(|candidate| match candidate.scope_basis {
                        IntentScopeBasis::ConservativeFile => true,
                        _ => candidate
                            .symbol
                            .as_deref()
                            .is_some_and(|symbol| approved_symbols.contains(symbol)),
                    }));
        let candidate_scope_digests = candidates
            .iter()
            .map(|candidate| candidate.candidate_digest.clone())
            .collect();
        let delivered_governing_records = candidates
            .iter()
            .flat_map(|candidate| candidate.record_choices.iter())
            .map(|record| record.iri.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let approved_inventory: BTreeMap<_, _> = selection
            .pages
            .iter()
            .flat_map(|page| &page.candidates)
            .map(|record| (record.iri.as_str(), record.assertion_digest.as_str()))
            .collect();
        let newly_implicated: Vec<_> = candidates
            .iter()
            .flat_map(|candidate| &candidate.record_choices)
            .filter(|record| {
                record.lifecycle == "accepted"
                    && approved_inventory.get(record.iri.as_str()).copied()
                        != Some(record.assertion_digest.as_str())
            })
            .cloned()
            .collect();
        let assessment = self.task.scope_assessments.last_mut().unwrap();
        assessment.pages = pages;
        assessment.candidate_scope_digests = candidate_scope_digests;
        assessment.delivered_governing_records = delivered_governing_records;
        assessment.disposition = if scoped { "approved" } else { "scope_escape" }.into();
        if !newly_implicated.is_empty() {
            let mut refreshed = selection.clone();
            refreshed.current_page = PurposeCandidatePage {
                revision: edit.revision.clone(),
                candidates: newly_implicated,
                retrieval: crate::harness::protocol::PurposeRetrieval::Page,
                next_cursor: None,
            };
            refreshed.pages.push(refreshed.current_page.clone());
            refreshed.cursor = None;
            refreshed.status = "selecting".into();
            refreshed.attempts = 0;
            self.task.purpose_selection = Some(refreshed);
            self.task.approved_change_scope = None;
            self.task.approved_revision = None;
            self.task.pending_edit = Some(edit.clone());
            self.task.mode = Mode::Plan;
            self.task.phase = Phase::AwaitingPlan;
            self.task.scope_assessments.last_mut().unwrap().disposition =
                "awaiting_governing_selection".into();
            self.intent_event(
                "scope_invalidated",
                "complete newly implicated knowledge requires renewed purpose/scope review before mutation",
            );
            self.persist()?;
            // Resolve the bounded semantic choices before presenting the plan gate so the
            // next human approval can inspect and bind concrete claims in one interaction.
            self.prepare_v2_purpose().await?;
            return Ok(false);
        }
        if !scoped {
            self.task.pending_edit = Some(edit.clone());
            self.task.phase = Phase::AwaitingPolicy;
            self.task.scope_assessments.last_mut().unwrap().disposition =
                "awaiting_scope_approval".into();
            self.task.last_response = format!(
                "The exact proposed diff touches a current definition/container not yet covered by the approved purpose scope. Review the persisted scope evidence for {} and use /approve or send feedback.",
                edit.file
            );
            self.intent_event(
                "scope_approval_required",
                &format!("unlinked proposed diff scope in {}", edit.file),
            );
            self.persist()?;
            return Ok(false);
        }
        self.intent_event(
            "scope_assessment",
            &format!("approved exact diff scope {} {edit_digest}", edit.file),
        );
        self.persist()?;
        Ok(true)
    }

    pub(super) fn resume_scope_review_after_governing_selection(&mut self) -> Result<bool> {
        let Some(edit) = self.task.pending_edit.clone() else {
            return Ok(false);
        };
        let Some(index) = self.task.scope_assessments.iter().position(|assessment| {
            assessment.matches_edit(&edit)
                && assessment.disposition == "awaiting_governing_selection"
        }) else {
            return Ok(false);
        };
        if edit.revision != self.task.knowledge_revision {
            // The governing detour (a missing-purpose capture, a link review)
            // moved the knowledge revision, so the parked edit can never pass
            // approval. Discard it and let the model re-propose under the
            // current revision instead of surfacing an unapprovable card.
            self.task.scope_assessments[index].disposition = "invalidated".into();
            self.discard_pending_edit(
                "governing knowledge changed during selection; re-propose the edit",
            )?;
            self.intent_event(
                "scope_edit_invalidated",
                &format!(
                    "governing knowledge changed during selection; parked edit to {} discarded",
                    edit.file
                ),
            );
            return Ok(false);
        }
        let assessment = &mut self.task.scope_assessments[index];
        assessment.disposition = "awaiting_scope_approval".into();
        self.task.phase = Phase::AwaitingPolicy;
        self.task.mode = Mode::Auto;
        self.task.last_response = format!(
            "The newly implicated governing records are resolved. Review the persisted exact definition/container scope for {} and use /approve or send feedback.",
            edit.file
        );
        self.intent_event(
            "scope_approval_required",
            &format!(
                "governing selection resolved for exact diff scope in {}",
                edit.file
            ),
        );
        Ok(true)
    }

    pub(super) fn approve_v2_edit_scope(&mut self, edit: &PendingEdit) -> Result<()> {
        if !self.uses_v2_intent() {
            return Ok(());
        }
        let assessment = self.task.scope_assessments.iter_mut().find(|assessment| {
            assessment.matches_edit(edit) && assessment.disposition == "awaiting_scope_approval"
        });
        let Some(assessment) = assessment else {
            return Ok(());
        };
        let approved_edit_digest = assessment.edit_digest.clone();
        let selected = self
            .task
            .purpose_selection
            .as_ref()
            .context("purpose selection missing")?;
        let purpose_iris: Vec<_> = selected
            .selected
            .iter()
            .filter(|record| record.role == "purpose")
            .map(|record| record.iri.clone())
            .collect();
        let scope = self
            .task
            .approved_change_scope
            .as_mut()
            .context("approved purpose/file scope missing")?;
        for candidate in assessment.pages.iter().flat_map(|page| &page.candidates) {
            if let Some(symbol) = &candidate.symbol {
                if !scope
                    .definition_scopes
                    .iter()
                    .any(|known| known.file == candidate.file && known.symbol == *symbol)
                {
                    scope.definition_scopes.push(ApprovedDefinitionScope {
                        file: candidate.file.clone(),
                        symbol: symbol.clone(),
                        source_digest: candidate.source_digest.clone(),
                        purpose_iris: purpose_iris.clone(),
                    });
                }
            }
        }
        assessment.disposition = "approved".into();
        self.intent_event(
            "scope_approved",
            &format!(
                "human approved exact affected scope {} {edit_digest}",
                edit.file,
                edit_digest = approved_edit_digest
            ),
        );
        Ok(())
    }

    pub(super) fn record_owned_v2_transition(&mut self, edit: &PendingEdit) {
        if let Some(scope) = self.task.approved_change_scope.as_mut() {
            scope
                .files
                .insert(edit.file.clone(), fingerprint(&edit.after));
            if let Some(after_digest) = fingerprint(&edit.after) {
                for definition in scope
                    .definition_scopes
                    .iter_mut()
                    .filter(|definition| definition.file == edit.file)
                {
                    definition.source_digest = after_digest.clone();
                }
            }
        }
        if self.uses_postedit_associations() {
            self.task.postedit_association = None;
            if let Some(state) = self.task.symbolic.as_mut() {
                state.association = None;
                state.capture_note = None;
            }
        }
    }

    pub(super) async fn prepare_postedit_associations(&mut self) -> Result<bool> {
        if !self.uses_postedit_associations() || self.task.edits.is_empty() {
            return Ok(false);
        }
        if self.uses_symbolic_intent() {
            return self.prepare_symbolic_associations().await;
        }
        if self.task.postedit_association.is_none() {
            let request = IntentCandidateRequest {
                files: changed_files(&self.task.edits)?,
                refresh_policy: IntentRefreshPolicy::SupportedFrozen,
                cursor: None,
                limit: Some(8),
            };
            let page: IntentCandidatePage = self.post("intent/candidates", &request).await?;
            self.task.postedit_association = Some(PostEditAssociationState {
                version: 1,
                request,
                current_page: page.clone(),
                pages: vec![page.clone()],
                cursor: page.next_cursor.clone(),
                decisions: vec![],
                proposed_bindings: vec![],
                rejected_bindings: vec![],
                unresolved: page.unresolved.clone(),
                pending_link_operation_id: None,
                status: "selecting".into(),
                attempts: 0,
            });
            self.intent_event(
                "association_candidates",
                "persisted bounded post-edit candidates",
            );
            self.persist()?;
        } else if self
            .task
            .postedit_association
            .as_ref()
            .is_some_and(|state| state.status == "unresolved")
        {
            let mut request = self
                .task
                .postedit_association
                .as_ref()
                .unwrap()
                .request
                .clone();
            request.cursor = None;
            let page: IntentCandidatePage = self.post("intent/candidates", &request).await?;
            let state = self.task.postedit_association.as_mut().unwrap();
            state.request = request;
            state.cursor = page.next_cursor.clone();
            state.unresolved = page.unresolved.clone();
            state.pages.push(page.clone());
            state.current_page = page;
            state.status = "selecting".into();
            self.intent_event(
                "association_retry",
                "retried unresolved post-edit discovery after human continuation",
            );
            self.persist()?;
        }
        loop {
            let state = self.task.postedit_association.as_ref().unwrap().clone();
            if state.status == "awaiting_review" {
                return self.prepare_intent_links(false).await;
            }
            if state.status == "resolved" {
                return Ok(false);
            }
            anyhow::ensure!(state.version == 1, "unsupported post-edit state version");
            anyhow::ensure!(
                state.current_page.knowledge_revision == self.task.knowledge_revision,
                "knowledge changed during post-edit association discovery"
            );
            if !state.unresolved.is_empty()
                || state.current_page.index.status != IntentIndexStatus::Current
            {
                self.task.postedit_association.as_mut().unwrap().status = "unresolved".into();
                self.task.phase = Phase::AwaitingInput;
                self.task.last_response = format!(
                    "Post-edit association discovery is unresolved: {}",
                    serde_json::to_string(&state.unresolved)?
                );
                self.persist()?;
                return Ok(true);
            }
            let decided: BTreeSet<_> = state
                .decisions
                .iter()
                .map(|item| item.candidate_digest.as_str())
                .collect();
            let candidate = state
                .current_page
                .candidates
                .iter()
                .find(|candidate| !decided.contains(candidate.candidate_digest.as_str()))
                .cloned();
            let Some(candidate) = candidate else {
                if let Some(cursor) = state.cursor {
                    let mut request = state.request.clone();
                    request.cursor = Some(cursor);
                    let page: IntentCandidatePage =
                        self.post("intent/candidates", &request).await?;
                    let selection = self.task.postedit_association.as_mut().unwrap();
                    selection.request = request;
                    selection.cursor = page.next_cursor.clone();
                    selection.unresolved.extend(page.unresolved.clone());
                    selection.pages.push(page.clone());
                    selection.current_page = page;
                    selection.attempts = 0;
                    self.persist()?;
                    continue;
                }
                if state.proposed_bindings.is_empty() {
                    self.task.postedit_association.as_mut().unwrap().status = "resolved".into();
                    self.persist()?;
                    return Ok(false);
                }
                let request = crate::harness::daemon::intent::IntentLinkRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    revision: self.task.knowledge_revision.clone(),
                    bindings: {
                        anyhow::ensure!(
                            state.proposed_bindings.iter().all(|binding| {
                                state.decisions.iter().any(|decision| {
                                    decision.selected_record_iris.contains(&binding.record_iri)
                                        && decision
                                            .selected_assertion_digests
                                            .get(&binding.record_iri)
                                            .is_some_and(|expected| {
                                                state.pages.iter().any(|page| {
                                                    page.candidates.iter().any(|candidate| {
                                                        candidate.file == binding.file
                                                            && candidate.symbol == binding.symbol
                                                            && candidate.source_digest
                                                                == binding
                                                                    .source_digest
                                                                    .clone()
                                                                    .unwrap_or_default()
                                                            && candidate.record_choices.iter().any(
                                                                |record| {
                                                                    record.iri == binding.record_iri
                                                                        && record.lifecycle
                                                                            == "accepted"
                                                                        && &record.assertion_digest
                                                                            == expected
                                                                },
                                                            )
                                                    })
                                                })
                                            })
                                })
                            }),
                            "post-edit binding record lifecycle or assertion digest is stale"
                        );
                        state.proposed_bindings
                    },
                };
                let operation_id = request.operation_id.clone();
                self.task.pending_intent_links = Some(request);
                let selection = self.task.postedit_association.as_mut().unwrap();
                selection.pending_link_operation_id = Some(operation_id);
                selection.status = "awaiting_review".into();
                self.persist()?;
                return self.prepare_intent_links(false).await;
            };
            let conservative = candidate.scope_basis == IntentScopeBasis::ConservativeFile;
            let choices: Vec<_> = candidate
                .record_choices
                .iter()
                .filter(|record| {
                    record.lifecycle == "accepted"
                        && !candidate.existing_record_iris.contains(&record.iri)
                        && !self.task.postedit_rejected_bindings.iter().any(|binding| {
                            binding.record_iri == record.iri
                                && binding.file == candidate.file
                                && binding.symbol == candidate.symbol
                                && binding.source_digest.as_deref()
                                    == Some(candidate.source_digest.as_str())
                        })
                })
                .cloned()
                .collect();
            if conservative {
                self.task
                    .postedit_association
                    .as_mut()
                    .unwrap()
                    .decisions
                    .push(AssociationDecision {
                        candidate_digest: candidate.candidate_digest.clone(),
                        selected_record_iris: vec![],
                        selected_assertion_digests: BTreeMap::new(),
                        rationale: "The current index proved only file scope and supplied no linkable entity identity; no entity link can be proposed.".into(),
                        disposition: "no_linkable_entity".into(),
                    });
                self.intent_event(
                    "association_no_linkable_entity",
                    &candidate.candidate_digest,
                );
                self.persist()?;
                continue;
            }
            if choices.is_empty() {
                let rationale = if candidate.record_choices.is_empty() {
                    "The daemon supplied no eligible accepted record choices for this proven entity."
                } else if candidate
                    .record_choices
                    .iter()
                    .all(|record| candidate.existing_record_iris.contains(&record.iri))
                {
                    "Every supplied eligible record is already associated with this proven entity."
                } else {
                    "All supplied record choices are ineligible because they are non-current or were previously rejected for this exact entity and source."
                };
                self.task
                    .postedit_association
                    .as_mut()
                    .unwrap()
                    .decisions
                    .push(AssociationDecision {
                        candidate_digest: candidate.candidate_digest.clone(),
                        selected_record_iris: vec![],
                        selected_assertion_digests: BTreeMap::new(),
                        rationale: rationale.into(),
                        disposition: "no_eligible_choices".into(),
                    });
                self.intent_event(
                    "association_no_eligible_choices",
                    &candidate.candidate_digest,
                );
                self.persist()?;
                continue;
            }
            let handles: Vec<_> = choices.iter().map(|record| record.handle.clone()).collect();
            let prompt = association_prompt(&candidate, &choices)?;
            let schema = association_schema(&handles);
            if super::model::json_request_bytes(&prompt, &schema)? > self.prompt_budget()? {
                self.task.phase = Phase::AwaitingInput;
                self.task.last_response = "One complete association candidate exceeds the configured model context. Human association guidance is required; the complete candidate remains in the journal.".into();
                self.intent_event("association_unresolved", &self.task.last_response.clone());
                self.persist()?;
                return Ok(true);
            }
            let (selected, rationale, disposition) = loop {
                let result: Result<_> = async {
                    let choice: AssociationChoice = self
                        .model_json(&prompt, "harness_association_selection", schema.clone())
                        .await?;
                    match choice {
                        AssociationChoice::Associate {
                            record_handles,
                            rationale,
                        } => Ok((
                            validate_association_handles(&record_handles, &choices)
                                .context(super::model::InvalidModelOutput)?,
                            rationale,
                            "associate",
                        )),
                        AssociationChoice::NoneApplies { rationale } => {
                            Ok((vec![], rationale, "none_applies"))
                        }
                    }
                }
                .await;
                match result {
                    Ok(selection) => break selection,
                    Err(error) if self.repair_candidate(&error)? => continue,
                    Err(error) => return Err(error),
                }
            };
            self.candidate_accepted();
            let new_bindings: Vec<_> = selected
                .iter()
                .map(|record_iri| crate::harness::daemon::intent::IntentBinding {
                    record_iri: record_iri.clone(),
                    file: candidate.file.clone(),
                    symbol: candidate.symbol.clone(),
                    planned_name: None,
                    source_digest: Some(candidate.source_digest.clone()),
                })
                .collect();
            let association = self.task.postedit_association.as_mut().unwrap();
            for binding in new_bindings {
                if !association.proposed_bindings.contains(&binding) {
                    association.proposed_bindings.push(binding);
                }
            }
            association.decisions.push(AssociationDecision {
                candidate_digest: candidate.candidate_digest.clone(),
                selected_assertion_digests: choices
                    .iter()
                    .filter(|record| selected.contains(&record.iri))
                    .map(|record| (record.iri.clone(), record.assertion_digest.clone()))
                    .collect(),
                selected_record_iris: selected,
                rationale,
                disposition: disposition.into(),
            });
            self.intent_event("association_selection", &candidate.candidate_digest);
            self.persist()?;
        }
    }
}

pub(super) fn changed_files(edits: &[PendingEdit]) -> Result<Vec<ChangedFile>> {
    let mut chains: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for edit in edits {
        chains
            .entry(edit.file.clone())
            .and_modify(|(_, after)| {
                *after = edit.after.clone();
            })
            .or_insert_with(|| (edit.before.clone(), edit.after.clone()));
    }
    Ok(chains
        .into_iter()
        .map(|(file, (before, after))| ChangedFile {
            file,
            before_digest: fingerprint(&before),
            after_digest: fingerprint(&after),
            changed_ranges: vec![changed_range(before.as_deref(), after.as_deref())],
        })
        .collect())
}

fn changed_range(before: Option<&str>, after: Option<&str>) -> HarnessSourceRange {
    let source = after.or(before).unwrap_or_default();
    let other = if after.is_some() {
        before.unwrap_or_default()
    } else {
        ""
    };
    changed_range_between(source, other)
}

fn changed_range_in_before(before: Option<&str>, after: Option<&str>) -> HarnessSourceRange {
    let source = before.or(after).unwrap_or_default();
    let other = after.unwrap_or_default();
    changed_range_between(source, other)
}

fn changed_range_between(source: &str, other: &str) -> HarnessSourceRange {
    let mut prefix = source
        .bytes()
        .zip(other.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !source.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = source[prefix..]
        .bytes()
        .rev()
        .zip(
            other.as_bytes()[prefix.min(other.len())..]
                .iter()
                .rev()
                .copied(),
        )
        .take_while(|(a, b)| a == b)
        .count();
    while suffix > 0 && !source.is_char_boundary(source.len() - suffix) {
        suffix -= 1;
    }
    let position = |offset: usize| {
        let head = &source[..offset.min(source.len())];
        let line = head.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let col = head
            .rsplit_once('\n')
            .map_or(head.len(), |(_, tail)| tail.len()) as u32;
        HarnessSourcePosition { line, col }
    };
    HarnessSourceRange {
        start: position(prefix),
        end: position(source.len().saturating_sub(suffix)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(handle: &str) -> crate::harness::protocol::PurposeCandidate {
        crate::harness::protocol::PurposeCandidate {
            handle: handle.into(),
            iri: format!("https://example.test/{handle}"),
            kind: "Requirement".into(),
            title: handle.into(),
            claim: crate::harness::protocol::CompleteClaim { literals: vec![] },
            lifecycle: "accepted".into(),
            assertion_digest: format!("digest-{handle}"),
            relations: vec![],
            legal_predicates: vec![],
        }
    }

    #[test]
    fn association_schema_avoids_provider_unsupported_unique_items() {
        let schema = association_schema(&["r0".into(), "r1".into()]);
        assert!(!serde_json::to_string(&schema)
            .unwrap()
            .contains("uniqueItems"));
    }

    #[test]
    fn association_prompt_has_one_complete_choice_representation() {
        let mut selected = record("r0");
        selected.title = "Unicode purpose π".into();
        selected
            .claim
            .literals
            .push(crate::harness::protocol::CandidateLiteral {
                predicate: "description".into(),
                value: "Preserve café labels 🫎".into(),
                datatype: None,
                language: Some("en".into()),
            });
        selected
            .relations
            .push(crate::harness::protocol::CandidateRelation {
                predicate: "isMotivatedBy".into(),
                target_iri: "https://example.test/ad".into(),
                incoming: false,
            });
        selected.legal_predicates = vec!["concerns".into(), "constrains".into()];
        let candidate: crate::harness::protocol::PostEditCandidate =
            serde_json::from_value(json!({
                "id": "entity-π",
                "file": "src/labels.rs",
                "symbol": "scip-rust crate labels/render().",
                "name": "render",
                "definition_range": {"start":{"line":2,"col":4},"end":{"line":2,"col":10}},
                "enclosing_range": {"start":{"line":1,"col":0},"end":{"line":8,"col":1}},
                "source_digest": "source-digest",
                "scope_basis": "changed_definition",
                "candidate_digest": "candidate-digest",
                "existing_record_iris": ["https://example.test/existing"],
                "record_choices": [selected.clone()]
            }))
            .unwrap();
        let choices = vec![selected];
        let prompt = association_prompt(&candidate, &choices).unwrap();
        let candidate_text = prompt
            .split_once("\nCandidate:\n")
            .unwrap()
            .1
            .split_once("\nRecord choices with complete claims/lifecycle/legal predicates:\n")
            .unwrap()
            .0;
        let choices_text = prompt
            .split_once("\nRecord choices with complete claims/lifecycle/legal predicates:\n")
            .unwrap()
            .1;
        let rendered_candidate: Value = serde_json::from_str(candidate_text).unwrap();
        let rendered_choices: Vec<crate::harness::protocol::PurposeCandidate> =
            serde_json::from_str(choices_text).unwrap();

        let mut expected_identity = serde_json::to_value(&candidate).unwrap();
        expected_identity
            .as_object_mut()
            .unwrap()
            .remove("record_choices");
        assert_eq!(rendered_candidate, expected_identity);
        assert_eq!(rendered_choices, choices);
        assert_eq!(prompt.matches(&choices[0].iri).count(), 1);
        assert!(prompt.contains("Preserve café labels 🫎"));

        let schema = association_schema(&["r0".into()]);
        let full_request = format!(
            "{prompt}{}{}",
            super::super::model::JSON_SCHEMA_MARKER,
            serde_json::to_string(&schema).unwrap()
        );
        assert_eq!(
            super::super::model::json_request_bytes(&prompt, &schema).unwrap(),
            full_request.len()
        );
        assert!(full_request.len() > full_request.chars().count());
    }

    #[test]
    fn empty_purpose_schema_omits_handle_dependent_variants() {
        let encoded = serde_json::to_string(&purpose_schema(&[], false, false)).unwrap();
        assert!(!encoded.contains("record_handle"));
        assert!(!encoded.contains("candidate_handle"));
        assert!(!encoded.contains("\"enum\":[]"));
        assert!(encoded.contains("\"const\":\"missing\""));
    }

    fn purpose_decisions(schema: &Value) -> Vec<&str> {
        schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["properties"]["decision"]["const"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn purpose_schema_masks_branches_from_controller_state() {
        let handles = vec!["r0".into()];
        assert_eq!(
            purpose_decisions(&purpose_schema(&[], false, false)),
            ["missing"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&[], true, false)),
            ["next_page"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&[], true, true)),
            ["next_page", "done"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&handles, false, false)),
            ["select", "skip", "missing"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&handles, true, false)),
            ["select", "skip", "next_page"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&handles, false, true)),
            ["select", "skip", "missing", "done"]
        );
        assert_eq!(
            purpose_decisions(&purpose_schema(&handles, true, true)),
            ["select", "skip", "next_page", "done"]
        );
        for has_next_page in [false, true] {
            for has_selected_purpose in [false, true] {
                for branch in purpose_schema(&handles, has_next_page, has_selected_purpose)["oneOf"]
                    .as_array()
                    .unwrap()
                {
                    assert_eq!(
                        branch["properties"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .next()
                            .map(String::as_str),
                        Some("decision")
                    );
                }
            }
        }
        let encoded = serde_json::to_string(&purpose_schema(&handles, false, false)).unwrap();
        assert!(encoded.contains("record_handle"));
        assert!(!encoded.contains("candidate_handle"));
    }

    #[test]
    fn purpose_choice_accepts_legacy_and_current_handle_names() {
        let current: PurposeChoice = serde_json::from_value(json!({"decision":"select","record_handle":"r0","role":"purpose","rationale":"current"})).unwrap();
        let legacy: PurposeChoice = serde_json::from_value(json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"legacy"})).unwrap();
        assert!(
            matches!(current, PurposeChoice::Select { candidate_handle, .. } if candidate_handle == "r0")
        );
        assert!(
            matches!(legacy, PurposeChoice::Select { candidate_handle, .. } if candidate_handle == "r0")
        );
        assert!(serde_json::from_value::<PurposeChoice>(json!({"decision":"select","record_handle":"r0","candidate_handle":"r0","role":"purpose","rationale":"duplicate"})).is_err());
    }

    #[test]
    fn association_handles_are_distinct_bounded_and_supplied() {
        let choices = vec![record("r0"), record("r1")];
        assert_eq!(
            validate_association_handles(&["r1".into(), "r0".into()], &choices).unwrap(),
            ["https://example.test/r1", "https://example.test/r0"]
        );
        assert!(validate_association_handles(&[], &choices).is_err());
        assert!(
            validate_association_handles(&["r0".into(), "r0".into()], &choices)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        assert!(validate_association_handles(&["r2".into()], &choices)
            .unwrap_err()
            .to_string()
            .contains("unknown supplied"));
        assert!(validate_association_handles(&vec!["r0".into(); 17], &choices).is_err());
    }

    #[test]
    fn sequential_edits_are_rebased_to_one_original_to_final_range() {
        let edits = vec![
            PendingEdit {
                file: "code.py".into(),
                before: Some("abc\n".into()),
                after: Some("abXc\n".into()),
                reason: String::new(),
                revision: "r1".into(),
            },
            PendingEdit {
                file: "code.py".into(),
                before: Some("abXc\n".into()),
                after: Some("QabXc\n".into()),
                reason: String::new(),
                revision: "r1".into(),
            },
        ];
        let changed = changed_files(&edits).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].before_digest, fingerprint(&Some("abc\n".into())));
        assert_eq!(
            changed[0].after_digest,
            fingerprint(&Some("QabXc\n".into()))
        );
        assert_eq!(changed[0].changed_ranges.len(), 1);
        assert_eq!(changed[0].changed_ranges[0].start.line, 0);
        assert_eq!(changed[0].changed_ranges[0].start.col, 0);
        assert_eq!(changed[0].changed_ranges[0].end.line, 0);
        assert_eq!(changed[0].changed_ranges[0].end.col, 4);
    }
}
