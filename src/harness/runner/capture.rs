//! Evidence paging, durable knowledge capture, and human capture review.
use super::model::{observation_preview, InvalidModelOutput};
use super::{bounded, Event, HttpFailure, Mode, Phase, ReviewItem, Runner};
use crate::harness::protocol::{
    CaptureRequest, CaptureResponse, CaptureTargets, CheckpointResponse, KnowledgeProposal,
    ReviewRequest,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// An identifier only selects bytes actually presented on this evidence page.
#[derive(Debug, Serialize)]
struct Evidence {
    id: String,
    source: String,
    text: String,
}

fn evidence_size(page: &[Evidence]) -> Result<usize> {
    // Include the dynamic enum's contribution as well as the rendered evidence.
    let ids: Vec<_> = page.iter().map(|item| &item.id).collect();
    Ok(serde_json::to_vec(page)?.len() + serde_json::to_vec(&ids)?.len() - 2)
}

/// Spend the real JSON-encoded prompt and schema budget. Split large events at
/// UTF-8 boundaries; the cursor advances only after durable capture acknowledgment.
fn evidence_page(
    events: &[Event],
    mut index: usize,
    mut offset: usize,
    end: usize,
    objective: &str,
    budget: usize,
) -> Result<(Vec<Evidence>, usize, usize)> {
    let page_id = format!("p{index}_{offset}");
    let mut evidence = vec![Evidence {
        id: format!("{page_id}_objective"),
        source: "User objective".into(),
        text: objective.into(),
    }];
    anyhow::ensure!(evidence_size(&evidence)? + 256 <= budget,
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
        let entry = |size: usize| Evidence {
            id: format!("{page_id}_e{index}_{offset}_{}", offset + size),
            source: format!("Event {index}, bytes {offset}..{}", offset + size),
            text: event[offset..offset + size].into(),
        };
        let mut low = 0;
        let mut high = (event.len() - offset).min(2048);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let mut size = middle;
            while !event.is_char_boundary(offset + size) {
                size -= 1;
            }
            evidence.push(entry(size));
            let fits = evidence_size(&evidence)? <= budget;
            evidence.pop();
            if fits {
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
        evidence.push(entry(low));
        offset += low;
        if offset == event.len() {
            index += 1;
            offset = 0;
        }
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
            let targets = context.capture_targets.as_ref().context(
                "daemon lacks typed capture choices; upgrade the project daemon to this harness build and retry (capture remains pending)",
            )?;
            let choices = TargetChoices::new(targets);
            let checkpoint_end = *self
                .task
                .capture_checkpoint_end
                .get_or_insert(self.task.events.len());
            let mut prompt = format!("You are the knowledge-capture sensor for a coding task. The harness requires this review independently of your coding actions. Extract only durable decisions, requirements, constraints, lessons, patterns, antipatterns supported by the supplied contemporaneous evidence. Do not invent rationale. Compare existing knowledge first. Return proposals (possibly empty) and a reason. Each proposal needs kind,title,description,evidence_ids (select supporting evidence IDs),files,components (component choice IDs),requirement (requirement choice ID or null),supersedes (same-kind record choice ID or null),retracts (record choice ID or null). Never accept knowledge.\nObjective: {}\nExisting knowledge:\n{}\n", self.task.objective, context.context);
            prompt.push_str(&format!("\nCurrent human guidance (instructions; select facts only from the evidence page): {}\n",self.task.guidance));
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
            prompt.push_str(&format!(
                "\nTyped relationship choices (bounded existing targets, not evidence):\n{}\n",
                serde_json::to_string(&choices)?
            ));
            prompt.push_str("\nUse only supplied choice IDs; use an empty components array when none applies. The daemon also anchors selected files to their owning components. A progress update, routine edit, or repeated task request alone does not warrant a durable record. Evidence IDs select verbatim journal bytes; do not return quotations or cite existing knowledge as task evidence. Evidence is paged from the journal; this page may be only part of a large observation. Do not infer a new requirement from code or command output alone. The harness processes all pages before clearing capture.\nEvidence:\n");
            let budget = self.prompt_budget()?.saturating_sub(
                prompt.len()
                    + "\nRequired JSON schema:\n".len()
                    + serde_json::to_string(&capture_schema(&[], &choices, &files))?.len(),
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
                .model_json(
                    &prompt,
                    "harness_capture",
                    capture_schema(&evidence, &choices, &files),
                )
                .await?;
            let proposals = assessment
                .resolve(&evidence, &choices, &files, &self.task.id)
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
                self.candidate_accepted();
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
                // Older journals persisted a fully resolved candidate without a
                // recovery budget. Only a definite pre-persistence rejection
                // permits replacing it; charge that original candidate first.
                if self.task.recovery.is_none() {
                    self.task.recovery = Some(super::RepairState {
                        id: uuid::Uuid::new_v4().to_string(),
                        purpose: "harness_capture".into(),
                        attempts: self.task.capture_repairs.saturating_add(1).min(3),
                        diagnostic: String::new(),
                        status: super::RecoveryStatus::Generating,
                    });
                }
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
        // Commit the page/review and its finished candidate budget together.
        // A restart must never charge the next page for this page's repairs.
        self.candidate_accepted();
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

#[derive(Debug, Serialize)]
struct TargetChoice {
    id: String,
    iri: String,
    label: String,
    kind: String,
}

#[derive(Debug, Serialize)]
struct TargetChoices {
    components: Vec<TargetChoice>,
    records: Vec<TargetChoice>,
}

impl TargetChoices {
    fn new(targets: &CaptureTargets) -> Self {
        let choices = |prefix: &str, values: &[crate::harness::protocol::CaptureTarget]| {
            values
                .iter()
                .enumerate()
                .map(|(index, target)| TargetChoice {
                    id: format!("{prefix}{index}"),
                    iri: target.iri.clone(),
                    label: target.label.clone(),
                    kind: target.kind.clone(),
                })
                .collect()
        };
        Self {
            components: choices("c", &targets.components),
            records: choices("r", &targets.records),
        }
    }

    fn resolve(&self, id: &str, kind: Option<&str>, component: bool) -> Result<String> {
        let options = if component {
            &self.components
        } else {
            &self.records
        };
        let target = options
            .iter()
            .find(|option| option.id == id)
            .with_context(|| {
                format!("unknown capture target choice {id}; select a supplied choice ID")
            })?;
        anyhow::ensure!(
            kind.is_none_or(|kind| kind == target.kind),
            "capture target {id} has kind {}, incompatible with this relationship",
            target.kind
        );
        Ok(target.iri.clone())
    }
}

/// The sensor selects references; only the controller constructs daemon inputs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SensorProposal {
    kind: String,
    title: String,
    description: String,
    evidence_ids: Vec<String>,
    files: Vec<String>,
    components: Vec<String>,
    requirement: Option<String>,
    supersedes: Option<String>,
    retracts: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Assessment {
    proposals: Vec<SensorProposal>,
    reason: String,
}

impl Assessment {
    fn resolve(
        &self,
        page: &[Evidence],
        targets: &TargetChoices,
        files: &[String],
        task: &str,
    ) -> Result<Vec<KnowledgeProposal>> {
        anyhow::ensure!(
            !self.reason.trim().is_empty() && self.proposals.len() <= 20,
            "capture review requires a reason and at most 20 proposals"
        );
        self.proposals
            .iter()
            .map(|proposal| {
                anyhow::ensure!(
                    !proposal.evidence_ids.is_empty(),
                    "capture requires supporting evidence IDs"
                );
                anyhow::ensure!(
                    proposal.files.iter().all(|file| files.contains(file)),
                    "capture linked an unobserved file"
                );
                anyhow::ensure!(
                    proposal.supersedes.is_none() || proposal.retracts.is_none(),
                    "one proposal cannot both supersede and retract"
                );
                let evidence = proposal
                    .evidence_ids
                    .iter()
                    .map(|id| {
                        let item = page.iter().find(|item| &item.id == id).with_context(|| {
                            format!(
                                "unknown evidence ID {id}; select evidence from the current page"
                            )
                        })?;
                        Ok(format!(
                            "Task {task}; {} [{}]:\n{}",
                            item.source, item.id, item.text
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let optional = |id: &Option<String>, kind: Option<&str>| {
                    id.as_deref()
                        .map(|id| targets.resolve(id, kind, false))
                        .transpose()
                };
                Ok(KnowledgeProposal {
                    kind: proposal.kind.clone(),
                    title: proposal.title.clone(),
                    description: proposal.description.clone(),
                    evidence,
                    files: proposal.files.clone(),
                    components: proposal
                        .components
                        .iter()
                        .map(|id| targets.resolve(id, Some("SystemComponent"), true))
                        .collect::<Result<_>>()?,
                    requirement: optional(&proposal.requirement, Some("Requirement"))?,
                    supersedes: optional(&proposal.supersedes, Some(&proposal.kind))?,
                    retracts: optional(&proposal.retracts, None)?,
                })
            })
            .collect()
    }
}

fn capture_schema(page: &[Evidence], targets: &TargetChoices, files: &[String]) -> Value {
    let array = |values: Vec<&str>| {
        if values.is_empty() {
            json!({"type":"array","maxItems":0,"items":{"type":"string"}})
        } else {
            json!({"type":"array","items":{"type":"string","enum":values}})
        }
    };
    let nullable = |values: Vec<&str>| {
        let values: Vec<Value> = std::iter::once(Value::Null)
            .chain(values.into_iter().map(|value| json!(value)))
            .collect();
        json!({"type":["string","null"],"enum":values})
    };
    json!({"type":"object","additionalProperties":false,"required":["proposals","reason"],"properties":{
        "reason":{"type":"string"},
        "proposals":{"type":"array","maxItems":20,"items":{"type":"object","additionalProperties":false,
            "required":["kind","title","description","evidence_ids","files","components","requirement","supersedes","retracts"],
            "properties":{
                "kind":{"type":"string","enum":["ArchitecturalDecision","Requirement","Constraint","Lesson","Pattern","AntiPattern"]},
                "title":{"type":"string"},"description":{"type":"string"},
                "evidence_ids":array(page.iter().map(|item| item.id.as_str()).collect()),
                "files":array(files.iter().map(String::as_str).collect()),
                "components":array(targets.components.iter().map(|target| target.id.as_str()).collect()),
                "requirement":nullable(targets.records.iter().filter(|target| target.kind == "Requirement").map(|target| target.id.as_str()).collect()),
                "supersedes":nullable(targets.records.iter().map(|target| target.id.as_str()).collect()),
                "retracts":nullable(targets.records.iter().map(|target| target.id.as_str()).collect())
            }
        }}
    }})
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
                reconstructed[cursor].push_str(&fragment.text);
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

    fn assessment(evidence_id: &str) -> Assessment {
        serde_json::from_value(json!({"reason":"The user states a durable rule.","proposals":[{
            "kind":"Constraint","title":"Preserve behavior","description":"Preserve public behavior.",
            "evidence_ids":[evidence_id],"files":[],"components":[],
            "requirement":null,"supersedes":null,"retracts":null
        }]})).unwrap()
    }

    #[test]
    fn capture_references_materialize_exact_evidence_and_reject_other_pages() {
        let events = vec![Event {
            message: "Preserve public behavior.\nλ😀".into(),
        }];
        let (page, _, _) = evidence_page(&events, 0, 0, 1, "Repair", 4096).unwrap();
        let choices = TargetChoices::new(&CaptureTargets::default());
        let candidate = assessment(&page[1].id);
        let resolved = candidate.resolve(&page, &choices, &[], "task-123").unwrap();
        assert_eq!(
            resolved[0].evidence,
            vec![format!(
                "Task task-123; Event 0, bytes 0..{} [{}]:\n{}",
                events[0].message.len(),
                page[1].id,
                events[0].message
            )]
        );
        for bad in [
            "Existing knowledge: Preserve public behavior",
            "r0",
            "p1_0_objective",
        ] {
            assert!(assessment(bad)
                .resolve(&page, &choices, &[], "task-123")
                .is_err());
        }
        let (later, _, _) = evidence_page(&events, 0, 9, 1, "Repair", 4096).unwrap();
        assert!(candidate
            .resolve(&later, &choices, &[], "task-123")
            .is_err());
    }

    #[test]
    fn capture_choices_are_typed_and_resolve_to_canonical_iris() {
        use crate::harness::protocol::CaptureTarget;
        let target = |iri: &str, kind: &str| CaptureTarget {
            iri: iri.into(),
            label: "Existing target".into(),
            kind: kind.into(),
        };
        let choices = TargetChoices::new(&CaptureTargets {
            components: vec![target("urn:component", "SystemComponent")],
            records: vec![
                target("urn:requirement", "Requirement"),
                target("urn:constraint", "Constraint"),
            ],
        });
        let (page, _, _) = evidence_page(&[], 0, 0, 0, "Preserve behavior", 4096).unwrap();
        let mut candidate = assessment(&page[0].id);
        candidate.proposals[0].components = vec!["c0".into()];
        candidate.proposals[0].requirement = Some("r0".into());
        candidate.proposals[0].supersedes = Some("r1".into());
        let resolved = candidate.resolve(&page, &choices, &[], "task").unwrap();
        assert_eq!(resolved[0].components, ["urn:component"]);
        assert_eq!(resolved[0].requirement.as_deref(), Some("urn:requirement"));
        assert_eq!(resolved[0].supersedes.as_deref(), Some("urn:constraint"));
        candidate.proposals[0].requirement = Some("r1".into());
        assert!(candidate.resolve(&page, &choices, &[], "task").is_err());
        candidate.proposals[0].requirement = None;
        candidate.proposals[0].supersedes = Some("r0".into());
        assert!(candidate.resolve(&page, &choices, &[], "task").is_err());
        candidate.proposals[0].supersedes = None;
        candidate.proposals[0].components = vec!["Ledger".into()];
        assert!(candidate.resolve(&page, &choices, &[], "task").is_err());
        candidate.proposals[0].components = vec![];
        candidate.proposals[0].files = vec!["unread.rs".into()];
        assert!(candidate.resolve(&page, &choices, &[], "task").is_err());
    }

    #[test]
    fn capture_schema_choices_and_encoded_budget_match_actual_page() {
        let choices = TargetChoices::new(&CaptureTargets::default());
        let empty = serde_json::to_vec(&capture_schema(&[], &choices, &[]))
            .unwrap()
            .len();
        let events = vec![Event {
            message: "\"λ😀\\\n".repeat(10000),
        }];
        let (page, _, _) = evidence_page(&events, 0, 0, 1, "Test", 4096).unwrap();
        let schema = capture_schema(&page, &choices, &[]);
        let encoded =
            serde_json::to_vec(&page).unwrap().len() + serde_json::to_vec(&schema).unwrap().len();
        assert!(encoded <= 4096 + empty);
        let properties = &schema["properties"]["proposals"]["items"]["properties"];
        assert_eq!(properties["components"]["maxItems"], 0);
        assert_eq!(properties["requirement"]["enum"], json!([null]));
        assert_eq!(
            properties["evidence_ids"]["items"]["enum"],
            json!(page.iter().map(|entry| &entry.id).collect::<Vec<_>>())
        );
    }

    #[tokio::test]
    async fn successful_capture_commits_page_and_repair_reset_atomically() {
        use crate::harness::protocol::ContextResponse;
        use crate::llm::{LlmConfig, StructuredOutputMode};
        use axum::{extract::State, routing::post, Json, Router};
        use std::{path::PathBuf, sync::Arc};

        struct Project(PathBuf);
        impl Drop for Project {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        async fn context(State(state): State<Arc<(PathBuf, bool)>>) -> Json<ContextResponse> {
            Json(ContextResponse {
                project_root: state.0.to_string_lossy().into_owned(),
                revision: "fixture".into(),
                context: String::new(),
                files: vec![],
                capture_targets: Some(Default::default()),
            })
        }
        async fn model(
            State(state): State<Arc<(PathBuf, bool)>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            let name = body["response_format"]["json_schema"]["name"]
                .as_str()
                .unwrap();
            let answer = if name == "harness_response_probe" {
                json!({"status":"ok"})
            } else if state.1 {
                let id = &body["response_format"]["json_schema"]["schema"]["properties"]
                    ["proposals"]["items"]["properties"]["evidence_ids"]["items"]["enum"][0];
                json!({"reason":"Preserve the explicitly requested behavior.","proposals":[{
                    "kind":"Lesson","title":"Preserve behavior","description":"Preserve the requested behavior.",
                    "evidence_ids":[id],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
                }]})
            } else {
                json!({"reason":"No durable claim on this page.","proposals":[]})
            };
            Json(
                json!({"choices":[{"message":{"content":answer.to_string()},"finish_reason":"stop"}]}),
            )
        }
        async fn capture() -> Json<Value> {
            Json(
                json!({"proposals":[{"iri":"urn:captured","title":"Preserve behavior","kind":"Lesson","links":[]}]}),
            )
        }
        for with_proposal in [false, true] {
            let project = Project(
                std::env::temp_dir()
                    .join(format!("moosedev-capture-commit-{}", uuid::Uuid::new_v4())),
            );
            std::fs::create_dir_all(&project.0).unwrap();
            let root = project.0.canonicalize().unwrap();
            let router = Router::new()
                .route("/api/v1/harness/context", post(context))
                .route("/api/v1/harness/capture", post(capture))
                .route("/v1/chat/completions", post(model))
                .with_state(Arc::new((root.clone(), with_proposal)));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let daemon = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let mut runner =
                Runner::create(root.clone(), daemon.clone(), "Preserve behavior".into())
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
            runner.task.events.push(Event {
                message: "Observed behavior\n".repeat(20_000),
            });
            runner.task.capture_due = true;
            runner.task.recovery = Some(super::super::RepairState {
                id: "previous-page-decision".into(),
                purpose: "harness_capture".into(),
                attempts: 2,
                diagnostic: "Choose supplied evidence IDs".into(),
                status: super::super::RecoveryStatus::Paused,
            });
            // Stop exactly at the inner durable acknowledgment boundary, before
            // the outer capture wrapper can perform any additional persistence.
            runner.capture_inner().await.unwrap();
            let stored: super::super::Task =
                serde_json::from_slice(&std::fs::read(&runner.journal).unwrap()).unwrap();
            assert!(stored.capture_due && stored.capture_offset > 0);
            assert!(
                stored.recovery.is_none(),
                "completed page retained its exhausted budget"
            );
            assert_eq!(stored.model_requests.last().unwrap()["attempt"], 3);
            assert_eq!(stored.reviews.len(), usize::from(with_proposal));
            let id = runner.task.id.clone();
            drop(runner);
            let mut resumed = Runner::load(root, daemon, &id).unwrap();
            resumed.begin_candidate("harness_capture").unwrap();
            assert_eq!(resumed.task.recovery.as_ref().unwrap().attempts, 1);
            assert_ne!(
                resumed.task.recovery.as_ref().unwrap().id,
                "previous-page-decision"
            );
            server.abort();
        }
    }
}
