//! The model proposes; this state machine owns reading, execution and capture.
use super::{
    config::{self, ModelRole},
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
mod index;
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
pub use symbolic::{CaptureNoteState, FailedRun, SymbolicAssociation, SymbolicState};
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
    /// Set by `configure_provider`; a runner nobody configured never indexes.
    index_refresh: Option<config::IndexRefresh>,
    /// `[harness.sandbox].read_paths`: standing capability, granted without a
    /// gate, so every surface that shows grants shows these too.
    standing_read_paths: Vec<String>,
    /// Edits counted at the last refresh, so a repeated finish does not rebuild.
    indexed_edits: Option<usize>,
    progress: Option<ProgressSender>,
    streaming: Option<Arc<Mutex<StreamedMessage>>>,
    last_saved: Mutex<Option<[u8; 32]>>,
}

pub use crate::harness::{DEFAULT_GUIDANCE, GUIDANCE_FILE};

/// What one mode's resolved guidance (the shared text plus that mode's section)
/// may occupy. Standing guidance sits in the never-truncated mandatory prompt,
/// so this is the bound that matters: it is taken from the observation budget.
pub const MAX_GUIDANCE_BYTES: usize = 4096;
/// What the file may occupy. The per-mode bound above is the real limit; this
/// one is a cheap outer guard, set high enough that it can never be the first
/// to reject a file both modes would accept.
pub const MAX_GUIDANCE_FILE_BYTES: usize = 3 * MAX_GUIDANCE_BYTES;

fn guidance(source: &str, text: &str) -> StandingGuidance {
    StandingGuidance {
        source: source.into(),
        sha256: sha256_hex(text),
        text: text.into(),
        plan: None,
        implement: None,
    }
}

/// Drop HTML comments, so a project can annotate its guidance without paying
/// for the annotation in every prompt. An unterminated comment runs to the end
/// of the file, as it does in HTML.
fn strip_comments(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        stripped.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("-->") else {
            return stripped;
        };
        rest = &rest[start + end + "-->".len()..];
    }
    stripped.push_str(rest);
    stripped
}

/// The heading that opens a mode's section: an ATX heading whose title is
/// exactly `plan` or `implement`, ignoring case. Every other heading is
/// ordinary content belonging to the section it sits in.
fn section_heading(line: &str) -> Option<ModelRole> {
    let title = line.trim_start_matches('#');
    let level = line.len() - title.len();
    if !(1..=6).contains(&level) {
        return None;
    }
    ModelRole::parse(&title.trim().to_ascii_lowercase())
}

/// Split the file into the shared text and each mode's section. Text before the
/// first section heading is shared; a heading inside a fenced code block is
/// content, so guidance may show markdown; a repeated heading is refused,
/// because there is no honest way to guess which one the writer meant.
fn split_guidance(text: &str) -> Result<(String, Option<String>, Option<String>)> {
    let (mut shared, mut plan, mut implement) = (String::new(), None, None);
    let mut current: Option<ModelRole> = None;
    let mut fence: Option<&str> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker));
        let fenced = fence.is_some();
        match (fence, marker) {
            (None, Some(marker)) => fence = Some(marker),
            (Some(open), Some(marker)) if open == marker => fence = None,
            _ => {}
        }
        if !fenced && marker.is_none() {
            if let Some(role) = section_heading(trimmed) {
                let section = match role {
                    ModelRole::Plan => &mut plan,
                    ModelRole::Implement => &mut implement,
                };
                anyhow::ensure!(
                    section.is_none(),
                    "{GUIDANCE_FILE} line {}: a second `{}` section; each mode has one",
                    index + 1,
                    role.as_str()
                );
                *section = Some(String::new());
                current = Some(role);
                continue;
            }
        }
        let target = match current {
            None => &mut shared,
            Some(ModelRole::Plan) => plan.as_mut().expect("its heading opened the section"),
            Some(ModelRole::Implement) => {
                implement.as_mut().expect("its heading opened the section")
            }
        };
        target.push_str(line);
        target.push('\n');
    }
    let trim = |text: String| text.trim().to_string();
    Ok((trim(shared), plan.map(trim), implement.map(trim)))
}

/// Read the project's standing guidance: [`GUIDANCE_FILE`] (not a symlink,
/// UTF-8, at most [`MAX_GUIDANCE_FILE_BYTES`]), empty when it holds only
/// whitespace, or the compiled default when it does not exist. A file replaces
/// the default rather than adding to it, which is what lets a project reword it.
pub fn load_standing_guidance(root: &Path) -> Result<StandingGuidance> {
    let path = root.join(GUIDANCE_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(guidance("default", DEFAULT_GUIDANCE.trim()));
        }
        Err(error) => return Err(error).context(format!("read {GUIDANCE_FILE}")),
    };
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "{GUIDANCE_FILE} must be a regular file"
    );
    anyhow::ensure!(
        meta.len() as usize <= MAX_GUIDANCE_FILE_BYTES,
        "{GUIDANCE_FILE} is {} bytes; standing guidance must fit {MAX_GUIDANCE_FILE_BYTES} bytes",
        meta.len()
    );
    let text = String::from_utf8(std::fs::read(&path)?)
        .context(format!("{GUIDANCE_FILE} must be UTF-8"))?;
    anyhow::ensure!(
        text.len() <= MAX_GUIDANCE_FILE_BYTES,
        "{GUIDANCE_FILE} must fit {MAX_GUIDANCE_FILE_BYTES} bytes"
    );
    let body = strip_comments(&text);
    if body.trim().is_empty() {
        return Ok(guidance("empty", ""));
    }
    let (shared, plan, implement) = split_guidance(&body)?;
    let standing = StandingGuidance {
        source: "file".into(),
        sha256: sha256_hex(text.trim()),
        text: shared,
        plan,
        implement,
    };
    for role in ModelRole::ALL {
        let resolved = standing.for_role(role).len();
        anyhow::ensure!(
            resolved <= MAX_GUIDANCE_BYTES,
            "{GUIDANCE_FILE}: the guidance {} mode receives is {resolved} bytes; \
             each mode's share (the text before the first section, plus its own section) \
             must fit {MAX_GUIDANCE_BYTES} bytes",
            role.as_str()
        );
    }
    Ok(standing)
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
            index_refresh: None,
            standing_read_paths: Vec::new(),
            indexed_edits: None,
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
            index_refresh: None,
            standing_read_paths: Vec::new(),
            indexed_edits: None,
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

    /// Journal which standing guidance the task carries, per mode, so a replay
    /// can tell which text reached which request.
    fn guidance_loaded(&mut self) {
        if let Some(standing) = self.task.standing_guidance.clone() {
            let sections: String = ModelRole::ALL
                .iter()
                .map(|role| format!(", {} {}", standing.for_role(*role).len(), role.as_str()))
                .collect();
            self.intent_event(
                "guidance_loaded",
                &format!(
                    "{}, {} bytes shared{sections}, sha256 {}",
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
        self.index_refresh = Some(provider.index_refresh);
        self.standing_read_paths = provider.standing_read_paths.clone();
    }

    /// Paths this project grants every task without asking.
    pub fn standing_read_paths(&self) -> &[String] {
        &self.standing_read_paths
    }

    /// Whether this runner rebuilds the code index at finish.
    pub fn set_index_refresh(&mut self, policy: config::IndexRefresh) {
        self.index_refresh = Some(policy);
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        /// A project root holding `text` as its standing guidance.
        fn with(text: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("moosedev-guidance-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(path.join(".moosedev")).unwrap();
            std::fs::write(path.join(GUIDANCE_FILE), text).unwrap();
            Self(path)
        }
        fn load(&self) -> Result<StandingGuidance> {
            load_standing_guidance(&self.0)
        }
        /// What each mode would receive, in `ModelRole::ALL` order.
        fn per_mode(&self) -> [String; 2] {
            let standing = self.load().unwrap();
            ModelRole::ALL.map(|role| standing.for_role(role))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_without_sections_reaches_both_modes_whole() {
        let fixture = Fixture::with("Prefer small functions.\n");
        let standing = fixture.load().unwrap();
        assert_eq!(standing.source, "file");
        assert_eq!(standing.text, "Prefer small functions.");
        assert_eq!(standing.plan, None);
        assert_eq!(standing.implement, None);
        assert_eq!(
            fixture.per_mode(),
            [
                "Prefer small functions.".to_string(),
                "Prefer small functions.".to_string()
            ]
        );
    }

    #[test]
    fn a_section_reaches_only_its_own_mode_after_the_shared_text() {
        let fixture = Fixture::with(
            "Build offline.\n\n## Plan\nName the check.\n\n## Implement\nOne file per action.\n",
        );
        let standing = fixture.load().unwrap();
        assert_eq!(standing.text, "Build offline.");
        assert_eq!(standing.plan.as_deref(), Some("Name the check."));
        assert_eq!(standing.implement.as_deref(), Some("One file per action."));
        assert_eq!(
            fixture.per_mode(),
            [
                "Build offline.\n\nName the check.".to_string(),
                "Build offline.\n\nOne file per action.".to_string(),
            ]
        );
    }

    #[test]
    fn a_section_may_stand_alone_and_leave_the_other_mode_the_shared_text() {
        let fixture = Fixture::with("## implement\nOne file per action.\n");
        let [plan, implement] = fixture.per_mode();
        assert_eq!(plan, "");
        assert_eq!(implement, "One file per action.");
    }

    #[test]
    fn only_plan_and_implement_head_a_section_and_case_does_not_matter() {
        let fixture = Fixture::with(
            "Shared.\n\n### IMPLEMENT\nMine.\n\n## Style notes\nOrdinary text.\n\n# plan\nPlanning.\n",
        );
        let standing = fixture.load().unwrap();
        assert_eq!(standing.text, "Shared.");
        assert_eq!(standing.plan.as_deref(), Some("Planning."));
        // An unrecognised heading belongs to the section it sits in.
        assert_eq!(
            standing.implement.as_deref(),
            Some("Mine.\n\n## Style notes\nOrdinary text.")
        );
    }

    #[test]
    fn a_heading_inside_a_fenced_block_is_content_so_guidance_may_show_markdown() {
        let fixture =
            Fixture::with("Shared.\n\n```md\n## Plan\nnot a section\n```\n\nStill shared.\n");
        let standing = fixture.load().unwrap();
        assert_eq!(standing.plan, None);
        assert!(standing.text.contains("## Plan"));
        assert!(standing.text.ends_with("Still shared."));
    }

    #[test]
    fn a_repeated_section_is_refused_by_line_rather_than_guessed_at() {
        let error = Fixture::with("## Plan\nOne.\n\n## Plan\nTwo.\n")
            .load()
            .unwrap_err()
            .to_string();
        assert!(error.contains("line 4"), "{error}");
        assert!(error.contains("second `plan` section"), "{error}");
    }

    #[test]
    fn comments_annotate_the_file_without_reaching_the_model() {
        let fixture =
            Fixture::with("<!-- why: -->Shared.\n\n<!--\n## Plan\nhidden\n-->\nStill shared.\n");
        let standing = fixture.load().unwrap();
        assert_eq!(standing.plan, None);
        assert_eq!(standing.text, "Shared.\n\n\nStill shared.");
        // A file that is nothing but commentary says nothing.
        assert_eq!(
            Fixture::with("<!-- todo -->\n").load().unwrap().source,
            "empty"
        );
    }

    #[test]
    fn each_mode_is_capped_on_its_own_share_while_the_file_holds_all_three() {
        let filler = |bytes: usize| "x".repeat(bytes);
        // Both modes may be nearly full at once: the file bound is not the
        // first to bite, and one mode's section does not eat the other's room.
        let fixture = Fixture::with(&format!(
            "{}\n\n## Plan\n{}\n\n## Implement\n{}\n",
            filler(1000),
            filler(MAX_GUIDANCE_BYTES - 1002),
            filler(MAX_GUIDANCE_BYTES - 1002)
        ));
        let [plan, implement] = fixture.per_mode();
        assert_eq!(plan.len(), MAX_GUIDANCE_BYTES);
        assert_eq!(implement.len(), MAX_GUIDANCE_BYTES);
        // One mode's share over the cap is refused, and named.
        let error = Fixture::with(&format!(
            "{}\n\n## Implement\n{}\n",
            filler(2048),
            filler(2048)
        ))
        .load()
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("guidance implement mode receives"),
            "{error}"
        );
        assert!(error.contains(&MAX_GUIDANCE_BYTES.to_string()), "{error}");
        // And the file has its own, larger, bound.
        let error = Fixture::with(&filler(MAX_GUIDANCE_FILE_BYTES + 1))
            .load()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&MAX_GUIDANCE_FILE_BYTES.to_string()),
            "{error}"
        );
    }

    #[test]
    fn the_shipped_example_loads_with_both_sections_inside_every_bound() {
        let fixture = Fixture::with(&crate::harness::guidance_example());
        let standing = fixture.load().unwrap();
        assert_eq!(standing.source, "file");
        // The example carries the compiled default, which a real file replaces.
        assert!(standing.text.starts_with(DEFAULT_GUIDANCE.trim()));
        assert!(standing.plan.is_some() && standing.implement.is_some());
        // Its commentary explains the file; it must not reach the model.
        for text in fixture.per_mode() {
            assert!(!text.contains("Copy it to"), "{text}");
            assert!(text.len() <= MAX_GUIDANCE_BYTES);
        }
    }

    #[test]
    fn a_journal_written_before_sections_serves_both_modes_its_whole_text() {
        let standing: StandingGuidance =
            serde_json::from_str(r#"{"source":"file","sha256":"abc","text":"Old."}"#).unwrap();
        assert_eq!(standing.for_role(ModelRole::Plan), "Old.");
        assert_eq!(standing.for_role(ModelRole::Implement), "Old.");
    }
}
