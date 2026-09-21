//! JSONL view of the real conversational controller for reproducible studies.
//! Review decisions are input supplied by the study driver, never this adapter.
use anyhow::{ensure, Context, Result};
use moosedev::{
    harness::{
        progress::Progress,
        session::{Command, Controller, Conversation, Update},
        startup::{resolve_daemon_executable, ProviderSettings, StartupOptions},
    },
    llm::LlmConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::BufRead, io::Write, path::PathBuf};
use tokio::sync::mpsc;

const HELP: &str =
    "harness_study_session --project PATH --daemon URL --daemon-exe PATH --model ID --endpoint URL

All five arguments are required. The project and study-owned daemon must already
be initialized and running. No daemon discovery, autospawn, remembered provider,
moosedev.toml, dotenv loading, or automatic model selection is performed by this
adapter: one frozen model answers every role.
The driver must validate repository target origins and freeze binary hashes.

stdin JSONL: {\"type\":\"input\",\"text\":\"...\"}, {\"type\":\"interrupt\"}, {\"type\":\"quit\"}
EOF requests a graceful quit. Ordinary slash commands use the input message.
stdout JSONL: state, progress, restore_input, closed. State retains the complete
native task and conversation, including model requests. Controller-level errors
remain native state observations; adapter failures emit JSON on stderr and exit 1.

The driver supplies a controlled environment. MOOSEDEV_LLM_API_KEY,
MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS, and MOOSEDEV_LLM_STRUCTURED_OUTPUT configure
the provider; explicit --model and --endpoint always override environment values.
MOOSEDEV_DATA_DIR and executor settings retain their ordinary harness semantics.
";

fn explicit_arguments(
    args: impl IntoIterator<Item = String>,
    allowed: &[&str],
) -> Result<BTreeMap<String, String>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        ensure!(allowed.contains(&key.as_str()), "unknown argument: {key}");
        let value = args
            .next()
            .with_context(|| format!("{key} needs a value"))?;
        ensure!(
            !value.trim().is_empty() && !value.starts_with("--"),
            "{key} needs a nonempty value"
        );
        ensure!(
            values.insert(key.clone(), value).is_none(),
            "duplicate {key}"
        );
    }
    for key in allowed {
        ensure!(values.contains_key(*key), "missing {key}");
    }
    Ok(values)
}

fn provider_settings(model: &str, endpoint: &str) -> Result<ProviderSettings> {
    let mut provider = ProviderSettings {
        response_policy: moosedev::harness::response::ResponsePolicy::from_env()?
            .unwrap_or_default(),
        config: LlmConfig::from_env()?,
        // One frozen model for every role; moosedev.toml is never read here.
        action_contract: None,
        plan: None,
        implement: None,
    };
    provider.select(Some(endpoint), model)?;
    Ok(provider)
}

struct Options {
    project: PathBuf,
    daemon: String,
    daemon_exe: PathBuf,
    model: String,
    endpoint: String,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>> {
        let mut args = args.into_iter().peekable();
        if args
            .peek()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
        {
            args.next();
            ensure!(args.next().is_none(), "--help takes no other arguments");
            return Ok(None);
        }
        let mut values = explicit_arguments(
            args,
            &[
                "--project",
                "--daemon",
                "--daemon-exe",
                "--model",
                "--endpoint",
            ],
        )?;
        let mut required = |key: &str| values.remove(key).with_context(|| format!("missing {key}"));
        Ok(Some(Self {
            project: required("--project")?.into(),
            daemon: required("--daemon")?,
            daemon_exe: required("--daemon-exe")?.into(),
            model: required("--model")?,
            endpoint: required("--endpoint")?,
        }))
    }

    fn startup(&self) -> Result<StartupOptions> {
        ensure!(self.project.is_absolute(), "--project must be absolute");
        ensure!(
            self.daemon_exe.is_absolute(),
            "--daemon-exe must be absolute"
        );
        let root = self.project.canonicalize().context("resolve --project")?;
        ensure!(root.is_dir(), "--project must be a directory");
        let startup = StartupOptions {
            root,
            daemon: Some(self.daemon.clone()),
            daemon_exe: Some(resolve_daemon_executable(Some(&self.daemon_exe))?),
        };
        ensure!(
            !startup.needs_initialization(),
            "study project must already be initialized"
        );
        Ok(startup)
    }

    fn provider(&self) -> Result<ProviderSettings> {
        provider_settings(&self.model, &self.endpoint)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessage {
    Input { text: String },
    Interrupt {},
    Quit {},
}

impl From<ClientMessage> for Command {
    fn from(value: ClientMessage) -> Self {
        match value {
            ClientMessage::Input { text } => Self::Input(text),
            ClientMessage::Interrupt {} => Self::Interrupt,
            ClientMessage::Quit {} => Self::Quit,
        }
    }
}

fn update_json(update: Update) -> Result<Value> {
    Ok(match update {
        Update::State(snapshot) => {
            let live = snapshot
                .live
                .lock()
                .map_err(|_| anyhow::anyhow!("live output lock poisoned"))?;
            json!({
                "type": "state", "busy": snapshot.busy, "status": snapshot.status,
                "task": snapshot.task, "conversation": snapshot.conversation,
                "model": snapshot.model, "endpoint": snapshot.endpoint,
                "live": { "assistant": live.assistant, "command": live.command },
            })
        }
        Update::Progress(progress) => {
            let (kind, text) = match progress {
                Progress::Status(text) => ("status", text),
                Progress::AssistantDelta(text) => ("assistant_delta", text),
                Progress::CommandOutput(text) => ("command_output", text),
            };
            json!({ "type": "progress", "kind": kind, "text": text })
        }
        Update::RestoreInput(text) => json!({ "type": "restore_input", "text": text }),
        Update::Closed => json!({ "type": "closed" }),
    })
}

fn emit(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_input(reader: impl BufRead, sender: mpsc::UnboundedSender<Result<ClientMessage>>) {
    for line in reader.lines() {
        let input = line.context("read JSONL input").and_then(|line| {
            serde_json::from_str::<ClientMessage>(&line).context("invalid JSONL input")
        });
        let stop = matches!(&input, Ok(ClientMessage::Quit {}) | Err(_));
        if sender.send(input).is_err() || stop {
            return;
        }
    }
    let _ = sender.send(Ok(ClientMessage::Quit {}));
}

async fn run(options: Options) -> Result<()> {
    let startup = options.startup()?;
    let provider = options.provider()?;
    let conversation = Conversation::new(startup.root.clone());
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (updates, mut update_rx) = mpsc::unbounded_channel();
    let controller = Controller::new(conversation, None, startup, provider, command_rx, updates);
    let controller = tokio::spawn(controller.run());
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    // Tokio's stdin worker cannot be cancelled and can hold runtime shutdown open
    // after an explicit quit. A detached reader exits with the process instead.
    std::thread::spawn(move || read_input(std::io::stdin().lock(), input_tx));
    let mut stdout = std::io::stdout().lock();
    let mut input_open = true;
    let mut input_error = None;
    let mut closed = false;
    loop {
        tokio::select! {
            input = input_rx.recv(), if input_open => {
                let command = match input {
                    Some(Ok(input)) => input.into(),
                    Some(Err(error)) => {
                        input_error = Some(error);
                        Command::Quit
                    }
                    None => Command::Quit,
                };
                if matches!(command, Command::Quit) {
                    input_open = false;
                }
                // If the controller has already closed, drain its final updates.
                let _ = commands.send(command);
            }
            update = update_rx.recv() => {
                let Some(update) = update else { break };
                closed = matches!(update, Update::Closed);
                emit(&mut stdout, &update_json(update)?)?;
                if closed { break; }
            }
        }
    }
    controller.await.context("session controller panicked")?;
    if let Some(error) = input_error {
        return Err(error);
    }
    ensure!(closed, "session controller ended without a closed event");
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match Options::parse(args) {
        Ok(Some(options)) => run(options).await,
        Ok(None) => {
            print!("{HELP}");
            Ok(())
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let _ = emit(
            &mut std::io::stderr(),
            &json!({"type": "error", "message": format!("{error:#}")}),
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments() -> Vec<String> {
        [
            "--project",
            "/fixture",
            "--daemon",
            "http://127.0.0.1:3000",
            "--daemon-exe",
            "/repo/target/release/moosedev",
            "--model",
            "exact/model",
            "--endpoint",
            "http://127.0.0.1:1234/v1",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn requires_explicit_nonduplicated_selection() {
        let options = Options::parse(arguments()).unwrap().unwrap();
        assert_eq!(options.model, "exact/model");
        assert_eq!(
            options.daemon_exe,
            PathBuf::from("/repo/target/release/moosedev")
        );
        for index in (0..10).step_by(2) {
            let mut args = arguments();
            args.drain(index..index + 2);
            assert!(Options::parse(args).is_err());
        }
        let mut duplicate = arguments();
        duplicate.extend(["--model".into(), "replacement".into()]);
        assert!(Options::parse(duplicate).is_err());
        assert!(Options::parse(["--help".into()]).unwrap().is_none());
    }

    #[test]
    fn neutral_probe_requires_exact_provider_without_project_options() {
        let args = [
            "--model",
            "exact/model",
            "--endpoint",
            "http://127.0.0.1:1234/v1",
        ];
        let parsed =
            explicit_arguments(args.map(str::to_owned), &["--model", "--endpoint"]).unwrap();
        assert_eq!(parsed["--model"], "exact/model");
        for invalid in [
            vec!["--model", "x"],
            vec!["--model", "x", "--model", "y"],
            vec!["--project", "/tmp"],
        ] {
            assert!(explicit_arguments(
                invalid.into_iter().map(str::to_owned),
                &["--model", "--endpoint"]
            )
            .is_err());
        }
    }

    #[test]
    fn eof_quits_and_malformed_input_is_not_dispatched() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        read_input(
            b"{\"type\":\"input\",\"text\":\"/approve\"}\n".as_slice(),
            sender,
        );
        assert!(
            matches!(receiver.try_recv().unwrap().unwrap().into(), Command::Input(text) if text == "/approve")
        );
        assert!(matches!(
            receiver.try_recv().unwrap().unwrap(),
            ClientMessage::Quit {}
        ));
        for malformed in [
            "{\"type\":\"accept\"}",
            "{\"type\":\"input\"}",
            "{\"type\":\"quit\",\"approve\":true}",
        ] {
            let (sender, mut receiver) = mpsc::unbounded_channel();
            read_input(malformed.as_bytes(), sender);
            assert!(receiver.try_recv().unwrap().is_err());
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn native_output_retains_unicode_and_command_boundaries() {
        let value = update_json(Update::Progress(Progress::CommandOutput("α\nβ".into()))).unwrap();
        let mut bytes = Vec::new();
        emit(&mut bytes, &value).unwrap();
        assert_eq!(bytes.iter().filter(|&&byte| byte == b'\n').count(), 1);
        let decoded: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded["kind"], "command_output");
        assert_eq!(decoded["text"], "α\nβ");
        assert_eq!(
            update_json(Update::RestoreInput("/accept id".into())).unwrap()["text"],
            "/accept id"
        );
        assert_eq!(update_json(Update::Closed).unwrap()["type"], "closed");
    }

    #[test]
    fn state_serializes_the_native_conversation_without_a_transcript_projection() {
        use moosedev::harness::session::{LiveOutput, Snapshot};
        use std::sync::{Arc, Mutex};

        let mut conversation = Conversation::new("/fixture".into());
        conversation.push("system", "Recorded native failure");
        let expected = serde_json::to_value(&conversation).unwrap();
        let snapshot = Snapshot {
            conversation,
            task: None,
            status: "Ready".into(),
            busy: false,
            endpoint: "http://127.0.0.1:1234/v1".into(),
            model: "exact/model".into(),
            live: Arc::new(Mutex::new(LiveOutput {
                assistant: "Partial answer".into(),
                command: "Partial output".into(),
            })),
        };
        let state = update_json(Update::State(Box::new(snapshot))).unwrap();
        assert_eq!(state["conversation"], expected);
        assert!(state["task"].is_null());
        assert_eq!(state["model"], "exact/model");
        assert_eq!(state["live"]["assistant"], "Partial answer");
    }
}
