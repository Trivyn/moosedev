//! The model proposes; this state machine owns reading, execution and capture.
use super::{
    config::ModelRole,
    executor::{self, Workspace},
    progress::{Progress, ProgressSender},
    protocol::*,
    response::{ActionContract, ResponseKey, ResponsePolicy, ResponseReceipt},
    startup::{ProviderSettings, RoleSettings},
};
use crate::llm::{LlmConfig, OpenAiCompatClient};
use crate::policy::{GateDisposition, PolicyDecision};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::harness::digest::sha256_hex;

mod actions;
mod approval;
mod capture;
mod dispatch;
mod finish;
mod links;
mod model;
mod permissions;
mod recovery;
mod review;
mod scope;
mod spec;
mod symbolic;
mod task;
#[cfg(test)]
mod test_support;
mod tools;
mod transport;
mod usage;
use actions::Step;
pub use links::IntentEvent;
use model::{action_schema, conversational_schema, ModelOutput, StreamedMessage};
pub use recovery::{RecoveryStatus, RepairState};
pub use scope::{ApprovedChangeScope, ApprovedDefinitionScope};
pub use symbolic::{CaptureNoteState, SymbolicAssociation, SymbolicState};
use task::{bounded, fingerprint, Intent};
pub use task::{
    CheckResult, Event, KnowledgeContextSnapshot, KnowledgeFileDossier, KnowledgeSearchResult,
    KnowledgeTurn, Mode, PendingEdit, PendingPermission, PendingSpecApproval, PermissionGrant,
    Phase, Plan, ReviewItem, StandingGuidance, Task,
};
use transport::{error_kind, HttpFailure};
pub use usage::UsageLedger;

/// Task journal contract. Journals written by earlier builds are refused;
/// there is no in-place migration.
pub const SCHEMA: u32 = 2;

const MAX_STEPS: usize = 256;
const MAX_FILES: usize = 100;
const MAX_PLAN_SUMMARY: usize = 4000;

pub struct Runner {
    pub task: Task,
    workspace: Workspace,
    daemon: String,
    http: reqwest::Client,
    journal: PathBuf,
    _lock: File,
    context: Option<ContextResponse>,
    config: Option<LlmConfig>,
    /// Per-role settings from `moosedev.toml`; a role without one uses `config`.
    plan: Option<RoleSettings>,
    implement: Option<RoleSettings>,
    /// One verified client per distinct model, so a task that moves between
    /// planning and implementation probes each model once, not on every switch.
    model_clients: Vec<(ResponseKey, OpenAiCompatClient)>,
    response_policy: Option<ResponsePolicy>,
    action_contract: Option<ActionContract>,
    progress: Option<ProgressSender>,
    streaming: Option<Arc<Mutex<StreamedMessage>>>,
    last_saved: Mutex<Option<[u8; 32]>>,
}

/// Compiled standing guidance used when a project has no `.moosedev/GUIDANCE.md`.
pub const DEFAULT_GUIDANCE: &str = include_str!("../../templates/harness/GUIDANCE.md");
/// Standing guidance is short by design; a larger file fails task creation.
pub const MAX_GUIDANCE_BYTES: usize = 4096;

fn guidance(source: &str, text: &str) -> StandingGuidance {
    StandingGuidance {
        source: source.into(),
        sha256: sha256_hex(text),
        text: text.into(),
    }
}

/// Read the project's standing guidance: `.moosedev/GUIDANCE.md` (not a symlink,
/// UTF-8, at most [`MAX_GUIDANCE_BYTES`]), empty when it holds only whitespace,
/// or the compiled default when it does not exist.
pub fn load_standing_guidance(root: &Path) -> Result<StandingGuidance> {
    let path = root.join(".moosedev/GUIDANCE.md");
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(guidance("default", DEFAULT_GUIDANCE.trim()));
        }
        Err(error) => return Err(error).context("read .moosedev/GUIDANCE.md"),
    };
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        ".moosedev/GUIDANCE.md must be a regular file"
    );
    anyhow::ensure!(
        meta.len() as usize <= MAX_GUIDANCE_BYTES,
        ".moosedev/GUIDANCE.md is {} bytes; standing guidance must fit {MAX_GUIDANCE_BYTES} bytes",
        meta.len()
    );
    let text =
        String::from_utf8(std::fs::read(&path)?).context(".moosedev/GUIDANCE.md must be UTF-8")?;
    anyhow::ensure!(
        text.len() <= MAX_GUIDANCE_BYTES,
        ".moosedev/GUIDANCE.md must fit {MAX_GUIDANCE_BYTES} bytes"
    );
    Ok(if text.trim().is_empty() {
        guidance("empty", "")
    } else {
        guidance("file", text.trim())
    })
}

pub fn default_daemon_url(root: &Path) -> Result<String> {
    let address = std::fs::read_to_string(root.join(".moosedev/http.addr"))
        .context("start the project daemon with moosedev --serve before using the harness")?;
    let address = address.trim();
    Ok(if address.starts_with("http") {
        address.to_string()
    } else {
        format!("http://{address}")
    })
}

impl Runner {
    pub async fn create(root: PathBuf, daemon_url: String, objective: String) -> Result<Self> {
        anyhow::ensure!(
            !objective.trim().is_empty() && objective.len() <= 16_000,
            "objective must contain 1..16000 bytes"
        );
        let workspace = Workspace::new(&root)?;
        let root = workspace.root().to_path_buf();
        let id = uuid::Uuid::new_v4().to_string();
        let (journal, lock) = Self::storage(&root, &id)?;
        let standing_guidance = load_standing_guidance(&root)?;
        let task = Task {
            id,
            objective,
            mode: Mode::Plan,
            phase: Phase::Planning,
            plan: None,
            approved_change_scope: None,
            symbolic: None,
            intent_events: vec![],
            intent_cycle: None,
            pending_intent_links: None,
            intent_refresh_pending: vec![],
            events: vec![],
            knowledge_events: vec![],
            pending_capture: None,
            capture_request: None,
            capture_reason: None,
            pending_edit: None,
            pending_permission: None,
            permission_grants: vec![],
            pending_spec: None,
            edits: vec![],
            last_error: None,
            last_error_kind: None,
            recovery: None,
            response_receipt: None,
            token_usage: UsageLedger::new(&journal),
            last_response: String::new(),
            knowledge_revision: String::new(),
            knowledge_turn_sequence: 0,
            knowledge_turns: Vec::new(),
            knowledge_context: None,
            knowledge_searches: Vec::new(),
            read_files: vec![],
            check_results: vec![],
            root,
            model_requests: vec![],
            batch_capture: false,
            reviews: vec![],
            turn_finished: false,
            conversation_context: String::new(),
            review_continuation: None,
            delivered_messages: Vec::new(),
            guidance: String::new(),
            capture_end: None,
            capture_checkpoint_end: None,
            schema: SCHEMA,
            snapshots: BTreeMap::new(),
            approved_revision: None,
            intent: None,
            capture_due: false,
            final_capture: false,
            after_review: Phase::Planning,
            resume_phase: Phase::Planning,
            steps: 0,
            capture_operations: vec![],
            capture_cursor: 0,
            completion_pending: false,
            cleanup_pending: false,
            source: BTreeMap::new(),
            standing_guidance: Some(standing_guidance),
        };
        let mut runner = Self {
            task,
            workspace,
            http: Self::client(&daemon_url)?,
            daemon: daemon_url.trim_end_matches('/').to_string(),
            journal,
            _lock: lock,
            context: None,
            config: None,
            plan: None,
            implement: None,
            model_clients: vec![],
            response_policy: None,
            action_contract: None,
            progress: None,
            streaming: None,
            last_saved: Mutex::new(None),
        };
        let context = runner.refresh(&[]).await?;
        Self::validate_daemon_contracts(&context)?;
        runner.event("Task created in Plan mode; current project knowledge retrieved.");
        runner.guidance_loaded();
        runner.persist()?;
        Ok(runner)
    }

    pub fn load(root: PathBuf, daemon_url: String, id: &str) -> Result<Self> {
        let workspace = Workspace::new(&root)?;
        let (journal, lock) = Self::storage(workspace.root(), id)?;
        anyhow::ensure!(
            !std::fs::symlink_metadata(&journal)?
                .file_type()
                .is_symlink(),
            "task journal cannot be a symlink"
        );
        let task: Task = serde_json::from_reader(File::open(&journal)?)?;
        anyhow::ensure!(
            task.schema == SCHEMA,
            "task journal schema {} is unsupported by this build; start a new task",
            task.schema
        );
        anyhow::ensure!(
            task.id == id && task.root == workspace.root(),
            "task identity or project mismatch"
        );
        task.token_usage.attach(&journal);
        let missing_guidance = task.standing_guidance.is_none();
        let mut runner = Self {
            task,
            workspace,
            http: Self::client(&daemon_url)?,
            daemon: daemon_url.trim_end_matches('/').to_string(),
            journal,
            _lock: lock,
            context: None,
            config: None,
            plan: None,
            implement: None,
            model_clients: vec![],
            response_policy: None,
            action_contract: None,
            progress: None,
            streaming: None,
            last_saved: Mutex::new(None),
        };
        if missing_guidance {
            // A journal from before the guidance file gets the compiled default.
            runner.task.standing_guidance = Some(guidance("default", DEFAULT_GUIDANCE.trim()));
            runner.guidance_loaded();
            runner.persist()?;
        }
        Ok(runner)
    }

    /// Journal which standing guidance the task carries.
    fn guidance_loaded(&mut self) {
        if let Some(standing) = self.task.standing_guidance.clone() {
            self.intent_event(
                "guidance_loaded",
                &format!(
                    "{}, {} bytes, sha256 {}",
                    standing.source,
                    standing.text.len(),
                    standing.sha256
                ),
            );
        }
    }

    pub fn configure(&mut self, config: LlmConfig, progress: Option<ProgressSender>) {
        self.config = Some(config);
        self.progress = progress;
    }

    /// Apply every role's resolved settings. Both frontends configure a runner
    /// through here, so they cannot disagree about which model answers.
    pub fn configure_provider(
        &mut self,
        provider: &ProviderSettings,
        progress: Option<ProgressSender>,
    ) {
        self.configure(provider.config.clone(), progress);
        self.set_response_policy(provider.response_policy);
        if let Some(contract) = provider.action_contract {
            self.set_action_contract(contract);
        }
        self.set_role(ModelRole::Plan, provider.plan.clone());
        self.set_role(ModelRole::Implement, provider.implement.clone());
    }

    /// Give one role its own model and levers, or return it to the default.
    pub fn set_role(&mut self, role: ModelRole, settings: Option<RoleSettings>) {
        match role {
            ModelRole::Plan => self.plan = settings,
            ModelRole::Implement => self.implement = settings,
        }
    }

    pub fn set_response_policy(&mut self, policy: ResponsePolicy) {
        self.response_policy = Some(policy);
    }

    /// Choose the action contract for this runner instead of reading
    /// `MOOSEDEV_HARNESS_ACTION_CONTRACT`.
    pub fn set_action_contract(&mut self, contract: ActionContract) {
        self.action_contract = Some(contract);
    }

    pub fn daemon_url(&self) -> &str {
        &self.daemon
    }

    pub fn enable_interactive(&mut self) -> Result<()> {
        self.task.batch_capture = true;
        // Import a legacy task's already-persisted pending review without losing it.
        if let (Some(request), Some(response)) = (
            self.task.capture_request.clone(),
            self.task.pending_capture.clone(),
        ) {
            self.task.reviews.push(ReviewItem {
                intent_links: None,
                request,
                response,
                reason: self.task.capture_reason.clone().unwrap_or_default(),
            });
            self.task.capture_request = None;
            self.task.pending_capture = None;
            self.task.capture_end.get_or_insert(self.task.events.len());
            self.commit_capture_page();
            if self.task.capture_due {
                self.task.review_continuation = Some(self.capture_work_phase());
            }
        }
        self.persist()
    }

    pub fn set_conversation_context(&mut self, context: String) {
        self.task.conversation_context = bounded(&context, 16_000);
    }
}
