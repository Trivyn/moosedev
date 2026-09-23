//! Durable, task-scoped command capabilities and their human gate.
use super::*;
use crate::harness::executor::CommandPermissions;

fn canonical_strings(paths: &[std::path::PathBuf]) -> Result<Vec<String>> {
    paths
        .iter()
        .map(|path| {
            path.to_str()
                .context("permission path must be UTF-8")
                .map(str::to_owned)
        })
        .collect()
}

fn path_covered(granted: &str, requested: &str) -> bool {
    let granted = Path::new(granted);
    let requested = Path::new(requested);
    granted == requested
        || std::fs::metadata(granted)
            .is_ok_and(|metadata| metadata.is_dir() && requested.starts_with(granted))
}

impl Runner {
    pub(super) fn command_permissions(&self) -> Result<CommandPermissions> {
        // Standing paths come first: they are read-only, project-wide, and
        // granted without a gate, so they must pass exactly the structural
        // checks a task grant passes. They are not surveyed here — the survey
        // is for a human about to decide, and nobody is deciding before a
        // command.
        let mut reads = self.standing_read_paths().to_vec();
        let mut writes = Vec::new();
        let mut network = false;
        for grant in &self.task.permission_grants {
            reads.extend(grant.read_paths.iter().cloned());
            writes.extend(grant.write_paths.iter().cloned());
            network |= grant.network;
        }
        let root = self.workspace.root();
        let scratch = self.scratch_path();
        let permissions = CommandPermissions::requested(root, &scratch, &reads, &writes, network);
        if permissions.is_err() {
            // A granted path can stop resolving after approval, for example
            // when the directory is deleted. Name what went wrong so it can be
            // fixed instead of failing every later command anonymously.
            if !self.standing_read_paths().is_empty() {
                CommandPermissions::requested(
                    root,
                    &scratch,
                    self.standing_read_paths(),
                    &[],
                    false,
                )
                .context(
                    "a standing [harness.sandbox] read path in moosedev.toml is no longer valid",
                )?;
            }
            for grant in &self.task.permission_grants {
                CommandPermissions::requested(
                    root,
                    &scratch,
                    &grant.read_paths,
                    &grant.write_paths,
                    grant.network,
                )
                .with_context(|| {
                    format!(
                        "task permission {id} is no longer valid; revoke it with /revoke-permission {id}",
                        id = grant.id
                    )
                })?;
            }
        }
        permissions
    }

    fn request_is_granted(&self, request: &CommandPermissions) -> Result<bool> {
        let current = self.command_permissions()?;
        Ok(request.read_paths.iter().all(|requested| {
            let requested = requested.to_string_lossy();
            current
                .read_paths
                .iter()
                .chain(&current.write_paths)
                .any(|granted| path_covered(&granted.to_string_lossy(), &requested))
        }) && request.write_paths.iter().all(|requested| {
            let requested = requested.to_string_lossy();
            current
                .write_paths
                .iter()
                .any(|granted| path_covered(&granted.to_string_lossy(), &requested))
        }) && (!request.network || current.network))
    }

    /// Build the frozen request a human will judge. `Err` here is only a
    /// journal-write failure: a request that cannot be granted comes back as a
    /// `refusal` the human still sees.
    fn pending_permission_for(
        &self,
        command: String,
        justification: String,
        read_paths: Vec<String>,
        write_paths: Vec<String>,
        network: bool,
    ) -> Result<(PendingPermission, bool)> {
        let surveyed = CommandPermissions::surveyed(
            self.workspace.root(),
            &self.scratch_path(),
            &read_paths,
            &write_paths,
            network,
        );
        let request_id = uuid::Uuid::new_v4().to_string();
        Ok(match surveyed {
            Ok((requested, findings)) => {
                let covered = self.request_is_granted(&requested).unwrap_or(false);
                (
                    PendingPermission {
                        request_id,
                        command,
                        justification,
                        read_paths: canonical_strings(&requested.read_paths)?,
                        write_paths: canonical_strings(&requested.write_paths)?,
                        network: requested.network,
                        revision: self.task.knowledge_revision.clone(),
                        refusal: None,
                        findings,
                        approved_grant: None,
                    },
                    covered,
                )
            }
            Err(error) => (
                PendingPermission {
                    request_id,
                    command,
                    justification,
                    read_paths,
                    write_paths,
                    network,
                    revision: self.task.knowledge_revision.clone(),
                    refusal: Some(super::bounded(&format!("{error:#}"), 600)),
                    findings: Default::default(),
                    approved_grant: None,
                },
                false,
            ),
        })
    }

    /// Park the task on a frozen request. The gate the human answers.
    fn park_on_permission(&mut self, pending: PendingPermission) -> Result<()> {
        self.intent_event("permission_requested", &pending.request_id);
        self.event(format!(
            "Permission requested: {}",
            serde_json::to_string(&pending)?
        ));
        self.task.pending_permission = Some(pending);
        self.task.phase = Phase::AwaitingPermission;
        self.persist()
    }

    /// A sandbox denial already names the paths it blocked, so the harness
    /// raises the gate itself rather than asking the model to restate them
    /// (Constraint cd9f1a96). The grant is then exactly as narrow as the
    /// denial was, instead of whatever directory the model guessed at.
    pub(super) fn raise_gate_for_denial(
        &mut self,
        command: &str,
        paths: &[String],
    ) -> Result<bool> {
        let (pending, covered) = self.pending_permission_for(
            command.to_string(),
            format!(
                "The sandbox blocked this command. Its output names {} path(s) outside the project.",
                paths.len()
            ),
            paths.to_vec(),
            Vec::new(),
            false,
        )?;
        // Already granted, so the denial was about something else: let the
        // model see the output rather than asking the human a settled question.
        if covered {
            return Ok(false);
        }
        self.park_on_permission(pending)?;
        Ok(true)
    }

    pub(super) async fn begin_permission_request(
        &mut self,
        command: String,
        justification: String,
        read_paths: Vec<String>,
        write_paths: Vec<String>,
        network: bool,
    ) -> Result<Option<executor::CommandResult>> {
        let (pending, covered) = self.pending_permission_for(
            command.clone(),
            justification.clone(),
            read_paths,
            write_paths,
            network,
        )?;
        if covered {
            self.intent_event(
                "permission_reused",
                &format!("task grants already cover: {justification}"),
            );
            return self.run_command(&command).await.map(Some);
        }
        self.park_on_permission(pending)?;
        Ok(None)
    }

    /// Record the grant and mark the frozen request approved. The command
    /// itself runs on the next step, where it streams progress and can be
    /// interrupted like any other command.
    pub async fn approve_permission(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingPermission,
            "no command permission awaiting approval"
        );
        let mut pending = self
            .task
            .pending_permission
            .clone()
            .context("no pending permission request")?;
        if !self.fresh_approval().await? {
            bail!("plan evidence changed before permission approval");
        }
        if self.task.knowledge_revision != pending.revision {
            self.task.mode = Mode::Plan;
            self.task.phase = Phase::AwaitingPlan;
            self.task.approved_revision = None;
            self.discard_pending_permission("knowledge changed before permission approval")?;
            self.persist()?;
            bail!("knowledge changed before permission approval");
        }
        if let Some(refusal) = &pending.refusal {
            bail!("this request cannot be granted as asked: {refusal}");
        }
        // Re-survey immediately before making the durable grant, and hold the
        // human to what they actually saw: if the scopes changed under them,
        // their approval was of something else.
        let (normalized, findings) = CommandPermissions::surveyed(
            self.workspace.root(),
            &self.scratch_path(),
            &pending.read_paths,
            &pending.write_paths,
            pending.network,
        )?;
        if findings != pending.findings {
            self.discard_pending_permission("the requested paths changed before approval")?;
            self.task.phase = Phase::Working;
            self.task.last_response = "The requested paths changed between the request and the approval, so the request was discarded rather than granted against something the human did not see. Request permission again if the command still needs it.".into();
            self.persist()?;
            bail!("the requested paths changed before approval; request it again");
        }
        let grant = PermissionGrant {
            id: uuid::Uuid::new_v4().to_string(),
            justification: pending.justification.clone(),
            read_paths: canonical_strings(&normalized.read_paths)?,
            write_paths: canonical_strings(&normalized.write_paths)?,
            network: normalized.network,
            approved_at: chrono::Utc::now().to_rfc3339(),
            findings,
        };
        self.task.permission_grants.push(grant.clone());
        pending.approved_grant = Some(grant.id.clone());
        self.forget_failure();
        self.task.phase = Phase::Working;
        self.intent_event(
            "permission_approved",
            &format!("{} for request {}", grant.id, pending.request_id),
        );
        self.event(format!(
            "Human approved task-scoped permission {} for exact pending command {}.",
            grant.id, pending.request_id
        ));
        self.task.pending_permission = Some(pending);
        self.persist()
    }

    pub(super) fn permission_approved(&self) -> bool {
        self.task
            .pending_permission
            .as_ref()
            .is_some_and(|pending| pending.approved_grant.is_some())
    }

    /// Run the exact command the human approved. Clearing the request and
    /// recording the command intent share one journal write in `run_command`,
    /// so an interruption never replays the command.
    pub(super) async fn run_approved_permission(&mut self) -> Result<()> {
        let pending = self
            .task
            .pending_permission
            .take()
            .context("no approved permission request")?;
        let granted = self
            .task
            .permission_grants
            .iter()
            .any(|grant| Some(&grant.id) == pending.approved_grant.as_ref());
        if !granted {
            self.task.last_response = format!(
                "The permission approved for request {} was revoked before its command ran. The command did not run. Choose an approach that stays within current permissions or explain why a different request is needed.",
                pending.request_id
            );
            self.event(format!(
                "Approved permission request {} lost its grant before running; the command did not run.",
                pending.request_id
            ));
            return self.persist();
        }
        self.end_unchanged_window();
        let result = self.run_command(&pending.command).await?;
        self.commit_command_result(&pending.command, result)
    }

    pub fn deny_permission(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingPermission,
            "no command permission awaiting denial"
        );
        let pending = self
            .task
            .pending_permission
            .take()
            .context("no pending permission request")?;
        self.task.phase = Phase::Working;
        self.task.last_response = match &pending.refusal {
            Some(refusal) => format!(
                "Permission request {} could not be granted as asked: {refusal}\nThe command did not run and no capability was granted. Request a narrower path that avoids this, or choose an approach that stays within current permissions.",
                pending.request_id
            ),
            None => format!(
                "Human denied permission request {}. The command did not run and no capability was granted. Choose an approach that stays within current permissions or explain why a different request is needed.",
                pending.request_id
            ),
        };
        self.intent_event("permission_denied", &pending.request_id);
        self.event(format!(
            "Human denied permission request: {}",
            serde_json::to_string(&pending)?
        ));
        self.persist()
    }

    pub fn revoke_permission(&mut self, id: &str) -> Result<()> {
        let index = self
            .task
            .permission_grants
            .iter()
            .position(|grant| grant.id == id)
            .context("unknown permission grant")?;
        let grant = self.task.permission_grants.remove(index);
        self.intent_event("permission_revoked", &grant.id);
        self.event(format!("Human revoked task permission {}.", grant.id));
        self.persist()
    }

    /// Grants are task-scoped: completion ends them, journaled one by one so
    /// the audit trail shows when each capability stopped applying.
    pub(super) fn expire_permissions(&mut self) {
        for grant in std::mem::take(&mut self.task.permission_grants) {
            self.intent_event("permission_expired", &grant.id);
            self.event(format!(
                "Task permission {} expired at completion.",
                grant.id
            ));
        }
    }

    pub(super) fn discard_pending_permission(&mut self, reason: &str) -> Result<()> {
        if let Some(pending) = self.task.pending_permission.take() {
            self.event(format!(
                "Discarded pending permission request: {reason}.\n{}",
                serde_json::to_string(&pending)?
            ));
        }
        Ok(())
    }
}
