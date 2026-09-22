//! The harness's own decisions. The coding model answers only `harness_action`
//! and one final `harness_capture_note`; obligations, associations and capture
//! typing are derived by the daemon from the approved plan, the resolved
//! definition scopes and the graph.
mod associate;
mod capture_note;
mod coverage;
mod grounding;
mod scope;
mod state;

use super::actions::Step;
use super::model::{Action, NoopEdit};
use super::{Mode, PendingEdit, Phase, Progress, Runner};
use crate::harness::protocol::CheckOutcome;
use anyhow::Result;
pub use state::{
    CaptureNoteState, FailedRun, SymbolicAssociation, SymbolicState, FRUITLESS_SEARCH_LIMIT,
    MAX_RETYPES, MAX_SCOPE_ESCAPES,
};
use state::{NoteAnswer, CAPTURE_NOTE_QUESTION};

impl Runner {
    pub(super) fn symbolic_state_mut(&mut self) -> &mut SymbolicState {
        self.task.symbolic.get_or_insert_with(Default::default)
    }

    /// Every applied edit starts a new association batch and a new note.
    pub(in crate::harness::runner) fn clear_symbolic_batch_state(&mut self, edit: &PendingEdit) {
        if let Some(scope) = self.task.approved_change_scope.as_mut() {
            scope
                .files
                .insert(edit.file.clone(), super::fingerprint(&edit.after));
            if let Some(after_digest) = super::fingerprint(&edit.after) {
                for definition in scope
                    .definition_scopes
                    .iter_mut()
                    .filter(|definition| definition.file == edit.file)
                {
                    definition.source_digest = after_digest.clone();
                }
            }
        }
        if let Some(state) = self.task.symbolic.as_mut() {
            state.association = None;
            state.capture_note = None;
        }
    }
}

impl Runner {
    /// Before permission checks: an edit outside the approved plan files
    /// becomes the ordinary replan transition naming the file, bounded per
    /// task. Returns `None` once parked for guidance.
    pub(in crate::harness::runner) fn symbolic_intercept(
        &mut self,
        action: Action,
    ) -> Result<Option<Action>> {
        if self.task.mode != Mode::Auto {
            return Ok(Some(action));
        }
        let file = match &action {
            Action::Edit { file, .. }
            | Action::Replace { file, .. }
            | Action::Write { file, .. } => file.clone(),
            _ => return Ok(Some(action)),
        };
        let plan_files = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        if plan_files.contains(&file) {
            return Ok(Some(action));
        }
        let state = self.symbolic_state_mut();
        state.scope_escapes += 1;
        let escapes = state.scope_escapes;
        let scope = plan_files.join(", ");
        if escapes > MAX_SCOPE_ESCAPES {
            self.candidate_accepted();
            let message = format!(
                "Edit to {file} is outside the approved plan files [{scope}] and this task's {MAX_SCOPE_ESCAPES} autonomous replans are used. Provide guidance or a new plan; pending work is preserved."
            );
            self.intent_event(
                "scope_escape_exhausted",
                &format!("{file}: escape {escapes}, bound {MAX_SCOPE_ESCAPES}"),
            );
            self.task.phase = Phase::AwaitingInput;
            self.task.last_response = message.clone();
            self.event(message.clone());
            if let Some(progress) = &self.progress {
                let _ = progress.send(Progress::Status(message));
            }
            return Ok(None);
        }
        self.intent_event(
            "scope_escape_replan",
            &format!("{file}: escape {escapes} of {MAX_SCOPE_ESCAPES}"),
        );
        self.event(format!(
            "Scope escape: the model proposed an edit to {file} outside the plan files [{scope}]; replanning ({escapes} of {MAX_SCOPE_ESCAPES})."
        ));
        Ok(Some(Action::Replan {
            reason: format!(
                "Edit to {file} is outside the approved plan files [{scope}]; replan with every file the change needs."
            ),
        }))
    }

    /// A query this task has already searched, answered from its stored result.
    ///
    /// `knowledge_searches` is never pruned, so the whole task's retrieval
    /// history is available and a byte-identical repeat needs no daemon round
    /// trip. A step is still charged, because the model did take an action and
    /// because a free repeat would let one query loop forever without ever
    /// reaching the task's step bound -- which is exactly what was observed:
    /// one query repeated 71 times in a single turn.
    ///
    /// Only answered from cache while the workspace cannot have changed since
    /// that search, so a repeat after an edit still re-reads the repository.
    pub(in crate::harness::runner) fn repeat_search_answer(
        &mut self,
        query: &str,
    ) -> Option<String> {
        if !self.task.edits.is_empty() {
            return None;
        }
        let earlier = self
            .task
            .knowledge_searches
            .iter()
            .find(|search| search.query == query)?;
        let records = earlier.evidence_iris.len();
        let omitted = earlier.omitted_record_count();
        let context = earlier.context.trim().to_owned();
        self.intent_event(
            "repeat_search",
            &format!("{records} records already delivered: {query}"),
        );
        // Show the stored records again rather than only naming them. Answering
        // a repeat with a bare "you already asked" would withhold the evidence
        // the model asked for, which is the defect this whole change corrects;
        // the point of the short circuit is to skip the daemon, not the answer.
        let body = if context.is_empty() {
            String::new()
        } else {
            format!("\n\nAccepted project knowledge for '{query}' (authoritative):\n{context}")
        };
        let delivery = if omitted == 0 {
            format!("{records} accepted record(s)")
        } else {
            format!("{records} accepted record(s) shown and {omitted} counted as omitted")
        };
        Some(format!(
            "You already searched the literal string '{query}' in this task; it returned {delivery}, unchanged and repeated below. Searching it again cannot add anything -- use a different action.{body}"
        ))
    }

    /// What to append to a search result: how many searches in a row have now
    /// matched nothing, and, once that reaches the limit, what is left to try.
    ///
    /// `search` matches LITERAL text, so rewording rarely rescues an empty
    /// result -- the knowledge is simply not recorded. A model that cannot tell
    /// it has exhausted the channel rewords forever (fourteen consecutive
    /// searches observed). The harness can count, so it counts and states the
    /// fact; it never asks the model to judge its own sufficiency, which would
    /// be a structured decision the symbolic layer can derive (Constraint
    /// cd9f1a96). The note does not block searching, and a search that returns
    /// anything resets the count.
    pub(in crate::harness::runner) fn fruitless_search_note(&mut self, fruitless: bool) -> String {
        let consecutive = {
            let state = self.symbolic_state_mut();
            state.fruitless_searches = if fruitless {
                state.fruitless_searches + 1
            } else {
                0
            };
            state.fruitless_searches
        };
        if !fruitless {
            return String::new();
        }
        let mut note = format!(" Searches in a row matching nothing: {consecutive}.");
        if consecutive == FRUITLESS_SEARCH_LIMIT {
            // Name only the actions this mode actually offers: in Plan mode
            // `command` is not among them, so a model cannot consult history.
            let remaining = match self.task.mode {
                Mode::Plan => "read(file) to look at the source, question(...) to ask the human, or reply(...) saying it is not recorded",
                Mode::Auto => "read(file) or command(...) to look at the source and its history, question(...) to ask the human, or reply(...) saying it is not recorded",
            };
            note.push_str(&format!(
                "\n\nSearching has not found this, and rewording will not help: the query is matched literally and the accepted graph holds no record of it. What remains: {remaining}."
            ));
            self.intent_event(
                "search_exhausted",
                &format!("{consecutive} consecutive searches matched nothing"),
            );
        }
        note
    }

    /// The required check that failed against exactly the current source:
    /// nothing has been edited since it ran, so running it again would only
    /// repeat the result.
    fn untested_failure(&self) -> Option<&FailedRun> {
        let edits = self.task.edits.len();
        self.task
            .symbolic
            .as_ref()?
            .last_failure
            .as_ref()
            .filter(|failure| failure.edits == edits)
    }

    /// A no-op edit says the source is the way the model wants it, so the
    /// required checks run instead of spending the repair budget -- unless
    /// exactly this source already failed a required check, in which case
    /// nothing has changed to test and the edit is repaired with that fact.
    /// A model near the floor often ends a repair by restating the file
    /// (Lesson c60cc811); the harness can tell whether that file is untested,
    /// so it decides rather than asking (Constraint cd9f1a96).
    pub(in crate::harness::runner) fn symbolic_noop_continuation(
        &mut self,
        error: anyhow::Error,
    ) -> Result<Step> {
        if !error.is::<NoopEdit>() {
            return Err(error);
        }
        if let Some(failure) = self.untested_failure() {
            return Err(error.context(format!(
                "the file already reads this way and `{}` failed against exactly this source; make an edit that changes the file to address that failure",
                failure.command
            )));
        }
        let state = self.symbolic_state_mut();
        state.noop_continuations += 1;
        self.intent_event(
            "noop_edit_continuation",
            "no-op edit treated as finish; running required checks",
        );
        self.event(
            "No-op edit: the source already matches the proposal; running required checks instead of repairing."
                .to_string(),
        );
        Ok(Step::Finish {
            summary: "The source already satisfies the requested change; running required checks."
                .into(),
        })
    }

    /// A finish reruns the required checks, so a finish with nothing edited
    /// since one of them failed would only repeat that failure: it is
    /// repaired with the check named, and the sandbox case points at the
    /// permission request. Three refusals park the task for the human, who
    /// can change the plan's checks; nothing else bounded the loop
    /// (badciv 301f7887: `cargo run` of a TUI, denied nine times in a row).
    pub(in crate::harness::runner) fn symbolic_finish_guard(&mut self, step: Step) -> Result<Step> {
        if !matches!(step, Step::Finish { .. }) {
            return Ok(step);
        }
        let Some(failure) = self.untested_failure().cloned() else {
            return Ok(step);
        };
        self.intent_event("finish_retest_refused", &failure.command);
        let command = failure.command;
        Err(if failure.ungrantable {
            anyhow::anyhow!(
                "required check `{command}` was blocked by the sandbox without naming a path or network need, and nothing has changed since; no permission request can help. Ask the human with question to change the plan's checks, or replan with a check that runs without a terminal"
            )
        } else if failure.denied {
            anyhow::anyhow!(
                "required check `{command}` was blocked by the sandbox against exactly this source and nothing has changed since; finishing would run it again unchanged. Request permission for it (request_permission with this exact command), make an edit, or ask the human with question to change the plan's checks if the check cannot run inside the sandbox"
            )
        } else {
            anyhow::anyhow!(
                "required check `{command}` already failed against exactly this source and nothing has changed since; finishing would run it again unchanged. Make an edit that addresses the failure"
            )
        })
    }

    /// A human answer or a new grant changes what a rerun would test, so it
    /// re-arms one rerun of the required checks.
    pub(in crate::harness::runner) fn forget_failure(&mut self) {
        if let Some(state) = self.task.symbolic.as_mut() {
            state.last_failure = None;
        }
    }
}

impl Runner {
    /// An applied edit, a command, a required-check result or a human answer
    /// is new evidence: a replan after it is a real replan.
    pub(in crate::harness::runner) fn end_unchanged_window(&mut self) {
        if let Some(state) = self.task.symbolic.as_mut() {
            state.unchanged_since_approval = false;
        }
    }

    /// The model's own replan changes nothing while the approved plan still
    /// governs and nothing new has arrived since approval.
    pub(in crate::harness::runner) fn replan_changes_nothing(&self) -> bool {
        self.task.mode == Mode::Auto
            && self.task.phase == Phase::Working
            && self.task.approved_revision.is_some()
            && self.task.approved_change_scope.is_some()
            && self.task.pending_edit.is_none()
            && self
                .task
                .symbolic
                .as_ref()
                .is_some_and(|state| state.unchanged_since_approval)
    }

    /// Continue the approved plan instead of reopening planning and approval.
    pub(in crate::harness::runner) fn symbolic_replan_continuation(&mut self, reason: &str) {
        let state = self.symbolic_state_mut();
        state.replan_continuations += 1;
        state.cycle_replan_continuations += 1;
        let count = state.replan_continuations;
        let files = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.join(", "))
            .unwrap_or_default();
        self.intent_event("replan_continuation", &format!("{count}: {reason}"));
        self.event(format!(
            "Replan continued the approved plan ({count}): {reason}"
        ));
        self.task.last_response = format!(
            "Replan not needed: nothing has changed since the plan was approved (no edit, command, check result or human answer since approval), so the approved plan still governs. Make the change it describes in {files}, or finish to run the required checks. An edit to a file outside the plan returns the task to planning automatically."
        );
    }

    /// A replan while already planning changes nothing.
    pub(in crate::harness::runner) fn symbolic_replan_noop(&mut self, reason: &str) {
        self.intent_event("replan_noop", reason);
        self.event(format!("Replan while planning changes nothing: {reason}"));
        self.task.last_response = "Already planning; replan changes nothing here. Propose the plan with plan(summary, files, checks), or read, search or ask first.".into();
    }

    /// Remember a failed required check against the edit count it ran at; a
    /// pass clears it. Free commands are not recorded: the plan's checks are
    /// the arbiter of completion, so a failed command the model chose to run
    /// is no reason to refuse a finish.
    pub(in crate::harness::runner) fn record_symbolic_check(
        &mut self,
        command: &str,
        success: bool,
        denied: bool,
        ungrantable: bool,
    ) {
        self.end_unchanged_window();
        let edits = self.task.edits.len();
        let after_edit = edits > 0;
        let state = self.symbolic_state_mut();
        state.last_failure = (!success).then(|| FailedRun {
            command: command.to_string(),
            edits,
            denied,
            ungrantable,
        });
        state.check_history.push(CheckOutcome {
            command: command.to_string(),
            success,
            after_edit,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{context_router, serve, Project};
    use super::*;

    #[tokio::test]
    async fn empty_searches_are_counted_and_the_exhausted_note_names_only_offered_actions() {
        let project = Project::new("fruitless-search");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(
            project.0.clone(),
            daemon,
            "When was the MOOSE emoji added?".into(),
        )
        .await
        .unwrap();

        // The first empty search states the count and nothing more: one poor
        // choice of words is not evidence that the knowledge is absent.
        let first = runner.fruitless_search_note(true);
        assert!(
            first.contains("Searches in a row matching nothing: 1"),
            "{first}"
        );
        assert!(!first.contains("What remains"), "{first}");

        // The second says the channel is exhausted. This is Plan mode, where
        // `command` is not offered, so it must not be suggested.
        let second = runner.fruitless_search_note(true);
        assert!(
            second.contains("Searches in a row matching nothing: 2"),
            "{second}"
        );
        assert!(second.contains("rewording will not help"), "{second}");
        assert!(
            second.contains("reply(...)") && second.contains("question(...)"),
            "{second}"
        );
        assert!(
            !second.contains("command(...)"),
            "must not offer an action Plan mode denies"
        );

        // It fires once per run, not on every later empty search...
        let third = runner.fruitless_search_note(true);
        assert!(
            third.contains("Searches in a row matching nothing: 3"),
            "{third}"
        );
        assert!(!third.contains("What remains"), "{third}");

        // ...and a search that finds something resets the count entirely.
        assert_eq!(runner.fruitless_search_note(false), "");
        assert_eq!(runner.task.symbolic.as_ref().unwrap().fruitless_searches, 0);
        let after = runner.fruitless_search_note(true);
        assert!(
            after.contains("Searches in a row matching nothing: 1"),
            "{after}"
        );

        // In Auto mode history is reachable, so the note may offer it.
        runner.task.mode = Mode::Auto;
        let auto = runner.fruitless_search_note(true);
        assert!(auto.contains("command(...)"), "{auto}");
        server.abort();
    }

    #[tokio::test]
    async fn a_repeated_query_is_answered_from_the_stored_result_while_nothing_changed() {
        let project = Project::new("repeat-search");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Explain the harness".into())
            .await
            .unwrap();

        // Nothing searched yet, and a different query, are both dispatched.
        assert!(runner.repeat_search_answer("harness").is_none());
        runner.search_knowledge("harness", None).await.unwrap();
        assert!(runner.repeat_search_answer("harness for models").is_none());

        // Give the stored search a body, so the repeat can be checked for
        // repeating the evidence rather than only naming it.
        runner.task.knowledge_searches[0].context = "[Lesson] Something durable".into();
        runner.task.knowledge_searches[0].evidence_iris = vec!["urn:record:1".into()];

        // A byte-identical repeat is served from the stored result, every time:
        // one query was observed repeated 71 times in a single turn, so this
        // must not be bounded into giving up and looping again.
        for _ in 0..70 {
            let answer = runner.repeat_search_answer("harness").unwrap();
            assert!(answer.contains("You already searched"), "{answer}");
            assert!(answer.contains("use a different action"), "{answer}");
            // Skip the daemon round trip, never the records themselves.
            assert!(answer.contains("[Lesson] Something durable"), "{answer}");
            assert!(answer.contains("1 accepted record(s)"), "{answer}");
        }
        assert_eq!(
            runner.task.knowledge_searches.len(),
            1,
            "no repeat re-ran the search"
        );

        // Once an edit lands the workspace may differ, so a repeat is dispatched
        // again rather than answered from a stale result.
        runner.task.edits.push(PendingEdit {
            file: "code.rs".into(),
            before: None,
            after: Some("fn main() {}".into()),
            reason: "applied".into(),
            revision: "r1".into(),
        });
        assert!(runner.repeat_search_answer("harness").is_none());
        server.abort();
    }

    #[tokio::test]
    async fn record_symbolic_check_ends_the_unchanged_window() {
        let project = Project::new("unchanged-window");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Keep the approved plan".into())
            .await
            .unwrap();
        for success in [true, false] {
            runner.symbolic_state_mut().unchanged_since_approval = true;
            runner.record_symbolic_check("true", success, false, false);
            assert!(
                !runner
                    .task
                    .symbolic
                    .as_ref()
                    .unwrap()
                    .unchanged_since_approval,
                "a check result (success {success}) is new evidence"
            );
        }
        server.abort();
    }
}
