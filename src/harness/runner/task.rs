//! The durable task journal: its typed state, journal events, and the
//! atomic checkpoint that persists them.
use super::*;
use crate::harness::digest::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Plan,
    Auto,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Planning,
    AwaitingPlan,
    AwaitingSpecApproval,
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
pub struct KnowledgeFileDossier {
    pub file: String,
    pub dossier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeContextSnapshot {
    pub topic: String,
    pub revision: String,
    pub context: String,
    pub files: Vec<KnowledgeFileDossier>,
    pub governing_constraints: Vec<GoverningConstraint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ContextRecord>,
    /// Exact record-delivery accounting supplied by the daemon, when supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_receipt: Option<ContextDeliveryReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchResult {
    pub query: String,
    pub revision: String,
    pub context: String,
    pub evidence_iris: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ContextRecord>,
    /// Exact record-delivery accounting supplied by the daemon, when supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_receipt: Option<ContextDeliveryReceipt>,
}

impl KnowledgeSearchResult {
    pub(super) fn selected_record_count(&self) -> usize {
        self.delivery_receipt
            .as_ref()
            .map_or(self.evidence_iris.len(), |receipt| receipt.records.len())
    }

    pub(super) fn omitted_record_count(&self) -> usize {
        self.delivery_receipt.as_ref().map_or(0, |receipt| {
            receipt
                .records
                .iter()
                .filter(|record| record.tier == ContextRecordDeliveryTier::Omitted)
                .count()
        })
    }
}

/// The graph evidence retrieved while one exact human query was active.
/// Sequence, rather than query text, is the stable identity: a repeated human
/// message starts a distinct turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeTurn {
    pub sequence: u64,
    pub query: String,
    pub retrieval_topic: String,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ContextRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub searches: Vec<KnowledgeSearchResult>,
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
pub(super) enum Intent {
    Edit(PendingEdit),
    Command(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_links: Option<crate::harness::daemon::intent::IntentLinkRequest>,
    pub request: CaptureRequest,
    pub response: CaptureResponse,
    pub reason: String,
}

/// The exact daemon-prepared batch displayed at the spec approval gate.
///
/// Keeping the preview in the task journal makes approval replayable after a
/// restart without asking the model to extract the specification again.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingSpecApproval {
    pub preview: SpecPrepareResponse,
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
    pub(super) intent_cycle: Option<String>,
    #[serde(default)]
    pub(super) pending_intent_links: Option<crate::harness::daemon::intent::IntentLinkRequest>,
    /// A reviewed intent/link receipt is already durable; only this fallible
    /// context refresh remains. Never replay the review while it is set.
    #[serde(default)]
    pub(super) intent_refresh_pending: Vec<String>,
    pub events: Vec<Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_events: Vec<usize>,
    pub pending_capture: Option<CaptureResponse>,
    pub capture_request: Option<CaptureRequest>,
    pub capture_reason: Option<String>,
    pub pending_edit: Option<PendingEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_spec: Option<PendingSpecApproval>,
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
    #[serde(default)]
    pub knowledge_turn_sequence: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_turns: Vec<KnowledgeTurn>,
    /// Legacy raw snapshot retained for schema-2 journal compatibility. New
    /// Knowledge rendering prefers `knowledge_turns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_context: Option<KnowledgeContextSnapshot>,
    /// Legacy flat search history retained for schema-2 journal compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_searches: Vec<KnowledgeSearchResult>,
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
    pub(super) conversation_context: String,
    #[serde(default)]
    pub(super) review_continuation: Option<Phase>,
    #[serde(default)]
    pub(super) delivered_messages: Vec<String>,
    #[serde(default)]
    pub(super) guidance: String,
    #[serde(default)]
    pub(super) capture_end: Option<usize>,
    #[serde(default)]
    pub(super) capture_checkpoint_end: Option<usize>,
    pub(super) schema: u32,
    pub(super) snapshots: BTreeMap<String, Option<String>>,
    pub(super) approved_revision: Option<String>,
    pub(super) intent: Option<Intent>,
    pub(super) capture_due: bool,
    pub(super) final_capture: bool,
    pub(super) after_review: Phase,
    pub(super) resume_phase: Phase,
    pub(super) steps: usize,
    pub(super) capture_operations: Vec<String>,
    pub(super) capture_cursor: usize,
    /// Human capture review is resolved; retry only completion checks on failure.
    #[serde(default)]
    pub(super) completion_pending: bool,
    /// Cancellation is durable even when scratch cleanup must be retried.
    #[serde(default)]
    pub cleanup_pending: bool,
    /// Verbatim recent source for generation; historical evidence lives in events.
    pub(super) source: BTreeMap<String, Option<String>>,
    /// The standing guidance this task was created with, replayed verbatim on
    /// resume. `None` only in journals written before the guidance file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing_guidance: Option<StandingGuidance>,
}

/// Standing guidance for the coding model: the project's `.moosedev/GUIDANCE.md`,
/// or the compiled default when the file is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingGuidance {
    /// `file`, `empty` (the file exists but says nothing) or `default`.
    pub source: String,
    pub sha256: String,
    pub text: String,
}

pub(super) fn fingerprint(value: &Option<String>) -> Option<String> {
    value.as_deref().map(sha256_hex)
}
pub(super) fn bounded(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated]", &s[..end])
}

impl Runner {
    pub(super) fn storage(root: &Path, id: &str) -> Result<(PathBuf, File)> {
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

    pub(super) fn event(&mut self, message: impl Into<String>) {
        let message = message.into();
        if let Some(progress) = &self.progress {
            let _ = progress.send(Progress::Status(bounded(&message, 2000)));
        }
        self.task.events.push(Event { message });
    }

    pub(super) fn knowledge_event(&mut self, message: impl Into<String>) {
        self.task.knowledge_events.push(self.task.events.len());
        self.task.events.push(Event {
            message: message.into(),
        });
    }

    pub(super) fn persist(&self) -> Result<()> {
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
}
