//! Explicit, source-grounded specification approval.
//!
//! Extraction is an LLM sensor step. The runner validates every source
//! reference, the daemon reconciles it without touching the project graph,
//! and only a later human approval can commit the frozen preview.
use super::*;
use serde::Deserialize;
use serde_json::json;

/// Records one section extraction may propose; the batch as a whole is bounded
/// by `MAX_SPEC_RECORDS`.
const MAX_SECTION_RECORDS: usize = 16;
/// Adjacent sections are extracted together while their text stays under
/// this size, so a one-paragraph section does not cost a model call of its
/// own and a long one is never diluted by the rest of the file.
const SECTION_TARGET_BYTES: usize = 2_048;

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
        // A new /approve-spec is the human asking again: an earlier extraction
        // that exhausted its repairs does not count against this one.
        if self
            .task
            .recovery
            .as_ref()
            .is_some_and(|repair| repair.purpose == SPEC_EXTRACT_PURPOSE)
        {
            self.candidate_accepted();
        }
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
        // A source the graph already approved at exactly this digest is
        // prepared from its own records: deterministic, and the model is not
        // asked to re-extract claims it could only reword.
        let reused = self.current_spec_records(path, &source_sha256).await?;
        let records = match &reused {
            Some((records, approval_iri)) => {
                self.intent_event("spec_records_reused", approval_iri);
                self.event(format!(
                    "Reused {} record(s) of the current approval {approval_iri} for {path}: the source digest is unchanged, so nothing was extracted.",
                    records.len()
                ));
                records.clone()
            }
            None => self.extract_spec(path, &source).await?,
        };
        validate_spec_records(path, &source, &records)?;
        let uncited = uncited_lines(path, &source, &records);
        if !uncited.is_empty() {
            self.intent_event(
                "spec_uncited",
                &uncited
                    .iter()
                    .map(SpecUncited::describe)
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }

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
            drafts: records,
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
        self.task.pending_spec = Some(PendingSpecApproval { preview, uncited });
        self.task.mode = Mode::Plan;
        self.task.phase = Phase::AwaitingSpecApproval;
        self.task.turn_finished = true;
        self.task.last_response = format!(
            "Prepared {} source-grounded record(s) from {path}{}{}; review the spec approval gate.",
            request.drafts.len(),
            if reused.is_some() {
                " (reused from its current approval; the source is unchanged)"
            } else {
                ""
            },
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

    /// The current approval's records when the source is byte-identical to
    /// what it approved, with that approval's IRI. `None` when there is no
    /// approval, the digest differs, or the daemon predates the route.
    async fn current_spec_records(
        &mut self,
        path: &str,
        source_sha256: &str,
    ) -> Result<Option<(Vec<SpecRecordDraft>, String)>> {
        let request = SpecCurrentRequest {
            path: path.to_owned(),
        };
        let current: SpecCurrentResponse = match self.post("spec/current", &request).await {
            Ok(current) => current,
            Err(error)
                if error
                    .downcast_ref::<HttpFailure>()
                    .is_some_and(|failure| failure.status == 404) =>
            {
                self.event(
                    "The daemon has no spec/current route; extracting the specification instead.",
                );
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        anyhow::ensure!(
            current.path == path,
            "daemon answered spec/current for a different path"
        );
        Ok(match (current.approval_iri, current.source_sha256) {
            (Some(iri), Some(sha)) if sha == source_sha256 && !current.drafts.is_empty() => {
                Some((current.drafts, iri))
            }
            _ => None,
        })
    }

    /// Extract a specification one section at a time. A single call over a
    /// whole file let a small model settle for a few one-line summaries and
    /// skip whole sections (badciv, 2026-09-23: 95 lines became 8 records,
    /// every table and per-item rule gone); a section at a time keeps its
    /// tables, lists and grammars in view. Each section's output is validated
    /// on its own and repaired with the diagnostic like any sensor output.
    async fn extract_spec(&mut self, path: &str, source: &str) -> Result<Vec<SpecRecordDraft>> {
        let lines: Vec<&str> = source.lines().collect();
        let headings = line_headings(&lines);
        let title = document_title(&lines);
        let sections = spec_sections(&lines, &headings);
        let mut records: Vec<SpecRecordDraft> = Vec::new();
        for (index, section) in sections.iter().enumerate() {
            // Ephemeral, not journaled: a section is one model call that can
            // take minutes, and the human should see which one is running.
            if let Some(progress) = &self.progress {
                let _ = progress.send(crate::harness::progress::Progress::Status(format!(
                    "Extracting {path}: part {} of {} (lines {}-{}, {})…",
                    index + 1,
                    sections.len(),
                    section.start,
                    section.end,
                    if section.heading.is_empty() {
                        "untitled"
                    } else {
                        &section.heading
                    }
                )));
            }
            let prompt = spec_prompt(
                path,
                source,
                &lines,
                &title,
                section,
                index + 1,
                sections.len(),
            );
            let proposed = loop {
                match self.extract_section(path, &lines, &prompt, section).await {
                    Ok(proposed) => {
                        self.candidate_accepted();
                        break proposed;
                    }
                    Err(error) => {
                        if !self.repair_candidate(&error)? {
                            return Err(error);
                        }
                    }
                }
            };
            for mut record in proposed {
                distinguish_title(&mut record, &records, &headings);
                records.push(record);
            }
        }
        anyhow::ensure!(
            !records.is_empty(),
            "no section of {path} states a requirement or constraint the extraction could find"
        );
        self.event(format!(
            "Extracted {} record(s) from {path} in {} section(s).",
            records.len(),
            sections.len()
        ));
        Ok(records)
    }

    /// One section's records, each checked against the lines the model saw.
    async fn extract_section(
        &mut self,
        path: &str,
        lines: &[&str],
        prompt: &str,
        section: &SpecSection,
    ) -> Result<Vec<SpecRecordDraft>> {
        let mut extracted: SpecExtraction = self
            .model_json(prompt, SPEC_EXTRACT_PURPOSE, spec_schema(path))
            .await?;
        // Gemma under JSON-schema decoding writes each curly quote as \u0002,
        // identically on every retry (badciv, 2026-09-23), so a repair cannot
        // fix it. The section's own text can: a character whose surroundings
        // occur in the source with exactly one character between them is that
        // character. Anything not restored this way still fails validation.
        let source: Vec<char> = lines[section.start - 1..section.end]
            .join("\n")
            .chars()
            .collect();
        let mut restored = Vec::new();
        for record in &mut extracted.records {
            let mut count = 0;
            for field in [&mut record.title, &mut record.description] {
                if let Some((text, fixed)) = restore_from_source(field, &source) {
                    if fixed > 0 {
                        *field = text;
                        count += fixed;
                    }
                }
            }
            if count > 0 {
                restored.push(format!("{} ({count})", record.title));
            }
        }
        if !restored.is_empty() {
            let detail = format!(
                "{path}:{}-{}: control characters restored from the source in {}",
                section.start,
                section.end,
                restored.join(", ")
            );
            self.intent_event("spec_text_restored", &detail);
            self.event(format!("Spec extraction: {detail}."));
        }
        let checked = (|| {
            anyhow::ensure!(
                extracted.action == "propose_spec_records",
                "spec extraction returned the wrong action"
            );
            anyhow::ensure!(
                extracted.records.len() <= MAX_SECTION_RECORDS,
                "a section may propose at most {MAX_SECTION_RECORDS} records"
            );
            for record in &extracted.records {
                validate_spec_record(path, section.end, record)?;
                for evidence in &record.evidence {
                    let (start, _) = evidence_lines(path, evidence)?;
                    anyhow::ensure!(
                        start >= section.start,
                        "evidence {evidence} is outside the lines shown ({}-{})",
                        section.start,
                        section.end
                    );
                }
            }
            Ok(())
        })();
        checked.map_err(|error| error.context(super::model::InvalidModelOutput))?;
        Ok(extracted.records)
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
        // A task that existed only to approve this spec has met its
        // objective; planning under it would ask the model to plan an
        // approval that already happened. A spec approved inside other work
        // leaves that work's objective alone.
        self.task.objective_pending |=
            self.task.objective == spec_approval_objective(&response.path);
        self.task.last_response = if self.task.objective_pending {
            format!(
                "Approved {} spec record(s) from {}. Describe what to do next; your message becomes this task's objective, and execution still requires /approve.",
                response.records.len(), response.path
            )
        } else {
            format!(
                "Approved {} spec record(s) from {}. Returned to Planning; execution still requires /approve.",
                response.records.len(), response.path
            )
        };
        self.event(format!(
            "Human approved specification {} as {}; accepted revision {}. Returned to Planning; execution was not approved.",
            response.path, response.approval_iri, response.result_revision
        ));
        self.persist()
    }
}

pub(super) const SPEC_EXTRACT_PURPOSE: &str = "harness_spec_extract";

/// The objective of a task started by `/approve-spec <path>`. Approval
/// fulfils it, so the task then waits for the human to name the next one.
pub fn spec_approval_objective(path: &str) -> String {
    format!("Approve specification {path}")
}

/// A run of the specification's lines extracted by one model call, with its
/// original 1-based, inclusive line numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecSection {
    start: usize,
    end: usize,
    heading: String,
}

fn spec_prompt(
    path: &str,
    source: &str,
    lines: &[&str],
    title: &str,
    section: &SpecSection,
    number: usize,
    total: usize,
) -> String {
    let numbered = (section.start..=section.end)
        .map(|line| format!("{line}: {}", lines[line - 1]))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are the extraction sensor for MOOSEDev's symbolic project memory. You are reading part {number} of {total} of a specification, lines {start}-{end} of {count}. Extract every requirement and hard constraint this part states. Record only what the cited lines state: never add an implication, a goal of your own, or a choice the text does not make. A statement counts whatever its grammar: the system's stated structure and each named component's responsibility, the identity, goals, rules and text a persona or agent is given, and the behaviour the system is meant to produce are Requirements when this part states them. Background story and motivation state nothing and are not records. Use kind exactly Requirement or Constraint: a Constraint is a hard rule the implementation must not violate (a limit, invariant, prohibition or required format); a Requirement is a capability or outcome the system must provide.\n\nEach description states its claim completely, in the specification's own terms: every value, name, table row, list item, grammar production and exception the cited lines give. A table, grammar or list of per-item rules is one record whose description restates all of it; never reduce it to a one-sentence summary. Separate claims are separate records. Copy quotation marks and other punctuation exactly as the specification writes them. Each evidence entry must be only an exact 1-based line reference in the form {path}:<line> or {path}:<start>-<end>, citing exactly the lines that state the claim, every row of a table or list included. If this part states no requirement or constraint, return an empty records list. Produce no more than {MAX_SECTION_RECORDS} non-duplicate records.\n\nDocument title: {title}\nSpecification path: {path}\nSpecification sha256: {sha}\nLine-addressed part ({heading}):\n{numbered}",
        start = section.start,
        end = section.end,
        count = lines.len(),
        heading = if section.heading.is_empty() { "untitled" } else { &section.heading },
        sha = sha256_hex(source),
    )
}

/// Replace each control character (other than newline and tab) in `text` with
/// the one character the source has between the same surroundings. They are
/// restored left to right, so the left context may run over characters
/// already restored (they are source text now); the right context stops at
/// the next one still garbled. At most 16 characters a side, narrowed to 8
/// and then 4 when the wider context does not occur; at least 6 characters
/// of context are needed.
/// Every occurrence must agree on the character. Returns the text and how many
/// characters were restored, or `None` when any one cannot be restored.
fn restore_from_source(text: &str, source: &[char]) -> Option<(String, usize)> {
    let mut chars: Vec<char> = text.chars().collect();
    let garbled: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_control() && !matches!(c, '\n' | '\t'))
        .map(|(at, _)| at)
        .collect();
    for (position, &at) in garbled.iter().enumerate() {
        let ceiling = garbled.get(position + 1).copied().unwrap_or(chars.len());
        let mut restored = None;
        for window in [16, 8, 4] {
            let left = &chars[at.saturating_sub(window)..at];
            let right = &chars[at + 1..(at + 1 + window).min(ceiling)];
            if left.len() + right.len() < 6 {
                continue;
            }
            let mut found: Option<char> = None;
            for candidate in left.len()..source.len().saturating_sub(right.len()) {
                let character = source[candidate];
                if character.is_control()
                    || source[candidate - left.len()..candidate] != *left
                    || source[candidate + 1..candidate + 1 + right.len()] != *right
                {
                    continue;
                }
                match found {
                    Some(other) if other != character => return None,
                    _ => found = Some(character),
                }
            }
            if found.is_some() {
                restored = found;
                break;
            }
        }
        chars[at] = restored?;
    }
    Some((chars.into_iter().collect(), garbled.len()))
}

/// The heading level of a markdown ATX heading line.
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line
        .strip_prefix("   ")
        .or_else(|| line.strip_prefix("  "))
        .or_else(|| line.strip_prefix(' '))
        .unwrap_or(line);
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    ((1..=6).contains(&level)
        && trimmed[level..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace))
    .then_some(level)
}

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// For every line, the text of the nearest heading at or above it, ignoring
/// `#` lines inside code fences.
fn line_headings(lines: &[&str]) -> Vec<String> {
    let mut current = String::new();
    let mut fenced = false;
    lines
        .iter()
        .map(|line| {
            if is_fence(line) {
                fenced = !fenced;
            } else if !fenced && heading_level(line).is_some() {
                current = line.trim().to_string();
            }
            current.clone()
        })
        .collect()
}

fn heading_name(heading: &str) -> &str {
    heading.trim_start_matches('#').trim()
}

fn document_title(lines: &[&str]) -> String {
    lines
        .iter()
        .find(|line| heading_level(line) == Some(1))
        .map(|line| heading_name(line).to_string())
        .unwrap_or_default()
}

/// Split at level-2 headings, then merge adjacent sections while the merged
/// text stays under `SECTION_TARGET_BYTES`. A part still over that size is
/// split again at its level-3 headings, and so on down to level 6, so a long
/// section with subsections is never read as one part (badciv-map.md's
/// `## Sections`: five tables in one part, more records than a part may
/// propose). A part with no heading left to split on stays whole: a table is
/// never cut. A part holding nothing but headings joins the next part (the
/// last one joins the previous), so a heading that states a rule on its own
/// is still read.
fn spec_sections(lines: &[&str], headings: &[String]) -> Vec<SpecSection> {
    let mut levels = vec![None; lines.len()];
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        if is_fence(line) {
            fenced = !fenced;
        } else if !fenced {
            levels[index] = heading_level(line);
        }
    }
    let merged = split_part(lines, &levels, 0, lines.len(), 2);
    let has_body = |from: usize, to: usize| {
        lines[from..to]
            .iter()
            .any(|line| !line.trim().is_empty() && heading_level(line).is_none())
    };
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut waiting: Option<usize> = None;
    for (from, to) in merged {
        let from = waiting.take().unwrap_or(from);
        if has_body(from, to) {
            parts.push((from, to));
        } else {
            waiting = Some(from);
        }
    }
    if waiting.is_some() {
        if let Some(last) = parts.last_mut() {
            last.1 = lines.len();
        }
    }
    parts
        .into_iter()
        .map(|(from, to)| SpecSection {
            start: from + 1,
            end: to,
            heading: headings[from].clone(),
        })
        .collect()
}

/// `from..to` as parts: whole when it fits the target or has no heading of
/// `level` or deeper to split on; otherwise cut at its level-`level` headings,
/// each piece split at the next level down, and adjacent pieces merged while
/// the merged text fits.
fn split_part(
    lines: &[&str],
    levels: &[Option<usize>],
    from: usize,
    to: usize,
    level: usize,
) -> Vec<(usize, usize)> {
    let bytes = |from: usize, to: usize| {
        lines[from..to]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
    };
    if level > 6 || bytes(from, to) <= SECTION_TARGET_BYTES {
        return vec![(from, to)];
    }
    let mut bounds: Vec<usize> = (from + 1..to)
        .filter(|&index| levels[index] == Some(level))
        .collect();
    if bounds.is_empty() {
        return split_part(lines, levels, from, to, level + 1);
    }
    bounds.insert(0, from);
    bounds.push(to);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for window in bounds.windows(2) {
        for (start, end) in split_part(lines, levels, window[0], window[1], level + 1) {
            match merged.last_mut() {
                Some(last) if bytes(last.0, end) <= SECTION_TARGET_BYTES => last.1 = end,
                _ => merged.push((start, end)),
            }
        }
    }
    merged
}

fn normalized_title(title: &str) -> String {
    spec_title_key(title)
}

/// Two sections may each name a record the same way ("Error format"). The
/// later one takes its section's heading rather than failing the batch.
fn distinguish_title(
    record: &mut SpecRecordDraft,
    earlier: &[SpecRecordDraft],
    headings: &[String],
) {
    let taken = |title: &str| {
        earlier.iter().any(|other| {
            other.kind == record.kind && normalized_title(&other.title) == normalized_title(title)
        })
    };
    if !taken(&record.title) {
        return;
    }
    let section = record
        .evidence
        .first()
        .and_then(|evidence| evidence.rsplit_once(':'))
        .and_then(|(_, range)| range.split('-').next()?.parse::<usize>().ok())
        .and_then(|line| headings.get(line.checked_sub(1)?))
        .map(|heading| heading_name(heading).to_string())
        .unwrap_or_default();
    let candidates = (!section.is_empty())
        .then(|| format!("{} — {section}", record.title))
        .into_iter()
        .chain((2..).map(|number| format!("{} ({number})", record.title)));
    for candidate in candidates {
        if candidate.len() <= MAX_SPEC_TITLE_BYTES && !taken(&candidate) {
            record.title = candidate;
            return;
        }
        if candidate.len() > MAX_SPEC_TITLE_BYTES + 8 {
            return;
        }
    }
}

/// Lines no record cites, grouped into ranges under their nearest heading.
/// Blank lines, fences and rules neither open nor close a range; a heading
/// closes one, so each range belongs to one section.
fn uncited_lines(path: &str, source: &str, records: &[SpecRecordDraft]) -> Vec<SpecUncited> {
    let lines: Vec<&str> = source.lines().collect();
    let headings = line_headings(&lines);
    let mut cited = vec![false; lines.len()];
    for evidence in records.iter().flat_map(|record| &record.evidence) {
        if let Ok((start, end)) = evidence_lines(path, evidence) {
            for line in start..=end.min(lines.len()) {
                cited[line - 1] = true;
            }
        }
    }
    let mut ranges: Vec<SpecUncited> = Vec::new();
    let mut open = false;
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        // A `#` line inside a fence is content (a comment in a grammar or an
        // example), not a heading, and may itself be uncited.
        if !fenced && heading_level(line).is_some() {
            open = false;
            continue;
        }
        if trimmed.is_empty()
            || trimmed
                .chars()
                .all(|character| matches!(character, '-' | '=' | '*' | '_' | '|' | ':' | ' '))
        {
            continue;
        }
        if cited[index] {
            open = false;
        } else if open {
            ranges.last_mut().unwrap().end = index + 1;
        } else {
            ranges.push(SpecUncited {
                start: index + 1,
                end: index + 1,
                heading: headings[index].clone(),
            });
            open = true;
        }
    }
    ranges
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
                "minItems": 0,
                "maxItems": MAX_SECTION_RECORDS,
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
        validate_spec_record(path, line_count, record)?;
        anyhow::ensure!(
            titles.insert((record.kind.clone(), normalized_title(&record.title))),
            "spec extraction contains a duplicate kind and title"
        );
    }
    Ok(())
}

/// One record's shape, with every evidence range inside lines `1..=line_count`.
fn validate_spec_record(path: &str, line_count: usize, record: &SpecRecordDraft) -> Result<()> {
    check_spec_draft(record).map_err(anyhow::Error::msg)?;
    for evidence in &record.evidence {
        validate_evidence(path, line_count, evidence)?;
    }
    Ok(())
}

/// The 1-based inclusive range a canonical evidence entry names.
fn evidence_lines(path: &str, evidence: &str) -> Result<(usize, usize)> {
    let range = evidence
        .strip_prefix(path)
        .and_then(|rest| rest.strip_prefix(':'))
        .with_context(|| {
            format!("spec evidence must reference the exact path {path}: {evidence}")
        })?;
    let (start, end) = range.split_once('-').unwrap_or((range, range));
    let start: usize = start
        .parse()
        .with_context(|| format!("invalid spec evidence line: {evidence}"))?;
    let end: usize = end
        .parse()
        .with_context(|| format!("invalid spec evidence line: {evidence}"))?;
    Ok((start, end))
}

fn validate_evidence(path: &str, line_count: usize, evidence: &str) -> Result<()> {
    let (start, end) = evidence_lines(path, evidence)?;
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
    use axum::{extract::State, routing::post, Json};
    use std::{path::PathBuf, sync::Arc};

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

    #[test]
    fn sections_split_at_level_two_keep_line_numbers_and_merge_small_ones() {
        let long = "x".repeat(2_100);
        let source = format!(
            "# Title\nIntro.\n## A\na\n## B\n{long}\n```\n## not a heading\n```\n## C\nc\n## D\n### only a sub-heading\n"
        );
        let lines: Vec<&str> = source.lines().collect();
        let headings = line_headings(&lines);
        let sections = spec_sections(&lines, &headings);
        assert_eq!(
            sections,
            vec![
                SpecSection { start: 1, end: 4, heading: "# Title".into() },
                SpecSection { start: 5, end: 9, heading: "## B".into() },
                SpecSection { start: 10, end: 13, heading: "## C".into() },
            ],
            "the preamble merges with A; B is too large to take C; D holds only headings and is merged into C"
        );
        assert_eq!(document_title(&lines), "Title");
        assert_eq!(
            headings[7], "## B",
            "a # line inside a fence is not a heading"
        );
    }

    #[test]
    fn an_oversized_part_splits_at_its_sub_headings() {
        // badciv-map.md's shape: one level-2 section over the target whose
        // subsections each carry their own table.
        let table = |name: &str| format!("### `[{name}]`\n\n{}", "| c | meaning |\n".repeat(70));
        let source = format!(
            "# Map\nIntro.\n## Sections\nEach grid is height lines.\n{}{}```\n### not a heading\n```\n{}## Tail\n{}\n",
            table("terrain"),
            table("climate"),
            table("resource"),
            "y".repeat(2_100),
        );
        let lines: Vec<&str> = source.lines().collect();
        let headings = line_headings(&lines);
        let sections = spec_sections(&lines, &headings);

        // Every line exactly once, in order.
        assert_eq!(sections.first().unwrap().start, 1);
        assert_eq!(sections.last().unwrap().end, lines.len());
        for pair in sections.windows(2) {
            assert_eq!(pair[1].start, pair[0].end + 1, "{sections:?}");
        }
        let heading_of = |line: &str| {
            sections
                .iter()
                .find(|section| lines[section.start - 1] == line)
                .map(|section| section.heading.clone())
        };
        // The oversized section is cut at its subsections, and a sub-heading
        // inside a fence cuts nothing.
        assert_eq!(
            heading_of("### `[climate]`").as_deref(),
            Some("### `[climate]`")
        );
        assert_eq!(
            heading_of("### `[resource]`").as_deref(),
            Some("### `[resource]`")
        );
        assert!(heading_of("### not a heading").is_none());
        let bytes = |section: &SpecSection| {
            lines[section.start - 1..section.end]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
        };
        for section in &sections {
            let fits = bytes(section) <= SECTION_TARGET_BYTES;
            let tail = section.heading == "## Tail";
            assert!(fits || tail, "{section:?} is {} bytes", bytes(section));
        }
        // A part over the target with nothing left to split on stays whole.
        let tail = sections.last().unwrap();
        assert_eq!(
            (tail.heading.as_str(), bytes(tail) > SECTION_TARGET_BYTES),
            ("## Tail", true)
        );
    }

    #[test]
    fn uncited_ranges_name_their_section_and_skip_headings_blanks_and_rules() {
        let source = "# T\nintro\n\n## Cited\none\ntwo\n\n## Left out\n| a | b |\n|---|---|\n| 1 | 2 |\n\nmore\n## Tail\nlast\n";
        let records = vec![SpecRecordDraft {
            kind: "Constraint".into(),
            title: "Cited".into(),
            description: "one two".into(),
            evidence: vec!["spec.md:5-6".into()],
        }];
        let uncited = uncited_lines("spec.md", source, &records);
        assert_eq!(
            uncited
                .iter()
                .map(SpecUncited::describe)
                .collect::<Vec<_>>(),
            vec![
                "line 2 (# T)".to_string(),
                "lines 9-13 (## Left out)".into(),
                "line 15 (## Tail)".into(),
            ]
        );
    }

    #[test]
    fn a_garbled_character_is_restored_only_from_unambiguous_source_context() {
        let source: Vec<char> =
            "Rules:\n- Do not “play tall” or wait for a perfect plan.\nA wounded enemy is food."
                .chars()
                .collect();
        assert_eq!(
            restore_from_source(
                "Do not \u{2}play tall\u{2} or wait for a perfect plan.",
                &source
            ),
            Some((
                "Do not “play tall” or wait for a perfect plan.".to_string(),
                2
            ))
        );
        assert_eq!(
            restore_from_source("A wounded enemy is food.", &source),
            Some(("A wounded enemy is food.".to_string(), 0)),
            "nothing to restore"
        );
        assert_eq!(
            restore_from_source("Never \u{2}hesitate\u{2} at all.", &source),
            None,
            "surroundings the source does not contain restore nothing"
        );
        let ambiguous: Vec<char> = "say “yes” now\nsay ‘yes” now".chars().collect();
        assert_eq!(
            restore_from_source("say \u{2}yes\u{2} now", &ambiguous),
            None,
            "two different characters between the same surroundings is a guess"
        );
        assert_eq!(
            restore_from_source("a\u{2}b", &source),
            None,
            "too little context to trust"
        );
    }

    #[test]
    fn a_section_of_only_a_heading_is_still_read() {
        // Too large to merge with its neighbours, a heading that states a rule
        // on its own must reach an extraction call rather than be dropped.
        let long = "x".repeat(2_100);
        let source = format!("# T\n## A\n{long}\n## Names are never rewritten\n");
        let lines: Vec<&str> = source.lines().collect();
        let headings = line_headings(&lines);
        assert_eq!(
            spec_sections(&lines, &headings),
            vec![SpecSection {
                start: 1,
                end: 4,
                heading: "# T".into()
            }],
            "the title joins A, and the trailing heading joins the part before it"
        );
    }

    #[test]
    fn a_hash_line_inside_a_fence_can_be_uncited() {
        let source = "## Grammar\n```\n# comment lines start with a hash\nrow := cell+\n```\n";
        let records = vec![SpecRecordDraft {
            kind: "Constraint".into(),
            title: "Rows".into(),
            description: "row := cell+".into(),
            evidence: vec!["spec.md:4".into()],
        }];
        assert_eq!(
            uncited_lines("spec.md", source, &records)
                .iter()
                .map(SpecUncited::describe)
                .collect::<Vec<_>>(),
            vec!["line 3 (## Grammar)".to_string()]
        );
    }

    #[test]
    fn titles_collide_the_way_the_daemon_compares_them() {
        let lines = ["## A", "Étiquette text.", "## B", "étiquette text."];
        let headings = line_headings(&lines);
        let first = SpecRecordDraft {
            kind: "Constraint".into(),
            title: "Étiquette".into(),
            description: "Étiquette text.".into(),
            evidence: vec!["spec.md:2".into()],
        };
        let mut second = SpecRecordDraft {
            title: "étiquette".into(),
            description: "étiquette text.".into(),
            evidence: vec!["spec.md:4".into()],
            ..first.clone()
        };
        distinguish_title(&mut second, std::slice::from_ref(&first), &headings);
        assert_eq!(second.title, "étiquette — B");
        assert!(validate_spec_records("spec.md", &lines.join("\n"), &[first, second]).is_ok());
    }

    #[test]
    fn a_repeated_title_takes_its_section_heading() {
        let lines = [
            "# T",
            "## Maps",
            "Error text names the line.",
            "## Units",
            "Error text names the unit.",
        ];
        let headings = line_headings(&lines);
        let record = |line: usize| SpecRecordDraft {
            kind: "Constraint".into(),
            title: "Error format".into(),
            description: lines[line - 1].into(),
            evidence: vec![format!("spec.md:{line}")],
        };
        let first = record(3);
        let mut second = record(5);
        distinguish_title(&mut second, std::slice::from_ref(&first), &headings);
        assert_eq!(second.title, "Error format — Units");
        let mut third = record(5);
        distinguish_title(&mut third, &[first.clone(), second.clone()], &headings);
        assert_eq!(third.title, "Error format (2)");
        let mut requirement = record(5);
        requirement.kind = "Requirement".into();
        distinguish_title(&mut requirement, &[first], &headings);
        assert_eq!(
            requirement.title, "Error format",
            "only the same kind collides"
        );
    }

    #[tokio::test]
    async fn extraction_runs_per_section_and_repeated_approvals_never_exhaust_repairs() {
        async fn model(State(_): State<Arc<PathBuf>>, Json(body): Json<Value>) -> Json<Value> {
            let tools = body["tools"].as_array().is_some();
            let prompt = body["messages"].to_string();
            // Nothing asks the provider to enforce a schema: the prompt
            // carries it, so requests are told apart by what they say.
            assert!(body.get("response_format").is_none(), "{body}");
            if tools || prompt.contains("neutral connection test") {
                return Json(if tools {
                    json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"probe","type":"function","function":{"name":"ready","arguments":"{\"status\":\"ok\"}"}}]},"finish_reason":"tool_calls"}]})
                } else {
                    json!({"choices":[{"message":{"role":"assistant","content":"{\"status\":\"ok\"}"},"finish_reason":"stop"}]})
                });
            }
            assert!(
                prompt.contains("extraction sensor") && prompt.contains("Required JSON schema:")
            );
            let mut fenced = false;
            let records = if prompt.contains("| small | 3 |") {
                // Answered the way LM Studio answers without response_format:
                // inside a markdown fence.
                fenced = true;
                assert!(prompt.contains("5: | small | 3 |"), "original line numbers");
                json!([{"kind":"Constraint","title":"Size limits","description":"| size | max |: small 3, large 9.","evidence":["spec.md:3-6"]}])
            } else if prompt.contains("Widgets must be “blue”.") {
                assert!(prompt.contains("9: Widgets must be “blue”."), "{prompt}");
                // Gemma writes curly quotes as U+0002 (badciv 2026-09-23). The
                // first answer garbles text the source does not contain, so it
                // cannot be restored and goes back to the model; every later
                // answer garbles the source's own quotes, which are restored
                // from the section text without another call.
                static CALLS: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    json!([{"kind":"Constraint","title":"Size limits","description":"Paint every \u{2}widget\u{2} now.","evidence":["spec.md:9"]}])
                } else {
                    assert_eq!(
                        prompt.contains("control character U+0002"),
                        call == 1,
                        "only the repair names the character"
                    );
                    json!([{"kind":"Constraint","title":"Size limits","description":"Widgets must be \u{2}blue\u{2}.","evidence":["spec.md:9"]}])
                }
            } else {
                json!([])
            };
            let mut content =
                json!({"action":"propose_spec_records","records":records}).to_string();
            if fenced {
                content = format!("```json\n{content}\n```");
            }
            Json(
                json!({"choices":[{"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}),
            )
        }
        async fn prepare(Json(request): Json<SpecPrepareRequest>) -> Json<SpecPrepareResponse> {
            Json(SpecPrepareResponse {
                operation_id: request.operation_id,
                owner_id: request.owner_id,
                path: request.path,
                source_sha256: request.source_sha256,
                knowledge_revision: request.knowledge_revision,
                entries: request
                    .drafts
                    .into_iter()
                    .enumerate()
                    .map(|(index, draft)| SpecPreviewEntry {
                        draft,
                        disposition: SpecDisposition::New {
                            iri: format!("https://moosedev.dev/kg/Constraint/{index}"),
                        },
                        existing: None,
                    })
                    .collect(),
                retirements: vec![],
                component: None,
                previous_approval_iri: None,
                already_approved: false,
            })
        }
        async fn checkpoint() -> Json<CheckpointResponse> {
            Json(CheckpointResponse {
                conforms: true,
                durable: true,
                revision: "fixture".into(),
                pending: vec![],
            })
        }

        let long = "p".repeat(2_000);
        let source = format!(
            "# Widgets\n## Sizes\n| size | max |\n|---|---|\n| small | 3 |\n| large | 9 |\n{long}\n## Colors\nWidgets must be “blue”.\n## Notes\nCommentary nobody cites.\n"
        );
        let project = Project::new("spec-sections");
        std::fs::write(project.0.join("spec.md"), &source).unwrap();
        let router = context_router()
            .route("/v1/chat/completions", post(model))
            .route("/api/v1/harness/spec/prepare", post(prepare))
            .route("/api/v1/harness/checkpoint", axum::routing::get(checkpoint));
        let (daemon, server) = serve(router, &project).await;
        let mut runner = Runner::create(
            project.0.clone(),
            daemon.clone(),
            "Approve specification spec.md".into(),
        )
        .await
        .unwrap();
        runner.configure(
            crate::llm::LlmConfig {
                base_url: format!("{daemon}/v1"),
                ..crate::harness::runner::test_support::test_config()
            },
            None,
        );
        // Four approvals of one task: each is a fresh human request, and a
        // successful section must not leave its attempt charged.
        for _ in 0..4 {
            runner.begin_spec_approval("spec.md", &[]).await.unwrap();
        }
        let pending = runner.task.pending_spec.as_ref().unwrap();
        let titles: Vec<_> = pending
            .preview
            .entries
            .iter()
            .map(|entry| entry.draft.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Size limits", "Size limits — Colors"]);
        assert_eq!(
            pending
                .uncited
                .iter()
                .map(SpecUncited::describe)
                .collect::<Vec<_>>(),
            vec!["line 7 (## Sizes)".to_string(), "line 11 (## Notes)".into()]
        );
        assert!(runner.task.recovery.is_none());
        assert!(runner
            .task
            .intent_events
            .iter()
            .any(|event| event.kind == "json_recovered"
                && event.detail == "harness_spec_extract: fence"));
        assert_eq!(
            pending.preview.entries[1].draft.description,
            "Widgets must be “blue”."
        );
        assert!(runner.task.intent_events.iter().any(|event| {
            event.kind
            == "spec_text_restored"
            && event.detail
                == "spec.md:8-11: control characters restored from the source in Size limits (2)"
        }));
        let extractions = runner
            .task
            .model_requests
            .iter()
            .filter(|request| request["purpose"] == SPEC_EXTRACT_PURPOSE)
            .count();
        assert_eq!(
            extractions, 9,
            "two sections (Sizes alone, Notes joined to Colors; the title line holds nothing to extract), four approvals, one repair"
        );
        server.abort();
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
        runner.task.pending_spec = Some(PendingSpecApproval {
            preview: pending,
            uncited: Vec::new(),
        });
        runner.task.phase = Phase::AwaitingSpecApproval;
        runner.task.plan = Some(Plan {
            summary: "stale pre-approval plan".into(),
            files: vec!["src/lib.rs".into()],
            checks: vec!["cargo check".into()],
            addresses: vec![],
        });
        runner.persist().unwrap();
        let id = runner.task.id.clone();
        drop(runner);

        let mut resumed = Runner::load(project.0.clone(), daemon, &id).unwrap();
        assert_eq!(resumed.task.phase, Phase::AwaitingSpecApproval);
        assert!(resumed.task.pending_spec.is_some());
        resumed.approve_spec().await.unwrap();
        assert!(
            !resumed.task.objective_pending,
            "a spec approved inside other work keeps that work's objective"
        );
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

    #[tokio::test]
    async fn an_approval_task_waits_for_its_next_objective_without_a_model_call() {
        async fn approve(Json(request): Json<SpecApproveRequest>) -> Json<SpecApproveResponse> {
            let source = "The user must explicitly approve extracted records.\n";
            Json(SpecApproveResponse {
                operation_id: request.operation_id,
                path: "spec.md".into(),
                source_sha256: sha256_hex(source),
                base_revision: "fixture".into(),
                result_revision: "fixture".into(),
                records: vec![],
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
        let project = Project::new("spec-objective");
        let source = "The user must explicitly approve extracted records.\n";
        std::fs::write(project.0.join("spec.md"), source).unwrap();
        let router = context_router().route("/api/v1/harness/spec/approve", post(approve));
        let (daemon, server) = serve(router, &project).await;
        let mut runner = Runner::create(
            project.0.clone(),
            daemon,
            spec_approval_objective("spec.md"),
        )
        .await
        .unwrap();
        let mut pending = preview(source);
        pending.owner_id = runner.task.id.clone();
        runner.task.pending_spec = Some(PendingSpecApproval {
            preview: pending,
            uncited: Vec::new(),
        });
        runner.task.phase = Phase::AwaitingSpecApproval;
        runner.approve_spec().await.unwrap();
        assert!(runner.task.objective_pending);
        assert_eq!(runner.task.phase, Phase::Planning, "Constraint a971fcb1");
        assert!(runner
            .task
            .last_response
            .contains("your message becomes this task's objective"));

        let error = runner.advance().await.unwrap_err();
        assert!(
            format!("{error:#}").contains("describe what to do next"),
            "{error:#}"
        );
        assert!(
            runner.task.model_requests.is_empty(),
            "no model was asked to plan"
        );

        runner
            .submit_message("Implement the main spec".into())
            .await
            .unwrap();
        assert_eq!(runner.task.objective, "Implement the main spec");
        assert!(runner.task.guidance.is_empty());
        assert!(!runner.task.objective_pending);
        assert!(runner.task.intent_events.iter().any(
            |event| event.kind == "objective_set" && event.detail == "Implement the main spec"
        ));

        // A later message is guidance under that objective, as before.
        runner
            .submit_message("Start with the map crate".into())
            .await
            .unwrap();
        assert_eq!(runner.task.objective, "Implement the main spec");
        assert_eq!(runner.task.guidance, "Start with the map crate");
        server.abort();
    }
}
