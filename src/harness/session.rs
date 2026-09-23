//! Durable conversation and human input, independent of the model's action protocol.
use super::{
    config::ModelRole,
    progress::{Progress as ProgressEvent, ProgressSender},
    runner::{spec_approval_objective, Mode, PermissionGrant, Phase, RecoveryStatus, Runner, Task},
    startup::{ProviderSettings, StartupOptions},
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: Option<String>,
    pub role: String,
    pub text: String,
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub root: PathBuf,
    pub messages: Vec<Message>,
    pub queued: Vec<QueuedMessage>,
    pub tasks: Vec<String>,
    pub active_task: Option<String>,
    #[serde(default)]
    seen_events: BTreeMap<String, usize>,
    schema: u32,
}

/// The journal fields that say whether a task can be reopened; every other
/// field is ignored so a listing never needs the full task type or a daemon.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskHeader {
    pub schema: u32,
    pub phase: Phase,
    pub objective: String,
}

/// One saved conversation as the resume listing and the launch choice see it.
#[derive(Debug, Clone)]
pub struct ConversationSummary {
    pub id: String,
    pub modified: std::time::SystemTime,
    /// The first human message.
    pub objective: Option<String>,
    /// `None`: no task; `Some(None)`: a journal this build cannot read.
    pub task: Option<Option<TaskHeader>>,
}

impl ConversationSummary {
    /// An active task at this build's journal schema that has not completed.
    pub fn unfinished(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            task.as_ref().is_some_and(|task| {
                task.schema == super::runner::SCHEMA && task.phase != Phase::Complete
            })
        })
    }
    /// The task's standing, as one phrase.
    pub fn task_status(&self) -> String {
        match &self.task {
            None => "no task".into(),
            Some(None) => "task journal unreadable by this build".into(),
            Some(Some(task)) if task.schema != super::runner::SCHEMA => {
                format!(
                    "task journal schema {} is not resumable by this build",
                    task.schema
                )
            }
            Some(Some(task)) => format!("{:?}", task.phase),
        }
    }
    /// One line for a listing: id, age, objective, task standing.
    pub fn describe(&self) -> String {
        let objective = self
            .task
            .as_ref()
            .and_then(|task| task.as_ref().map(|task| task.objective.as_str()))
            .or(self.objective.as_deref())
            .unwrap_or("(empty)");
        let objective: String = objective
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        format!(
            "{} · {} · {objective} · {}",
            self.id,
            age(self.modified),
            self.task_status()
        )
    }
}

/// "12m ago", "3h ago", "2d ago": enough to tell sessions apart.
fn age(modified: std::time::SystemTime) -> String {
    let seconds = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match seconds {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

impl Conversation {
    pub fn new(root: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            root,
            messages: Vec::new(),
            queued: Vec::new(),
            tasks: Vec::new(),
            active_task: None,
            seen_events: BTreeMap::new(),
            schema: 1,
        }
    }
    fn directory(root: &Path) -> PathBuf {
        root.join(".moosedev/harness/conversations")
    }
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        uuid::Uuid::parse_str(id).context("invalid conversation ID")?;
        let value: Self = serde_json::from_slice(&std::fs::read(
            Self::directory(root).join(format!("{id}.json")),
        )?)?;
        anyhow::ensure!(
            value.schema == 1 && value.id == id && value.root == root,
            "conversation identity mismatch"
        );
        Ok(value)
    }
    /// Every saved conversation, newest first, with what a human needs to
    /// choose one: when it was last written, what was asked, and where its
    /// task stands. Reads only the journal's header fields, no daemon.
    pub fn summaries(root: &Path) -> Result<Vec<ConversationSummary>> {
        let mut summaries = Vec::new();
        for id in Self::list(root)? {
            // A conversation from another project root or a damaged file is
            // not one this session can reopen; leave it out of the choice.
            let Ok(conversation) = Self::load(root, &id) else {
                continue;
            };
            let modified =
                std::fs::metadata(Self::directory(root).join(format!("{id}.json")))?.modified()?;
            let objective = conversation
                .messages
                .iter()
                .find(|message| message.role == "user")
                .map(|message| message.text.clone());
            let task = conversation.active_task.as_deref().map(|task| {
                std::fs::read(
                    root.join(".moosedev/harness/tasks")
                        .join(format!("{task}.json")),
                )
                .ok()
                .and_then(|bytes| serde_json::from_slice::<TaskHeader>(&bytes).ok())
            });
            summaries.push(ConversationSummary {
                id,
                modified,
                objective,
                task,
            });
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.modified));
        Ok(summaries)
    }
    /// The newest conversation whose task this build can pick up.
    pub fn last_unfinished(root: &Path) -> Result<Option<ConversationSummary>> {
        Ok(Self::summaries(root)?
            .into_iter()
            .find(ConversationSummary::unfinished))
    }
    pub fn list(root: &Path) -> Result<Vec<String>> {
        let directory = Self::directory(root);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                {
                    ids.push(id.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
    fn prepare_directory(root: &Path) -> Result<PathBuf> {
        let mut directory = root.to_path_buf();
        for component in [".moosedev", "harness", "conversations"] {
            directory.push(component);
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) => anyhow::ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "conversation storage must be a real directory"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&directory)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(directory)
    }
    fn lease(root: &Path, id: &str) -> Result<File> {
        uuid::Uuid::parse_str(id)?;
        let directory = Self::prepare_directory(root)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(directory.join(format!("{id}.lock")))?;
        file.try_lock_exclusive()
            .context("this conversation is already open in another harness")?;
        Ok(file)
    }
    pub fn save(&self) -> Result<()> {
        let directory = Self::prepare_directory(&self.root)?;
        let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(temporary, directory.join(format!("{}.json", self.id)))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    pub fn push(&mut self, role: &str, text: impl Into<String>) {
        self.messages.push(Message {
            id: None,
            role: role.into(),
            text: text.into(),
            task: self.active_task.clone(),
        });
    }
    pub fn enqueue(&mut self, text: String) -> Result<()> {
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= 16_000,
            "Messages must contain 1..16000 bytes; shorten this message before sending."
        );
        let id = uuid::Uuid::new_v4().to_string();
        self.push("user", text.clone());
        self.messages.last_mut().unwrap().id = Some(id.clone());
        self.queued.push(QueuedMessage { id, text });
        self.save()
    }
    pub fn context(&self) -> String {
        // Conversation is expendable prompt context; accepted knowledge is not.
        let mut budget = 12_000usize;
        let mut selected = Vec::new();
        let current = self
            .messages
            .iter()
            .rposition(|message| message.role == "user" && message.task == self.active_task);
        for message in self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(index, message)| {
                (message.role == "user" || message.role == "assistant")
                    && Some(*index) != current
                    && !self
                        .queued
                        .iter()
                        .any(|queued| message.id.as_ref() == Some(&queued.id))
            })
            .map(|(_, message)| message)
        {
            let value = format!("{}: {}", message.role, message.text);
            if value.len() > budget {
                break;
            }
            budget -= value.len();
            selected.push(value);
        }
        selected.reverse();
        selected.join("\n\n")
    }
    fn sync_task(&mut self, task: &Task) {
        let seen = *self.seen_events.get(&task.id).unwrap_or(&0);
        for (index, event) in task.events.iter().enumerate().skip(seen) {
            if transcript_role(index, &task.knowledge_events).is_some() {
                self.push("activity", event.message.clone());
            }
        }
        self.seen_events.insert(task.id.clone(), task.events.len());
    }
}

fn transcript_role(index: usize, knowledge_events: &[usize]) -> Option<&'static str> {
    (!knowledge_events.contains(&index)).then_some("activity")
}

/// The task stopped because its model repair budget is spent; `last_response`
/// holds the request for human guidance.
fn parked_for_guidance(task: &Task) -> bool {
    task.phase == Phase::AwaitingInput
        && task
            .recovery
            .as_ref()
            .is_some_and(|repair| repair.status == RecoveryStatus::AwaitingGuidance)
}

fn should_append_last_response(
    last_response: &str,
    prior_response: &str,
    phase: Phase,
    outcome_ok: bool,
    interrupted: bool,
    searched_knowledge: bool,
) -> bool {
    outcome_ok
        && !interrupted
        && !searched_knowledge
        && !last_response.is_empty()
        && (last_response != prior_response || phase == Phase::AwaitingInput)
}

#[derive(Debug)]
pub enum Command {
    Input(String),
    Interrupt,
    Quit,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub conversation: Conversation,
    pub task: Option<Task>,
    pub status: String,
    pub busy: bool,
    pub endpoint: String,
    pub model: String,
    pub live: Arc<Mutex<LiveOutput>>,
    /// `[harness.sandbox].read_paths`: ambient capability a human judging any
    /// gate needs to see, because it is already in force.
    pub standing_read_paths: Vec<String>,
}

/// The controller owns streaming buffers; views share a read-only handle instead
/// of independently reconstructing and truncating every streamed delta.
#[derive(Debug, Default)]
pub struct LiveOutput {
    pub assistant: String,
    pub command: String,
}

pub enum Update {
    State(Box<Snapshot>),
    Progress(ProgressEvent),
    RestoreInput(String),
    Closed,
}

pub struct Controller {
    conversation: Conversation,
    runner: Option<Runner>,
    active_snapshot: Option<Task>,
    startup: StartupOptions,
    provider: ProviderSettings,
    daemon: Option<String>,
    status: String,
    output: mpsc::UnboundedSender<Update>,
    input: mpsc::UnboundedReceiver<Command>,
    progress: ProgressSender,
    progress_rx: mpsc::UnboundedReceiver<ProgressEvent>,
    live: Arc<Mutex<LiveOutput>>,
    startup_notices: Vec<String>,
    auto: bool,
    deferred: VecDeque<Command>,
    quitting: bool,
    lease: Option<File>,
    available_models: Vec<String>,
}

impl Controller {
    pub fn new(
        conversation: Conversation,
        runner: Option<Runner>,
        startup: StartupOptions,
        provider: ProviderSettings,
        input: mpsc::UnboundedReceiver<Command>,
        output: mpsc::UnboundedSender<Update>,
    ) -> Self {
        let (progress, progress_rx) = mpsc::unbounded_channel();
        Self {
            conversation,
            runner,
            active_snapshot: None,
            startup,
            provider,
            daemon: None,
            status: "Connecting…".into(),
            input,
            output,
            progress,
            progress_rx,
            live: Arc::default(),
            startup_notices: Vec::new(),
            auto: false,
            deferred: VecDeque::new(),
            quitting: false,
            lease: None,
            available_models: Vec::new(),
        }
    }
    fn publish(&self, busy: bool) {
        let active = self.active_model();
        let _ = self.output.send(Update::State(Box::new(Snapshot {
            conversation: self.conversation.clone(),
            task: self
                .runner
                .as_ref()
                .map(|r| r.task.clone())
                .or_else(|| self.active_snapshot.clone()),
            status: self.status.clone(),
            busy,
            endpoint: active.base_url,
            model: active.model,
            live: self.live.clone(),
            standing_read_paths: self
                .runner
                .as_ref()
                .map(|r| r.standing_read_paths().to_vec())
                .unwrap_or_default(),
        })));
    }
    fn save_conversation(&self) -> Result<()> {
        if self.startup.needs_initialization() || self.lease.is_none() {
            return Ok(());
        }
        self.conversation.save()
    }
    fn acquire(&mut self) -> Result<()> {
        if self.lease.is_none() && !self.startup.needs_initialization() {
            let lease = Conversation::lease(&self.conversation.root, &self.conversation.id)?;
            // A prior owner may have saved after the startup screen loaded.
            // Re-read only after obtaining exclusive ownership of the journal.
            if Conversation::directory(&self.conversation.root)
                .join(format!("{}.json", self.conversation.id))
                .exists()
            {
                self.conversation =
                    Conversation::load(&self.conversation.root, &self.conversation.id)?;
            }
            self.lease = Some(lease);
        }
        for notice in self.startup_notices.drain(..) {
            self.conversation.push("system", notice);
        }
        Ok(())
    }
    pub fn with_startup_notice(mut self, notice: Option<String>) -> Self {
        self.startup_notices.extend(notice);
        self
    }
    async fn interrupt(&mut self) {
        self.auto = false;
        if let Some(runner) = &mut self.runner {
            if runner.task.phase == Phase::Complete
                || (runner.task.phase == Phase::Cancelled && !runner.task.cleanup_pending)
            {
                return;
            }
            let result = runner.cancel().await;
            self.conversation.sync_task(&runner.task);
            if let Err(error) = result {
                self.fail(error);
                return;
            }
            let _ = self.save_conversation();
            self.status = "Interrupted. /continue resumes; follow-up text replans.".into();
        }
    }
    /// Persist a choice to `moosedev.toml` and apply whatever the reload
    /// resolved, even when an environment override makes the save ineffective.
    fn select_model(
        &mut self,
        role: Option<ModelRole>,
        endpoint: Option<&str>,
        model: &str,
    ) -> Result<()> {
        let saved = self
            .provider
            .persist_selection(&self.conversation.root, role, endpoint, model);
        self.configure()?;
        saved
    }
    /// The model answering now: the plan model until a plan is approved, the
    /// implementation model after. The header and the state feed show this one.
    fn active_model(&self) -> crate::llm::LlmConfig {
        let mode = self
            .runner
            .as_ref()
            .map(|runner| runner.task.mode)
            .or_else(|| self.active_snapshot.as_ref().map(|task| task.mode));
        let role = match mode {
            Some(Mode::Auto) => ModelRole::Implement,
            Some(Mode::Plan) | None => ModelRole::Plan,
        };
        self.provider.for_role(role).config
    }
    fn configure(&mut self) -> Result<()> {
        if let Some(runner) = &mut self.runner {
            runner.enable_interactive()?;
            runner.configure_provider(&self.provider, Some(self.progress.clone()));
            runner.set_conversation_context(self.conversation.context());
        }
        Ok(())
    }
    async fn connect(&mut self) -> Result<()> {
        if self.startup.needs_initialization() {
            for notice in self.startup_notices.drain(..) {
                self.conversation.push("system", notice);
            }
            self.status = "Project initialization is needed. Use /init or /quit.".into();
            self.conversation.push("system","This project is not initialized. /init creates project configuration, .env, and managed agent instructions. Use /init to initialize, or /quit to exit.");
            return Ok(());
        }
        self.acquire()?;
        self.status = "Connecting to project daemon…".into();
        self.publish(true);
        let startup = self.startup.clone();
        let connection = startup.ensure_daemon();
        tokio::pin!(connection);
        self.daemon = Some(loop {
            tokio::select! {
                result = &mut connection => break result?,
                command = self.input.recv() => match command {
                    Some(Command::Input(text)) if !text.trim_start().starts_with('/') => {
                        self.status = match self.conversation.enqueue(text) {
                            Ok(()) => "Connecting; your message is saved and queued.".into(),
                            Err(error) => error.to_string(),
                        };
                        self.publish(true);
                    },
                    Some(Command::Quit) | None => {
                        self.quitting = true;
                        return Ok(());
                    },
                    Some(Command::Interrupt) => {
                        self.status = "Connection interrupted. Use /connect to retry.".into();
                        return Ok(());
                    },
                    Some(command) => {
                        self.deferred.push_back(command);
                        self.status = "Command queued until connection finishes. Esc stops connecting.".into();
                        self.publish(true);
                    },
                }
            }
        });
        self.save_conversation()?;
        if self.runner.is_none() {
            if let Some(id) = self.conversation.active_task.clone() {
                match Runner::load(
                    self.conversation.root.clone(),
                    self.daemon.clone().unwrap(),
                    &id,
                ) {
                    Ok(runner) => self.runner = Some(runner),
                    Err(error) => {
                        // A journal from an earlier build cannot be resumed;
                        // the conversation continues with a fresh task.
                        self.conversation.active_task = None;
                        self.conversation.push(
                            "system",
                            format!("Could not resume task {id}: {error:#} Describe the work again to start a new task."),
                        );
                        self.save_conversation()?;
                    }
                }
            }
        }
        if self.provider.config.model.is_empty() {
            self.discover_models(true).await?;
        }
        self.configure()?;
        self.auto = !self.conversation.queued.is_empty();
        self.status = if self.provider.config.model.is_empty() {
            "Daemon connected. Choose a model with /model."
        } else {
            "Ready. Describe the work, or ask about this project."
        }
        .into();
        if self
            .runner
            .as_ref()
            .is_some_and(|runner| runner.task.cleanup_pending)
        {
            self.status = "Cancelled; scratch cleanup is pending. Esc retries cleanup; /continue retries cleanup before resuming.".into();
            self.conversation.push("system", self.status.clone());
            self.save_conversation()?;
        }
        self.save_conversation()?;
        Ok(())
    }
    async fn discover_models(&mut self, select_single: bool) -> Result<()> {
        self.status = format!("Discovering models at {}…", self.provider.config.base_url);
        self.publish(true);
        self.available_models = match self.provider.models().await {
            Ok(models) => models,
            Err(error) => {
                self.conversation.push("system",format!("Model discovery failed at {}: {error:#}\nLoad a model and enable the local server in LM Studio, then use /model to retry. To use another endpoint, enter /model <endpoint> <model ID>.",self.provider.config.base_url));
                self.status =
                    "Model server unavailable. Setup instructions are in the conversation.".into();
                return Ok(());
            }
        };
        if self.available_models.len() == 1 && select_single && !self.startup.needs_initialization()
        {
            let model = self.available_models[0].clone();
            self.select_model(None, None, &model)?;
            self.conversation.push(
                "system",
                format!(
                    "Using {model} at {}. Use /model to change the model.",
                    self.provider.config.base_url
                ),
            );
            self.status = format!("Ready · {model}");
            self.auto = !self.conversation.queued.is_empty();
        } else if self.available_models.is_empty() {
            self.conversation.push("system",format!("No models were found at {}. Load a model in LM Studio, then enter /model to discover it.",self.provider.config.base_url));
            self.status = "No model is loaded. Use /model after loading one in LM Studio.".into();
        } else {
            let listing = self
                .available_models
                .iter()
                .enumerate()
                .map(|(i, model)| format!("{}. {model}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            self.conversation.push("system",format!("Models at {}:\n{listing}\nChoose with /model <number> or /model <model ID>. To use another endpoint: /model <endpoint> <model ID>.\nIn use: {}. Give planning or implementation its own model with /model plan <model ID> or /model implement <model ID>.",self.provider.config.base_url,self.provider.describe()));
            self.status = "Select a model with /model <number> or its exact ID.".into();
        }
        self.save_conversation()
    }
    pub async fn run(mut self) {
        self.publish(true);
        if let Err(error) = self.connect().await {
            self.fail(error.context("Connection failed; use /connect to retry"));
        }
        self.publish(false);
        loop {
            if self.quitting {
                break;
            }
            // Consume already-submitted steering before beginning another action.
            while let Ok(command) = self.input.try_recv() {
                self.deferred.push_back(command);
            }
            if let Some(command) = self.deferred.pop_front() {
                if !self.handle_command(command).await {
                    break;
                }
                continue;
            }
            if self.auto {
                if let Err(error) = self.deliver().await {
                    self.fail(error);
                }
                // A task whose spec approval just met its objective waits for
                // the human to name the next one; planning would ask the model
                // to plan an approval that already happened.
                if self.auto
                    && self.runner.as_ref().is_some_and(|r| {
                        matches!(
                            r.task.phase,
                            Phase::Planning | Phase::Working | Phase::Verifying
                        ) && !r.task.objective_pending
                    })
                {
                    match self.advance().await {
                        Ok(false) => break,
                        Ok(true) => {}
                        Err(error) => self.fail(error),
                    }
                    continue;
                }
                self.auto = false;
            }
            self.publish(false);
            let Some(command) = self.input.recv().await else {
                break;
            };
            if !self.handle_command(command).await {
                break;
            }
        }
        let _ = self.save_conversation();
        let _ = self.output.send(Update::Closed);
    }
    async fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::Quit => return false,
            Command::Interrupt => self.interrupt().await,
            Command::Input(text) => {
                if let Err(error) = self.input(text).await {
                    self.fail(error);
                }
            }
        }
        true
    }
    fn fail(&mut self, error: anyhow::Error) {
        self.status = format!("{error:#}");
        self.conversation.push("system", self.status.clone());
        let _ = self.save_conversation();
        self.auto = false;
    }
    async fn deliver(&mut self) -> Result<()> {
        if self.conversation.queued.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            !self.provider.config.model.is_empty(),
            "Choose a model with /model before sending work."
        );
        self.daemon
            .as_ref()
            .context("Daemon is unavailable. Use /connect.")?;
        if self
            .runner
            .as_ref()
            .is_some_and(|r| r.task.phase == Phase::Complete)
        {
            self.runner = None;
        }
        if self.runner.is_none() {
            let text = self.conversation.queued[0].text.clone();
            self.start_task(text).await?;
            self.link_message(&self.conversation.queued[0].id.clone());
            self.conversation.queued.remove(0);
            self.save_conversation()?;
        }
        while let Some(text) = self.conversation.queued.first().cloned() {
            let context = self.conversation.context();
            let runner = self.runner.as_mut().unwrap();
            runner.set_conversation_context(context);
            runner.submit_message_once(&text.id, text.text).await?;
            self.link_message(&text.id);
            self.conversation.queued.remove(0);
            self.save_conversation()?;
        }
        Ok(())
    }
    /// Create the conversation's active task from an objective.
    async fn start_task(&mut self, objective: String) -> Result<()> {
        anyhow::ensure!(
            !self.provider.config.model.is_empty(),
            "Choose a model with /model before sending work."
        );
        let daemon = self
            .daemon
            .clone()
            .context("Daemon is unavailable. Use /connect.")?;
        let runner = Runner::create(self.conversation.root.clone(), daemon, objective).await?;
        self.conversation.active_task = Some(runner.task.id.clone());
        self.conversation.tasks.push(runner.task.id.clone());
        self.runner = Some(runner);
        self.configure()
    }
    fn link_message(&mut self, id: &str) {
        if let Some(message) = self
            .conversation
            .messages
            .iter_mut()
            .find(|m| m.id.as_deref() == Some(id))
        {
            message.task = self.conversation.active_task.clone();
        }
    }
    async fn advance(&mut self) -> Result<bool> {
        let conversation = self.conversation.context();
        self.runner
            .as_mut()
            .unwrap()
            .set_conversation_context(conversation);
        *self.live.lock().unwrap() = LiveOutput::default();
        self.status = "Working… Esc interrupts; messages queue before the next action.".into();
        self.publish(true);
        // The runner is borrowed only for the current action. Dropping this future
        // interrupts generation/commands before cancel reconciles its durable intent.
        let mut runner = self.runner.take().unwrap();
        let prior_response = runner.task.last_response.clone();
        let prior_knowledge_events = runner.task.knowledge_events.len();
        self.active_snapshot = Some(runner.task.clone());
        let mut quit = false;
        let result = {
            let operation = runner.advance();
            tokio::pin!(operation);
            loop {
                tokio::select! {
                    result = &mut operation => break Some(result),
                    event = self.progress_rx.recv() => {
                        if let Some(event) = event {
                            self.progress_event(event);
                        }
                    },
                    command = self.input.recv() => match command {
                        Some(Command::Input(text)) if !text.trim_start().starts_with('/') => {
                            self.status = match self.conversation.enqueue(text) {
                                Ok(()) => "Message queued; it will be delivered before the next action.".into(),
                                Err(error) => error.to_string(),
                            };
                            self.publish(true);
                        },
                        Some(Command::Input(text)) if text.trim() == "/quit" => {
                            quit = true;
                            break None;
                        },
                        Some(Command::Input(text)) => {
                            if matches!(text.split_whitespace().next(), Some("/approve" | "/approve-spec" | "/deny" | "/accept" | "/reject" | "/no-knowledge")) {
                                self.status = "Review commands must be submitted while the gate is displayed. Your command is retained for resubmission.".into();
                                let _ = self.output.send(Update::RestoreInput(text));
                            } else {
                                self.status = format!("Queued command {text}; it will run after this step.");
                                self.deferred.push_back(Command::Input(text));
                            }
                            self.publish(true);
                        },
                        Some(Command::Interrupt) => break None,
                        Some(Command::Quit) | None => {
                            quit = true;
                            break None;
                        },
                    }
                }
            }
        };
        while let Ok(event) = self.progress_rx.try_recv() {
            self.progress_event(event);
        }
        let interrupted = result.is_none();
        let outcome = match result {
            Some(result) => result,
            None => runner.cancel().await,
        };
        self.conversation.sync_task(&runner.task);
        let searched_knowledge = runner.task.knowledge_events.len() > prior_knowledge_events;
        let live_assistant = self.live.lock().unwrap().assistant.clone();
        if !live_assistant.is_empty() {
            let suffix = assistant_suffix(
                interrupted,
                outcome.is_err(),
                runner.task.model_requests.last(),
            );
            self.conversation
                .push("assistant", format!("{live_assistant}{suffix}"));
        } else if should_append_last_response(
            &runner.task.last_response,
            &prior_response,
            runner.task.phase,
            outcome.is_ok(),
            interrupted,
            searched_knowledge,
        ) {
            self.conversation
                .push("assistant", runner.task.last_response.clone());
        }
        // A conversational envelope can contain introductory prose while the
        // action contains the actual answer. Deduplicate within this step only:
        // the same answer to a later user question still deserves a visible turn.
        if outcome.is_ok()
            && !interrupted
            && !searched_knowledge
            && runner.task.phase == Phase::AwaitingInput
            && !live_assistant.is_empty()
            && !runner.task.last_response.is_empty()
            && runner.task.last_response != live_assistant
        {
            self.conversation
                .push("assistant", runner.task.last_response.clone());
        }
        // A spent repair budget parks the task for guidance. The runner keeps
        // the rejection in its journal; the transcript needs the request the
        // human is being asked to answer, which the error alone does not carry.
        let parked = outcome.is_err() && parked_for_guidance(&runner.task);
        if parked {
            self.conversation
                .push("assistant", runner.task.last_response.clone());
        }
        *self.live.lock().unwrap() = LiveOutput::default();
        self.status = if interrupted {
            "Interrupted. /continue resumes; follow-up text replans.".into()
        } else if parked {
            "Guidance needed".into()
        } else {
            format!("{:?}", runner.task.phase)
        };
        self.runner = Some(runner);
        self.active_snapshot = None;
        self.save_conversation()?;
        if interrupted {
            self.auto = false;
        }
        self.publish(false);
        if !parked {
            outcome?;
        }
        Ok(!quit)
    }
    fn progress_event(&mut self, event: ProgressEvent) {
        let mut live = self.live.lock().unwrap();
        match &event {
            ProgressEvent::Status(value) => self.status.clone_from(value),
            ProgressEvent::AssistantDelta(value) => live.assistant.push_str(value),
            ProgressEvent::CommandOutput(value) => {
                live.command.push_str(value);
                if live.command.len() > 32_000 {
                    let mut start = live.command.len() - 32_000;
                    while !live.command.is_char_boundary(start) {
                        start += 1;
                    }
                    live.command.drain(..start);
                }
            }
        }
        drop(live);
        let _ = self.output.send(Update::Progress(event));
    }
    async fn input(&mut self, text: String) -> Result<()> {
        let mut text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        let submitted_text = text.clone();
        if !text.starts_with('/')
            && self
                .runner
                .as_ref()
                .is_some_and(|runner| is_spec_approval_alias(&runner.task.phase, &text))
        {
            text = "/approve-spec".into();
        }
        if !text.starts_with('/') {
            anyhow::ensure!(
                !self.startup.needs_initialization(),
                "Use /init to initialize this project before sending work."
            );
            self.acquire()?;
            self.conversation.enqueue(text)?;
            self.auto = true;
            return Ok(());
        }
        if !self.startup.needs_initialization() {
            self.conversation.push("control", submitted_text);
            self.save_conversation()?;
        }
        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap();
        match command {
            "/help" => self.conversation.push("system", HELP),
            "/connect" => self.connect().await?,
            "/init" => {
                let result = self.startup.initialize().await?;
                self.conversation.push("system", result);
                self.connect().await?;
            }
            "/new" => {
                self.save_conversation()?;
                let old = self.conversation.id.clone();
                self.runner = None;
                self.lease = None;
                self.conversation = Conversation::new(self.conversation.root.clone());
                self.acquire()?;
                self.conversation.push("system", format!("New conversation. Previous conversation {old} and all its obligations are saved; /resume {old} returns to it."));
                self.save_conversation()?;
                self.status = "Ready".into();
            }
            "/resume" => {
                let target = match parts.next() {
                    Some("last") => Some(
                        Conversation::last_unfinished(&self.conversation.root)?
                            .map(|summary| summary.id)
                            .context("No saved conversation has unfinished work.")?,
                    ),
                    Some(id) => Some(id.to_owned()),
                    None => None,
                };
                if let Some(id) = target {
                    anyhow::ensure!(
                        !self.startup.needs_initialization(),
                        "This project has no initialized conversation storage."
                    );
                    anyhow::ensure!(
                        id != self.conversation.id,
                        "This conversation is already open."
                    );
                    let lease = Conversation::lease(&self.conversation.root, &id)?;
                    let conversation = Conversation::load(&self.conversation.root, &id)?;
                    self.save_conversation()?;
                    self.runner = None;
                    self.lease = Some(lease);
                    self.conversation = conversation;
                    self.connect().await?;
                    self.auto = false; // Resumption never implies fresh approval or command replay.
                } else {
                    let listing: Vec<String> = Conversation::summaries(&self.conversation.root)?
                        .iter()
                        .map(|summary| {
                            let marker = if summary.id == self.conversation.id {
                                " (open)"
                            } else {
                                ""
                            };
                            format!("{}{marker}", summary.describe())
                        })
                        .collect();
                    self.conversation.push(
                        "system",
                        format!(
                            "Saved conversations, newest first:\n{}\nUse /resume <id>, or /resume last for the newest with unfinished work.",
                            listing.join("\n")
                        ),
                    );
                }
            }
            "/model" => {
                let arguments: Vec<_> = parts.collect();
                if arguments.is_empty() {
                    self.discover_models(self.provider.config.model.is_empty())
                        .await?;
                } else {
                    // A leading role word scopes the choice; alone it is a model ID.
                    let (role, arguments) = match arguments.as_slice() {
                        [role, rest @ ..] if !rest.is_empty() => match ModelRole::parse(role) {
                            Some(role) => (Some(role), rest),
                            None => (None, arguments.as_slice()),
                        },
                        _ => (None, arguments.as_slice()),
                    };
                    let (endpoint, model) = match arguments {
                        [model] => (None, *model),
                        [endpoint, model] => (Some(*endpoint), *model),
                        _ => bail!("Use /model [plan|implement] <id> or /model [plan|implement] <endpoint> <id>."),
                    };
                    anyhow::ensure!(
                        !self.startup.needs_initialization(),
                        "Use /init before saving a project model selection."
                    );
                    let model = if endpoint.is_none()
                        && !self.available_models.iter().any(|id| id == model)
                    {
                        if let Ok(index) = model.parse::<usize>() {
                            self.available_models.get(index.checked_sub(1).context("Model numbers start at 1.")?).context("No model with that number; use /model to list available models.")?.clone()
                        } else {
                            model.to_string()
                        }
                    } else {
                        model.to_string()
                    };
                    self.select_model(role, endpoint, &model)?;
                    if endpoint.is_some() && role.is_none() {
                        self.available_models.clear();
                    }
                    self.status = format!("Selected {}", self.provider.describe());
                    self.auto = !self.conversation.queued.is_empty();
                }
            }
            "/approve" | "/approve-spec" | "/deny" | "/permissions" | "/revoke-permission"
            | "/accept" | "/reject" | "/no-knowledge" | "/plan" | "/review" | "/continue" => {
                // A spec approval is planning work in its own right: it needs
                // no prior description of work, so the spec names the task.
                if command == "/approve-spec" {
                    if let Some(path) = parts.clone().next() {
                        let finished = self
                            .runner
                            .as_ref()
                            .is_some_and(|runner| runner.task.phase == Phase::Complete);
                        if self.runner.is_none() || finished {
                            self.acquire()?;
                            self.start_task(spec_approval_objective(path)).await?;
                            self.save_conversation()?;
                        }
                    }
                }
                let runner = self
                    .runner
                    .as_mut()
                    .context("No active task. Describe work first.")?;
                match command {
                    "/approve" => match runner.task.phase {
                        Phase::AwaitingPlan => runner.approve_plan().await?,
                        Phase::AwaitingPolicy => runner.approve_policy().await?,
                        Phase::AwaitingPermission => runner.approve_permission().await?,
                        _ => bail!("There is no plan, edit, or permission approval pending."),
                    },
                    "/deny" => {
                        anyhow::ensure!(
                            runner.task.phase == Phase::AwaitingPermission,
                            "There is no permission request pending."
                        );
                        runner.deny_permission()?;
                    }
                    "/permissions" => {
                        self.conversation.push(
                            "system",
                            format_permission_grants(
                                &runner.task.permission_grants,
                                runner.standing_read_paths(),
                            ),
                        );
                    }
                    "/revoke-permission" => {
                        let id = parts.next().context(
                            "Use /revoke-permission <grant ID>; /permissions lists grants.",
                        )?;
                        anyhow::ensure!(
                            parts.next().is_none(),
                            "Use /revoke-permission <grant ID>."
                        );
                        runner.revoke_permission(id)?;
                    }
                    "/approve-spec" => match parts.next() {
                        None => {
                            anyhow::ensure!(
                                runner.task.phase == Phase::AwaitingSpecApproval,
                                "There is no spec approval pending. Use /approve-spec <path> [covered paths] first."
                            );
                            runner.approve_spec().await?;
                        }
                        Some(path) => {
                            let covers: Vec<String> = parts.map(str::to_string).collect();
                            runner.begin_spec_approval(path, &covers).await?;
                        }
                    },
                    "/accept" | "/reject" => {
                        let accept = command == "/accept";
                        if let Some(id) = parts.next() {
                            let id = if let Ok(index) = id.parse::<usize>() {
                                runner
                                    .task
                                    .reviews
                                    .get(
                                        index
                                            .checked_sub(1)
                                            .context("Review numbers start at 1.")?,
                                    )
                                    .context("No review with that number.")?
                                    .request
                                    .operation_id
                                    .clone()
                            } else {
                                id.to_string()
                            };
                            runner.review_operation(&id, accept).await?;
                        } else if !runner.task.reviews.is_empty() {
                            let ids: Vec<_> = runner
                                .task
                                .reviews
                                .iter()
                                .map(|r| r.request.operation_id.clone())
                                .collect();
                            for id in ids {
                                runner.review_operation(&id, accept).await?;
                            }
                        } else {
                            runner.review(accept).await?;
                        }
                    }
                    "/no-knowledge" => runner.confirm_no_knowledge().await?,
                    "/plan" => runner.mode_plan().await?,
                    "/review" => runner.request_review()?,
                    "/continue" => match runner.task.phase {
                        Phase::Cancelled => runner.resume().await?,
                        Phase::Planning | Phase::Working | Phase::Verifying => {}
                        // The repair budget is not re-armed here: only new
                        // guidance permits a fresh cycle. Say what is waited on.
                        Phase::AwaitingInput => bail!(
                            "Guidance is needed before the task can continue: {}",
                            runner.task.last_response
                        ),
                        Phase::AwaitingPlan => bail!("Nothing is interrupted; /approve the displayed plan or send feedback."),
                        Phase::AwaitingSpecApproval => bail!("Nothing is interrupted; /approve-spec accepts the displayed spec preview, or send feedback."),
                        Phase::AwaitingPolicy | Phase::AwaitingPermission => bail!("Nothing is interrupted; /approve or /deny the displayed request."),
                        Phase::AwaitingReview => bail!("Nothing is interrupted; /accept, /reject or /no-knowledge resolves the displayed review."),
                        Phase::Complete => bail!("Task complete. Describe the next request to continue this conversation."),
                    },
                    _ => unreachable!(),
                }
                self.conversation.sync_task(&runner.task);
                self.save_conversation()?;
                self.auto = !matches!(command, "/review" | "/permissions" | "/revoke-permission");
            }
            "/quit" => {
                self.quitting = true;
            }
            _ => bail!("Unknown command {command}. Use /help."),
        }
        Ok(())
    }
}

fn is_spec_approval_alias(phase: &Phase, value: &str) -> bool {
    if *phase != Phase::AwaitingSpecApproval {
        return false;
    }
    let value = value.trim().trim_end_matches(['.', '!']).trim_end();
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "i approve the spec" | "approve the spec"
    )
}

fn format_permission_grants(grants: &[PermissionGrant], standing: &[String]) -> String {
    // Standing paths are listed first and always: they are permanent, granted
    // without a gate, and saying "no grants" while they are in force would be
    // the most misleading thing this view could do.
    let mut text = String::new();
    if !standing.is_empty() {
        text.push_str(&format!(
            "Standing read access from [harness.sandbox] in {} (every task, no approval):\n  {}\n\n",
            crate::config::FILE_NAME,
            standing.join("\n  ")
        ));
    }
    if grants.is_empty() {
        text.push_str("No active task-scoped permission grants.");
        return text;
    }
    text.push_str("Active task-scoped permission grants:\n");
    for grant in grants {
        text.push_str(&format!(
            "\n{} · {}\n  approved: {}\n",
            grant.id,
            grant.justification.replace('\n', " "),
            grant.approved_at
        ));
        text.push_str(&format!(
            "  read: {}\n",
            if grant.read_paths.is_empty() {
                "none".into()
            } else {
                grant.read_paths.join(", ")
            }
        ));
        text.push_str(&format!(
            "  write: {}\n",
            if grant.write_paths.is_empty() {
                "none".into()
            } else {
                grant.write_paths.join(", ")
            }
        ));
        text.push_str(&format!(
            "  network: {}\n",
            if grant.network { "enabled" } else { "disabled" }
        ));
    }
    text.push_str("\nUse /revoke-permission <grant ID> to revoke one.");
    text
}

fn assistant_suffix(
    interrupted: bool,
    failed: bool,
    request: Option<&serde_json::Value>,
) -> &'static str {
    if interrupted {
        return "\n[interrupted]";
    }
    if !failed {
        return "";
    }
    if request.is_some_and(|request| request["purpose"] == "harness_capture_note") {
        return "\n[capture assessment failed; see the session error]";
    }
    let complete = request.is_some_and(|request| {
        request["interrupted"] != true
            && request["response"]
                .as_str()
                .is_some_and(|text| serde_json::from_str::<serde_json::Value>(text).is_ok())
    });
    if complete {
        "\n[step failed; see the session error]"
    } else {
        "\n[incomplete model response; see the session error]"
    }
}

pub const HELP: &str = "Describe work or ask about the project. Plan approval is required before changes.\n/approve — approve the displayed plan, exact edit, or permission request\n/approve-spec <path> [covered paths] — preview a repository spec for graph approval, anchored to the component covering those paths (dir/, file, or .); repeat without a path to accept\n/deny — deny the displayed permission request\n/permissions · /revoke-permission <grant ID> — inspect or revoke task-scoped access\n/review — review accumulated knowledge\n/accept [operation] · /reject [operation] — review one operation, or all displayed operations\n/no-knowledge — confirm the consolidated no-change assessment\n/plan — return to planning · /continue — resume interrupted work\n/new · /resume [conversation ID | last] — list saved conversations, or reopen one · /model [endpoint] [model ID]\n/connect — reconnect · /init — initialize this project · /expand — toggle activity · /help · /quit\nEnter submits · Ctrl-J inserts a newline · Alt-Enter and Shift-Enter are terminal-dependent aliases · Esc/Ctrl-C interrupts · Ctrl-D quits when the composer is empty · Ctrl-A/E moves to line start/end · Ctrl-U clears input · Tab switches views · Mouse wheel, PageUp/PageDown, and Alt-Up/Down scroll · Dragging selects text and copies it on release.";

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("moosedev-session-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn permission_grant_summary_includes_ids_and_capabilities() {
        let grants = vec![serde_json::from_value(serde_json::json!({
            "id": "grant-1",
            "justification": "Use the system toolchain",
            "read_paths": ["/opt/toolchain"],
            "write_paths": ["/tmp/tool-cache"],
            "network": true,
            "approved_at": "2026-09-21T00:00:00Z"
        }))
        .unwrap()];
        let text = format_permission_grants(&grants, &[]);
        assert!(text.contains("grant-1 · Use the system toolchain"));
        assert!(text.contains("approved: 2026-09-21T00:00:00Z"));
        assert!(text.contains("read: /opt/toolchain"));
        assert!(text.contains("write: /tmp/tool-cache"));
        assert!(text.contains("network: enabled"));
        assert!(text.contains("/revoke-permission <grant ID>"));
        assert_eq!(
            format_permission_grants(&[], &[]),
            "No active task-scoped permission grants."
        );
    }
    #[test]
    fn transcript_queue_and_task_links_survive_reload() {
        let temporary = Fixture::new();
        let root = temporary.path().canonicalize().unwrap();
        let mut conversation = Conversation::new(root.clone());
        conversation.active_task = Some("task-one".into());
        conversation.tasks.push("task-one".into());
        conversation
            .enqueue("Please explain λ\nthen fix it".into())
            .unwrap();
        let restored = Conversation::load(&root, &conversation.id).unwrap();
        assert_eq!(restored.queued[0].text, "Please explain λ\nthen fix it");
        assert!(!restored.queued[0].id.is_empty());
        assert_eq!(restored.messages[0].task.as_deref(), Some("task-one"));
        assert_eq!(Conversation::list(&root).unwrap(), [conversation.id]);
    }
    #[test]
    fn summaries_rank_newest_first_and_pick_resumable_work() {
        let temporary = Fixture::new();
        let root = temporary.path().canonicalize().unwrap();
        let tasks = root.join(".moosedev/harness/tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        let journal = |phase: &str, schema: u32| serde_json::json!({"schema": schema, "phase": phase, "objective": "Finish the skeleton\nsecond line", "other": {"ignored": true}});
        // Oldest: complete work.
        let mut done = Conversation::new(root.clone());
        done.push("user", "Explain the layout");
        done.active_task = Some("11111111-1111-4111-8111-111111111111".into());
        std::fs::write(
            tasks.join("11111111-1111-4111-8111-111111111111.json"),
            journal("Complete", super::super::runner::SCHEMA).to_string(),
        )
        .unwrap();
        done.save().unwrap();
        // A cancelled task from this build: resumable.
        let mut cancelled = Conversation::new(root.clone());
        cancelled.push("user", "Finish the skeleton");
        cancelled.active_task = Some("22222222-2222-4222-8222-222222222222".into());
        std::fs::write(
            tasks.join("22222222-2222-4222-8222-222222222222.json"),
            journal("Cancelled", super::super::runner::SCHEMA).to_string(),
        )
        .unwrap();
        cancelled.save().unwrap();
        // Newest: an old-schema journal this build refuses.
        let mut stale = Conversation::new(root.clone());
        stale.push("user", "Old work");
        stale.active_task = Some("33333333-3333-4333-8333-333333333333".into());
        std::fs::write(
            tasks.join("33333333-3333-4333-8333-333333333333.json"),
            journal("Working", 1).to_string(),
        )
        .unwrap();
        stale.save().unwrap();
        // Stamp the three in the past, oldest first, so ordering does not
        // depend on save timing.
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        for (id, offset) in [(&done.id, 20u64), (&cancelled.id, 10), (&stale.id, 0)] {
            let path = root
                .join(".moosedev/harness/conversations")
                .join(format!("{id}.json"));
            std::fs::File::open(&path)
                .unwrap()
                .set_modified(base - std::time::Duration::from_secs(offset))
                .unwrap();
        }

        let summaries = Conversation::summaries(&root).unwrap();
        let ids: Vec<_> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [stale.id.as_str(), cancelled.id.as_str(), done.id.as_str()]
        );
        assert!(summaries[0]
            .describe()
            .contains("schema 1 is not resumable by this build"));
        assert!(summaries[1]
            .describe()
            .contains("Finish the skeleton · Cancelled"));
        assert!(!summaries[1].describe().contains("second line"));
        assert!(summaries[2].describe().contains("Complete"));
        assert_eq!(
            Conversation::last_unfinished(&root).unwrap().map(|s| s.id),
            Some(cancelled.id.clone())
        );

        let empty = Conversation::new(root.clone());
        empty.save().unwrap();
        let summaries = Conversation::summaries(&root).unwrap();
        assert!(summaries[0].describe().ends_with("(empty) · no task"));
        assert!(!summaries[0].unfinished());
    }
    #[test]
    fn prompt_is_bounded_without_truncating_durable_transcript() {
        let mut conversation = Conversation::new(PathBuf::from("/unused"));
        for n in 0..100 {
            conversation.push("assistant", format!("{n} {}", "λ".repeat(200)));
        }
        assert!(conversation.context().len() <= 12_000);
        assert_eq!(conversation.messages.len(), 100);
        assert!(conversation.context().contains("99 "));
    }
    #[test]
    fn knowledge_events_stay_out_of_the_conversation_transcript() {
        assert_eq!(transcript_role(3, &[4]), Some("activity"));
        assert_eq!(transcript_role(4, &[4]), None);
        assert_eq!(transcript_role(0, &[]), Some("activity"));
    }
    #[test]
    fn knowledge_search_results_are_not_promoted_to_assistant_turns() {
        assert!(!should_append_last_response(
            "Full accepted graph context",
            "",
            Phase::Planning,
            true,
            false,
            true,
        ));
        assert!(should_append_last_response(
            "A user-facing answer",
            "",
            Phase::AwaitingInput,
            true,
            false,
            false,
        ));
    }
    #[test]
    fn pending_and_current_guidance_are_not_repeated_in_history() {
        let root = Fixture::new();
        let mut conversation = Conversation::new(root.path().to_path_buf());
        conversation.active_task = Some("task".into());
        conversation.push("user", "Earlier question");
        conversation.push("assistant", "Earlier answer");
        conversation.enqueue("Latest guidance".into()).unwrap();
        assert!(!conversation.context().contains("Latest guidance"));
        conversation.queued.clear();
        let context = conversation.context();
        assert!(!context.contains("Latest guidance"));
        assert!(context.contains("Earlier question"));
        assert!(context.contains("Earlier answer"));
    }
    #[test]
    fn acquire_preserves_configuration_notice_after_reloading_the_journal() {
        let root = Fixture::new();
        let conversation = Conversation::new(root.path().to_path_buf());
        conversation.save().unwrap();
        let options = StartupOptions {
            root: root.path().to_path_buf(),
            daemon: None,
            daemon_exe: None,
        };
        let (_, input) = mpsc::unbounded_channel();
        let (output, _) = mpsc::unbounded_channel();
        let mut controller = Controller::new(
            conversation,
            None,
            options,
            ProviderSettings::fallback(),
            input,
            output,
        )
        .with_startup_notice(Some("Model configuration failed: invalid endpoint".into()));
        controller.acquire().unwrap();
        controller.save_conversation().unwrap();
        let saved = Conversation::load(root.path(), &controller.conversation.id).unwrap();
        assert!(saved
            .messages
            .iter()
            .any(|m| m.text.contains("Model configuration failed")));
        controller.acquire().unwrap();
        assert_eq!(controller.conversation.messages.len(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn session_storage_rejects_symlink_ancestors() {
        let root = Fixture::new();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".moosedev")).unwrap();
        let conversation = Conversation::new(root.path().to_path_buf());
        assert!(conversation.save().is_err());
        assert!(!outside.path().join("harness").exists());
    }
    #[test]
    fn conversation_lease_prevents_concurrent_resume() {
        let root = Fixture::new();
        let id = uuid::Uuid::new_v4().to_string();
        let lease = Conversation::lease(root.path(), &id).unwrap();
        assert!(Conversation::lease(root.path(), &id).is_err());
        drop(lease);
        assert!(Conversation::lease(root.path(), &id).is_ok());
    }
    #[test]
    fn rejected_input_does_not_enter_the_durable_queue() {
        let root = Fixture::new();
        let mut conversation = Conversation::new(root.path().to_path_buf());
        assert!(conversation.enqueue("x".repeat(16_001)).is_err());
        assert!(conversation.queued.is_empty());
        assert!(conversation.messages.is_empty());
    }
    #[test]
    fn failed_steps_do_not_label_complete_model_responses_as_incomplete() {
        let complete = serde_json::json!({"response":"{\"action\":\"plan\"}"});
        assert!(assistant_suffix(false, true, Some(&complete)).contains("step failed"));
        assert!(!assistant_suffix(false, true, Some(&complete)).contains("incomplete"));
        let partial = serde_json::json!({"response":"{\"message\":\"working","interrupted":true});
        assert!(assistant_suffix(false, true, Some(&partial)).contains("incomplete model response"));
        assert_eq!(
            assistant_suffix(true, true, Some(&complete)),
            "\n[interrupted]"
        );
        let capture = serde_json::json!({"purpose":"harness_capture_note","response":null});
        assert!(assistant_suffix(false, true, Some(&capture)).contains("capture assessment failed"));
    }
    #[test]
    fn spec_approval_phrases_are_narrow_and_normalized() {
        let gate = Phase::AwaitingSpecApproval;
        assert!(is_spec_approval_alias(&gate, "I APPROVE   the spec!"));
        assert!(is_spec_approval_alias(&gate, " approve the spec. "));
        assert!(!is_spec_approval_alias(&gate, "I approve this spec"));
        assert!(!is_spec_approval_alias(
            &gate,
            "I approve the spec and the plan"
        ));
        assert!(!is_spec_approval_alias(&gate, "approve the spec?"));
        assert!(!is_spec_approval_alias(
            &Phase::AwaitingPlan,
            "I approve the spec"
        ));
    }
    #[tokio::test]
    async fn quitting_uninitialized_project_does_not_initialize_it() {
        let root = Fixture::new();
        let conversation = Conversation::new(root.path().to_path_buf());
        let options = StartupOptions {
            root: root.path().to_path_buf(),
            daemon: None,
            daemon_exe: None,
        };
        let (input, input_rx) = mpsc::unbounded_channel();
        let (output, _output_rx) = mpsc::unbounded_channel();
        input.send(Command::Quit).unwrap();
        Controller::new(
            conversation,
            None,
            options,
            ProviderSettings::fallback(),
            input_rx,
            output,
        )
        .run()
        .await;
        assert!(!root.path().join(".moosedev").exists());
    }
}
