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
    AwaitingPermission,
    /// A harness question with options (`Task::pending_choice`), answered
    /// with `/choose`.
    AwaitingChoice,
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
    /// IRIs of the governing rules the model says this plan implements,
    /// resolved against the rules delivered for its files. These, not the
    /// summary's prose, become the capture's `isMotivatedBy` edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// IRIs of the governing rules the model says the existing code already
    /// satisfies unchanged, resolved like `addresses` (which wins when both
    /// name a rule). A claim only: it settles the rule for coverage and the
    /// approval gate, and never becomes a knowledge edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub satisfied: Vec<String>,
    /// The governing rules delivered for the plan files that the plan leaves
    /// open: not in `addresses` or `satisfied`, whatever its summary says of
    /// them. The approval gate names them; `/approve` records them as
    /// deferred.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_rules: Vec<OpenRule>,
    /// Questions the planner left for the human to decide before building,
    /// answered with `/choose <n> <option>` or by their default on approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_choices: Vec<OpenChoice>,
}

/// A governing rule a proposed plan leaves open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRule {
    pub iri: String,
    pub label: String,
    pub kind: String,
    /// The plan's summary speaks to it (as plan coverage reads it): it may
    /// say the rule is deferred or does not apply, but the plan does not
    /// list it as implemented.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mentioned: bool,
}

/// A question a proposed plan leaves for the human, with its options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenChoice {
    pub question: String,
    pub options: Vec<String>,
    pub default: String,
    /// The option the human chose, or the default once the plan is approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// One plan the human approved during this task. A replan replaces
/// `Task::plan`; this history is what the final capture note and the first-edit
/// guard see of the earlier ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedPlan {
    pub summary: String,
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// The plan's `satisfied` claims at approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub satisfied: Vec<String>,
    /// IRIs of every governing rule delivered for the plan files at approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules_in_view: Vec<String>,
    /// IRIs of the rules the plan left open, deferred by its approval. They
    /// stay open: spec progress counts only recorded `isMotivatedBy` edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<String>,
    /// `Task::edits` index where this plan's work begins.
    pub edit_start: usize,
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
    pub governing_rules: Vec<GoverningRule>,
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

/// One exact command/capability request frozen for human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingPermission {
    pub request_id: String,
    pub command: String,
    pub justification: String,
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    pub network: bool,
    pub revision: String,
    /// Why the request cannot be granted as asked, when it cannot. A refused
    /// request still reaches the human: they see what was asked and why it was
    /// turned down, and the model is told so it can ask for something narrower.
    /// Refusing without asking is how a gate becomes an unattended halt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// What surveying the requested scopes noticed, for the human to weigh.
    #[serde(
        default,
        skip_serializing_if = "crate::harness::executor::PermissionFindings::is_empty"
    )]
    pub findings: crate::harness::executor::PermissionFindings,
    /// Set once the human approves: the grant that authorizes this exact
    /// command, which the next step then runs inside the interruptible loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_grant: Option<String>,
}

impl PendingPermission {
    /// A refused request authorizes nothing and cannot be approved.
    pub fn is_refused(&self) -> bool {
        self.refusal.is_some()
    }
}

/// A question the harness asks the human, with the options it can carry
/// out. The harness asks only what the symbolic layer cannot default: whether
/// an approved plan may grow (by a file an edit or a module declaration
/// needs), or whether work may be verified with planned files missing
/// (Constraint cd9f1a96 keeps such questions away from the model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingChoice {
    pub id: String,
    pub kind: ChoiceKind,
    pub prompt: String,
    pub options: Vec<ChoiceOption>,
    /// The key of the option taken when the human answers `/choose` alone.
    pub default: String,
}

/// What a pending choice is about, with what its options act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceKind {
    /// The model proposed an edit to `file`, outside the approved plan.
    ScopeAdd { file: String },
    /// A finish came back with planned `files` still missing.
    MissingPlannedFile { files: Vec<String> },
    /// A finish came back with planned `files` that exist but have no edit
    /// of the model's since the plan was approved.
    UneditedPlannedFiles { files: Vec<String> },
    /// A module declaration or import in `declared_in` names `file`, which
    /// neither exists nor is planned.
    MissingModule { file: String, declared_in: String },
}

impl ChoiceKind {
    /// The journal name of the kind.
    pub fn name(&self) -> &'static str {
        match self {
            ChoiceKind::ScopeAdd { .. } => "scope_add",
            ChoiceKind::MissingPlannedFile { .. } => "missing_planned_file",
            ChoiceKind::UneditedPlannedFiles { .. } => "unedited_planned_files",
            ChoiceKind::MissingModule { .. } => "missing_module",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub key: String,
    pub label: String,
}

/// A durable capability approved for the remainder of this task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionGrant {
    pub id: String,
    pub justification: String,
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    pub network: bool,
    pub approved_at: String,
    /// What the human was shown about these scopes when they approved.
    #[serde(
        default,
        skip_serializing_if = "crate::harness::executor::PermissionFindings::is_empty"
    )]
    pub findings: crate::harness::executor::PermissionFindings,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Intent {
    Edit(PendingEdit),
    Command(String),
    PermissionedCommand {
        command: String,
        grant_ids: Vec<String>,
    },
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
    /// Specification lines no extracted record cites, shown at the gate so
    /// the human sees what the batch leaves out, not only what it holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncited: Vec<SpecUncited>,
    /// Why scoping the records to the specification's parts failed, when it
    /// did: every record then governs the specification's own component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoping_failed: Option<String>,
}

/// A run of specification lines no record cites, under its nearest heading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecUncited {
    pub start: usize,
    pub end: usize,
    pub heading: String,
}

impl SpecUncited {
    pub fn describe(&self) -> String {
        let lines = if self.start == self.end {
            format!("line {}", self.start)
        } else {
            format!("lines {}-{}", self.start, self.end)
        };
        if self.heading.is_empty() {
            lines
        } else {
            format!("{lines} ({})", self.heading)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub objective: String,
    /// The objective was a spec approval that has now been granted. The task
    /// waits in Planning without a model call, and the human's next message
    /// becomes the objective instead of guidance under a finished one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub objective_pending: bool,
    pub mode: Mode,
    pub phase: Phase,
    pub plan: Option<Plan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_plans: Vec<ApprovedPlan>,
    /// Proposals the human dropped from a pending capture, by operation id:
    /// 0-based indices into that capture's proposals. Sent as the review's
    /// `rejected` entries when the capture is accepted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review_drops: BTreeMap<String, Vec<usize>>,
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
    pub pending_permission: Option<PendingPermission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_grants: Vec<PermissionGrant>,
    /// The harness question awaiting the human (`Phase::AwaitingChoice`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_choice: Option<PendingChoice>,
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
    /// `last_response` is a tool observation for the model (a page, a read,
    /// command output), not prose for the human, so the conversation does
    /// not replay it as an assistant turn.
    #[serde(default)]
    pub last_response_observation: bool,
    /// The model itself ended its turn with a reply or a question, and no
    /// model action or human message has come since. A message then answers
    /// the model, rather than following a harness park.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handed_back: bool,
    /// The harness parked approved work while the approved plan still
    /// stands (a spent or repeated repair, a stalled failure, a repeated
    /// read, inspect or command). A message then is judged against the plan
    /// as a handback is, so a one-line hint continues the plan instead of
    /// costing a replan and its approval. Cleared by the next accepted model
    /// action, an answer, new guidance, a return to Plan or an approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan_stands_park: bool,
    /// What the language servers reported after the last applied code edit:
    /// current state every prompt shows and finish is gated on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<super::langserver::DiagnosticsSnapshot>,
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
    /// Working-set files, least recently read or edited first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) source_recency: Vec<String>,
    /// Files the prompt that produced the current action showed only as an
    /// outline. An edit to one of them is not applied until its full source
    /// has been shown.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub(super) source_outlined: std::collections::BTreeSet<String>,
    /// The outlined files of the last prompt whose action the harness
    /// accepted. A file outlined now but not then is named as newly outlined;
    /// a repair prompt, rebuilt after a rejected action, names it again.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub(super) source_outlined_seen: std::collections::BTreeSet<String>,
    /// The files the last prompt showed in full. They stay in full while
    /// they fit: a file that changes tier resends the prompt after it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub(super) source_full: std::collections::BTreeSet<String>,
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
    /// Of the file as read, before it is split into sections.
    pub sha256: String,
    /// What both modes receive: everything before the first section heading.
    pub text: String,
    /// The `## Plan` section, added to [`Self::text`] in Plan mode. Absent in
    /// every journal written before sections existed, which is why both are
    /// `#[serde(default)]`: such a task keeps sending `text` to both modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The `## Implement` section, added to [`Self::text`] in Auto mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implement: Option<String>,
}

impl Task {
    /// The review of the task's final capture, after every required check
    /// passed: where `/rework` can send the work back instead of completing.
    pub fn at_final_review(&self) -> bool {
        self.phase == Phase::AwaitingReview && self.final_capture
    }
}

impl StandingGuidance {
    /// The guidance one role sees: the shared text, then that role's section.
    pub fn for_role(&self, role: ModelRole) -> String {
        let section = match role {
            ModelRole::Plan => self.plan.as_deref(),
            ModelRole::Implement => self.implement.as_deref(),
        };
        match section.map(str::trim).filter(|text| !text.is_empty()) {
            Some(section) if self.text.is_empty() => section.to_string(),
            Some(section) => format!("{}\n\n{section}", self.text),
            None => self.text.clone(),
        }
    }
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
