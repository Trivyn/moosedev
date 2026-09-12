//! The model proposes; this state machine owns reading, execution and capture.
use super::{
    executor::{self, Workspace},
    progress::{Progress, ProgressSender},
    protocol::*,
    response::{ResponseKey, ResponsePolicy, ResponseReceipt},
};
use crate::harness::digest::sha256_hex;
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
mod capture;
mod change_intent;
mod intent_v2;
mod model;
mod recovery;
mod symbolic;
#[cfg(test)]
mod test_support;
mod usage;
use actions::Step;
pub use change_intent::IntentEvent;
pub use intent_v2::{ApprovedChangeScope, ApprovedDefinitionScope};
use model::{action_schema, conversational_schema, ModelOutput, StreamedMessage};
pub use recovery::{RecoveryStatus, RepairState};
pub use symbolic::{CaptureNoteState, SymbolicAssociation, SymbolicState};
pub use usage::UsageLedger;

/// Task journal contract. Journals written by earlier builds are refused;
/// there is no in-place migration.
pub const SCHEMA: u32 = 2;

const MAX_STEPS: usize = 256;
const MAX_FILES: usize = 100;
const MAX_PLAN_SUMMARY: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Plan,
    Auto,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Planning,
    AwaitingPlan,
    Working,
    AwaitingInput,
    AwaitingPolicy,
    Verifying,
    AwaitingReview,
    Cancelled,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub summary: String,
    pub files: Vec<String>,
    pub checks: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub command: String,
    pub success: bool,
    pub output: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEdit {
    pub file: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub reason: String,
    pub revision: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Intent {
    Edit(PendingEdit),
    Command(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_links: Option<super::daemon::intent::IntentLinkRequest>,
    pub request: CaptureRequest,
    pub response: CaptureResponse,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub objective: String,
    pub mode: Mode,
    pub phase: Phase,
    pub plan: Option<Plan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_change_scope: Option<ApprovedChangeScope>,
    /// Derived scope, associations, capture note and recovery counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbolic: Option<SymbolicState>,
    #[serde(default)]
    pub intent_events: Vec<IntentEvent>,
    #[serde(default)]
    intent_cycle: Option<String>,
    #[serde(default)]
    pending_intent_links: Option<super::daemon::intent::IntentLinkRequest>,
    /// A reviewed intent/link receipt is already durable; only this fallible
    /// context refresh remains. Never replay the review while it is set.
    #[serde(default)]
    intent_refresh_pending: Vec<String>,
    pub events: Vec<Event>,
    pub pending_capture: Option<CaptureResponse>,
    pub capture_request: Option<CaptureRequest>,
    pub capture_reason: Option<String>,
    pub pending_edit: Option<PendingEdit>,
    #[serde(default)]
    pub edits: Vec<PendingEdit>,
    pub last_error: Option<String>,
    /// Typed class of `last_error`: model_output, daemon_rejection, service,
    /// or other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_kind: Option<String>,
    #[serde(default)]
    pub recovery: Option<RepairState>,
    #[serde(default)]
    pub response_receipt: Option<ResponseReceipt>,
    /// Physical-request accounting; separate from project capture evidence.
    #[serde(default)]
    pub token_usage: UsageLedger,
    pub last_response: String,
    pub knowledge_revision: String,
    pub read_files: Vec<String>,
    pub check_results: Vec<CheckResult>,
    pub root: PathBuf,
    pub model_requests: Vec<Value>,
    #[serde(default)]
    pub batch_capture: bool,
    #[serde(default)]
    pub reviews: Vec<ReviewItem>,
    #[serde(default)]
    pub turn_finished: bool,
    #[serde(default)]
    conversation_context: String,
    #[serde(default)]
    review_continuation: Option<Phase>,
    #[serde(default)]
    delivered_messages: Vec<String>,
    #[serde(default)]
    guidance: String,
    #[serde(default)]
    capture_end: Option<usize>,
    #[serde(default)]
    capture_checkpoint_end: Option<usize>,
    schema: u32,
    snapshots: BTreeMap<String, Option<String>>,
    approved_revision: Option<String>,
    intent: Option<Intent>,
    capture_due: bool,
    final_capture: bool,
    after_review: Phase,
    resume_phase: Phase,
    steps: usize,
    capture_operations: Vec<String>,
    capture_cursor: usize,
    /// Human capture review is resolved; retry only completion checks on failure.
    #[serde(default)]
    completion_pending: bool,
    /// Cancellation is durable even when scratch cleanup must be retried.
    #[serde(default)]
    pub cleanup_pending: bool,
    /// Verbatim recent source for generation; historical evidence lives in events.
    source: BTreeMap<String, Option<String>>,
}

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

#[derive(Debug)]
struct HttpFailure {
    status: u16,
    message: String,
}
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "daemon HTTP {}: {}", self.status, self.message)
    }
}
impl std::error::Error for HttpFailure {}

/// Typed class of a failed step, derived by error-type downcast only. Order
/// matters: a daemon 400 re-wrapped as `InvalidModelOutput` spends repair
/// budget and stays `model_output`.
fn error_kind(error: &anyhow::Error) -> &'static str {
    if error.is::<model::InvalidModelOutput>() {
        "model_output"
    } else if let Some(failure) = error.downcast_ref::<HttpFailure>() {
        if (400..500).contains(&failure.status) {
            "daemon_rejection"
        } else {
            "service"
        }
    } else if error.is::<reqwest::Error>() || error.is::<std::io::Error>() {
        "service"
    } else {
        "other"
    }
}

fn fingerprint(value: &Option<String>) -> Option<String> {
    value.as_deref().map(sha256_hex)
}
fn bounded(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated]", &s[..end])
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
    fn storage(root: &Path, id: &str) -> Result<(PathBuf, File)> {
        uuid::Uuid::parse_str(id).context("invalid task ID")?;
        let dir = root.join(".moosedev/harness/tasks");
        for path in [
            root.join(".moosedev"),
            root.join(".moosedev/harness"),
            dir.clone(),
        ] {
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => anyhow::ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "task storage must be a real directory"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        let lock_path = dir.join("runner.lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.try_lock_exclusive()
            .context("another harness owns this workspace")?;
        Ok((dir.join(format!("{id}.json")), lock))
    }

    fn client(daemon: &str) -> Result<reqwest::Client> {
        let url = reqwest::Url::parse(daemon)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "invalid daemon URL scheme"
        );
        anyhow::ensure!(
            matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            ),
            "harness requires a loopback project daemon"
        );
        Ok(reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }

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

    fn event(&mut self, message: impl Into<String>) {
        let message = message.into();
        if let Some(progress) = &self.progress {
            let _ = progress.send(Progress::Status(bounded(&message, 2000)));
        }
        self.task.events.push(Event { message });
    }

    fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.task)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let mut last_saved = self
            .last_saved
            .lock()
            .map_err(|_| anyhow::anyhow!("journal checkpoint lock poisoned"))?;
        if last_saved.as_ref() == Some(&digest) {
            return Ok(());
        }
        let dir = self.journal.parent().context("journal has no parent")?;
        let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.journal)?;
        File::open(dir)?.sync_all()?;
        *last_saved = Some(digest);
        Ok(())
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let response = self
            .http
            .post(format!("{}/api/v1/harness/{path}", self.daemon))
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(HttpFailure {
                status: status.as_u16(),
                message: format!("{path}: {}", bounded(&text, 4000)),
            }
            .into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn checkpoint(&self, operation: Option<&str>) -> Result<CheckpointResponse> {
        let mut request = self
            .http
            .post(format!("{}/api/v1/harness/checkpoint", self.daemon));
        if let Some(id) = operation {
            request = request.query(&[("operation_id", id)]);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "checkpoint failed: {status}: {}",
            bounded(&text, 4000)
        );
        Ok(serde_json::from_str(&text)?)
    }

    async fn refresh(&mut self, files: &[String]) -> Result<ContextResponse> {
        let response: ContextResponse = self
            .post(
                "context",
                &ContextRequest {
                    topic: format!("{} {}", self.task.objective, self.task.guidance)
                        .trim()
                        .to_owned(),
                    files: files.to_vec(),
                },
            )
            .await?;
        anyhow::ensure!(
            Path::new(&response.project_root).canonicalize()? == self.workspace.root(),
            "daemon belongs to a different project"
        );
        anyhow::ensure!(
            files
                .iter()
                .all(|file| response.files.iter().any(|entry| &entry.file == file)),
            "daemon omitted requested file context"
        );
        self.update_knowledge_revision(response.revision.clone());
        self.context = Some(response.clone());
        Ok(response)
    }

    fn snapshot(&self, files: &[String]) -> Result<BTreeMap<String, Option<String>>> {
        files
            .iter()
            .map(|file| Ok((file.clone(), fingerprint(&self.workspace.read(file)?))))
            .collect()
    }

    async fn fresh_approval(&mut self) -> Result<bool> {
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
                self.refresh(std::slice::from_ref(&file)).await?;
                let source = self.workspace.read(&file)?;
                if !self.task.read_files.contains(&file) {
                    anyhow::ensure!(
                        self.task.read_files.len() < MAX_FILES,
                        "working set exceeds {MAX_FILES} files; narrow the task"
                    );
                    self.task.read_files.push(file.clone());
                }
                self.event(format!(
                    "Read {file}: {}",
                    source.as_deref().unwrap_or("[file does not exist]")
                ));
                self.task.last_response = format!("Read {file} with its governing knowledge.");
                self.task.source.insert(file, source);
            }
            Step::Search { query } => {
                let mut hits = String::new();
                for file in &files {
                    if file.contains(&query) {
                        hits.push_str(&format!("Path: {file}\n"));
                    }
                    if hits.len() > 12_000 {
                        break;
                    }
                    if let Ok(Some(text)) = self.workspace.read(file) {
                        for (line, text) in text
                            .lines()
                            .enumerate()
                            .filter(|(_, text)| text.contains(&query))
                            .take(10)
                        {
                            hits.push_str(&format!(
                                "{file}:{}: {}\n",
                                line + 1,
                                bounded(text, 300)
                            ));
                            if hits.len() > 12_000 {
                                break;
                            }
                        }
                    }
                    if hits.len() > 12_000 {
                        break;
                    }
                }
                self.task.last_response = if hits.is_empty() {
                    "No matches.".into()
                } else {
                    hits
                };
                self.event(self.task.last_response.clone());
            }
            Step::Plan {
                summary,
                files,
                checks,
            } => {
                self.refresh(&files).await?;
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
                let result = self.run_command(&command).await?;
                self.task.last_response = result.output;
                self.task.capture_due = true;
                self.task.after_review = Phase::Working;
                self.task.intent = None;
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
            Step::Replan { reason } => {
                self.end_intent_cycle("model replan");
                self.task.mode = Mode::Plan;
                self.task.phase = Phase::Planning;
                self.task.approved_revision = None;
                self.task.last_response = reason;
                self.task.read_files.clear();
                self.task.source.clear();
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

    fn apply_edit(&mut self, edit: PendingEdit) -> Result<()> {
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

    async fn run_command(&mut self, command: &str) -> Result<executor::CommandResult> {
        anyhow::ensure!(
            !command.trim().is_empty() && command.len() <= 4000,
            "command must contain 1..4000 bytes"
        );
        self.task.intent = Some(Intent::Command(command.to_string()));
        self.persist()?;
        let scratch = self.scratch_path();
        let result = executor::command_with_progress(
            self.workspace.root(),
            &scratch,
            command,
            self.progress.clone(),
        )
        .await;
        if let Ok(result) = &result {
            self.event(format!(
                "Command: {command}\nSuccess: {}\n{}",
                result.success, result.output
            ));
            // Keep the persisted intent until the caller commits its typed
            // check result or capture obligation in the same task-journal
            // update. A crash between execution and that update must reconcile
            // as uncertain, never replay a command whose outcome was lost.
        }
        result
    }

    fn scratch_path(&self) -> PathBuf {
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
            self.task.check_results.push(CheckResult {
                command,
                success: result.success,
                output: result.output.clone(),
            });
            if !result.success {
                self.task.phase = Phase::Working;
                self.task.last_response = format!(
                    "Required verification failed. Repair before completion.\n{}",
                    result.output
                );
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
        self.apply_edit(edit)
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
            if self.task.intent.is_some() {
                anyhow::ensure!(
                    self.task.phase == Phase::AwaitingInput,
                    "reconcile interrupted action first"
                );
                // Record identity with the same journal write as the acknowledgment.
                if let Some(id) = id {
                    self.task.delivered_messages.push(id.to_owned());
                }
                self.task.guidance = text.clone();
                self.answer(text).await?;
                self.task.turn_finished = false;
                return self.persist();
            }
        }
        self.abandon_pending_intent("new human guidance").await?;
        self.event(format!("Human response: {text}"));
        if let Some(id) = id {
            self.task.delivered_messages.push(id.to_owned());
        }
        self.task.guidance = text.clone();
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.turn_finished = false;
        self.task.steps = 0;
        self.end_intent_cycle("new human guidance");
        self.task.approved_revision = None;
        self.discard_pending_edit("new human guidance invalidated the proposed edit")?;
        self.task.completion_pending = false;
        self.task.check_results.clear();
        self.task.final_capture = false;
        self.task.mode = Mode::Plan;
        self.task.after_review = Phase::Planning;
        self.task.read_files.clear();
        self.task.source.clear();
        if self.task.phase == Phase::AwaitingReview {
            self.task.review_continuation = Some(Phase::Planning);
        } else {
            self.task.phase = Phase::Planning;
        }
        // Existing uncertain capture requests remain frozen for idempotent retry.
        self.persist()
    }

    fn discard_pending_edit(&mut self, reason: &str) -> Result<()> {
        if let Some(edit) = self.task.pending_edit.take() {
            self.event(format!(
                "Discarded pending policy edit: {reason}.\n{}",
                serde_json::to_string(&edit)?
            ));
        }
        Ok(())
    }

    async fn finish(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.reviews.is_empty()
                && self.task.capture_request.is_none()
                && !self.task.capture_due,
            "knowledge review remains unresolved"
        );
        anyhow::ensure!(
            self.task.edits.is_empty() || self.symbolic_associations_resolved(),
            "post-edit association assessment remains unresolved"
        );
        // Publish resolved review and this retryable completion intent together.
        // A failed checkpoint must not send the user back to a no-change review.
        self.task.completion_pending = true;
        self.task.phase = Phase::Verifying;
        self.persist()?;
        if !self.fresh_approval().await? {
            self.task.final_capture = false;
            self.task.completion_pending = false;
            return self.persist();
        }
        let files = self.task.plan.as_ref().context("no plan")?.files.clone();
        anyhow::ensure!(
            self.snapshot(&files)? == self.task.snapshots,
            "source changed after verification; return to planning"
        );
        let expected = &self.task.plan.as_ref().unwrap().checks;
        anyhow::ensure!(
            expected.len() == self.task.check_results.len()
                && expected
                    .iter()
                    .zip(&self.task.check_results)
                    .all(|(a, b)| a == &b.command && b.success),
            "required checks have not passed"
        );
        for id in &self.task.capture_operations {
            let checkpoint = self.checkpoint(Some(id)).await?;
            anyhow::ensure!(
                checkpoint.conforms && checkpoint.durable && checkpoint.pending.is_empty(),
                "capture operation {id} remains unresolved"
            );
        }
        let checkpoint = self.checkpoint(None).await?;
        anyhow::ensure!(
            checkpoint.conforms && checkpoint.durable,
            "graph validation or durable checkpoint failed"
        );
        anyhow::ensure!(
            self.task.approved_revision.as_deref() == Some(&checkpoint.revision),
            "knowledge changed during completion; refresh and approve the plan again"
        );
        self.update_knowledge_revision(checkpoint.revision);
        executor::cleanup_task(&self.scratch_path())?;
        self.task.phase = Phase::Complete;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        self.event("Complete: required checks passed, human knowledge review resolved, graph validated and durably checkpointed.");
        self.persist()
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
        self.task.approved_revision = None;
        self.discard_pending_edit("human returned the task to Plan")?;
        self.task.completion_pending = false;
        self.task.final_capture = false;
        self.task.check_results.clear();
        self.task.after_review = Phase::Planning;
        self.task.read_files.clear();
        self.task.source.clear();
        self.task.steps = 0;
        self.end_intent_cycle("human replan");
        self.event("Human returned the task to Plan.");
        self.persist()
    }

    pub async fn cancel(&mut self) -> Result<()> {
        self.preserve_stream();
        anyhow::ensure!(
            self.task.phase != Phase::Complete,
            "completed task cannot be cancelled"
        );
        if self.task.phase != Phase::Cancelled {
            self.task.resume_phase = self.task.phase;
            self.task.phase = Phase::Cancelled;
            self.task.cleanup_pending = true;
            self.event("Cancelled; unresolved actions, capture obligations, and scratch cleanup preserved.");
            self.persist()?;
        }
        self.retry_cancelled_cleanup()
    }

    fn retry_cancelled_cleanup(&mut self) -> Result<()> {
        if !self.task.cleanup_pending {
            return Ok(());
        }
        match executor::cleanup_task(&self.scratch_path()) {
            Ok(()) => {
                self.task.cleanup_pending = false;
                self.task.last_error = None;
                self.event("Cancelled task scratch cleanup finished.");
                self.persist()
            }
            Err(error) => {
                let message = format!("Cancellation took effect; scratch cleanup is pending: {error:#}. Press Esc or retry cancel to clean up; /continue (headless resume) retries cleanup before resuming work.");
                self.task.last_error = Some(message.clone());
                self.event(message.clone());
                self.persist()?;
                bail!(message)
            }
        }
    }

    fn reconcile(&mut self) -> Result<()> {
        if let Some(intent) = self.task.intent.clone() {
            match intent {
                Intent::Edit(edit) => {
                    let current = self.workspace.read(&edit.file)?;
                    if current == edit.after {
                        if !self.task.edits.iter().any(|past| {
                            past.file == edit.file
                                && past.before == edit.before
                                && past.after == edit.after
                                && past.revision == edit.revision
                        }) {
                            self.task.edits.push(edit.clone());
                        }
                        self.task
                            .snapshots
                            .insert(edit.file.clone(), fingerprint(&current));
                        self.task.source.insert(edit.file.clone(), current);
                        self.task.intent = None;
                        self.task.capture_due = true;
                        self.task.after_review = Phase::Working;
                        self.event(format!("Recovered completed edit to {} without replay. Before: {:?}; after: {:?}", edit.file, edit.before, edit.after));
                    } else {
                        self.task.phase = Phase::AwaitingInput;
                        self.task.last_response = "Interrupted edit has not reached its expected result. Inspect the file and answer before proceeding; it will not be replayed automatically.".into();
                    }
                }
                Intent::Command(command) => {
                    self.task.phase = Phase::AwaitingInput;
                    self.task.last_response = format!("Command outcome is unknown after interruption: {command}. Inspect and acknowledge before proceeding. It will not be replayed automatically.");
                }
            }
        }
        self.persist()
    }

    pub async fn resume(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.task.phase != Phase::Complete,
            "task is already complete"
        );
        self.retry_cancelled_cleanup()?;
        if self.task.phase == Phase::Cancelled {
            self.task.phase = self.task.resume_phase;
        }
        self.reconcile()?;
        self.resume_intent_refresh().await?;
        let files = self.task.read_files.clone();
        self.refresh(&files).await?;
        // Recover an outstanding capture before approval freshness can change
        // the phase: AwaitingPlan cannot advance an uncertain capture request.
        if self.task.mode == Mode::Auto
            && matches!(self.task.phase, Phase::Working | Phase::Verifying)
            && self.task.intent.is_none()
            && !self.task.capture_due
            && self.task.capture_request.is_none()
        {
            self.fresh_approval().await?;
        }
        self.event("Resumed with refreshed project knowledge.");
        self.persist()
    }

    pub async fn answer(&mut self, text: String) -> Result<()> {
        anyhow::ensure!(
            self.task.phase == Phase::AwaitingInput && !text.trim().is_empty(),
            "no question awaiting an answer"
        );
        self.event(format!("Human response: {text}"));
        self.task.guidance = text.clone();
        self.task.recovery = None;
        self.task.last_response = text;
        self.task.steps = 0;
        if self.task.intent.take().is_some() {
            self.task.mode = Mode::Plan;
            self.task.approved_revision = None;
            self.task.check_results.clear();
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
