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
        let mut reads = Vec::new();
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
            // A granted tree can stop validating after approval, for example
            // when a tool links out of it. Name the grant so it can be revoked
            // instead of failing every later command anonymously.
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

    pub(super) async fn begin_permission_request(
        &mut self,
        command: String,
        justification: String,
        read_paths: Vec<String>,
        write_paths: Vec<String>,
        network: bool,
    ) -> Result<Option<executor::CommandResult>> {
        let requested = CommandPermissions::requested(
            self.workspace.root(),
            &self.scratch_path(),
            &read_paths,
            &write_paths,
            network,
        )?;
        if self.request_is_granted(&requested)? {
            self.intent_event(
                "permission_reused",
                &format!("task grants already cover: {justification}"),
            );
            return self.run_command(&command).await.map(Some);
        }
        let pending = PendingPermission {
            request_id: uuid::Uuid::new_v4().to_string(),
            command,
            justification,
            read_paths: canonical_strings(&requested.read_paths)?,
            write_paths: canonical_strings(&requested.write_paths)?,
            network: requested.network,
            revision: self.task.knowledge_revision.clone(),
            approved_grant: None,
        };
        self.intent_event("permission_requested", &pending.request_id);
        self.event(format!(
            "Permission requested: {}",
            serde_json::to_string(&pending)?
        ));
        self.task.pending_permission = Some(pending);
        self.task.phase = Phase::AwaitingPermission;
        self.persist()?;
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
        // Re-run validation immediately before making the durable grant.
        let normalized = CommandPermissions::requested(
            self.workspace.root(),
            &self.scratch_path(),
            &pending.read_paths,
            &pending.write_paths,
            pending.network,
        )?;
        let grant = PermissionGrant {
            id: uuid::Uuid::new_v4().to_string(),
            justification: pending.justification.clone(),
            read_paths: canonical_strings(&normalized.read_paths)?,
            write_paths: canonical_strings(&normalized.write_paths)?,
            network: normalized.network,
            approved_at: chrono::Utc::now().to_rfc3339(),
        };
        self.task.permission_grants.push(grant.clone());
        pending.approved_grant = Some(grant.id.clone());
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
        self.task.last_response = format!(
            "Human denied permission request {}. The command did not run and no capability was granted. Choose an approach that stays within current permissions or explain why a different request is needed.",
            pending.request_id
        );
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
