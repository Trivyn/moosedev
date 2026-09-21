//! One model step: refresh evidence, ask for one action, validate it, and
//! execute the resulting step under the approval and policy gates.
use super::*;

impl Runner {
    fn repository_search_hits(&self, query: &str, files: &[String]) -> Vec<String> {
        const COLLECTION_LIMIT: usize = 12_000;

        let mut hits = Vec::new();
        let mut bytes = 0;
        'files: for file in files {
            if file.contains(query) {
                let hit = format!("Path: {file}\n");
                bytes += hit.len();
                hits.push(hit);
            }
            if bytes > COLLECTION_LIMIT {
                break;
            }
            if let Ok(Some(source)) = self.workspace.read(file) {
                for (line, text) in source
                    .lines()
                    .enumerate()
                    .filter(|(_, text)| text.contains(query))
                    .take(10)
                {
                    let hit = format!("{file}:{}: {}\n", line + 1, bounded(text, 300));
                    bytes += hit.len();
                    hits.push(hit);
                    if bytes > COLLECTION_LIMIT {
                        break 'files;
                    }
                }
            }
        }
        hits
    }

    pub(super) async fn fresh_approval(&mut self) -> Result<bool> {
        let files = self
            .task
            .plan
            .as_ref()
            .context("no approved plan")?
            .files
            .clone();
        let context = self.refresh(&files).await?;
        let sources = self.snapshot(&files)?;
        if self.task.approved_revision.as_deref() != Some(&context.revision)
            || sources != self.task.snapshots
        {
            self.task.mode = Mode::Plan;
            self.task.phase = Phase::AwaitingPlan;
            self.task.approved_revision = None;
            self.task.approved_change_scope = None;
            self.task.snapshots = sources;
            self.task.check_results.clear();
            self.discard_pending_edit("source or accepted knowledge changed")?;
            self.discard_pending_permission("source or accepted knowledge changed")?;
            self.abandon_pending_intent("source or accepted knowledge changed")
                .await?;
            self.invalidate_capture_typing("source or accepted knowledge changed");
            self.intent_event("intent_invalidated", "source or accepted knowledge changed");
            self.start_intent_cycle();
            self.event("Source or accepted knowledge changed. Refreshed evidence; review and approve the plan again.");
            self.persist()?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Read a file into the working set: refresh its governing knowledge, record
    /// its source and journal the read. Leaves `last_response` to the caller.
    pub(super) async fn read_into_working_set(&mut self, file: &str) -> Result<()> {
        let file = file.to_string();
        self.refresh(std::slice::from_ref(&file)).await?;
        let source = self.workspace.read(&file)?;
        if !self.task.read_files.contains(&file) {
            anyhow::ensure!(
                self.task.read_files.len() < MAX_FILES,
                "working set exceeds {MAX_FILES} files; narrow the task"
            );
            self.task.read_files.push(file.clone());
        }
        self.symbolic_state_mut()
            .read_snapshots
            .insert(file.clone(), fingerprint(&source));
        self.event(format!(
            "Read {file}: {}",
            source.as_deref().unwrap_or("[file does not exist]")
        ));
        self.task.source.insert(file, source);
        Ok(())
    }

    pub async fn advance(&mut self) -> Result<()> {
        self.task.last_error = None;
        self.task.last_error_kind = None;
        let result = loop {
            match self.advance_inner().await {
                Err(error) if self.repair_candidate(&error)? => continue,
                outcome => break outcome,
            }
        };
        if let Err(error) = &result {
            self.task.last_error = Some(format!("{error:#}"));
            self.task.last_error_kind = Some(error_kind(error).into());
            self.event(format!("Step rejected or interrupted: {error:#}"));
        }
        self.persist()?;
        result
    }

    async fn advance_inner(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.task.cleanup_pending,
            "cancelled task cleanup is pending; retry cancel or resume before continuing"
        );
        anyhow::ensure!(
            matches!(
                self.task.phase,
                Phase::Planning | Phase::Working | Phase::Verifying
            ),
            "task is waiting for a human action"
        );
        if self.task.intent.is_some() {
            self.reconcile()?;
            return Ok(());
        }
        if self.task.pending_intent_links.is_some() {
            self.prepare_intent_links().await?;
            return Ok(());
        }
        if self.task.capture_due || self.task.capture_request.is_some() {
            return self.capture().await;
        }
        if self.task.completion_pending {
            return self.finish().await;
        }
        if self.task.mode == Mode::Auto && !self.fresh_approval().await? {
            return Ok(());
        }
        if self.task.mode == Mode::Auto && self.permission_approved() {
            return self.run_approved_permission().await;
        }
        if self.task.phase == Phase::Verifying {
            return self.verify_next().await;
        }
        anyhow::ensure!(
            self.task.steps < MAX_STEPS,
            "task reached {MAX_STEPS} model steps; inspect and provide new guidance"
        );
        let targets = self.task.read_files.clone();
        let context = self.refresh(&targets).await?;
        // A previously read file may have changed through another client.
        for file in &targets {
            self.task
                .source
                .insert(file.clone(), self.workspace.read(file)?);
        }
        let files = self.workspace.files()?;
        let prompt = self.prompt(&context, &files)?;
        let output: ModelOutput = self
            .model_json(&prompt, "harness_action", self.action_schema())
            .await?;
        let (message, action) = output.parts();
        // A scope escape arrives here as a replan too; only the model's own
        // replan can be continued.
        let proposed_replan = matches!(action, model::Action::Replan { .. });
        let Some(action) = self.symbolic_intercept(action)? else {
            return self.persist();
        };
        self.validate_permission(&action)?;
        let step = match self.validate_action(action) {
            Ok(step) => step,
            Err(error) => match self.symbolic_noop_continuation(&error) {
                Some(step) => step,
                None => return Err(error.context(model::InvalidModelOutput)),
            },
        };
        self.candidate_accepted();
        if !message.trim().is_empty() {
            self.task.last_response = message.clone();
            self.event(format!("Assistant: {message}"));
        }
        self.task.steps += 1;
        self.event(format!("Model action: {}", serde_json::to_string(&step)?));
        self.persist()?;
        match step {
            Step::Inspect { event, offset } => {
                let observation = self
                    .task
                    .events
                    .get(event)
                    .context("unknown journal event")?;
                let mut end = offset.saturating_add(2000).min(observation.message.len());
                while !observation.message.is_char_boundary(end) {
                    end -= 1;
                }
                self.task.last_response = format!(
                    "Journal event {event}, bytes {offset}..{end} of {}:\n{}",
                    observation.message.len(),
                    &observation.message[offset..end]
                );
            }
            Step::Read { file } => {
                self.read_into_working_set(&file).await?;
                self.task.last_response = format!("Read {file} with its governing knowledge.");
            }
            Step::Search { query } => {
                // A query already asked in this task returns the same records;
                // answer from the stored result rather than spending a daemon
                // round trip on it again.
                if let Some(answer) = self.repeat_search_answer(&query) {
                    self.task.last_response = answer.clone();
                    self.knowledge_event(answer);
                    return self.persist();
                }
                let accepted_header =
                    format!("Accepted project knowledge for '{query}' (authoritative):\n");
                let repository_header = "\n\nRepository matches:\n";
                let last_result_budget = self.next_last_result_budget(&context)?;
                let evidence_budget = last_result_budget.saturating_sub(
                    accepted_header.len() + repository_header.len() + "No matches.".len(),
                );
                let knowledge = self.search_knowledge(&query, Some(evidence_budget)).await?;
                let hits = self.repository_search_hits(&query, &files);
                let delivered_records = knowledge.evidence_iris.len();
                let selected_records = knowledge.selected_record_count();
                let omitted_records = knowledge.omitted_record_count();
                let repository_matches = hits.len();
                let record_detail = if omitted_records == 0 {
                    format!("{delivered_records} records")
                } else {
                    format!("{delivered_records} records delivered, {omitted_records} omitted")
                };
                self.intent_event(
                    "knowledge_search",
                    &format!("{record_detail}, {repository_matches} repository matches: {query}"),
                );
                let repository = if hits.is_empty() {
                    "No matches.".to_string()
                } else {
                    let fixed = match selected_records {
                        0 => "Repository matches:\n".len(),
                        _ => {
                            accepted_header.len()
                                + knowledge.context.trim().len()
                                + repository_header.len()
                        }
                    };
                    render_search_lines(&hits, last_result_budget.saturating_sub(fixed))
                };
                // A search that matched nothing is where models loop, so say what
                // was actually searched and how long this has been going on.
                let exhaustion =
                    self.fruitless_search_note(selected_records == 0 && repository_matches == 0);
                // Accepted knowledge answers first; repository matches follow.
                self.task.last_response = match (selected_records, repository_matches) {
                    (0, 0) => format!(
                        "No accepted knowledge and no repository text matched the literal string '{query}'.{exhaustion}"
                    ),
                    (0, _) => format!("Repository matches:\n{repository}"),
                    _ => format!(
                        "{accepted_header}{}{repository_header}{repository}",
                        knowledge.context.trim()
                    ),
                };
                self.knowledge_event(self.task.last_response.clone());
            }
            Step::Plan {
                summary,
                files,
                checks,
            } => {
                let context = self.refresh(&files).await?;
                // Before anything is stored: a plan whose summary skips a
                // governing rule goes back once with a note naming it.
                if self.plan_coverage_return(&summary, &context) {
                    return self.persist();
                }
                self.task.snapshots = self.snapshot(&files)?;
                self.task.read_files.retain(|file| files.contains(file));
                self.task.source.retain(|file, _| files.contains(file));
                self.task.plan = Some(Plan {
                    summary: summary.clone(),
                    files,
                    checks,
                });
                self.task.approved_change_scope = None;
                self.start_intent_cycle();
                self.event(format!(
                    "Proposed plan: {}",
                    serde_json::to_string(&self.task.plan)?
                ));
                self.task.last_response = summary;
                self.task.approved_revision = None;
                self.task.after_review = Phase::AwaitingPlan;
                self.task.capture_due = true;
                self.capture().await?;
            }
            Step::Edit {
                file,
                before,
                after,
            } => {
                if !self.fresh_approval().await? {
                    return Ok(());
                }
                // Edits reach here only in Auto and only for files already read:
                // the first-edit guard turns an unread edit into a read.
                if self
                    .ground_edit(&file, before.as_deref(), after.as_deref())
                    .await
                {
                    return self.persist();
                }
                let context = self.refresh(std::slice::from_ref(&file)).await?;
                let policy = &context.files[0].policy;
                let mut reason = String::new();
                match policy {
                    PolicyDecision::Gate {
                        disposition: GateDisposition::Deny,
                        reason,
                        ..
                    } => bail!("daemon denied edit: {reason}"),
                    PolicyDecision::Gate {
                        disposition: GateDisposition::RequirePlan,
                        reason,
                        ..
                    } => {
                        self.task.mode = Mode::Plan;
                        self.task.phase = Phase::AwaitingPlan;
                        self.task.approved_revision = None;
                        self.event(reason.clone());
                        return Ok(());
                    }
                    PolicyDecision::Gate { reason: r, .. } => reason = r.clone(),
                    _ => {}
                }
                if after.is_none() {
                    reason.push_str(" File deletion requires human approval.");
                }
                let edit = PendingEdit {
                    file,
                    before,
                    after,
                    reason,
                    revision: context.revision,
                };
                self.persist()?;
                if !edit.reason.is_empty() {
                    self.task.pending_edit = Some(edit);
                    self.task.phase = Phase::AwaitingPolicy;
                } else {
                    self.apply_edit(edit)?;
                }
            }
            Step::Command { command } => {
                if !self.fresh_approval().await? {
                    return Ok(());
                }
                self.end_unchanged_window();
                let result = self.run_command(&command).await?;
                self.commit_command_result(result)?;
            }
            Step::RequestPermission {
                command,
                justification,
                read_paths,
                write_paths,
                network,
            } => {
                if !self.fresh_approval().await? {
                    return Ok(());
                }
                if let Some(result) = self
                    .begin_permission_request(
                        command,
                        justification,
                        read_paths,
                        write_paths,
                        network,
                    )
                    .await?
                {
                    self.end_unchanged_window();
                    self.commit_command_result(result)?;
                }
            }
            Step::Question { question } => {
                self.event(format!("Assistant: {question}"));
                self.task.last_response = question;
                self.task.phase = Phase::AwaitingInput;
            }
            Step::Reply { message: reply } => {
                if reply != message {
                    self.event(format!("Assistant: {reply}"));
                }
                self.task.last_response = reply;
                self.task.turn_finished = true;
                self.task.capture_due = true;
                self.task.after_review = Phase::AwaitingInput;
                self.capture().await?;
            }
            Step::Replan { reason } if self.task.mode == Mode::Plan => {
                self.symbolic_replan_noop(&reason);
            }
            Step::Replan { reason } if proposed_replan && self.replan_changes_nothing() => {
                self.symbolic_replan_continuation(&reason);
                self.ground_disputed_plan(&reason).await;
            }
            Step::Replan { reason } => {
                if proposed_replan {
                    self.intent_event("model_replan", &reason);
                }
                self.end_unchanged_window();
                self.end_intent_cycle("model replan");
                self.task.mode = Mode::Plan;
                self.task.phase = Phase::Planning;
                self.task.approved_revision = None;
                self.task.last_response = reason;
                // The working set stays: the next plan keeps only its own files,
                // and a file outside them comes back through the first-edit guard.
                self.task.capture_due = true;
                self.task.after_review = Phase::Planning;
            }
            Step::Finish { summary } => {
                self.task.last_response = summary;
                if self.prepare_symbolic_associations().await? {
                    return Ok(());
                }
                self.task.phase = Phase::Verifying;
                self.task.check_results.clear();
            }
        }
        Ok(())
    }

    pub(super) fn apply_edit(&mut self, edit: PendingEdit) -> Result<()> {
        anyhow::ensure!(edit.before != edit.after, "edit makes no change");
        self.task.intent = Some(Intent::Edit(edit.clone()));
        self.persist()?;
        self.workspace
            .apply(&edit.file, edit.before.as_deref(), edit.after.as_deref())?;
        self.task.edits.push(edit.clone());
        self.task
            .snapshots
            .insert(edit.file.clone(), fingerprint(&edit.after));
        self.task
            .source
            .insert(edit.file.clone(), edit.after.clone());
        self.clear_symbolic_batch_state(&edit);
        self.end_unchanged_window();
        self.intent_event("edit_applied", &edit.file);
        self.event(format!(
            "Applied edit {}\nBefore:\n{}\nAfter:\n{}",
            edit.file,
            edit.before.as_deref().unwrap_or("[absent]"),
            edit.after.as_deref().unwrap_or("[deleted]")
        ));
        self.task.intent = None;
        self.task.pending_edit = None;
        self.task.check_results.clear();
        self.task.phase = Phase::Working;
        self.task.capture_due = true;
        self.task.after_review = Phase::Working;
        self.persist()
    }

    pub(super) async fn run_command(&mut self, command: &str) -> Result<executor::CommandResult> {
        anyhow::ensure!(
            !command.trim().is_empty() && command.len() <= 4000,
            "command must contain 1..4000 bytes"
        );
        let permissions = self.command_permissions()?;
        let grant_ids: Vec<_> = self
            .task
            .permission_grants
            .iter()
            .map(|grant| grant.id.clone())
            .collect();
        self.task.intent = Some(if grant_ids.is_empty() {
            Intent::Command(command.to_string())
        } else {
            Intent::PermissionedCommand {
                command: command.to_string(),
                grant_ids: grant_ids.clone(),
            }
        });
        self.persist()?;
        let scratch = self.scratch_path();
        let result = executor::command_with_permissions(
            self.workspace.root(),
            &scratch,
            command,
            &permissions,
            self.progress.clone(),
        )
        .await;
        if let Ok(result) = &result {
            self.event(format!(
                "Command: {command}\nPermission grants: {}\nSuccess: {}\n{}",
                if grant_ids.is_empty() {
                    "none".into()
                } else {
                    grant_ids.join(", ")
                },
                result.success,
                result.output
            ));
            // Keep the persisted intent until the caller commits its typed
            // check result or capture obligation in the same task-journal
            // update. A crash between execution and that update must reconcile
            // as uncertain, never replay a command whose outcome was lost.
        }
        result
    }

    pub(super) fn commit_command_result(&mut self, result: executor::CommandResult) -> Result<()> {
        self.task.last_response = result.output;
        self.task.capture_due = true;
        self.task.after_review = Phase::Working;
        self.task.intent = None;
        self.persist()
    }

    pub(super) fn scratch_path(&self) -> PathBuf {
        self.task
            .root
            .join(".moosedev/harness/scratch")
            .join(&self.task.id)
    }

    async fn verify_next(&mut self) -> Result<()> {
        let plan = self.task.plan.as_ref().context("no plan")?;
        let index = self.task.check_results.len();
        if let Some(command) = plan.checks.get(index).cloned() {
            let result = self.run_command(&command).await?;
            self.record_symbolic_check(&command, result.success);
            let failure = (!result.success).then(|| check_failure_response(&command, &result));
            if let (false, Some(code)) = (result.success, unrunnable_exit(&result)) {
                self.intent_event("check_unrunnable", &format!("exit {code}: {command}"));
            }
            self.task.check_results.push(CheckResult {
                command,
                success: result.success,
                output: result.output.clone(),
            });
            if let Some(response) = failure {
                self.task.phase = Phase::Working;
                self.task.last_response = response;
                self.task.capture_due = true;
                self.task.after_review = Phase::Working;
            }
            self.task.intent = None;
            return Ok(());
        }
        anyhow::ensure!(
            !self.task.check_results.is_empty()
                && self.task.check_results.iter().all(|c| c.success),
            "required checks have not passed"
        );
        self.task.final_capture = true;
        self.task.capture_due = true;
        self.task.after_review = Phase::Verifying;
        self.capture().await
    }
}

/// Repository matches share the last-result budget only after accepted graph
/// evidence. Admit complete lines and count the rest; never make generic
/// observation clipping responsible for fitting this search response.
fn render_search_lines(lines: &[String], budget: usize) -> String {
    let mut out = String::new();
    let mut kept = 0;
    for line in lines {
        let omitted = lines.len().saturating_sub(kept + 1);
        let notice = repository_omission_notice(omitted);
        if out.len() + line.len() + notice.len() <= budget {
            out.push_str(line);
            kept += 1;
        } else {
            break;
        }
    }
    let omitted = lines.len().saturating_sub(kept);
    if omitted > 0 {
        let notice = repository_omission_notice(omitted);
        if out.len() + notice.len() <= budget {
            out.push_str(&notice);
        }
    }
    out
}

fn repository_omission_notice(omitted: usize) -> String {
    if omitted == 0 {
        String::new()
    } else {
        format!("{omitted} further repository match(es) omitted by the prompt byte budget.\n")
    }
}

/// The shell's statuses for a command that could not start at all.
fn unrunnable_exit(result: &executor::CommandResult) -> Option<i32> {
    result.exit_code.filter(|code| matches!(code, 126 | 127))
}

/// What the model is told about a failed required check. A check the shell
/// could not start tested nothing, so it is named as an invalid check rather
/// than a failure to repair in the code.
fn check_failure_response(command: &str, result: &executor::CommandResult) -> String {
    match unrunnable_exit(result) {
        Some(code) => format!(
            "Required check could not run: the shell exited {code} (command not found or not executable), so the change was not tested. The check itself is invalid, not the environment: replan with checks that are shell command lines, such as the task's verification commands.\nCheck: {command}\n{}",
            result.output
        ),
        None => format!(
            "Required verification failed. Repair before completion.\n{}",
            result.output
        ),
    }
}

#[cfg(test)]
mod check_failure_tests {
    use super::*;

    fn result(exit_code: Option<i32>, output: &str) -> executor::CommandResult {
        executor::CommandResult {
            success: false,
            output: output.into(),
            exit_code,
        }
    }

    #[test]
    fn a_check_the_shell_cannot_start_is_named_invalid_not_a_code_failure() {
        let missing = result(Some(127), "/bin/sh: The: command not found");
        let response = check_failure_response("The implementation must be idempotent.", &missing);
        assert!(
            response.starts_with("Required check could not run"),
            "{response}"
        );
        assert!(response.contains("not the environment"), "{response}");
        assert!(response.contains("Check: The implementation must be idempotent."));
        assert_eq!(unrunnable_exit(&missing), Some(127));
        assert_eq!(unrunnable_exit(&result(Some(126), "")), Some(126));
        for ordinary in [
            result(Some(1), "FAILED (failures=1)"),
            result(None, "killed"),
        ] {
            assert_eq!(unrunnable_exit(&ordinary), None);
            assert!(check_failure_response("python3 -m unittest", &ordinary)
                .starts_with("Required verification failed. Repair before completion."));
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::test_support::{context_router, serve, Project};
    use super::*;

    #[tokio::test]
    async fn command_observation_keeps_durable_intent_until_caller_commits_outcome() {
        let project = Project::new("command-commit");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(
            project.0.clone(),
            daemon.clone(),
            "Observe a verification command".into(),
        )
        .await
        .unwrap();

        // Exercise the internal observation boundary directly, stopping before
        // verify_next/Action::Command can commit its result and next phase.
        // A nested sandbox may reject execution; either outcome must preserve
        // the durable intent until the caller has committed the next state.
        let _result = runner.run_command("false").await;
        let stored: Task = serde_json::from_reader(File::open(&runner.journal).unwrap()).unwrap();
        assert!(matches!(stored.intent, Some(Intent::Command(ref command)) if command == "false"));
        let id = runner.task.id.clone();
        drop(runner);
        let mut resumed = Runner::load(project.0.clone(), daemon, &id).unwrap();
        resumed.resume().await.unwrap();
        assert_eq!(resumed.task.phase, Phase::AwaitingInput);
        assert!(resumed.task.last_response.contains("unknown"));
        server.abort();
    }
}
