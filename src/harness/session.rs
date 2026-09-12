//! Durable conversation and human input, independent of the model's action protocol.
use super::{
    progress::{Progress as ProgressEvent, ProgressSender},
    runner::{Phase, Runner, Task},
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
        for event in task.events.iter().skip(seen) {
            self.push("activity", event.message.clone());
        }
        self.seen_events.insert(task.id.clone(), task.events.len());
    }
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
        let _ = self.output.send(Update::State(Box::new(Snapshot {
            conversation: self.conversation.clone(),
            task: self
                .runner
                .as_ref()
                .map(|r| r.task.clone())
                .or_else(|| self.active_snapshot.clone()),
            status: self.status.clone(),
            busy,
            endpoint: self.provider.config.base_url.clone(),
            model: self.provider.config.model.clone(),
            live: self.live.clone(),
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
    fn configure(&mut self) -> Result<()> {
        if let Some(runner) = &mut self.runner {
            runner.enable_interactive()?;
            runner.configure(self.provider.config.clone(), Some(self.progress.clone()));
            runner.set_response_policy(self.provider.response_policy);
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
            self.provider.select(None, &model)?;
            self.provider.save(&self.conversation.root)?;
            self.configure()?;
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
            self.conversation.push("system",format!("Models at {}:\n{listing}\nChoose with /model <number> or /model <model ID>. To use another endpoint: /model <endpoint> <model ID>.",self.provider.config.base_url));
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
                if self.auto
                    && self.runner.as_ref().is_some_and(|r| {
                        matches!(
                            r.task.phase,
                            Phase::Planning | Phase::Working | Phase::Verifying
                        )
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
        let daemon = self
            .daemon
            .clone()
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
            let runner = Runner::create(self.conversation.root.clone(), daemon, text).await?;
            self.conversation.active_task = Some(runner.task.id.clone());
            self.conversation.tasks.push(runner.task.id.clone());
            self.runner = Some(runner);
            self.configure()?;
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
                            if matches!(text.split_whitespace().next(), Some("/approve" | "/accept" | "/reject" | "/no-knowledge")) {
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
        let live_assistant = self.live.lock().unwrap().assistant.clone();
        if !live_assistant.is_empty() {
            let suffix = assistant_suffix(
                interrupted,
                outcome.is_err(),
                runner.task.model_requests.last(),
            );
            self.conversation
                .push("assistant", format!("{live_assistant}{suffix}"));
        } else if outcome.is_ok()
            && !interrupted
            && !runner.task.last_response.is_empty()
            && (runner.task.last_response != prior_response
                || runner.task.phase == Phase::AwaitingInput)
        {
            self.conversation
                .push("assistant", runner.task.last_response.clone());
        }
        // A conversational envelope can contain introductory prose while the
        // action contains the actual answer. Deduplicate within this step only:
        // the same answer to a later user question still deserves a visible turn.
        if outcome.is_ok()
            && !interrupted
            && runner.task.phase == Phase::AwaitingInput
            && !live_assistant.is_empty()
            && !runner.task.last_response.is_empty()
            && runner.task.last_response != live_assistant
        {
            self.conversation
                .push("assistant", runner.task.last_response.clone());
        }
        *self.live.lock().unwrap() = LiveOutput::default();
        self.status = if interrupted {
            "Interrupted. /continue resumes; follow-up text replans.".into()
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
        outcome?;
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
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
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
            self.conversation.push("control", text.clone());
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
                if let Some(id) = parts.next() {
                    anyhow::ensure!(
                        !self.startup.needs_initialization(),
                        "This project has no initialized conversation storage."
                    );
                    anyhow::ensure!(
                        id != self.conversation.id,
                        "This conversation is already open."
                    );
                    let lease = Conversation::lease(&self.conversation.root, id)?;
                    let conversation = Conversation::load(&self.conversation.root, id)?;
                    self.save_conversation()?;
                    self.runner = None;
                    self.lease = Some(lease);
                    self.conversation = conversation;
                    self.connect().await?;
                    self.auto = false; // Resumption never implies fresh approval or command replay.
                } else {
                    self.conversation.push(
                        "system",
                        format!(
                            "Saved conversations:\n{}\nUse /resume <id>.",
                            Conversation::list(&self.conversation.root)?.join("\n")
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
                    let (endpoint, model) = match arguments.as_slice() {
                        [model] => (None, *model),
                        [endpoint, model] => (Some(*endpoint), *model),
                        _ => bail!("Use /model <id> or /model <endpoint> <id>."),
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
                    self.provider.select(endpoint, &model)?;
                    if endpoint.is_some() {
                        self.available_models.clear();
                    }
                    self.provider.save(&self.conversation.root)?;
                    self.configure()?;
                    self.status = format!(
                        "Selected {} at {}",
                        self.provider.config.model, self.provider.config.base_url
                    );
                    self.auto = !self.conversation.queued.is_empty();
                }
            }
            "/approve" | "/accept" | "/reject" | "/no-knowledge" | "/plan" | "/review"
            | "/continue" => {
                let runner = self
                    .runner
                    .as_mut()
                    .context("No active task. Describe work first.")?;
                match command {
                    "/approve" => match runner.task.phase {
                        Phase::AwaitingPlan => runner.approve_plan().await?,
                        Phase::AwaitingPolicy => runner.approve_policy().await?,
                        _ => bail!("There is no plan or edit approval pending."),
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
                    "/continue" => {
                        if runner.task.phase == Phase::Cancelled {
                            runner.resume().await?;
                        }
                    }
                    _ => unreachable!(),
                }
                self.conversation.sync_task(&runner.task);
                self.save_conversation()?;
                self.auto = command != "/review";
            }
            "/quit" => {
                self.quitting = true;
            }
            _ => bail!("Unknown command {command}. Use /help."),
        }
        Ok(())
    }
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

pub const HELP: &str = "Describe work or ask about the project. Plan approval is required before changes.\n/approve — approve the displayed plan or exact edit\n/review — review accumulated knowledge\n/accept [operation] · /reject [operation] — review one operation, or all displayed operations\n/no-knowledge — confirm the consolidated no-change assessment\n/plan — return to planning · /continue — resume interrupted work\n/new · /resume [conversation ID] · /model [endpoint] [model ID]\n/connect — reconnect · /init — initialize this project · /expand — toggle activity · /help · /quit\nEnter submits · Alt-Enter inserts a newline · Esc/Ctrl-C interrupts · Ctrl-D quits when the composer is empty · Ctrl-A/E moves to line start/end · Ctrl-U clears input · Tab switches views · PageUp/PageDown and Alt-Up/Down scroll.";

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
