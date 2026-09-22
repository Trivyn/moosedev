//! Explicit, source-grounded specification approval.
//!
//! Extraction is an LLM sensor step. The runner validates every source
//! reference, the daemon reconciles it without touching the project graph,
//! and only a later human approval can commit the frozen preview.
use super::*;
use serde::Deserialize;
use serde_json::json;

const MAX_SPEC_RECORDS: usize = 32;
const MAX_SPEC_TITLE_BYTES: usize = 240;
const MAX_SPEC_DESCRIPTION_BYTES: usize = 4_000;
const MAX_SPEC_EVIDENCE: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecExtraction {
    action: String,
    records: Vec<SpecRecordDraft>,
}

impl Runner {
    /// Extract and reconcile a repository specification, then stop at a
    /// durable human gate. This operation does not write project knowledge.
    pub async fn begin_spec_approval(&mut self, path: &str, covers: &[String]) -> Result<()> {
        let path = path.trim();
        anyhow::ensure!(
            !path.is_empty(),
            "usage: /approve-spec <repo-relative-path> [covered paths]"
        );
        let covers = validate_covers(covers)?;
        anyhow::ensure!(
            self.task.mode == Mode::Plan
                && matches!(
                    self.task.phase,
                    Phase::Planning
                        | Phase::AwaitingPlan
                        | Phase::AwaitingInput
                        | Phase::AwaitingSpecApproval
                ),
            "spec approval can only start while the task is in Plan"
        );
        anyhow::ensure!(
            self.task.pending_capture.is_none()
                && self.task.capture_request.is_none()
                && self.task.reviews.is_empty()
                && self.task.intent.is_none()
                && self.task.pending_intent_links.is_none()
                && self.task.intent_refresh_pending.is_empty()
                && !self.task.capture_due
                && !self.task.final_capture
                && !self.task.completion_pending
                && self
                    .task
                    .symbolic
                    .as_ref()
                    .is_none_or(|state| state.capture_note.is_none() && state.association.is_none()),
            "finish or abandon the pending task operation before approving a specification"
        );
        anyhow::ensure!(
            !self.task.cleanup_pending,
            "cancelled task cleanup is pending; resume before approving a specification"
        );

        let source = self
            .workspace
            .read(path)?
            .with_context(|| format!("specification does not exist: {path}"))?;
        anyhow::ensure!(!source.trim().is_empty(), "specification is empty: {path}");
        anyhow::ensure!(
            !graph_derived_spec(&source),
            "{path} is derived from accepted project knowledge; approve or revise the cited graph records instead of mining the spec back into the graph"
        );
        let source_sha256 = sha256_hex(&source);
        let checkpoint = self.checkpoint_status().await?;
        anyhow::ensure!(
            checkpoint.conforms && checkpoint.pending.is_empty(),
            "project knowledge must be conforming with no pending operation before spec preparation"
        );
        let knowledge_revision = checkpoint.revision;
        self.update_knowledge_revision(knowledge_revision.clone());
        let prompt = spec_prompt(path, &source);
        let extracted: SpecExtraction = self
            .model_json(&prompt, "harness_spec_extract", spec_schema(path))
            .await?;
        anyhow::ensure!(
            extracted.action == "propose_spec_records",
            "spec extraction returned the wrong action"
        );
        validate_spec_records(path, &source, &extracted.records)?;

        // Bind the preview to exactly the bytes seen by the extraction sensor.
        let current = self
            .workspace
            .read(path)?
            .with_context(|| format!("specification disappeared during extraction: {path}"))?;
        anyhow::ensure!(
            sha256_hex(&current) == source_sha256,
            "specification changed during extraction; run /approve-spec {path} again"
        );

        let operation_id = uuid::Uuid::new_v4().to_string();
        let request = SpecPrepareRequest {
            operation_id,
            owner_id: self.task.id.clone(),
            path: path.to_owned(),
            source_sha256: source_sha256.clone(),
            knowledge_revision: knowledge_revision.clone(),
            drafts: extracted.records,
            covers,
        };
        let preview: SpecPrepareResponse = self.post("spec/prepare", &request).await?;
        anyhow::ensure!(
            preview.operation_id == request.operation_id
                && preview.owner_id == request.owner_id
                && preview.path == request.path
                && preview.source_sha256 == source_sha256
                && preview.knowledge_revision == knowledge_revision,
            "daemon returned a spec preview for different evidence"
        );
        anyhow::ensure!(
            preview.entries.len() == request.drafts.len()
                && preview
                    .entries
                    .iter()
                    .map(|entry| &entry.draft)
                    .eq(request.drafts.iter()),
            "daemon spec preview did not preserve the validated extraction batch"
        );

        // A pending execution plan was grounded before this governing batch.
        // It cannot survive even the preview because successful approval will
        // change accepted knowledge.
        self.task.plan = None;
        self.task.approved_change_scope = None;
        self.task.approved_revision = None;
        self.task.snapshots.clear();
        self.task.check_results.clear();
        self.task.pending_spec = Some(PendingSpecApproval { preview });
        self.task.mode = Mode::Plan;
        self.task.phase = Phase::AwaitingSpecApproval;
        self.task.turn_finished = true;
        self.task.last_response = format!(
            "Prepared {} source-grounded record(s) from {path}{}; review the spec approval gate.",
            request.drafts.len(),
            match &self
                .task
                .pending_spec
                .as_ref()
                .and_then(|pending| pending.preview.component.as_ref())
            {
                Some(plan) => format!(", anchored to component {}", plan.name),
                None => String::new(),
            }
        );
        self.event(format!(
            "Prepared spec approval preview for {path} at source sha256 {source_sha256}; no project knowledge was written."
        ));
        self.persist()
    }

    /// Commit the exact frozen preview currently displayed to the human.
    /// Execution remains in Plan and requires a separate `/approve` later.
    pub async fn approve_spec(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingSpecApproval,
            "no specification is awaiting approval"
        );
        let pending = self
            .task
            .pending_spec
            .as_ref()
            .context("spec approval gate has no pending preview")?;
        let preview = pending.preview.clone();
        let current = self
            .workspace
            .read(&preview.path)?
            .with_context(|| format!("specification no longer exists: {}", preview.path))?;
        anyhow::ensure!(
            sha256_hex(&current) == preview.source_sha256,
            "specification changed after preview; run /approve-spec {} again",
            preview.path
        );

        let response: SpecApproveResponse = self
            .post(
                "spec/approve",
                &SpecApproveRequest {
                    operation_id: preview.operation_id.clone(),
                    owner_id: self.task.id.clone(),
                },
            )
            .await?;
        anyhow::ensure!(
            response.operation_id == preview.operation_id
                && response.path == preview.path
                && response.source_sha256 == preview.source_sha256
                && response.base_revision == preview.knowledge_revision,
            "daemon approved a different specification preview"
        );
        anyhow::ensure!(
            response.checkpoint.conforms
                && response.checkpoint.durable
                && response.checkpoint.revision == response.result_revision,
            "spec approval did not produce a conforming durable checkpoint"
        );

        // Refresh before clearing the pending operation. If the daemon commit
        // succeeded but refresh fails, restart/retry reuses the operation ID.
        let context = self.refresh(&[]).await?;
        anyhow::ensure!(
            context.revision == response.result_revision,
            "approved knowledge checkpoint does not match refreshed context"
        );
        self.task.pending_spec = None;
        self.task.plan = None;
        self.task.approved_change_scope = None;
        self.task.approved_revision = None;
        self.task.snapshots.clear();
        self.task.read_files.clear();
        self.task.source.clear();
        self.task.check_results.clear();
        self.task.capture_due = false;
        self.task.final_capture = false;
        self.task.completion_pending = false;
        self.task.review_continuation = None;
        self.task.after_review = Phase::Planning;
        self.task.resume_phase = Phase::Planning;
        self.task.mode = Mode::Plan;
        self.task.phase = Phase::Planning;
        self.task.steps = 0;
        self.task.turn_finished = false;
        self.task.last_response = format!(
            "Approved {} spec record(s) from {}. Returned to Planning; execution still requires /approve.",
            response.records.len(), response.path
        );
        self.event(format!(
            "Human approved specification {} as {}; accepted revision {}. Returned to Planning; execution was not approved.",
            response.path, response.approval_iri, response.result_revision
        ));
        self.persist()
    }
}

fn spec_prompt(path: &str, source: &str) -> String {
    let numbered = source
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}: {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are the extraction sensor for MOOSEDev's symbolic project memory. Extract only requirements and hard constraints explicitly stated by this specification. Do not infer goals, implementation choices, patterns, lessons, or architectural decisions. Preserve each complete normative claim in its description. Use kind exactly Requirement or Constraint: a Constraint is a hard rule the implementation must not violate (a limit, invariant, prohibition or required format); a Requirement is a capability or outcome the system must provide. Each evidence entry must be only an exact 1-based line reference in the form {path}:<line> or {path}:<start>-<end>; cite the narrowest lines that directly state the claim. Produce no more than {MAX_SPEC_RECORDS} non-duplicate records.\n\nSpecification path: {path}\nSpecification sha256: {}\nLine-addressed specification:\n{numbered}",
        sha256_hex(source)
    )
}

/// The paths a spec governs, as the human typed them: repo-relative, no
/// traversal, `.` for the whole project. The daemon decides directory versus
/// file against the working tree.
fn validate_covers(covers: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for raw in covers {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let normalized = if raw == "." || raw == "./" {
            ".".to_string()
        } else {
            let trimmed = raw.trim_start_matches("./");
            let directory = trimmed.ends_with('/');
            let body = trimmed.trim_end_matches('/');
            anyhow::ensure!(
                !body.is_empty()
                    && !trimmed.contains('\\')
                    && trimmed.matches('/').count()
                        == body.matches('/').count() + usize::from(directory)
                    && body
                        .split('/')
                        .all(|segment| !segment.is_empty() && segment != ".." && segment != "."),
                "covered path must be repo-relative without traversal: {raw}"
            );
            if directory {
                format!("{body}/")
            } else {
                body.to_string()
            }
        };
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

/// Specs rendered from an existing decision cluster are a view of the graph,
/// not a second source of authority. Re-mining them creates duplicates and can
/// invert lifecycle edges, so refuse the two explicit provenance markers used
/// by the project workflow.
fn graph_derived_spec(source: &str) -> bool {
    source.contains("https://moosedev.dev/kg/")
        || source.to_ascii_lowercase().contains("graph wins")
}

fn spec_schema(path: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action", "records"],
        "properties": {
            "action": {"type": "string", "const": "propose_spec_records"},
            "records": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_SPEC_RECORDS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "title", "description", "evidence"],
                    "properties": {
                        "kind": {"type": "string", "enum": ["Requirement", "Constraint"]},
                        "title": {"type": "string", "minLength": 1, "maxLength": MAX_SPEC_TITLE_BYTES},
                        "description": {"type": "string", "minLength": 1, "maxLength": MAX_SPEC_DESCRIPTION_BYTES},
                        "evidence": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_SPEC_EVIDENCE,
                            "items": {"type": "string", "pattern": format!("^{}:[1-9][0-9]*(?:-[1-9][0-9]*)?$", regex_escape(path))}
                        }
                    }
                }
            }
        }
    })
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn validate_spec_records(path: &str, source: &str, records: &[SpecRecordDraft]) -> Result<()> {
    anyhow::ensure!(
        !records.is_empty() && records.len() <= MAX_SPEC_RECORDS,
        "spec extraction requires 1..{MAX_SPEC_RECORDS} records"
    );
    let line_count = source.lines().count().max(1);
    let mut titles = std::collections::BTreeSet::new();
    for record in records {
        anyhow::ensure!(
            matches!(record.kind.as_str(), "Requirement" | "Constraint"),
            "spec extraction may only propose Requirement or Constraint records"
        );
        anyhow::ensure!(
            !record.title.trim().is_empty() && record.title.len() <= MAX_SPEC_TITLE_BYTES,
            "spec record title must contain 1..{MAX_SPEC_TITLE_BYTES} bytes"
        );
        anyhow::ensure!(
            !record.description.trim().is_empty()
                && record.description.len() <= MAX_SPEC_DESCRIPTION_BYTES,
            "spec record description must contain 1..{MAX_SPEC_DESCRIPTION_BYTES} bytes"
        );
        let normalized_title = record
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        anyhow::ensure!(
            titles.insert((record.kind.clone(), normalized_title)),
            "spec extraction contains a duplicate kind and title"
        );
        anyhow::ensure!(
            !record.evidence.is_empty() && record.evidence.len() <= MAX_SPEC_EVIDENCE,
            "spec record evidence requires 1..{MAX_SPEC_EVIDENCE} source line ranges"
        );
        for evidence in &record.evidence {
            validate_evidence(path, line_count, evidence)?;
        }
    }
    Ok(())
}

fn validate_evidence(path: &str, line_count: usize, evidence: &str) -> Result<()> {
    let range = evidence
        .strip_prefix(path)
        .and_then(|rest| rest.strip_prefix(':'))
        .with_context(|| {
            format!("spec evidence must reference the exact path {path}: {evidence}")
        })?;
    let (start, end) = match range.split_once('-') {
        Some((start, end)) => (start, end),
        None => (range, range),
    };
    let start: usize = start
        .parse()
        .with_context(|| format!("invalid spec evidence line: {evidence}"))?;
    let end: usize = end
        .parse()
        .with_context(|| format!("invalid spec evidence line: {evidence}"))?;
    let canonical = if start == end {
        format!("{path}:{start}")
    } else {
        format!("{path}:{start}-{end}")
    };
    anyhow::ensure!(
        evidence == canonical && start > 0 && start <= end && end <= line_count,
        "spec evidence is not a valid canonical line range within {path}: {evidence}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::runner::test_support::{context_router, serve, Project};
    use axum::{routing::post, Json};

    fn draft(evidence: &str) -> SpecRecordDraft {
        SpecRecordDraft {
            kind: "Requirement".into(),
            title: "Keep explicit approval".into(),
            description: "The user must explicitly approve the extracted records.".into(),
            evidence: vec![evidence.into()],
        }
    }

    fn preview(source: &str) -> SpecPrepareResponse {
        let draft = draft("spec.md:1");
        SpecPrepareResponse {
            operation_id: "9d8c95b7-79e3-465c-9db3-bb3cbf098cbc".into(),
            owner_id: String::new(),
            path: "spec.md".into(),
            source_sha256: sha256_hex(source),
            knowledge_revision: "fixture".into(),
            entries: vec![SpecPreviewEntry {
                draft,
                disposition: SpecDisposition::New {
                    iri: "https://moosedev.dev/kg/Requirement/spec-test".into(),
                },
                existing: None,
            }],
            retirements: vec![],
            component: None,
            previous_approval_iri: None,
            already_approved: false,
        }
    }

    #[test]
    fn covered_paths_are_repo_relative_and_deduplicated() {
        let covers =
            |raw: &[&str]| validate_covers(&raw.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            covers(&["badciv-map/", "./badciv-map/", "", ".", "spec.md"]).unwrap(),
            vec!["badciv-map/".to_string(), ".".into(), "spec.md".into()]
        );
        assert!(covers(&["../other/"]).is_err());
        assert!(covers(&["/abs/path"]).is_err());
        assert!(covers(&["a//b"]).is_err());
    }

    #[test]
    fn source_evidence_must_be_exact_canonical_and_in_bounds() {
        let source = "first\nsecond\nthird\n";
        for accepted in ["spec.md:1", "spec.md:2-3"] {
            validate_spec_records("spec.md", source, &[draft(accepted)]).unwrap();
        }
        for rejected in [
            "other.md:1",
            "spec.md:0",
            "spec.md:01",
            "spec.md:3-2",
            "spec.md:1-1",
            "spec.md:4",
            "spec.md:1 claim",
        ] {
            assert!(
                validate_spec_records("spec.md", source, &[draft(rejected)]).is_err(),
                "unexpectedly accepted {rejected}"
            );
        }
    }

    #[test]
    fn extraction_rejects_non_governing_kinds() {
        let mut record = draft("spec.md:1");
        record.kind = "ArchitecturalDecision".into();
        assert!(validate_spec_records("spec.md", "claim", &[record]).is_err());
    }

    #[test]
    fn schema_has_a_dedicated_action_and_path_bound_evidence() {
        let schema = spec_schema("docs/spec.v1.md");
        assert_eq!(
            schema["properties"]["action"]["const"],
            "propose_spec_records"
        );
        assert_eq!(
            schema["properties"]["records"]["items"]["properties"]["kind"]["enum"],
            json!(["Requirement", "Constraint"])
        );
        assert_eq!(
            schema["properties"]["records"]["items"]["properties"]["evidence"]["items"]["pattern"],
            "^docs/spec\\.v1\\.md:[1-9][0-9]*(?:-[1-9][0-9]*)?$"
        );
    }

    #[test]
    fn graph_derived_specs_are_not_mined_back_into_the_graph() {
        assert!(graph_derived_spec(
            "Derived from https://moosedev.dev/kg/Requirement/example"
        ));
        assert!(graph_derived_spec(
            "If this document disagrees with accepted knowledge, the graph wins."
        ));
        assert!(!graph_derived_spec(
            "The project knowledge graph must remain locally queryable."
        ));
    }

    #[tokio::test]
    async fn pending_preview_survives_restart_and_approval_returns_to_planning() {
        async fn approve(Json(request): Json<SpecApproveRequest>) -> Json<SpecApproveResponse> {
            let source = "The user must explicitly approve extracted records.\n";
            Json(SpecApproveResponse {
                operation_id: request.operation_id,
                path: "spec.md".into(),
                source_sha256: sha256_hex(source),
                base_revision: "fixture".into(),
                result_revision: "fixture".into(),
                records: vec![SpecApprovedRecord {
                    kind: "Requirement".into(),
                    title: "Keep explicit approval".into(),
                    iri: "https://moosedev.dev/kg/Requirement/spec-test".into(),
                    disposition: SpecDisposition::New {
                        iri: "https://moosedev.dev/kg/Requirement/spec-test".into(),
                    },
                }],
                retirements: vec![],
                approval_iri: "https://moosedev.dev/kg/ArchitecturalDecision/spec-test".into(),
                checkpoint: CheckpointResponse {
                    conforms: true,
                    durable: true,
                    revision: "fixture".into(),
                    pending: vec![],
                },
            })
        }

        let project = Project::new("spec-approval-runner");
        let source = "The user must explicitly approve extracted records.\n";
        std::fs::write(project.0.join("spec.md"), source).unwrap();
        let router = context_router().route("/api/v1/harness/spec/approve", post(approve));
        let (daemon, server) = serve(router, &project).await;
        let mut runner = Runner::create(
            project.0.clone(),
            daemon.clone(),
            "Implement an approved specification".into(),
        )
        .await
        .unwrap();
        let mut pending = preview(source);
        pending.owner_id = runner.task.id.clone();
        runner.task.pending_spec = Some(PendingSpecApproval { preview: pending });
        runner.task.phase = Phase::AwaitingSpecApproval;
        runner.task.plan = Some(Plan {
            summary: "stale pre-approval plan".into(),
            files: vec!["src/lib.rs".into()],
            checks: vec!["cargo check".into()],
        });
        runner.persist().unwrap();
        let id = runner.task.id.clone();
        drop(runner);

        let mut resumed = Runner::load(project.0.clone(), daemon, &id).unwrap();
        assert_eq!(resumed.task.phase, Phase::AwaitingSpecApproval);
        assert!(resumed.task.pending_spec.is_some());
        resumed.approve_spec().await.unwrap();
        assert_eq!(resumed.task.phase, Phase::Planning);
        assert_eq!(resumed.task.mode, Mode::Plan);
        assert!(resumed.task.pending_spec.is_none());
        assert!(resumed.task.plan.is_none());
        assert!(resumed
            .task
            .last_response
            .contains("execution still requires /approve"));
        server.abort();
    }
}
