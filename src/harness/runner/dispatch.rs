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
    pub(super) async fn read_into_working_set(&mut self, file: &str) -> Result<ContextResponse> {
        let file = file.to_string();
        let context = self.refresh(std::slice::from_ref(&file)).await?;
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
        self.task.source.insert(file.clone(), source);
        self.touch_source(&file);
        Ok(context)
    }

    /// The first-edit guard exists so an edit's author has seen its target's
    /// source and the knowledge governing it. A file that does not exist yet
    /// has no source to see, so when reading it brings no governing rule or
    /// linked record the proposal's prompt did not already carry, the author
    /// saw everything the read shows and the edit proceeds in this step
    /// (`Some(true)`). When the read does bring something new, the file is now
    /// read and the model proposes again with it in view (`Some(false)`).
    /// `None`: not the first edit of a new file. badciv 14ad550e spent one
    /// turn per new file on a read that answered "[file does not exist]".
    async fn read_new_edit_target(
        &mut self,
        action: &model::Action,
        delivered: &ContextResponse,
    ) -> Result<Option<bool>> {
        let file = match action {
            model::Action::Replace { file, .. }
            | model::Action::Write { file, .. }
            | model::Action::Edit { file, .. } => file.clone(),
            _ => return Ok(None),
        };
        if self.task.read_files.contains(&file) || self.workspace.read(&file)?.is_some() {
            return Ok(None);
        }
        let read = self.read_into_working_set(&file).await?;
        let seen: std::collections::BTreeSet<&str> = delivered
            .governing_rules
            .iter()
            .map(|rule| rule.iri.as_str())
            .chain(delivered.evidence_iris.iter().map(String::as_str))
            // Rules delivered when a plan of this task was approved were in
            // view for the whole of its work, not only on the turn they came.
            .chain(
                self.task
                    .approved_plans
                    .iter()
                    .flat_map(|plan| plan.rules_in_view.iter().map(String::as_str)),
            )
            .collect();
        let unseen: Vec<&str> = read
            .governing_rules
            .iter()
            .map(|rule| rule.iri.as_str())
            .chain(read.evidence_iris.iter().map(String::as_str))
            .filter(|iri| !seen.contains(iri))
            .collect();
        if unseen.is_empty() {
            self.intent_event("first_edit_satisfied_absent", &file);
            self.event(format!(
                "First edit of new file {file}: reading it brought no governing knowledge the proposal had not seen, so the edit proceeds."
            ));
            return Ok(Some(true));
        }
        self.event(format!(
            "First-edit guard: {file} does not exist yet, but it is governed by {} record(s) the proposal had not seen; the edit will not execute. Propose it again with them in view.",
            unseen.len()
        ));
        self.task.last_response = format!(
            "Read {file} with its governing knowledge; propose the edit again with it in view."
        );
        Ok(Some(false))
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
            // Every retry would build the same prompt: stop for the human with
            // what outgrew the budget and what to do (Constraint 927d5176).
            if let Some(overflow) = error.downcast_ref::<model::PromptOverflow>() {
                self.task.last_response = overflow.guidance();
                self.event(self.task.last_response.clone());
                self.task.phase = Phase::AwaitingInput;
                self.persist()?;
                return Ok(());
            }
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
            !self.task.objective_pending,
            "the specification is approved; describe what to do next before planning"
        );
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
        let (prompt, source) = self.prompt(&context, &files)?;
        self.task.source_outlined = source.outlined();
        if let Some(receipt) = source.receipt() {
            self.intent_event("source_delivery", &receipt);
        }
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
        if self.read_new_edit_target(&action, &context).await? == Some(false) {
            self.candidate_accepted();
            if !message.trim().is_empty() {
                self.event(format!("Assistant: {message}"));
            }
            self.task.steps += 1;
            return self.persist();
        }
        let step = match self.validate_action(action) {
            Ok(step) => step,
            Err(error) => self
                .symbolic_noop_continuation(error)
                .map_err(|error| error.context(model::InvalidModelOutput))?,
        };
        let step = self
            .symbolic_finish_guard(step)
            .map_err(|error| error.context(model::InvalidModelOutput))?;
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
                addresses,
            } => {
                let context = self.refresh(&files).await?;
                // Before anything is stored: a plan whose summary skips a
                // governing rule goes back once with a note naming it.
                if self.plan_coverage_return(&summary, &context) {
                    return self.persist();
                }
                let addresses = self.resolve_plan_addresses(&addresses, &context);
                self.task.snapshots = self.snapshot(&files)?;
                self.task.read_files.retain(|file| files.contains(file));
                self.task.source.retain(|file, _| files.contains(file));
                self.task.plan = Some(Plan {
                    summary: summary.clone(),
                    files,
                    checks,
                    addresses,
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
                if self.refuse_repeated_command(&command) {
                    return self.persist();
                }
                self.end_unchanged_window();
                let result = self.run_command(&command).await?;
                self.commit_command_result(&command, result)?;
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
                        command.clone(),
                        justification,
                        read_paths,
                        write_paths,
                        network,
                    )
                    .await?
                {
                    self.end_unchanged_window();
                    self.commit_command_result(&command, result)?;
                }
            }
            Step::Question { question } => {
                self.event(format!("Assistant: {question}"));
                self.task.last_response = question;
                self.task.phase = Phase::AwaitingInput;
            }
            Step::Reply {
                message: reply,
                then,
            } => {
                if reply != message {
                    self.event(format!("Assistant: {reply}"));
                }
                if then == ReplyThen::Continue && self.continue_after_reply(&reply) {
                    return self.persist();
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
                self.refresh_code_index().await?;
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
        self.touch_source(&edit.file);
        self.clear_symbolic_batch_state(&edit);
        self.end_unchanged_window();
        self.intent_event("edit_applied", &edit.file);
        if self.is_approved_spec(&edit.file) {
            self.intent_event("spec_edited", &edit.file);
        }
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
        let mut grant_ids: Vec<_> = self
            .task
            .permission_grants
            .iter()
            .map(|grant| grant.id.clone())
            .collect();
        // A command that ran on standing capability is a permissioned command,
        // even with no task grant: journaling it as bare would understate what
        // it could reach.
        if !self.standing_read_paths().is_empty() {
            grant_ids.push("moosedev.toml:[harness.sandbox]".into());
        }
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

    pub(super) fn commit_command_result(
        &mut self,
        command: &str,
        result: executor::CommandResult,
    ) -> Result<()> {
        let denial = self.note_sandbox_denial(command, &result);
        self.task.last_response = result.output;
        match denial {
            // The denial already names the blocked paths, so ask the human
            // directly instead of spending a model turn on a request the
            // symbolic layer can write itself (Constraint cd9f1a96).
            Some(SandboxDenial::Grantable { paths, .. }) if !paths.is_empty() => {
                self.task.capture_due = true;
                self.task.after_review = Phase::Working;
                self.task.intent = None;
                if self.raise_gate_for_denial(command, &paths)? {
                    return Ok(());
                }
                self.task.last_response.push_str(SANDBOX_DENIAL_HINT);
                self.task.last_response.push_str(&format!(
                    "Paths named in the output: {}\n",
                    paths.join(", ")
                ));
            }
            Some(SandboxDenial::Grantable { .. }) => {
                self.task.last_response.push_str(SANDBOX_DENIAL_HINT);
            }
            Some(SandboxDenial::Ungrantable) => {
                self.task.last_response.push_str(UNGRANTABLE_DENIAL_HINT)
            }
            None => {}
        }
        self.task.capture_due = true;
        self.task.after_review = Phase::Working;
        self.task.intent = None;
        self.persist()
    }

    /// Journal a command the OS sandbox appears to have blocked. The caller
    /// appends the hint; nothing is granted here (Constraint 3a829c5c).
    fn note_sandbox_denial(
        &mut self,
        command: &str,
        result: &executor::CommandResult,
    ) -> Option<SandboxDenial> {
        let network_granted = self
            .task
            .permission_grants
            .iter()
            .any(|grant| grant.network);
        let denial = classify_denial(result, network_granted, &self.task.root);
        match &denial {
            Some(SandboxDenial::Grantable { .. }) => self.intent_event("sandbox_denial", command),
            Some(SandboxDenial::Ungrantable) => {
                self.intent_event("sandbox_denial_ungrantable", command)
            }
            None => {}
        }
        denial
    }

    pub(super) fn scratch_path(&self) -> PathBuf {
        self.task
            .root
            .join(".moosedev/harness/scratch")
            .join(&self.task.id)
    }

    /// Vacuous-check returns already spent on this task.
    fn vacuous_returns(&self) -> usize {
        self.task
            .intent_events
            .iter()
            .filter(|event| event.kind == "check_vacuous_returned")
            .count()
    }

    /// Whether any required check passed without verifying anything. The
    /// completion line reports this rather than claiming the checks passed.
    pub(super) fn vacuous_checks(&self) -> Vec<String> {
        self.task
            .intent_events
            .iter()
            .filter(|event| event.kind == "check_vacuous_unmet")
            .map(|event| event.detail.clone())
            .collect()
    }

    async fn verify_next(&mut self) -> Result<()> {
        let plan = self.task.plan.as_ref().context("no plan")?;
        let index = self.task.check_results.len();
        if let Some(command) = plan.checks.get(index).cloned() {
            let result = self.run_command(&command).await?;
            let denial = self.note_sandbox_denial(&command, &result);
            let ungrantable = denial == Some(SandboxDenial::Ungrantable);
            self.record_symbolic_check(&command, result.success, denial.is_some(), ungrantable);
            let failure = (!result.success)
                .then(|| check_failure_response(&command, &result, denial.as_ref()));
            if let (false, Some(code)) = (result.success, unrunnable_exit(&result)) {
                self.intent_event("check_unrunnable", &format!("exit {code}: {command}"));
            }
            self.task.check_results.push(CheckResult {
                command: command.clone(),
                success: result.success,
                output: result.output.clone(),
            });
            if let Some(response) = failure {
                // A denial that names nothing grantable is not a decision the
                // model can make: no request_permission can be valid, and only
                // the human can change the plan's checks. Park without a call.
                if ungrantable {
                    self.intent_event("check_ungrantable", &command);
                    self.event(format!(
                        "Required check `{command}` cannot run inside the sandbox and names nothing to grant; waiting for the human to change the plan's checks."
                    ));
                    self.task.phase = Phase::AwaitingInput;
                } else {
                    self.task.phase = Phase::Working;
                }
                self.task.last_response = response;
                self.task.capture_due = true;
                self.task.after_review = Phase::Working;
            } else if let Some(reason) = vacuous_reason(&result) {
                // The check passed without running anything, so it has not
                // verified the change. Return it to the model once, the way an
                // unaddressed rule returns a plan: it can add a test, and if it
                // does not, the task still finishes — with the journal and the
                // completion line saying what actually happened. A project with
                // no tests yet is never wedged.
                self.intent_event("check_vacuous", &format!("{reason}: {command}"));
                if self.vacuous_returns() < VACUOUS_RETURN_LIMIT {
                    self.intent_event("check_vacuous_returned", &command);
                    self.task.phase = Phase::Working;
                    self.task.last_response = format!(
                        "Required check `{command}` succeeded but ran no tests ({reason}), so it has not verified this change. Add a test that exercises what you changed and fails without it, then finish again."
                    );
                    self.task.capture_due = true;
                    self.task.after_review = Phase::Working;
                } else {
                    self.intent_event("check_vacuous_unmet", &command);
                }
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

/// Output signatures of a test runner that executed nothing. A check that exits
/// 0 having run no test proves only that the code builds, so the harness must
/// not accept it as the verification its completion line claims.
///
/// Deterministic substring matching, like the sandbox denials below: the
/// symbolic layer reads the runner's own report rather than asking a model
/// whether a check meant anything.
/// Vacuous-check returns per task before the task is allowed to finish anyway.
/// One, matching the plan-coverage return limit: the nudge is worth sending
/// once, and a project that genuinely has no tests must not be trapped.
const VACUOUS_RETURN_LIMIT: usize = 1;

const VACUOUS_CHECKS: [&str; 6] = [
    "running 0 tests",
    "no tests ran",
    "No tests found",
    "collected 0 items",
    "0 passing",
    "Tests:       0 total",
];

/// Why a check that succeeded nonetheless verified nothing, or `None`.
///
/// A failed check is never vacuous: its failure is the signal, and
/// [`classify_denial`] already owns reading that output.
fn vacuous_reason(result: &executor::CommandResult) -> Option<&'static str> {
    if !result.success {
        return None;
    }
    VACUOUS_CHECKS
        .iter()
        .find(|signature| result.output.contains(**signature))
        .copied()
}

const PATH_DENIALS: [&str; 5] = [
    "Operation not permitted",
    "Permission denied",
    "os error 1)",
    "os error 13)",
    "Read-only file system",
];
const NETWORK_DENIALS: [&str; 8] = [
    "Could not resolve host",
    "failed to lookup address",
    "Temporary failure in name resolution",
    "Network is unreachable",
    // Package managers the sandbox runs offline say so in their own words:
    // Cargo under CARGO_NET_OFFLINE (badciv a2e43815 spent its task digging
    // through registry caches for a crate one network grant would fetch), uv,
    // pip, and Node's resolver.
    "but --offline was specified",
    "Network connectivity is disabled",
    "Failed to establish a new connection",
    "getaddrinfo",
];
/// Appended to a failed command's observation when the output shows the OS
/// sandbox blocked it. It steers the model to the typed request; it never
/// requests or grants anything itself.
const SANDBOX_DENIAL_HINT: &str = "\nThe harness sandbox blocked this command; this is not a defect in the project and not something to work around. If the command needs a path outside the project or network access, your next action is request_permission with this exact command, the blocked absolute path in read_paths or write_paths (or network true), and a short justification; the human then approves or denies it. Do not reply that it cannot be done, replan, or substitute a weaker check.\n";

/// Appended instead when the denial names nothing a grant could cover: a
/// request_permission would be refused by validation, so the model is told
/// not to make one.
const UNGRANTABLE_DENIAL_HINT: &str = "\nThe harness sandbox blocked this command, and its output names no path outside the project and no network need, so a permission request has nothing to grant: the program probably needs a terminal, a device or a process right the sandbox never provides. Do not request permission for it. Choose a command that runs without one, or ask the human with question.\n";

/// Paths a denial can never be about: devices and process pseudo-files are not
/// grantable, and the trusted-PATH binaries are already readable inside the
/// sandbox (they appear as the reporting program, `/bin/sh: x: Permission denied`).
const NEVER_GRANTABLE: [&str; 7] = [
    "/dev/",
    "/proc/",
    "/sys/",
    "/bin/",
    "/sbin/",
    "/usr/bin/",
    "/usr/sbin/",
];

/// What a sandbox denial can be turned into: a typed permission request for
/// the resources the output names, or nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SandboxDenial {
    Grantable { paths: Vec<String>, network: bool },
    Ungrantable,
}

/// Classify a failed command's OS denial signature by what a grant could
/// cover. This only selects what the model is told (or, for a required check,
/// that the human is asked instead); permission is still requested by the
/// model and granted by the human.
fn classify_denial(
    result: &executor::CommandResult,
    network_granted: bool,
    root: &std::path::Path,
) -> Option<SandboxDenial> {
    if result.success {
        return None;
    }
    let path_denied = PATH_DENIALS
        .iter()
        .any(|signature| result.output.contains(signature));
    let network = !network_granted
        && NETWORK_DENIALS
            .iter()
            .any(|signature| result.output.contains(signature));
    if !path_denied && !network {
        return None;
    }
    let paths = if path_denied {
        grantable_paths(&result.output, root)
    } else {
        Vec::new()
    };
    Some(if paths.is_empty() && !network {
        SandboxDenial::Ungrantable
    } else {
        SandboxDenial::Grantable { paths, network }
    })
}

/// Absolute (or `~/`) paths named in a command's output that a grant could
/// cover: outside the project root (which includes the task scratch) and not
/// in `NEVER_GRANTABLE`. Deterministic text scanning; no guessing beyond it.
fn grantable_paths(output: &str, root: &std::path::Path) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let separators = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';'
            )
    };
    for token in output.split(separators) {
        let token = token.trim_end_matches([':', '.', ',']);
        if token.len() < 2 || !(token.starts_with('/') || token.starts_with("~/")) {
            continue;
        }
        if NEVER_GRANTABLE
            .iter()
            .any(|prefix| token.starts_with(prefix))
            || std::path::Path::new(token).starts_with(root)
        {
            continue;
        }
        if !paths.iter().any(|known| known == token) {
            paths.push(token.to_owned());
        }
    }
    paths
}

/// What the model is told about a failed required check. A check the shell
/// could not start tested nothing, so it is named as an invalid check rather
/// than a failure to repair in the code; a check the sandbox blocked is named
/// as a permission need rather than either; a check the sandbox blocked with
/// nothing to grant is written for the human, who is the only one who can
/// change the plan's checks.
fn check_failure_response(
    command: &str,
    result: &executor::CommandResult,
    denial: Option<&SandboxDenial>,
) -> String {
    match (unrunnable_exit(result), denial) {
        (Some(code), _) => format!(
            "Required check could not run: the shell exited {code} (command not found or not executable), so the change was not tested. The check itself is invalid, not the environment: replan with checks that are shell command lines, such as the task's verification commands.\nCheck: {command}\n{}",
            result.output
        ),
        (None, Some(SandboxDenial::Ungrantable)) => format!(
            "Required check `{command}` was blocked by the sandbox, and its output names no path outside the project and no network need, so no permission grant can make it run — it likely needs a terminal or device the sandbox never provides.\n{}\nReply with a check that can run without one (the plan returns for approval), or /plan to change the plan's checks.",
            result.output.trim_end()
        ),
        (None, Some(SandboxDenial::Grantable { paths, .. })) => {
            let named = if paths.is_empty() {
                String::new()
            } else {
                format!("Paths named in the output: {}\n", paths.join(", "))
            };
            format!(
                "Required check was blocked by the sandbox, so the change was not tested.\nCheck: {command}\n{}{SANDBOX_DENIAL_HINT}{named}",
                result.output
            )
        }
        (None, None) => format!(
            "Required verification failed. Repair before completion.\n{}",
            result.output
        ),
    }
}

impl Runner {
    /// The journal event of the last run of exactly `command`, when nothing
    /// that could change its output has happened since: no applied edit, no
    /// human message or decision, no permission change. Rerunning it would
    /// print the same thing (badciv a2e43815 ran one `ls` of the cargo
    /// registry four times in a row).
    fn unchanged_command_run(&self, command: &str) -> Option<usize> {
        let prefix = format!("Command: {command}\nPermission grants: ");
        let last = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with(&prefix))?;
        let changed = self.task.events[last + 1..].iter().any(|event| {
            let message = event.message.as_str();
            message.starts_with("Applied edit")
                || message.starts_with("Human ")
                || message.starts_with("Task permission")
        });
        (!changed).then_some(last)
    }

    /// Refuse an exact repeat of a command whose output cannot have changed,
    /// pointing at the earlier result. A second refusal in a row parks the
    /// task for the human instead of spending more model calls. True when the
    /// command must not run.
    fn refuse_repeated_command(&mut self, command: &str) -> bool {
        let Some(event) = self.unchanged_command_run(command) else {
            return false;
        };
        let refused_before = self.task.events[event + 1..]
            .iter()
            .any(|later| later.message.starts_with("Not run: this exact command"));
        self.intent_event(
            "command_repeat_refused",
            &format!("event {event}: {command}"),
        );
        if refused_before {
            let message = format!(
                "The model keeps proposing a command that already ran at event {event} with nothing changed since, so it would print the same output:\n{command}\nGuidance is needed: say what to try instead, grant what it lacks, or /plan to change the approach."
            );
            self.event(format!(
                "Not run: this exact command repeated again; parked for guidance (event {event})."
            ));
            self.task.last_response = message;
            self.task.phase = Phase::AwaitingInput;
            self.task.turn_finished = true;
            return true;
        }
        let message = format!(
            "Not run: this exact command ran at event {event} and nothing has changed since (no edit, no permission change, no message from the human), so it would print the same output. Its result is in event {event}; inspect({event}, 0) pages it. Take a different step: change the code, run a different command, request_permission if the sandbox blocked it, or ask the human with question."
        );
        self.event(message.clone());
        self.task.last_response = message;
        true
    }
}

impl Runner {
    /// The model said its reply is not the end of its turn (`then:
    /// continue`): it is about to act. Without this the turn ended on "I will
    /// now begin…" and the human had to say "ok" (badciv, six runs; replayed,
    /// Gemma chose reply on that prompt 6 of 6 times). Show the reply and ask
    /// for the next action. Once per human message, so a model that keeps
    /// replying still hands the turn back. True when the turn continues.
    fn continue_after_reply(&mut self, reply: &str) -> bool {
        if !matches!(self.task.phase, Phase::Planning | Phase::Working) {
            return false;
        }
        let since_human = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with("Human "))
            .map_or(0, |index| index + 1);
        if self.task.events[since_human..]
            .iter()
            .any(|event| event.message.starts_with(REPLY_CONTINUED))
        {
            return false;
        }
        self.intent_event("reply_continued", &bounded(reply, 200));
        let next = if self.task.mode == Mode::Plan {
            "in Plan mode the next action is plan"
        } else {
            "take the next step of the approved plan, or finish if the work is done"
        };
        self.event(format!(
            "{REPLY_CONTINUED} the reply said the model is about to act: {next}."
        ));
        self.task.last_response = format!(
            "{}\n\n(Continuing: you said you are about to act, so take that action now; {next}.)",
            reply.trim()
        );
        true
    }
}

/// Journal marker of a continued reply; one per human message.
const REPLY_CONTINUED: &str = "Continuing after a reply:";

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

    /// A check that exits 0 without running a test proves only that the code
    /// builds. This is the badciv-map case: `cargo test -p badciv-map` reported
    /// "running 0 tests ... ok" on a crate with no tests, and the task
    /// completed claiming required checks passed.
    #[test]
    fn a_passing_check_that_ran_no_tests_is_vacuous_and_a_failing_one_never_is() {
        let passed = |output: &str| executor::CommandResult {
            success: true,
            output: output.into(),
            exit_code: Some(0),
        };
        assert_eq!(
            vacuous_reason(&passed(
                "   Compiling badciv-map v0.1.0\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed\n"
            )),
            Some("running 0 tests")
        );
        for (output, signature) in [
            ("no tests ran in 0.01s", "no tests ran"),
            ("No tests found, exiting with code 0", "No tests found"),
            ("collected 0 items", "collected 0 items"),
            ("  0 passing (2ms)", "0 passing"),
            ("Tests:       0 total", "Tests:       0 total"),
        ] {
            assert_eq!(vacuous_reason(&passed(output)), Some(signature), "{output}");
        }
        // A check that ran something is not vacuous, whatever else it printed.
        assert_eq!(
            vacuous_reason(&passed(
                "running 12 tests\ntest result: ok. 12 passed; 0 failed\n"
            )),
            None
        );
        // A failed check is never vacuous: the failure is the signal, and
        // classify_denial owns reading that output.
        assert_eq!(
            vacuous_reason(&result(Some(101), "running 0 tests\nerror: build failed")),
            None
        );
    }

    #[test]
    fn a_check_the_shell_cannot_start_is_named_invalid_not_a_code_failure() {
        let missing = result(Some(127), "/bin/sh: The: command not found");
        let response =
            check_failure_response("The implementation must be idempotent.", &missing, None);
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
            assert_eq!(classify_denial(&ordinary, false, root()), None);
            assert!(
                check_failure_response("python3 -m unittest", &ordinary, None)
                    .starts_with("Required verification failed. Repair before completion.")
            );
        }
    }

    fn root() -> &'static std::path::Path {
        std::path::Path::new("/Users/dev/project")
    }

    #[test]
    fn a_check_the_sandbox_blocked_is_named_a_permission_need() {
        let blocked = result(
            Some(101),
            "failed to read configuration file `/outside/config.toml`\nOperation not permitted (os error 1)",
        );
        let denial = classify_denial(&blocked, false, root());
        assert_eq!(
            denial,
            Some(SandboxDenial::Grantable {
                paths: vec!["/outside/config.toml".into()],
                network: false
            })
        );
        let response = check_failure_response("cargo check", &blocked, denial.as_ref());
        assert!(response.starts_with("Required check was blocked by the sandbox"));
        assert!(response.contains("request_permission"), "{response}");
        assert!(
            response.ends_with("Paths named in the output: /outside/config.toml\n"),
            "{response}"
        );
        assert!(!response.contains("Repair before completion"), "{response}");

        let offline = result(Some(6), "curl: (6) Could not resolve host: example.org");
        assert_eq!(
            classify_denial(&offline, false, root()),
            Some(SandboxDenial::Grantable {
                paths: vec![],
                network: true
            })
        );
        // With network already granted, a lookup failure is a real failure.
        assert_eq!(classify_denial(&offline, true, root()), None);
        let cargo_offline = result(
            Some(101),
            "error: failed to download `zerocopy-derive v0.8.57`\n\nCaused by:\n  attempting to make an HTTP request, but --offline was specified",
        );
        assert_eq!(
            classify_denial(&cargo_offline, false, root()),
            Some(SandboxDenial::Grantable {
                paths: vec![],
                network: true
            })
        );
        let passed = executor::CommandResult {
            success: true,
            output: "Permission denied".into(),
            exit_code: Some(0),
        };
        assert_eq!(classify_denial(&passed, false, root()), None);
    }

    #[test]
    fn a_denial_naming_nothing_grantable_is_written_for_the_human() {
        // The badciv shape: a TUI dying on the terminal; the only path is the
        // task's own scratch build inside the project root.
        let tui = result(
            Some(1),
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s\n     Running `/Users/dev/project/.moosedev/harness/scratch/301f7887/build/debug/badciv-tui`\nError: Os { code: 1, kind: PermissionDenied, message: \"Operation not permitted\" }\n",
        );
        let denial = classify_denial(&tui, false, root());
        assert_eq!(denial, Some(SandboxDenial::Ungrantable));
        let response = check_failure_response("cargo run -p badciv-tui", &tui, denial.as_ref());
        assert!(
            response.starts_with("Required check `cargo run -p badciv-tui` was blocked by the sandbox, and its output names no path"),
            "{response}"
        );
        assert!(
            response.contains("Reply with a check that can run without one"),
            "{response}"
        );
        assert!(!response.contains("request_permission"), "{response}");

        // Devices and the reporting shell are never grantable; a home path is.
        assert_eq!(
            classify_denial(
                &result(Some(1), "open /dev/tty: Operation not permitted"),
                false,
                root()
            ),
            Some(SandboxDenial::Ungrantable)
        );
        assert_eq!(
            classify_denial(
                &result(Some(126), "/bin/sh: ./run.sh: Permission denied"),
                false,
                root()
            ),
            Some(SandboxDenial::Ungrantable)
        );
        assert_eq!(
            grantable_paths(
                "failed to read `~/.cargo/config.toml`: Permission denied (os error 13)",
                root()
            ),
            vec!["~/.cargo/config.toml".to_string()]
        );
        assert_eq!(
            grantable_paths("cat: /private/tmp/note.txt: Operation not permitted\ncat: /private/tmp/note.txt: again", root()),
            vec!["/private/tmp/note.txt".to_string()]
        );
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
