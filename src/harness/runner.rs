//! The model proposes; this state machine owns reading, execution and capture.
use super::{
    executor::{self, Workspace},
    progress::{Progress, ProgressSender},
    protocol::*,
    response::{ResponseKey, ResponsePolicy, ResponseReceipt},
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

mod actions;
mod approval;
mod capture;
mod dispatch;
mod finish;
mod links;
mod model;
mod recovery;
mod review;
mod scope;
mod symbolic;
mod task;
#[cfg(test)]
mod test_support;
mod transport;
mod usage;
use actions::Step;
pub use links::IntentEvent;
use model::{action_schema, conversational_schema, ModelOutput, StreamedMessage};
pub use recovery::{RecoveryStatus, RepairState};
pub use scope::{ApprovedChangeScope, ApprovedDefinitionScope};
pub use symbolic::{CaptureNoteState, SymbolicAssociation, SymbolicState};
use task::{bounded, fingerprint, Intent};
pub use task::{CheckResult, Event, Mode, PendingEdit, Phase, Plan, ReviewItem, Task};
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
    model_client: Option<(ResponseKey, OpenAiCompatClient)>,
    response_policy: Option<ResponsePolicy>,
    progress: Option<ProgressSender>,
    streaming: Option<Arc<Mutex<StreamedMessage>>>,
    last_saved: Mutex<Option<[u8; 32]>>,
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
            pending_capture: None,
            capture_request: None,
            capture_reason: None,
            pending_edit: None,
            edits: vec![],
            last_error: None,
            last_error_kind: None,
            recovery: None,
            response_receipt: None,
            token_usage: UsageLedger::new(&journal),
            last_response: String::new(),
            knowledge_revision: String::new(),
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
            model_client: None,
            response_policy: None,
            progress: None,
            streaming: None,
            last_saved: Mutex::new(None),
        };
        let context = runner.refresh(&[]).await?;
        Self::validate_daemon_contracts(&context)?;
        runner.event("Task created in Plan mode; current project knowledge retrieved.");
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
        Ok(Self {
            task,
            workspace,
            http: Self::client(&daemon_url)?,
            daemon: daemon_url.trim_end_matches('/').to_string(),
            journal,
            _lock: lock,
            context: None,
            config: None,
            model_client: None,
            response_policy: None,
            progress: None,
            streaming: None,
            last_saved: Mutex::new(None),
        })
    }

    pub fn configure(&mut self, config: LlmConfig, progress: Option<ProgressSender>) {
        self.config = Some(config);
        self.progress = progress;
    }

    pub fn set_response_policy(&mut self, policy: ResponsePolicy) {
        self.response_policy = Some(policy);
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
