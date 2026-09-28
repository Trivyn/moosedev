//! Human decisions on a task: plan, edit and permission approval, review
//! requests, steering messages, returning to Plan, and answering a question.
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
        self.record_approved_plan(&context);
        let edits = self.task.edits.len();
        let state = self.symbolic_state_mut();
        state.unchanged_since_approval = true;
        state.cycle_replan_continuations = 0;
        state.auto_verifications = 0;
        // A new approval starts a new cycle: only edits made under it count,
        // and no arm from before it may fire.
        state.auto_verify_armed = None;
        state.cycle_edit_start = edits;
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
        let (file, existed, after) = (edit.file.clone(), edit.before.is_some(), edit.after.clone());
        self.apply_edit(edit)?;
        let fresh = self
            .check_applied_edit(&file, existed, after.as_deref())
            .await;
        self.arm_auto_verify(fresh);
        self.persist()
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
            anyhow::ensure!(
                self.task.intent.is_none() || self.task.phase == Phase::AwaitingInput,
                "reconcile interrupted action first"
            );
        }
        let interrupted = self.task.phase == Phase::AwaitingInput && self.task.intent.is_some();
        // When the model itself handed approved work back (a reply or a
        // question), the harness judges the message against the approved plan
        // (Disposition). Treating every message as new guidance returned the
        // task to Plan, so "continue with the plan" in badciv 1e6cd3e7 cost a
        // replan and its approval. A harness park (scope escapes, a check
        // nothing can grant) still replans, as does /plan; in Plan a message
        // is guidance as before.
        let judged = self.task.phase == Phase::AwaitingInput
            && self.task.mode == Mode::Auto
            && self.task.handed_back
            && !self.task.objective_pending;
        if judged && self.context.is_none() {
            // A reloaded task has no delivered rules yet; fetch them as resume
            // does. Unreachable knowledge leaves the rule check out, and an
            // unclear message is still announced.
            let files = self.task.read_files.clone();
            let _ = self.refresh(&files).await;
        }
        let disposition = judged.then(|| self.message_disposition(&text));
        let continues = matches!(
            disposition,
            Some(Disposition::Continue | Disposition::Unclear)
        );
        if interrupted || continues {
            // Record identity with the same journal write as the acknowledgment.
            if let Some(id) = id {
                self.task.delivered_messages.push(id.to_owned());
            }
            self.task.guidance = text.clone();
            self.answer(text.clone()).await?;
            self.task.turn_finished = false;
            match disposition {
                Some(Disposition::Continue) => {
                    self.intent_event("message_disposition", "continue");
                }
                Some(Disposition::Unclear) => {
                    self.intent_event("message_disposition", "unclear");
                    self.event(
                        "Continuing under the approved plan. If your message changes what the plan does, /plan replans.",
                    );
                    // In the last result, which the next action replaces, so it
                    // does not outlive the approval it describes.
                    self.task.last_response = format!(
                        "{text}\n\n(The approved plan stays in force. If this message changes what the plan does, use replan.)"
                    );
                }
                _ => {}
            }
            return self.persist();
        }
        if let Some(Disposition::Replan(reason)) = &disposition {
            self.intent_event("message_disposition", &format!("replan: {reason}"));
            self.event(format!("Returning to Plan: {reason}"));
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
        if std::mem::take(&mut self.task.objective_pending) {
            // The approval this task was started for is done; the human's
            // next request is what the task is now for.
            self.intent_event("objective_set", &bounded(&text, 400));
            self.event(format!("Objective set: {text}"));
            self.task.objective = text.clone();
            self.task.guidance.clear();
            self.task.approved_plans.clear();
        } else {
            self.task.guidance = text.clone();
        }
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.turn_finished = false;
        self.task.handed_back = false;
        self.task.steps = 0;
        // Guidance leads to a new plan, so the old plan's failed check no
        // longer says anything about what a rerun would test.
        self.forget_failure();
        self.end_intent_cycle("new human guidance");
        self.task.approved_revision = None;
        self.discard_pending_edit("new human guidance invalidated the proposed edit")?;
        self.discard_pending_permission("new human guidance invalidated the request")?;
        self.task.completion_pending = false;
        self.task.check_results.clear();
        self.task.final_capture = false;
        self.task.mode = Mode::Plan;
        self.task.after_review = Phase::Planning;
        // What the model read stays: guidance is about the plan, and source
        // is re-read from disk before every prompt.
        if self.overflowed() {
            self.clear_working_set();
        }
        if self.task.phase == Phase::AwaitingReview {
            self.task.review_continuation = Some(Phase::Planning);
        } else {
            self.task.phase = Phase::Planning;
        }
        // Existing uncertain capture requests remain frozen for idempotent retry.
        self.persist()
    }

    /// How a message answering handed-back approved work relates to the
    /// plan, judged from the plan's files, the rules it implements and the
    /// repository, never by a model.
    fn message_disposition(&self, text: &str) -> Disposition {
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|word| !word.is_empty())
            .map(|word| word.to_lowercase().replace('\'', ""))
            .collect();
        let Some(plan) = self.task.plan.as_ref() else {
            return Disposition::Unclear;
        };
        for token in text.split_whitespace() {
            if let Some(path) = named_path(token) {
                let covered = plan.files.iter().any(|file| {
                    file == &path
                        || file
                            .strip_prefix(path.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                });
                if !covered {
                    return Disposition::Replan(format!(
                        "your message names {path}, which the approved plan does not cover."
                    ));
                }
            }
        }
        if let Some(rule) = self.context.as_ref().and_then(|context| {
            context
                .governing_rules
                .iter()
                .find(|rule| !plan.addresses.contains(&rule.iri) && names_label(text, &rule.label))
        }) {
            return Disposition::Replan(format!(
                "your message names the rule \"{}\", which the approved plan does not implement.",
                rule.label
            ));
        }
        // Stopping or changing course is never read as carrying on.
        if let Some(word) = words
            .iter()
            .find(|word| STOP_WORDS.contains(&word.as_str()))
        {
            return Disposition::Replan(format!(
                "your message says \"{word}\", which may change or stop the approved work."
            ));
        }
        if !words.is_empty()
            && words.len() <= 8
            && words
                .iter()
                .all(|word| CONTINUE_WORDS.contains(&word.as_str()))
        {
            return Disposition::Continue;
        }
        Disposition::Unclear
    }

    /// The task stopped because its prompt outgrew the budget.
    fn overflowed(&self) -> bool {
        self.task.last_error_kind.as_deref() == Some("context_overflow")
    }

    /// Empty what the model has read. Only an overflow stop does this: its
    /// working set would rebuild the same prompt, and the stop message says
    /// the working set is cleared.
    fn clear_working_set(&mut self) {
        self.task.read_files.clear();
        self.task.source.clear();
        self.clear_source_order();
        if let Some(state) = self.task.symbolic.as_mut() {
            state.read_snapshots.clear();
        }
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
        self.discard_pending_permission("human returned the task to Plan")?;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        self.task.check_results.clear();
        self.task.after_review = Phase::Planning;
        if self.overflowed() {
            self.clear_working_set();
        }
        self.task.steps = 0;
        self.forget_failure();
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
        self.forget_failure();
        self.task.knowledge_turn_sequence = self.task.knowledge_turn_sequence.saturating_add(1);
        self.task.guidance = text.clone();
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.handed_back = false;
        self.task.steps = 0;
        // A task stopped because its prompt outgrew the budget would build the
        // same prompt again: the answer returns it to Plan with an empty
        // working set, as the stop message says.
        let overflow = self.overflowed();
        if self.task.intent.take().is_some() || overflow {
            self.task.mode = Mode::Plan;
            self.task.approved_revision = None;
            self.task.check_results.clear();
        }
        if overflow {
            self.clear_working_set();
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

impl Runner {
    /// Keep the approved plan beside `task.plan`, which the next replan
    /// replaces. Approving the same plan again (knowledge changed underneath
    /// it) refreshes its entry instead of adding one.
    fn record_approved_plan(&mut self, context: &ContextResponse) {
        let Some(plan) = self.task.plan.as_ref() else {
            return;
        };
        let rules_in_view = context
            .governing_rules
            .iter()
            .map(|rule| rule.iri.clone())
            .collect();
        let edit_start = self.task.edits.len();
        match self.task.approved_plans.last_mut() {
            Some(last) if last.summary == plan.summary && last.files == plan.files => {
                last.addresses = plan.addresses.clone();
                last.rules_in_view = rules_in_view;
            }
            _ => {
                let entry = ApprovedPlan {
                    summary: plan.summary.clone(),
                    files: plan.files.clone(),
                    addresses: plan.addresses.clone(),
                    rules_in_view,
                    edit_start,
                };
                self.task.approved_plans.push(entry);
            }
        }
    }
}

/// How a message answering handed-back approved work relates to the plan.
enum Disposition {
    /// Only a request to carry on: the approval stands.
    Continue,
    /// It names a file or rule outside the approved plan: replan.
    Replan(String),
    /// Anything else: continue under the approval, and say so.
    Unclear,
}

/// A path a message token names: `src/x.rs`, `x.rs`, `src\\x.rs` or
/// `src/x.rs:42`, but not a URL, a version, a ratio or an abbreviation.
fn named_path(token: &str) -> Option<String> {
    let token = token
        .trim_matches(|c: char| "`'\"()[]{}<>,;!?".contains(c))
        .trim_end_matches(['.', ':'])
        .replace('\\', "/");
    if token.contains("://") {
        return None;
    }
    // A line or anchor suffix (`:42`, `:42:7`, `#L42`) names the same file.
    let token = token.split('#').next().unwrap_or_default();
    let mut path = token;
    while let Some((head, tail)) = path.rsplit_once(':') {
        if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        path = head;
    }
    let slashed = path.contains('/');
    let path = path.trim_start_matches("./").trim_end_matches('/');
    let lettered = |part: &str| part.chars().any(|c| c.is_alphabetic());
    let segments_ok = !path.is_empty() && path.split('/').all(lettered);
    let has_extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(stem, extension)| {
            lettered(stem)
                && stem.len() > 1
                && (1..=8).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
                && lettered(extension)
        });
    (segments_ok && (slashed || has_extension)).then(|| path.to_string())
}

/// Whether `text` names `label` as whole words, case-insensitively, so a short
/// label ("NP-7") counts and does not match inside a longer word.
fn names_label(text: &str, label: &str) -> bool {
    let (text, label) = (text.to_lowercase(), label.trim().to_lowercase());
    if label.is_empty() {
        return false;
    }
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(label.as_str()).any(|(start, found)| {
        boundary(text[..start].chars().next_back())
            && boundary(text[start + found.len()..].chars().next())
    })
}

/// Words that stop or change course: a message holding one is never read as
/// carrying on.
const STOP_WORDS: &[&str] = &[
    "no", "not", "dont", "stop", "wait", "halt", "hold", "cancel", "abort", "instead", "undo",
    "revert", "rather", "never", "skip",
];

/// Words a message made only of means "carry on with the approved plan".
const CONTINUE_WORDS: &[&str] = &[
    "continue", "go", "on", "ahead", "yes", "yep", "y", "ok", "okay", "sure", "proceed", "carry",
    "with", "the", "plan", "please", "resume", "keep", "going", "do", "it", "that", "sounds",
    "good", "fine", "approved",
];

#[cfg(test)]
mod disposition_tests {
    use super::{named_path, names_label, STOP_WORDS};

    #[test]
    fn paths_are_named_with_suffixes_and_backslashes_but_not_versions_or_ratios() {
        assert_eq!(
            named_path("`src/code.txt:42`,").as_deref(),
            Some("src/code.txt")
        );
        assert_eq!(named_path("src\\other.rs").as_deref(), Some("src/other.rs"));
        assert_eq!(named_path("other.rs.").as_deref(), Some("other.rs"));
        assert_eq!(named_path("badciv-sim/").as_deref(), Some("badciv-sim"));
        assert_eq!(named_path("src/a.rs#L10").as_deref(), Some("src/a.rs"));
        for not_a_path in [
            "3/4",
            "v1.2",
            "e.g.",
            "i.e.",
            "https://x.io/a.rs",
            "ok",
            "1.5x",
        ] {
            assert_eq!(named_path(not_a_path), None, "{not_a_path}");
        }
    }

    #[test]
    fn rule_labels_match_as_whole_words_short_ones_included() {
        assert!(names_label("Apply NP-7 here.", "NP-7"));
        assert!(!names_label("Apply NP-70 here.", "NP-7"));
        assert!(names_label(
            "make sure retries stop at the configured limit",
            "Retries stop at the configured limit"
        ));
    }

    #[test]
    fn stop_words_cover_refusals() {
        for word in ["no", "stop", "dont", "instead", "wait"] {
            assert!(STOP_WORDS.contains(&word), "{word}");
        }
    }
}
