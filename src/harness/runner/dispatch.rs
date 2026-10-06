//! One model step: refresh evidence, ask for one action, validate it, and
//! execute the resulting step under the approval and policy gates.
use super::recover;
use super::*;

/// What a served read is: a file outlined for space, one outside the step's
/// scope, or one the prompt already shows in full.
#[derive(Debug, Clone, Copy)]
enum Serve {
    Outlined,
    OutsideScope,
    Shown,
}

/// The status and message of a provider refusal (payment, authentication
/// or permission), whether an action request or the compatibility probe met it.
fn refusal_of(error: &anyhow::Error) -> Option<(u16, String)> {
    let cause = error
        .downcast_ref::<crate::llm::CompletionError>()
        .or_else(|| {
            error
                .downcast_ref::<crate::harness::response::ProbeError>()
                .map(|probe| &probe.cause)
        })?;
    match cause {
        crate::llm::CompletionError::Refused { status, message } => {
            Some((*status, message.clone()))
        }
        _ => None,
    }
}

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

    /// Whether the approved plan still stands: accepted knowledge and the
    /// plan's files unchanged since approval. The one refresh is the step's
    /// context: dossiers for the files already read, and governing rules for
    /// those and the plan's other files too, so a plan file's rules are in
    /// view before its first write and the step needs no second refresh.
    pub(super) async fn fresh_approval(&mut self) -> Result<bool> {
        let files = self
            .task
            .plan
            .as_ref()
            .context("no approved plan")?
            .files
            .clone();
        let context = self.refresh_approved_scope().await?;
        let sources = self.snapshot(&files)?;
        if self.task.approved_revision.as_deref() != Some(&context.revision)
            || sources != self.task.snapshots
        {
            self.task.mode = Mode::Plan;
            self.task.phase = Phase::AwaitingPlan;
            self.task.approved_revision = None;
            self.task.approved_change_scope = None;
            self.task.snapshots = sources;
            self.disarm_harness_arms();
            // Edits made under the withdrawn approval were checked against
            // source or knowledge that has since changed.
            self.symbolic_state_mut().coverage_reset = true;
            self.task.check_results.clear();
            self.discard_pending_edit("source or accepted knowledge changed")?;
            self.discard_pending_permission("source or accepted knowledge changed")?;
            self.discard_pending_choice("source or accepted knowledge changed")?;
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

    /// The approved step's context: dossiers for the files read, governing
    /// rules for those and the plan's other files.
    pub(super) async fn refresh_approved_scope(&mut self) -> Result<ContextResponse> {
        let files = self
            .task
            .plan
            .as_ref()
            .context("no approved plan")?
            .files
            .clone();
        let read = self.task.read_files.clone();
        let unread: Vec<String> = files
            .iter()
            .filter(|file| !read.contains(file))
            .cloned()
            .collect();
        self.refresh_with_rules(&read, &unread).await
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
    /// A preloaded scope file the prompt showed in full is handled the same
    /// way: its author saw its current source, so reading it in only has to
    /// bring nothing new.
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
        let preloaded = self.preloaded_in_full(&file);
        if self.task.read_files.contains(&file)
            || (!preloaded && self.workspace.read(&file)?.is_some())
        {
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
        let (kind, shown) = if preloaded {
            (
                "first_edit_satisfied_preloaded",
                format!("{file} was shown in full as a scope file"),
            )
        } else {
            (
                "first_edit_satisfied_absent",
                format!("{file} does not exist yet"),
            )
        };
        if unseen.is_empty() {
            self.intent_event(kind, &file);
            self.event(if preloaded {
                format!("First edit of {file}: it was shown in full as a scope file and reading it brought no governing knowledge the proposal had not seen, so the edit proceeds.")
            } else {
                format!("First edit of new file {file}: reading it brought no governing knowledge the proposal had not seen, so the edit proceeds.")
            });
            return Ok(Some(true));
        }
        self.event(format!(
            "First-edit guard: {shown}, but it is governed by {} record(s) the proposal had not seen; the edit will not execute. Propose it again with them in view.",
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
        // Set again only by the path that leaves an observation this advance.
        self.task.last_response_observation = false;
        // A floor fallback holds for the step it happened in, however that
        // step ended.
        self.rule_claims_floor_only = false;
        self.step_prompt = None;
        self.looking_parked = false;
        let result = loop {
            match self.advance_inner().await {
                Err(error) if self.repair_candidate(&error)? => continue,
                outcome => break outcome,
            }
        };
        // A read, inspect or search loop parked: the model names what it is
        // missing before the human is asked.
        if result.is_ok() && std::mem::take(&mut self.looking_parked) {
            if let Some(prompt) = self.step_prompt.take() {
                self.ask_what_is_missing(prompt).await?;
            }
        }
        if let Err(error) = &result {
            self.task.last_error = Some(format!("{error:#}"));
            self.task.last_error_kind = Some(error_kind(error).into());
            self.event(format!("Step rejected or interrupted: {error:#}"));
            // Every retry would build the same prompt: stop for the human with
            // what outgrew the budget and what to do (Constraint 927d5176).
            if let Some(overflow) = error.downcast_ref::<model::PromptOverflow>() {
                // Recover, don't park: the prompt cannot shrink by itself, so
                // the harness finishes as best it can.
                if recover::recover_enabled() {
                    self.task.stuck_recoveries = recover::STUCK_CONTINUES;
                    self.recover("context overflow", &overflow.guidance());
                    // The recovery owns the error: the step is not failed.
                    self.task.last_error = None;
                    self.task.last_error_kind = None;
                    self.persist()?;
                    return Ok(());
                }
                self.task.last_response = overflow.guidance();
                self.event(self.task.last_response.clone());
                self.task.phase = Phase::AwaitingInput;
                self.persist()?;
                return Ok(());
            }
            // The provider refused for payment, authentication or permission:
            // every request, the next compatibility probe included, would be
            // refused the same way (badciv orG2: 402 Payment Required, retried
            // 600+ times a replicate). Stop for the human with what to fix.
            if let Some((status, message)) = refusal_of(error) {
                self.intent_event("provider_refused", &format!("HTTP {status}"));
                self.task.last_response = format!(
                    "The model provider refused the request (HTTP {status}: {}). Nothing was sent again. Fix the account, key or model access (for HTTP 402, add credit), then continue.",
                    super::bounded(&message, 300)
                );
                self.event(self.task.last_response.clone());
                self.task.phase = Phase::AwaitingInput;
                self.task.turn_finished = true;
                self.park_under_approved_plan();
                self.persist()?;
                return Ok(());
            }
            // A response past the content limit is a runaway generation: the
            // same request would repeat it, so stop for the human with what
            // happened instead of leaving the step to be resent unchanged.
            if let Some(crate::llm::CompletionError::TooLarge(detail)) =
                error.downcast_ref::<crate::llm::CompletionError>()
            {
                self.intent_event("response_size_exceeded", detail);
                if recover::recover_enabled() {
                    self.recover(
                        "response too large",
                        &format!("The model's response passed the size limit ({detail}). Write less at once: one file or one function per step."),
                    );
                    self.task.last_error = None;
                    self.task.last_error_kind = None;
                    self.persist()?;
                    return Ok(());
                }
                self.task.last_response = format!(
                    "The model's response passed the size limit ({detail}); sending the same request again would repeat it. Guidance is needed: say what to write next, or /plan to split the work."
                );
                self.event(self.task.last_response.clone());
                self.task.phase = Phase::AwaitingInput;
                self.task.turn_finished = true;
                self.park_under_approved_plan();
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
        // The step cap: no more model steps, so the harness finishes the task
        // as best it can instead of stopping for the human.
        if self.task.steps >= MAX_STEPS
            && recover::recover_enabled()
            && self.task.best_effort.is_none()
        {
            self.task.stuck_recoveries = recover::STUCK_CONTINUES;
            self.recover(
                "step cap",
                &format!("The task reached {MAX_STEPS} model steps."),
            );
        }
        if self.task.best_effort.is_some() {
            return self.best_effort_finish().await;
        }
        if self.auto_fix_due() {
            return self.auto_apply_fix().await;
        }
        if self.auto_verify_due() {
            return self.auto_finish().await;
        }
        anyhow::ensure!(
            !self.task.objective_pending,
            "the specification is approved; describe what to do next before planning"
        );
        anyhow::ensure!(
            self.task.steps < MAX_STEPS,
            "task reached {MAX_STEPS} model steps; inspect and provide new guidance"
        );
        let files = self.workspace.files()?;
        // Entering a planning cycle, the harness searches the objective's
        // words itself, before the model's first step of it.
        self.gather_for_objective(&files).await?;
        let targets = self.task.read_files.clone();
        // The step's scope, before its refresh: in Plan mode the scope files
        // that may be preloaded bring their governing rules, not dossiers
        // (Auto already sends the approved plan's unread files).
        let preload = self.scope_candidates(&files);
        let rule_files = if self.task.mode == Mode::Plan
            && self
                .context
                .as_ref()
                .is_none_or(|context| context.context_contracts.contains(&1))
        {
            preload.rule_files()
        } else {
            Vec::new()
        };
        // An approved step's context was refreshed by `fresh_approval` above
        // for exactly the files read.
        let reuse = self.context.as_ref().filter(|context| {
            self.task.mode == Mode::Auto
                && context.files.len() == targets.len()
                && targets
                    .iter()
                    .all(|file| context.files.iter().any(|entry| &entry.file == file))
        });
        let context = match reuse {
            Some(context) => context.clone(),
            None => self.refresh_with_rules(&targets, &rule_files).await?,
        };
        self.preload_scope(&context, preload);
        // A previously read or preloaded file may have changed through
        // another client.
        let mut reload: std::collections::BTreeSet<String> =
            self.task.source.keys().cloned().collect();
        reload.extend(targets.iter().cloned());
        for file in reload {
            let text = self.workspace.read(&file)?;
            self.task.source.insert(file, text);
        }
        let built = match self.prompt_with_plan(&context, &files) {
            // Rule claims past the daemon's fixed floor are the first thing a
            // crowded prompt gives up: ask again for the floor alone, so the
            // budget share never stops a step that fitted before it.
            Err(error)
                if error.is::<model::PromptOverflow>()
                    && self.claim_budget_accepted()
                    && self.rule_claim_budget().is_some() =>
            {
                // For the rest of this step, so its later refreshes and the
                // retrieved-claim fill stay at the floor too.
                self.rule_claims_floor_only = true;
                let floor = if self.task.mode == Mode::Auto {
                    self.refresh_approved_scope().await?
                } else {
                    self.refresh_with_rules(&targets, &rule_files).await?
                };
                self.intent_event(
                    "rule_claims_floor",
                    "prompt overflowed with the rule-claim budget; rebuilt with the fixed floor",
                );
                self.prompt_with_plan(&floor, &files)
                    .map(|built| (floor, built))
            }
            built => built.map(|built| (context, built)),
        };
        let (context, (prompt, source, plan)) = match built {
            // The scope's own rules must never stop a step: without them and
            // the preloads, the prompt is the one the step had before.
            Err(error) if error.is::<model::PromptOverflow>() && !rule_files.is_empty() => {
                self.withdraw_scope_preload();
                let context = self.refresh(&targets).await?;
                let built = self.prompt_with_plan(&context, &files)?;
                (context, built)
            }
            built => built?,
        };
        self.commit_rules_snapshot();
        if !source.swapped.is_empty() {
            let total: usize = self.task.source.values().flatten().map(String::len).sum();
            self.intent_event(
                "source_swap",
                &format!(
                    "{}; working set {} files, {total} bytes; source budget {}",
                    source.swapped.join(", "),
                    self.task.source.len(),
                    source.budget
                ),
            );
        }
        self.task.source_outlined = source.outlined();
        self.task.source_full = source.full();
        self.source_budget = Some(source.budget);
        if let Some(receipt) = source.receipt() {
            self.intent_event("source_delivery", &receipt);
        }
        // What each section of this prompt took, on every request: compact
        // in the intent journal, whole on the request's journal entry, both
        // written when the request is sent ([`Self::model_json`]).
        self.context_plan = Some(plan);
        self.step_prompt = Some(prompt.clone());
        let output: ModelOutput = self
            .model_json(&prompt, "harness_action", self.action_schema())
            .await?;
        let (message, action) = output.parts();
        // A scope escape arrives here as a replan too; only the model's own
        // replan can be continued.
        let proposed_replan = matches!(action, model::Action::Replan { .. });
        // A file an earlier approved plan listed joins the plan without
        // asking, so the scope check below lets its edit through.
        self.auto_scope_add(&action).await;
        let Some(action) = self.symbolic_intercept(action)? else {
            self.source_outlines_seen();
            return self.persist();
        };
        self.validate_permission(&action)?;
        if self.read_new_edit_target(&action, &context).await? == Some(false) {
            self.candidate_accepted();
            self.source_outlines_seen();
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
        self.source_outlines_seen();
        // What the prompt that produced this action showed as the Last
        // result, before its narration replaces it: gates that judge whether
        // text is still in view read this.
        let shown = self.task.last_response.clone();
        if !message.trim().is_empty() {
            self.task.last_response = message.clone();
            self.event(format!("Assistant: {message}"));
        }
        self.task.steps += 1;
        self.task.handed_back = false;
        self.task.plan_stands_park = false;
        self.event(format!("Model action: {}", serde_json::to_string(&step)?));
        self.persist()?;
        self.task.last_response_observation = matches!(
            step,
            Step::Inspect { .. }
                | Step::Read { .. }
                | Step::ReadRefused { .. }
                | Step::ReadOutlined { .. }
                | Step::ReadOutsideScope { .. }
                | Step::ReadShown { .. }
                | Step::Search { .. }
        );
        match step {
            Step::Inspect { event, offset } => {
                if self.refuse_repeated_inspect(event, offset, &shown) {
                    return self.persist();
                }
                // A page is as large as the next prompt can show unclipped, so
                // an event that fits arrives whole in one step. A fixed 2 KB
                // page overwrote itself: badciv 3ba41310 paged one 13.9 KB
                // search result thirteen times.
                let page = self
                    .next_inspect_budget(&context)?
                    .saturating_sub(INSPECT_HEADER_RESERVE)
                    .max(1);
                let observation = self
                    .task
                    .events
                    .get(event)
                    .context("unknown journal event")?;
                let size = observation.message.len();
                let mut end = offset.saturating_add(page).min(size);
                while !observation.message.is_char_boundary(end) {
                    end -= 1;
                }
                // Always at least one character, so paging advances.
                if end == offset && end < size {
                    end += 1;
                    while !observation.message.is_char_boundary(end) {
                        end += 1;
                    }
                }
                self.task.last_response = format!(
                    "Journal event {event}, bytes {offset}..{end} of {size}:\n{}",
                    &observation.message[offset..end]
                );
            }
            Step::Read { file } => {
                self.read_into_working_set(&file).await?;
                self.task.last_response = format!("Read {file} with its governing knowledge.");
                self.serve_read_batch().await?;
            }
            Step::ReadRefused { file, reason } => {
                self.refuse_read(&file, &reason);
                self.serve_read_batch().await?;
            }
            Step::ReadOutlined { file } => {
                self.serve_outlined_read(&file, &context, Serve::Outlined)?;
                self.serve_read_batch().await?;
            }
            Step::ReadShown { file } => {
                self.serve_outlined_read(&file, &context, Serve::Shown)?;
                self.serve_read_batch().await?;
            }
            Step::ReadOutsideScope { file } => {
                self.serve_outlined_read(&file, &context, Serve::OutsideScope)?;
                self.serve_read_batch().await?;
            }
            Step::Search { query } => {
                // A query already asked in this task returns the same records;
                // answer from the stored result rather than spending a daemon
                // round trip on it again.
                let next = if !search_park_enabled() {
                    "use a different action."
                } else if self.task.mode == Mode::Plan {
                    "propose the plan, or ask the human with question."
                } else {
                    "edit, run a check, or finish if the work is done."
                };
                if let Some(answer) = self.repeat_search_answer(&query, next) {
                    if self.park_repeated_search(&query) {
                        return self.persist();
                    }
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
                satisfied,
                stubs,
                unchanged,
                open_choices,
            } => {
                let context = self.refresh(&files).await?;
                // Before anything is stored: a plan whose summary skips a
                // governing rule goes back once with a note naming it. A
                // rule already settled, or one the plan says the existing
                // code satisfies, needs no answer.
                let (claimed, _) = symbolic::satisfied_entries(&satisfied, &addresses, &context);
                if self.plan_coverage_return(&summary, &context, &claimed) {
                    return self.persist();
                }
                if self.spec_deferral_return(&files, &addresses, &claimed, &context) {
                    return self.persist();
                }
                let addresses = self.resolve_plan_addresses(&addresses, &context);
                let satisfied = self.resolve_plan_satisfied(&satisfied, &addresses, &context);
                self.journal_rules_settled(&context.governing_rules, &satisfied);
                let stubs = self.resolve_plan_stubs(&stubs, &files);
                let unchanged = self.resolve_plan_unchanged(&unchanged, &files);
                self.task.snapshots = self.snapshot(&files)?;
                self.task.read_files.retain(|file| files.contains(file));
                self.task.source.retain(|file, _| files.contains(file));
                // A stored plan ends the planning round: its one return for
                // the spec's open rules is spent or not needed.
                self.symbolic_state_mut().spec_deferral_returns = 0;
                self.task.plan = Some(Plan {
                    summary: summary.clone(),
                    files,
                    checks,
                    addresses,
                    satisfied,
                    stubs,
                    unchanged,
                    open_rules: Vec::new(),
                    open_choices,
                });
                self.task.approved_change_scope = None;
                self.start_intent_cycle();
                self.event(format!(
                    "Proposed plan: {}",
                    serde_json::to_string(&self.task.plan)?
                ));
                // What the stored plan leaves open, for the approval gate
                // (the coverage check journaled any it kept unmet). Harness
                // state, not the model's: the event above, which later steps
                // see, is the plan as proposed. A new plan replaces what the
                // last one left open and the answers to its choices.
                let open_rules = self.open_rules(&context);
                if let Some(plan) = self.task.plan.as_mut() {
                    plan.open_rules = open_rules;
                }
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
                    let (file, existed, after) =
                        (edit.file.clone(), edit.before.is_some(), edit.after.clone());
                    self.apply_edit(edit)?;
                    self.settle_applied_edit(&file, existed, after.as_deref(), false)
                        .await;
                    self.persist()?;
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
                self.task.handed_back = true;
            }
            Step::Reply {
                message: reply,
                then,
            } => {
                if reply != message {
                    self.event(format!("Assistant: {reply}"));
                }
                // A reply that waits while approved work is under way asks the
                // human nothing (that is what question is for): badciv
                // 1e6cd3e7's a4b wrote "Let's fix badciv-sim/Cargo.toml first"
                // and the turn ended. It continues like `then: continue`,
                // once per human message.
                let mid_work = self.task.mode == Mode::Auto && self.task.phase == Phase::Working;
                if (then == ReplyThen::Continue || mid_work) && self.continue_after_reply(&reply) {
                    return self.persist();
                }
                self.task.last_response = reply;
                self.task.turn_finished = true;
                self.task.handed_back = true;
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
                    if self.hold_replan(&files, &reason) {
                        return Ok(());
                    }
                    self.intent_event("model_replan", &reason);
                }
                self.enter_replan(reason);
            }
            Step::Finish { summary } => {
                // The model's own finish: a failure now is its run, not the
                // harness's.
                if let Some(state) = self.task.symbolic.as_mut() {
                    state.auto_verification = None;
                }
                self.begin_verification(summary).await?;
            }
        }
        Ok(())
    }

    /// Leave approved work for planning with `reason` as the planner's last
    /// result: the model's replan, or the human's choice at a scope escape.
    pub(super) fn enter_replan(&mut self, reason: String) {
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

    /// After an applied edit: check it with the language servers, then arm
    /// what the harness may do next by itself on that fresh result (a
    /// preferred fix, or the required checks), unless the result names a
    /// missing module the human is asked about instead. `chained` keeps the
    /// count of fixes the harness applied in a row; a model edit restarts it.
    pub(super) async fn settle_applied_edit(
        &mut self,
        file: &str,
        existed: bool,
        after: Option<&str>,
        chained: bool,
    ) {
        if !chained {
            self.symbolic_state_mut().auto_fix_chain = 0;
        }
        let fresh = self.check_applied_edit(file, existed, after).await;
        if self.ask_missing_module_after_edit() {
            return;
        }
        self.arm_auto_fix(fresh);
        self.arm_auto_verify(fresh);
        if let Some(note) = self.note_source_revisit() {
            self.task.last_response = format!("{note}{}", self.task.last_response);
        }
    }

    /// From finished work to the required checks. The language-server,
    /// unfinished-plan and stub gates each send the model back once for a
    /// source state (planned files still missing then ask the human); then
    /// the code index refresh and the link review, then Verifying. The model's
    /// finish, the no-op continuation and the harness's own auto-verify all
    /// come through here.
    pub(super) async fn begin_verification(&mut self, summary: String) -> Result<()> {
        // The best-effort finish runs the checks whatever the source holds:
        // the gates that send the model back first are for a model that can
        // still act on them.
        if self.task.best_effort.is_none() && self.finish_gate_refused()? {
            return self.persist();
        }
        self.task.last_response = summary;
        self.refresh_code_index().await?;
        if self.prepare_symbolic_associations().await? {
            return Ok(());
        }
        self.task.phase = Phase::Verifying;
        self.task.check_results.clear();
        Ok(())
    }

    /// The finish's gates: settled diagnostics, then planned files missing
    /// or unedited, then stubs. True when one sent the model back.
    fn finish_gate_refused(&mut self) -> Result<bool> {
        // Settled language-server errors or lints: send the model back once
        // with them, before any check runs. A repeat finish on the same
        // result goes on to the checks, which decide.
        let refusal = self
            .task
            .diagnostics
            .as_mut()
            .filter(|d| d.blocks_finish())
            .map(|diagnostics| {
                diagnostics.finish_refused = true;
                (
                    diagnostics.render(DIAGNOSTICS_BYTES),
                    format!(
                        "{} error(s), {} warning(s), {} lint(s)",
                        diagnostics.errors.len(),
                        diagnostics.warnings.len(),
                        diagnostics.lints.len()
                    ),
                )
            });
        if let Some((block, counts)) = refusal {
            self.intent_event("finish_refused_diagnostics", &counts);
            self.event(format!(
                "Finish refused: the language server reports {counts} in the current source."
            ));
            self.task.last_response = format!(
                "Not finished: the language server reports problems in the current source. Errors fail the required checks; fix them, and fix the warnings and lints too unless they are wrong for this code, then finish.\n{block}"
            );
            return Ok(true);
        }
        // Planned files missing or unedited, then stubs: each sends the model
        // back once for a source state; files still missing then ask the
        // human.
        Ok(self.refuse_unfinished_plan()? || self.refuse_stubbed_finish())
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
        self.progressed();
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
        let unchanged = (!result.success)
            .then(|| self.unchanged_failure_note())
            .flatten();
        if result.success {
            self.note_pass(command);
        }
        // A blocked command is the permission gates' to answer.
        let unblocked_failure = !result.success && denial.is_none();
        let stalled = unblocked_failure
            .then(|| self.note_failure(command, &result.output))
            .flatten();
        self.task.last_response = format!("{}{}", stalled.unwrap_or_default(), result.output);
        if let Some(note) = unchanged {
            self.task.last_response.push_str(&note);
        }
        // Every command path ends here: a model command, an already covered
        // permission request, and an approved one run on the next advance.
        self.task.last_response_observation = true;
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
        if unblocked_failure {
            self.ask_missing_module_in_output(&result.output);
        }
        self.task.capture_due = true;
        self.task.after_review = Phase::Working;
        self.task.intent = None;
        self.persist()
    }

    /// A note when this failure reports exactly the errors the previous failed
    /// command did, although source was edited in between: the edit did not
    /// touch what fails. badciv 839ebeec rebuilt through four edits of the same
    /// file with the same three errors at the same lines, each time reading
    /// only a tail of the output. The current command's event is the last
    /// `Command:` event in the journal.
    fn unchanged_failure_note(&self) -> Option<String> {
        let mut failures = self
            .task
            .events
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, event)| event.message.starts_with("Command: "));
        let (current, latest) = failures.next()?;
        let latest = super::source::failed_command_output(&latest.message)?;
        let (previous, earlier) = failures.find_map(|(index, event)| {
            super::source::failed_command_output(&event.message).map(|output| (index, output))
        })?;
        let edits = self.task.events[previous + 1..current]
            .iter()
            .filter(|event| event.message.starts_with("Applied edit"))
            .count();
        let errors = error_lines(latest);
        (edits > 0 && !errors.is_empty() && errors == error_lines(earlier)).then(|| {
            format!(
                "\n[Harness: the same {} error line(s) as the failed command at event {previous}, although {edits} edit(s) were applied since; those edits did not change what fails. Read the code the errors point at, and the definitions they name, before editing again.]\n",
                errors.len()
            )
        })
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
            if result.success {
                self.note_pass(&command);
            }
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
                // The best-effort finish: a failing check ends the task
                // incomplete, with what it decided captured as unverified. An
                // ungrantable denial stays the human's stop, below.
                if self.task.best_effort.is_some() && !ungrantable {
                    self.task.last_response = response;
                    return self.capture_incomplete(&command).await;
                }
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
                let by_harness =
                    self.task.symbolic.as_ref().is_some_and(|state| {
                        state.auto_verification == Some(self.task.edits.len())
                    });
                self.task.last_response = if by_harness {
                    format!("{}\n{response}", symbolic::AUTO_VERIFY_FAILED)
                } else {
                    response
                };
                // A blocked check is the permission gates' to answer.
                if let Some(note) = denial
                    .is_none()
                    .then(|| self.note_failure(&command, &result.output))
                    .flatten()
                {
                    self.task.last_response.insert_str(0, &note);
                }
                if denial.is_none() {
                    self.ask_missing_module_in_output(&result.output);
                }
                // The loop detector, through `note_failure`, may just have
                // ended the recoveries: this failure is the best-effort one.
                if self.task.best_effort.is_some() && !ungrantable {
                    return self.capture_incomplete(&command).await;
                }
                self.task.capture_due = true;
                self.task.after_review = Phase::Working;
            } else if let Some(reason) = vacuous_reason(&result) {
                // The check passed without running anything, so it has not
                // verified the change. Return it to the model once, the way an
                // unaddressed rule returns a plan: it can add a test, and if it
                // does not, the task still finishes — with the journal and the
                // completion line saying what actually happened. A project with
                // no tests yet is never wedged, and neither is a plan that
                // leaves stubs: the approved scaffold has nothing to test yet
                // (the a4b rerun's skeleton plan cost 2 parks this way).
                self.intent_event("check_vacuous", &format!("{reason}: {command}"));
                let scaffold = self
                    .task
                    .plan
                    .as_ref()
                    .is_some_and(|plan| !plan.stub_files().is_empty());
                // A best-effort finish has no model step left to add a test.
                if !scaffold
                    && self.task.best_effort.is_none()
                    && self.vacuous_returns() < VACUOUS_RETURN_LIMIT
                {
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

/// The error lines of a failed command's output, with the locations
/// compilers print under them: what "the same failure" compares.
pub(super) fn error_lines(output: &str) -> std::collections::BTreeSet<&str> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.starts_with("error") && !lower.starts_with("error: could not compile"))
                || line.starts_with("--> ")
        })
        .collect()
}

/// Bytes the language-server block may take in a prompt or a refusal.
/// Room for about three errors with the compiler's full text, and the lints.
pub(super) const DIAGNOSTICS_BYTES: usize = 4_000;

/// How a refused repeat inspect begins, in the journal and the Last result.
const INSPECT_REFUSED: &str = "Not shown again: inspect of";
/// Journals a search repeat that parked the task.
const SEARCH_REPEAT_PARKED: &str = "Search repeated after its stored answer:";

/// `MOOSEDEV_HARNESS_SEARCH_PARK=off` keeps answering a repeated search
/// from its stored result without ever parking.
fn search_park_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_SEARCH_PARK").map_or(true, |value| value.trim() != "off")
}
pub(super) const READ_REFUSED: &str = "Not read again:";

/// Whether an inspect of a page that has left the prompt is served again
/// (once) instead of refused. `MOOSEDEV_HARNESS_INSPECT_RESERVE=off`
/// restores the refusal of any repeat in the run.
fn inspect_reserve_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_INSPECT_RESERVE").map_or(true, |value| value.trim() != "off")
}
/// Room for the "Journal event N, bytes a..b of c:" line above a page.
const INSPECT_HEADER_RESERVE: usize = 96;
/// Bytes kept for the note that says where a served outlined read that did
/// not fit continues.
const PAGE_NOTE_RESERVE: usize = 128;
/// Vacuous-check returns per task before the task is allowed to finish anyway.
/// One, matching the plan-coverage return limit: the nudge is worth sending
/// once, and a project that genuinely has no tests must not be trapped.
const VACUOUS_RETURN_LIMIT: usize = 1;

/// Output signatures of a test runner that executed nothing. A check that exits
/// 0 having run no test proves only that the code builds, so the harness must
/// not accept it as the verification its completion line claims.
///
/// Deterministic substring matching, like the sandbox denials below: the
/// symbolic layer reads the runner's own report rather than asking a model
/// whether a check meant anything.
const VACUOUS_CHECKS: [&str; 7] = [
    "running 0 tests",
    "0 passed",
    "no tests ran",
    "No tests found",
    "collected 0 items",
    "0 passing",
    "Tests:       0 total",
];

/// Why a check that succeeded nonetheless verified nothing, or `None`.
///
/// A failed check is never vacuous: its failure is the signal, and
/// [`classify_denial`] already owns reading that output. A zero signature
/// alone is not enough: `cargo test` prints "running N tests" once per test
/// binary, and its doc-test stage reports "running 0 tests" even when the
/// integration tests ran 14 (badciv 3ba41310, where the model then added a
/// filler unit test to satisfy the harness). The check verified nothing only
/// when no count in its output shows a test ran.
fn vacuous_reason(result: &executor::CommandResult) -> Option<&'static str> {
    if !result.success || tests_ran(&result.output) {
        return None;
    }
    VACUOUS_CHECKS
        .iter()
        .find(|signature| result.output.contains(**signature))
        .copied()
}

/// Whether any runner in the output reports a test that passed: cargo's and
/// pytest's "N passed", mocha's "N passing", jest's "Tests: … N passed". A
/// test that was listed but ignored or skipped verified nothing: badciv
/// be128e71 finished with `parse_map` unimplemented because cargo's "running 1
/// test" for an `#[ignore]`d fixture counted as a run.
fn tests_ran(output: &str) -> bool {
    tests_passed(output) > 0
}

/// Tests the runners in `output` report as passed, summed over every report
/// line (cargo prints one per test binary). Python's unittest prints "Ran N
/// tests" and then OK or FAILED, never a passed count; a failure fails the
/// check, so its N counts only for a check that succeeded.
pub(super) fn tests_passed(output: &str) -> u64 {
    let count = |token: &str| {
        token
            .trim_matches(|c: char| !c.is_ascii_digit())
            .parse::<u64>()
            .ok()
    };
    output
        .lines()
        .map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.len() >= 3 && tokens[0] == "Ran" && tokens[2].starts_with("test") {
                return count(tokens[1]).unwrap_or(0);
            }
            tokens
                .windows(2)
                .find(|pair| pair[1].starts_with("passed") || pair[1].starts_with("passing"))
                .and_then(|pair| count(pair[0]))
                .unwrap_or(0)
        })
        .sum()
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
    /// registry four times in a row). A capture checkpoint's confirmation or
    /// review is no such change ([`review::is_human_progress`]).
    pub(super) fn unchanged_command_run(&self, command: &str) -> Option<usize> {
        let prefix = format!("Command: {command}\nPermission grants: ");
        let last = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with(&prefix))?;
        let changed = self.task.events[last + 1..].iter().any(|event| {
            let message = event.message.as_str();
            message.starts_with("Applied edit")
                || review::is_progress(message)
                || message.starts_with("Task permission")
        });
        (!changed).then_some(last)
    }

    /// The searches answered from their stored result since the last
    /// progress (a human answer, an applied edit or a proposed plan), each
    /// with how many times, in the order first searched. Each query's stored
    /// answers are matched by their whole opening, never parsed out of the
    /// text, so a query holding the answer's own words cannot be misread.
    fn repeated_searches(&self) -> Vec<(String, usize)> {
        let start = self
            .task
            .events
            .iter()
            .rposition(|event| {
                let message = event.message.as_str();
                review::is_progress(message)
                    || message.starts_with("Applied edit")
                    || message.starts_with("Proposed plan: ")
            })
            .map_or(0, |progress| progress + 1);
        let stretch = &self.task.events[start..];
        let mut repeats: Vec<(String, usize)> = Vec::new();
        for search in &self.task.knowledge_searches {
            if repeats.iter().any(|(seen, _)| *seen == search.query) {
                continue;
            }
            let opening = format!(
                "{} '{}' in this task; it returned ",
                symbolic::SEARCH_REPEATED,
                search.query
            );
            let count = stretch
                .iter()
                .filter(|event| event.message.starts_with(&opening))
                .count();
            if count > 0 {
                repeats.push((search.query.clone(), count));
            }
        }
        repeats
    }

    /// A search asked again after its stored result was already repeated
    /// once since the last progress parks the task for the human, as a
    /// repeated read, page or command does (gate audit invariant I1: every
    /// refusal has a way out). Before, the stored answer never escalated:
    /// simH1 prompt 2 searched four queries 211 times in Planning without a
    /// plan, and Qwen3.5-9B repeated one search 60 times (Lesson cb5cbfb0).
    /// While a required check fails, the steer before a park comes first.
    /// True when the step was answered here.
    fn park_repeated_search(&mut self, query: &str) -> bool {
        if !search_park_enabled() {
            return false;
        }
        let repeats = self.repeated_searches();
        if !repeats.iter().any(|(seen, _)| seen == query) {
            return false;
        }
        if let Some(steer) = self.steer_before_park() {
            self.task.last_response = steer;
            return true;
        }
        // Asks: the first search, each stored answer, and this one.
        let listed = repeats
            .iter()
            .map(|(seen, count)| {
                let asks = count + 1 + usize::from(seen == query);
                format!("'{seen}' ({asks} times)")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let without = if self.task.mode == Mode::Plan {
            "without proposing a plan"
        } else {
            "without editing"
        };
        self.intent_event("search_repeat_parked", &listed);
        self.looking_parked = true;
        self.stop_stuck(
            "search loop",
            format!("{SEARCH_REPEAT_PARKED} '{query}' parked for guidance."),
            format!(
                "The model keeps searching for {listed} {without}; project knowledge and the repository have nothing more on them. Guidance is needed: give the missing information, say to proceed on an assumption, or /plan to change the approach."
            ),
        );
        true
    }

    /// Whether the Last result, which the prompt shows, is this page.
    fn inspect_page_showing(event: usize, offset: usize, shown: &str) -> bool {
        shown.starts_with(&format!("Journal event {event}, bytes {offset}.."))
    }

    /// Whether an inspect of this page, journaled at or after `before`,
    /// would be refused: it was requested earlier in the run and either the
    /// prompt still shows it or it was already served twice.
    /// `shown` is the Last result the prompt that asked for it showed.
    pub(super) fn inspect_refusal_due(
        &self,
        event: usize,
        offset: usize,
        before: usize,
        shown: &str,
    ) -> bool {
        let earlier = self.inspect_requests_before(event, offset, before);
        !earlier.is_empty()
            && (!inspect_reserve_enabled()
                || Self::inspect_page_showing(event, offset, shown)
                || earlier.len() >= 2)
    }

    /// The earlier requests for exactly this page in the current run of
    /// inspects (back to the last other model action or human progress),
    /// latest first.
    fn inspect_requests_before(&self, event: usize, offset: usize, before: usize) -> Vec<usize> {
        let Ok(action) = serde_json::to_string(&Step::Inspect { event, offset }) else {
            return Vec::new();
        };
        let action = format!("Model action: {action}");
        let mut found = Vec::new();
        for (index, earlier) in self.task.events[..before].iter().enumerate().rev() {
            let message = earlier.message.as_str();
            if message == action {
                found.push(index);
            } else if review::is_progress(message)
                || (message.starts_with("Model action: ")
                    && !message.starts_with("Model action: {\"action\":\"inspect\""))
            {
                break;
            }
        }
        found
    }

    /// Refuse an inspect of a page the prompt still shows, or of one already
    /// served twice in this run of inspects. A page served once and since
    /// replaced by another is served again: the model no longer has it
    /// (badciv replicates, 3 of 6: a page refused as "already shown" after
    /// it had left the prompt, then parked). The two-serve bound keeps an
    /// alternation of pages closed (badciv f2fe1f61 alternated two pages 132
    /// times). A second refusal in the run parks the task for the human, as
    /// a repeated command does. True when the page must not be shown again.
    fn refuse_repeated_inspect(&mut self, event: usize, offset: usize, shown: &str) -> bool {
        // The last event is this step's own journaled action.
        let before = self.task.events.len().saturating_sub(1);
        let earlier = self.inspect_requests_before(event, offset, before);
        let Some(&latest) = earlier.first() else {
            return false;
        };
        let showing = Self::inspect_page_showing(event, offset, shown);
        if !self.inspect_refusal_due(event, offset, before, shown) {
            self.intent_event(
                "inspect_served_again",
                &format!("event {event} offset {offset}, first at event {latest}"),
            );
            return false;
        }
        let first = *earlier.last().unwrap_or(&latest);
        let refused_before = self.task.events[first + 1..]
            .iter()
            .any(|later| later.message.starts_with(INSPECT_REFUSED));
        self.intent_event(
            "inspect_repeat_refused",
            &format!("event {event} offset {offset}, first at event {first}"),
        );
        if refused_before {
            // While a required check fails, the first look that would park
            // gets the failure's source instead (the steer before a park).
            if let Some(steer) = self.steer_before_park() {
                self.task.last_response = steer;
                return true;
            }
            self.looking_parked = true;
            self.stop_stuck(
                "inspect loop",
                format!("{INSPECT_REFUSED} parked for guidance (event {event}, offset {offset})."),
                format!(
                    "The model keeps asking for pages of journal event {event} it already had, without acting on them. Guidance is needed: say what to do next, or /plan to change the approach."
                ),
            );
            return true;
        }
        let next = if self.task.mode == Mode::Plan {
            "Take a different step: propose the plan, or ask the human with question."
        } else {
            "Take a different step: edit, run a check, search, or finish if the work is done."
        };
        let message = if showing {
            format!(
                "{INSPECT_REFUSED} event {event} at offset {offset} is the Last result above, and journal events never change. {next}"
            )
        } else {
            format!(
                "{INSPECT_REFUSED} event {event} at offset {offset} was already shown {} times in this run, most recently at event {latest}; showing it again will not change it. {next}",
                earlier.len()
            )
        };
        self.event(message.clone());
        self.task.last_response = message;
        true
    }

    /// The rest of a response's leading reads ([`tools::batched_reads`]), run
    /// after its first. Several files cannot share the one Last result, so
    /// each file a read would take in or serve joins the working set while
    /// it has room, as a re-read outside the scope does; a file refused or
    /// served outlined is named, not shown, and a refusal here never parks.
    /// The Last result keeps the first read's answer, with a line for the
    /// rest. Nothing runs once the first read parked the task.
    async fn serve_read_batch(&mut self) -> Result<()> {
        let files = std::mem::take(&mut self.read_batch);
        if files.is_empty() || self.task.phase == Phase::AwaitingInput {
            return Ok(());
        }
        let mut read = Vec::new();
        let mut not_read = Vec::new();
        for file in files {
            let step = self.read_step(file.clone());
            self.event(format!("Model action: {}", serde_json::to_string(&step)?));
            let room =
                self.task.read_files.len() < MAX_FILES || self.task.read_files.contains(&file);
            match step {
                Step::Read { .. } | Step::ReadOutsideScope { .. } if room => {
                    self.read_into_working_set(&file).await?;
                    read.push(format!("`{file}`"));
                }
                Step::ReadRefused { reason, .. } => {
                    not_read.push(format!("`{file}` ({reason})"));
                }
                _ => not_read.push(format!(
                    "`{file}` (outlined or no room in the working set; read it alone to see it)"
                )),
            }
        }
        self.intent_event(
            "read_batch",
            &format!("read {}; not read {}", read.len(), not_read.len()),
        );
        let mut line = String::new();
        if !read.is_empty() {
            line.push_str(&format!(
                "\nAlso read from the same response, now in the working set with their governing knowledge: {}.",
                read.join(", ")
            ));
        }
        if !not_read.is_empty() {
            line.push_str(&format!(
                "\nNot read from the same response: {}.",
                not_read.join("; ")
            ));
        }
        self.task.last_response.push_str(&line);
        Ok(())
    }

    /// Refuse a read whose file the prompt already covers, leaving the source
    /// tiers as they were. A second refusal of the same file while the model
    /// is only looking (reads, inspects and searches since the last human
    /// message, edit or other action) parks the task for the human, so
    /// refusals cannot become the next loop. Refusals of different files do
    /// not add up: a planner reading several files in turn is not looping
    /// (badciv replicates: one counter for every file parked 5 of 6).
    fn refuse_read(&mut self, file: &str, reason: &str) {
        // The last event is this step's own journaled action.
        let before = self.task.events.len().saturating_sub(1);
        let this_file = [
            format!("{READ_REFUSED} `{file}` "),
            format!("{READ_REFUSED} {file} "),
        ];
        let refused_before = self.looking_run(before).any(|(_, earlier)| {
            this_file
                .iter()
                .any(|prefix| earlier.message.starts_with(prefix))
        });
        self.intent_event("read_repeat_refused", &format!("{file}: {reason}"));
        if refused_before {
            if let Some(steer) = self.steer_before_park() {
                self.task.last_response = steer;
                return;
            }
            self.looking_parked = true;
            self.stop_stuck(
                "read loop",
                format!("{READ_REFUSED} parked for guidance ({file})."),
                format!(
                    "The model keeps asking to read files whose current text it already has ({file} last), without planning or editing. Guidance is needed: say what to do next, or /plan to change the approach."
                ),
            );
            return;
        }
        let message = format!("{READ_REFUSED} {reason}");
        self.event(message.clone());
        self.task.last_response = message;
    }

    /// The journal events before `before`, latest first, back to the model's
    /// last progress: the last human message, applied edit, guarded edit
    /// attempt or model action other than a read, inspect or search. A
    /// capture checkpoint's confirmation or review is not progress
    /// ([`review::is_human_progress`]).
    pub(super) fn looking_run(&self, before: usize) -> impl Iterator<Item = (usize, &Event)> {
        self.task.events[..before.min(self.task.events.len())]
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, earlier)| {
                let message = earlier.message.as_str();
                let looking = ["read", "inspect", "search"].iter().any(|kind| {
                    message.starts_with(&format!("Model action: {{\"action\":\"{kind}\""))
                });
                // An edit a guard turned into a read journals as a read, after
                // its guard's event: that attempt is progress, not looking.
                let guarded_edit = ["First-edit guard:", "Edit guard:", "Fix guard:"]
                    .iter()
                    .any(|guard| message.starts_with(guard));
                !(review::is_progress(message)
                    || message.starts_with("Applied edit")
                    || guarded_edit
                    || (message.starts_with("Model action: ") && !looking))
            })
    }

    /// Serve the current text of `file`, outlined in Source for space, as the
    /// Last result, leaving the working set and its tiers as they are: no
    /// source tier moves, so the read cannot outline the next file it needs.
    /// A text larger than the Last result can show is served from its start,
    /// with where to page the rest from. The read snapshot is refreshed to
    /// the served text, and a Last result that holds all of it lets an edit
    /// of the file through the outline guard. A file outside the step's
    /// scope (`outside_scope`) is served the same way, never having been in
    /// the working set, and says so.
    fn serve_outlined_read(
        &mut self,
        file: &str,
        context: &ContextResponse,
        serve: Serve,
    ) -> Result<()> {
        let text = self
            .workspace
            .read(file)?
            .context("a served file no longer exists")?;
        self.symbolic_state_mut()
            .read_snapshots
            .insert(file.to_string(), fingerprint(&Some(text.clone())));
        let (respond, prefix, kind): (fn(&str, &str) -> String, _, _) = match serve {
            Serve::OutsideScope => (
                actions::outside_scope_text_response,
                actions::OUTSIDE_SCOPE_SERVED,
                "read_served_outside_scope",
            ),
            Serve::Outlined => (
                actions::outlined_text_response,
                actions::OUTLINED_SERVED,
                "outlined_read_served",
            ),
            Serve::Shown => (
                actions::shown_text_response,
                actions::SHOWN_SERVED,
                "shown_read_served",
            ),
        };
        let whole = respond(file, &text);
        let budget = self.next_inspect_budget(context)?;
        // A file shown in full that the Last result cannot hold whole is not
        // served a part of: the copy under Source is the whole text, and a
        // part would then be refused as a repeat.
        if matches!(serve, Serve::Shown) && whole.len() > budget {
            let reason = self
                .redundant_read(file)
                .unwrap_or_else(|| format!("{file} is shown in full under Source."));
            self.refuse_read(file, &reason);
            return Ok(());
        }
        let size = text.len();
        let header = format!("{prefix} {file} ({size} bytes):\n");
        let event = self.task.events.len();
        let shown = if whole.len() <= budget {
            size
        } else {
            let mut end = budget
                .saturating_sub(whole.len() - size + PAGE_NOTE_RESERVE)
                .min(size);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            end
        };
        self.event(format!("{header}{text}"));
        self.intent_event(kind, &format!("{file}: bytes 0..{shown} of {size}"));
        self.task.last_response = if shown == size {
            whole
        } else {
            let offset = header.len() + shown;
            format!(
                "{}\n[Bytes 0..{shown} of {size} shown; the rest is in journal event {event}: inspect({event}, {offset}).]",
                respond(file, &text[..shown])
            )
        };
        Ok(())
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
            self.stop_stuck(
                "command repeat",
                format!(
                    "Not run: this exact command repeated again; parked for guidance (event {event})."
                ),
                message,
            );
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
    /// for the next action. Once per human message or per progress (an edit
    /// or a required-check result since the last continued reply: badciv P5's
    /// a4b made four edits between two replies and was handed back), so a
    /// model that only keeps replying still hands the turn back. True when
    /// the turn continues.
    fn continue_after_reply(&mut self, reply: &str) -> bool {
        if !matches!(self.task.phase, Phase::Planning | Phase::Working) {
            return false;
        }
        let since_human = self
            .task
            .events
            .iter()
            .rposition(|event| review::is_progress(&event.message))
            .map_or(0, |index| index + 1);
        let now = (self.task.edits.len(), self.task.check_results.len());
        let progressed = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.reply_continued_at)
            .is_some_and(|(edits, checks)| now.0 > edits || now.1 > checks);
        if !progressed
            && self.task.events[since_human..]
                .iter()
                .any(|event| event.message.starts_with(REPLY_CONTINUED))
        {
            return false;
        }
        self.symbolic_state_mut().reply_continued_at = Some(now);
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

/// Journal marker of a continued reply; one per human message or progress.
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
        // badciv 3ba41310: cargo's unit binary and doc-tests ran nothing, the
        // integration binary ran 14. Not vacuous.
        assert_eq!(
            vacuous_reason(&passed(
                "     Running unittests src/lib.rs\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n\n     Running tests/roundtrip.rs\n\nrunning 14 tests\ntest parse_tiny_fixture ... ok\n\ntest result: ok. 14 passed; 0 failed; 0 ignored\n\n   Doc-tests badciv_map\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n"
            )),
            None
        );
        // Every binary empty: still vacuous. Other runners' positive counts
        // clear their own zero-looking lines.
        assert_eq!(
            vacuous_reason(&passed(
                "running 0 tests\ntest result: ok. 0 passed\n\n   Doc-tests x\n\nrunning 0 tests\n"
            )),
            Some("running 0 tests")
        );
        for output in [
            "collected 3 items\n\n3 passed in 0.02s",
            "  2 passing (4ms)\n  0 pending",
            "Tests:       5 passed, 5 total",
        ] {
            assert_eq!(vacuous_reason(&passed(output)), None, "{output}");
        }
        // codex: cargo's empty doc-test stage beside unittest's own report.
        assert_eq!(
            vacuous_reason(&passed(
                "running 0 tests\ntest result: ok. 0 passed\n....\nRan 4 tests in 0.01s\n\nOK\n"
            )),
            None
        );
        // badciv be128e71: the only test was #[ignore]d. Listed is not run.
        assert_eq!(
            vacuous_reason(&passed(
                "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n\n     Running tests/tiny_fixture.rs\n\nrunning 1 test\ntest test_tiny_fixture ... ignored\n\ntest result: ok. 0 passed; 0 failed; 1 ignored\n"
            )),
            Some("running 0 tests")
        );
        assert_eq!(
            vacuous_reason(&passed("running 1 test\ntest x ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored\n")),
            Some("0 passed")
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

#[cfg(test)]
mod human_progress_tests {
    //! badciv run 14 (c7abc2d0): every headless capture checkpoint journaled
    //! "Human confirmed…", which ended each guard's window, so 15 rounds of
    //! inspect, `ls`, read and `cargo test` ran without a refusal.
    use super::test_support::{context_router, serve, Project};
    use super::*;

    const CONFIRMED: &str = "Human confirmed that no durable knowledge changed at this checkpoint.";
    const ACCEPTED: &str = "Human accepted captured knowledge.\n{}";
    const REJECTED: &str = "Human rejected captured knowledge.\n{}";
    const ANSWER: &str = "Human response: use the fixture file";

    async fn runner(prefix: &str) -> (Runner, Project, tokio::task::JoinHandle<()>) {
        let project = Project::new(prefix);
        let (daemon, server) = serve(context_router(), &project).await;
        let runner = Runner::create(project.0.clone(), daemon, "Build the map".into())
            .await
            .unwrap();
        (runner, project, server)
    }

    #[tokio::test]
    async fn a_command_repeated_across_a_capture_checkpoint_is_refused() {
        let (mut runner, _project, server) = runner("progress-command").await;
        runner.event("Command: cargo test\nPermission grants: none\nSuccess: true\nok");
        let ran = runner.task.events.len() - 1;
        for neutral in [CONFIRMED, ACCEPTED, REJECTED] {
            runner.event(neutral);
            assert_eq!(runner.unchanged_command_run("cargo test"), Some(ran));
        }
        assert!(runner.refuse_repeated_command("cargo test"));
        assert!(runner
            .task
            .last_response
            .starts_with("Not run: this exact command ran at event"));
        // A human answer can change what the command prints.
        runner.event(ANSWER);
        assert_eq!(runner.unchanged_command_run("cargo test"), None);
        server.abort();
    }

    #[tokio::test]
    async fn an_inspect_repeated_across_a_capture_checkpoint_is_refused() {
        let (mut runner, _project, server) = runner("progress-inspect").await;
        let inspect = format!(
            "Model action: {}",
            serde_json::to_string(&Step::Inspect {
                event: 0,
                offset: 0
            })
            .unwrap()
        );
        runner.event(inspect.clone());
        let first = runner.task.events.len() - 1;
        runner.event(CONFIRMED);
        runner.event(ACCEPTED);
        // This step's own journaled action, while the page is the Last
        // result: the confirmation and review between did not end the run.
        runner.event(inspect.clone());
        runner.task.last_response = "Journal event 0, bytes 0..10 of 10:\n0123456789".into();
        let before = runner.task.events.len() - 1;
        assert_eq!(runner.inspect_requests_before(0, 0, before), [first]);
        let shown = runner.task.last_response.clone();
        assert!(runner.refuse_repeated_inspect(0, 0, &shown));
        runner.event(ANSWER);
        runner.event(inspect);
        let before = runner.task.events.len() - 1;
        assert!(runner.inspect_requests_before(0, 0, before).is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn a_looking_run_continues_across_a_capture_checkpoint() {
        let (mut runner, _project, server) = runner("progress-looking").await;
        let read = "Model action: {\"action\":\"read\",\"file\":\"src/parse.rs\"}";
        runner.event(read);
        runner.event(format!("{READ_REFUSED} `src/parse.rs` is current"));
        let refused = runner.task.events.len() - 1;
        runner.event(CONFIRMED);
        runner.event(REJECTED);
        runner.event(read);
        let before = runner.task.events.len() - 1;
        let run: Vec<usize> = runner.looking_run(before).map(|(index, _)| index).collect();
        assert!(run.contains(&refused), "{run:?}");
        // So a second refused read stops the loop, as it does with no
        // checkpoint: the harness recovers, which ends the run.
        runner.refuse_read("src/parse.rs", "`src/parse.rs` is current");
        assert_ne!(runner.task.phase, Phase::AwaitingInput);
        assert!(runner
            .task
            .events
            .last()
            .unwrap()
            .message
            .starts_with("Harness recovery (read loop, 1 of 2)"));
        runner.event(read);
        let before = runner.task.events.len() - 1;
        assert_eq!(runner.looking_run(before).count(), 0);
        // A human answer ends the run too.
        runner.event(ANSWER);
        runner.event(read);
        let before = runner.task.events.len() - 1;
        assert_eq!(runner.looking_run(before).count(), 0);
        server.abort();
    }

    #[tokio::test]
    async fn a_capture_checkpoint_does_not_rearm_a_continued_reply() {
        let (mut runner, _project, server) = runner("progress-reply").await;
        runner.task.phase = Phase::Working;
        assert!(runner.continue_after_reply("I will now write the test."));
        runner.event(CONFIRMED);
        assert!(!runner.continue_after_reply("I will now write the test."));
        runner.event(ANSWER);
        assert!(runner.continue_after_reply("I will now write the test."));
        server.abort();
    }
}
