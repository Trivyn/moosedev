//! Harness questions to the human (`PendingChoice`). The symbolic layer
//! defaults what it can (Constraint cd9f1a96); what it cannot, whether the
//! approved plan may grow or whether work may be verified with planned files
//! missing, it asks the human with the options it can carry out, instead of
//! answering it by replanning or by verifying a false completion.
use super::*;

/// Whether a scope escape asks the human. `MOOSEDEV_HARNESS_SCOPE_CHOICE=off`
/// keeps the automatic replan for study variants.
pub(super) fn scope_choice_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_SCOPE_CHOICE").map_or(true, |value| value.trim() != "off")
}

fn option(key: &str, label: String) -> ChoiceOption {
    ChoiceOption {
        key: key.into(),
        label,
    }
}

impl Runner {
    /// Park the task on a question for the human. The gate `/choose` answers.
    fn park_on_choice(&mut self, choice: PendingChoice) -> Result<()> {
        let keys: Vec<&str> = choice
            .options
            .iter()
            .map(|option| option.key.as_str())
            .collect();
        let subject = match &choice.kind {
            ChoiceKind::ScopeAdd { file } => file.clone(),
            ChoiceKind::MissingPlannedFile { files } => files.join(", "),
        };
        self.intent_event(
            "choice_asked",
            &format!(
                "{} {subject}: {} (default {})",
                choice.kind.name(),
                keys.join(", "),
                choice.default
            ),
        );
        self.event(format!("Harness question: {}", choice.prompt));
        self.task.pending_choice = Some(choice);
        self.task.phase = Phase::AwaitingChoice;
        self.persist()
    }

    /// The model proposed an edit to `file`, outside the approved `plan_files`.
    pub(super) fn ask_scope_add(&mut self, file: &str, plan_files: &[String]) -> Result<()> {
        self.park_on_choice(PendingChoice {
            id: uuid::Uuid::new_v4().to_string(),
            kind: ChoiceKind::ScopeAdd { file: file.into() },
            prompt: format!(
                "The model wants to edit `{file}`, which is outside the approved plan ({}).",
                plan_files.join(", ")
            ),
            options: vec![
                option(
                    "add",
                    format!("Add {file} to the approved plan; the model makes its edit"),
                ),
                option("replan", "Return to Plan to rework the plan".into()),
                option(
                    "refuse",
                    "Refuse: the model continues within the plan".into(),
                ),
            ],
            default: "add".into(),
        })
    }

    /// A repeat finish with planned `files` still missing.
    pub(super) fn ask_missing_planned_files(&mut self, files: Vec<String>) -> Result<()> {
        let listed = files.join(", ");
        self.park_on_choice(PendingChoice {
            id: uuid::Uuid::new_v4().to_string(),
            prompt: format!(
                "The model finished twice with planned file(s) {listed} still missing. The required checks cannot show that missing files were meant to be left out."
            ),
            options: vec![
                option("write", format!("The model writes {listed}")),
                option("drop", format!("Remove {listed} from the plan and verify")),
                option("finish", "Verify anyway".into()),
            ],
            default: "write".into(),
            kind: ChoiceKind::MissingPlannedFile { files },
        })
    }

    /// The human's answer to the pending question: `key` names one of its
    /// options. A choice that needs the approval checks it first, as a
    /// permission approval does, and a failure leaves the question pending.
    pub async fn choose(&mut self, key: &str) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingChoice,
            "no harness question awaiting a choice"
        );
        let pending = self
            .task
            .pending_choice
            .clone()
            .context("no pending harness question")?;
        let key = match key.trim() {
            "" => pending.default.as_str(),
            key => key,
        };
        anyhow::ensure!(
            pending.options.iter().any(|option| option.key == key),
            "unknown choice {key}; choose one of: {}",
            pending
                .options
                .iter()
                .map(|option| option.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        match (&pending.kind, key) {
            (ChoiceKind::ScopeAdd { file }, "add") => {
                self.require_approval().await?;
                let unaddressed = self.add_to_approved_plan(file).await?;
                if unaddressed.is_empty() {
                    self.settle_choice(&pending, key);
                    self.task.phase = Phase::Working;
                    self.task.last_response =
                        format!("`{file}` was added to the approved plan. Make your edit.");
                    self.intent_event("scope_added", file);
                    self.event(format!("Human added {file} to the approved plan."));
                } else {
                    // The approval never saw these rules, so it cannot stand
                    // for an edit they govern: the plan must take them up.
                    let labels = unaddressed.join("; ");
                    self.intent_event("scope_add_needs_replan", &format!("{file}: {labels}"));
                    self.replan_for_scope(
                        &pending,
                        key,
                        file,
                        "rules the approved plan does not address",
                        format!(
                            "`{file}` is governed by rules the approved plan does not address ({labels}); replanning so the plan can address them."
                        ),
                    );
                }
            }
            (ChoiceKind::ScopeAdd { file }, "replan") => {
                let scope = self.plan_files_listing();
                self.replan_for_scope(
                    &pending,
                    key,
                    file,
                    "chosen by the human",
                    format!(
                        "Edit to {file} is outside the approved plan files [{scope}]; replan with every file the change needs."
                    ),
                );
            }
            (ChoiceKind::ScopeAdd { file }, "refuse") => {
                let scope = self.plan_files_listing();
                self.settle_choice(&pending, key);
                self.task.phase = Phase::Working;
                self.task.last_response = format!(
                    "`{file}` is outside the approved plan and the human declined to add it. Continue within the plan files: {scope}."
                );
                self.event(format!(
                    "Human declined to add {file} to the approved plan."
                ));
            }
            (ChoiceKind::MissingPlannedFile { files }, "write") => {
                self.settle_choice(&pending, key);
                self.task.phase = Phase::Working;
                self.task.last_response =
                    format!("Write the missing planned file(s): {}.", files.join(", "));
            }
            (ChoiceKind::MissingPlannedFile { files }, "drop") => {
                self.require_approval().await?;
                self.drop_from_approved_plan(files);
                self.settle_choice(&pending, key);
                self.event(format!(
                    "Human removed {} from the approved plan.",
                    files.join(", ")
                ));
                self.verify_after_choice(format!(
                    "Verifying without {}, which the human removed from the plan.",
                    files.join(", ")
                ))
                .await?;
            }
            (ChoiceKind::MissingPlannedFile { files }, "finish") => {
                self.require_approval().await?;
                let at = self.task.edits.len();
                self.symbolic_state_mut().unfinished_accepted_at = Some(at);
                self.settle_choice(&pending, key);
                self.intent_event("finish_forced_missing", &files.join(", "));
                self.verify_after_choice(format!(
                    "Verifying with planned file(s) {} missing, as the human chose.",
                    files.join(", ")
                ))
                .await?;
            }
            (kind, key) => bail!("{} has no option {key}", kind.name()),
        }
        self.persist()
    }

    /// What the plan says, and what the answer changes, must still be what
    /// the human saw: a changed source or knowledge revision withdraws the
    /// approval and the question with it.
    async fn require_approval(&mut self) -> Result<()> {
        if !self.fresh_approval().await? {
            bail!("plan evidence changed before the choice; review and approve the plan again");
        }
        Ok(())
    }

    /// Close the scope question by returning to Plan with `reason` as the
    /// planner's last result, counted as a scope escape: the human's
    /// `replan`, or an `add` the approval cannot stand for (`why`).
    fn replan_for_scope(
        &mut self,
        pending: &PendingChoice,
        key: &str,
        file: &str,
        why: &str,
        reason: String,
    ) {
        let scope = self.plan_files_listing();
        let state = self.symbolic_state_mut();
        state.scope_escapes += 1;
        let escapes = state.scope_escapes;
        self.settle_choice(pending, key);
        self.intent_event(
            "scope_escape_replan",
            &format!("{file}: escape {escapes}, {why}"),
        );
        self.event(format!(
            "Scope escape: the model proposed an edit to {file} outside the plan files [{scope}]; replanning ({why})."
        ));
        self.enter_replan(reason);
        // The next approval may keep this plan's edits if it only grows it.
        self.symbolic_state_mut().scope_replan = true;
    }

    fn plan_files_listing(&self) -> String {
        self.task
            .plan
            .as_ref()
            .map(|plan| plan.files.join(", "))
            .unwrap_or_default()
    }

    /// Journal the answer and close the question.
    fn settle_choice(&mut self, pending: &PendingChoice, key: &str) {
        self.intent_event("choice_made", &format!("{}:{key}", pending.kind.name()));
        self.event(format!(
            "Human chose {key}: {}",
            pending
                .options
                .iter()
                .find(|option| option.key == key)
                .map_or("", |option| option.label.as_str())
        ));
        self.task.pending_choice = None;
    }

    /// The model's finish, resumed by the human's answer: the same gates and
    /// link review as any finish, then Verifying.
    async fn verify_after_choice(&mut self, summary: String) -> Result<()> {
        self.task.phase = Phase::Working;
        // The answer is durable before verification can fail (an index
        // refresh): the same finish then goes on without asking again.
        self.persist()?;
        if let Some(state) = self.task.symbolic.as_mut() {
            state.auto_verification = None;
        }
        self.begin_verification(summary).await
    }

    /// Amend the approval with `file`: it keeps standing, and what approval
    /// derives for plan files (the source snapshot the approval is checked
    /// against, obligations and definition scopes, the approved-plan history
    /// evidence and auto-verify read) is derived again for the whole plan,
    /// without asking the human a second time. A failure leaves the plan as
    /// it was. So does a file governed by rules the approval never had in
    /// view and the plan does not address (by its `addresses` or its
    /// summary, as plan coverage reads it): their labels are returned, and
    /// the caller replans.
    async fn add_to_approved_plan(&mut self, file: &str) -> Result<Vec<String>> {
        let snapshot = fingerprint(&self.workspace.read(file)?);
        let plan = self.task.plan.as_mut().context("no approved plan")?;
        if !plan.files.iter().any(|known| known == file) {
            plan.files.push(file.to_string());
        }
        let previous = self.task.snapshots.insert(file.to_string(), snapshot);
        let context = match self.derive_amended_scope().await {
            Ok(context) => context,
            Err(error) => {
                self.revert_addition(file, previous);
                return Err(error);
            }
        };
        let in_view = self
            .task
            .approved_plans
            .last()
            .map(|approved| approved.rules_in_view.clone())
            .unwrap_or_default();
        let brought: Vec<_> = context
            .governing_rules
            .iter()
            .filter(|rule| !in_view.contains(&rule.iri))
            .cloned()
            .collect();
        let unaddressed = self.unaddressed_rules(&brought);
        if !unaddressed.is_empty() {
            self.revert_addition(file, previous);
            return Ok(unaddressed.into_iter().map(|rule| rule.label).collect());
        }
        if let Some(approved) = self.task.approved_plans.last_mut() {
            if !approved.files.iter().any(|known| known == file) {
                approved.files.push(file.to_string());
            }
            for rule in &context.governing_rules {
                if !approved.rules_in_view.contains(&rule.iri) {
                    approved.rules_in_view.push(rule.iri.clone());
                }
            }
        }
        Ok(Vec::new())
    }

    /// Undo an amendment with `file`: the plan, the snapshot it replaced
    /// (`previous`), and what deriving the amended scope added for the file.
    /// (Its obligation IRIs are merged into the scope's; the next approval
    /// derives the whole scope again.)
    fn revert_addition(&mut self, file: &str, previous: Option<Option<String>>) {
        if let Some(plan) = self.task.plan.as_mut() {
            plan.files.retain(|known| known != file);
        }
        match previous {
            Some(previous) => self.task.snapshots.insert(file.to_string(), previous),
            None => self.task.snapshots.remove(file),
        };
        if let Some(scope) = self.task.approved_change_scope.as_mut() {
            scope.files.remove(file);
            scope
                .definition_scopes
                .retain(|definition| definition.file != file);
        }
        if let Some(state) = self.task.symbolic.as_mut() {
            state.obligations.remove(file);
        }
    }

    /// The amended plan's step context (as `fresh_approval` refreshes it)
    /// and its derived scope, under the approved knowledge revision.
    async fn derive_amended_scope(&mut self) -> Result<ContextResponse> {
        let context = self.refresh_approved_scope().await?;
        anyhow::ensure!(
            self.task.approved_revision.as_deref() == Some(context.revision.as_str()),
            "knowledge changed while amending the plan; review and approve the plan again"
        );
        self.derive_symbolic_scope(&context).await?;
        Ok(context)
    }

    /// Take `files` out of the plan, its latest approved entry and what the
    /// approval is checked against.
    fn drop_from_approved_plan(&mut self, files: &[String]) {
        let dropped = |file: &String| files.contains(file);
        if let Some(plan) = self.task.plan.as_mut() {
            plan.files.retain(|file| !dropped(file));
        }
        if let Some(approved) = self.task.approved_plans.last_mut() {
            approved.files.retain(|file| !dropped(file));
        }
        self.task.snapshots.retain(|file, _| !dropped(file));
        if let Some(scope) = self.task.approved_change_scope.as_mut() {
            scope.files.retain(|file, _| !dropped(file));
        }
        if let Some(state) = self.task.symbolic.as_mut() {
            state.obligations.retain(|file, _| !dropped(file));
        }
    }

    pub(super) fn discard_pending_choice(&mut self, reason: &str) -> Result<()> {
        if let Some(pending) = self.task.pending_choice.take() {
            self.event(format!(
                "Discarded pending harness question: {reason}.\n{}",
                serde_json::to_string(&pending)?
            ));
        }
        Ok(())
    }
}
