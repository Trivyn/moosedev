#![cfg(feature = "harness")]
//! Exercise the conversation controller through its human/event channels.
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use moosedev::harness::{
    protocol::{CheckpointResponse, ContextRequest, ContextResponse, FileContext},
    runner::Phase,
    session::{Command, Controller, Conversation, Snapshot, Update},
    startup::{ProviderSettings, StartupOptions},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify, Semaphore};

struct StateData {
    root: PathBuf,
    calls: AtomicUsize,
    probe_calls: AtomicUsize,
    started: Notify,
    release: Semaphore,
    prompts: Mutex<Vec<String>>,
    models: Mutex<Vec<String>>,
    replies: Mutex<VecDeque<Value>>,
}
type Shared = Arc<StateData>;
async fn models(State(state): State<Shared>) -> Json<Value> {
    Json(
        json!({"data":state.models.lock().unwrap().iter().map(|id|json!({"id":id})).collect::<Vec<_>>()}),
    )
}
async fn health(State(state): State<Shared>) -> Json<Value> {
    Json(
        json!({"status":"ok","project_graph":moosedev::graph::PROJECT_KG_GRAPH_IRI,"project_root":state.root,"data_dir":state.root.join(".moosedev")}),
    )
}
async fn context(
    State(state): State<Shared>,
    Json(request): Json<ContextRequest>,
) -> Json<ContextResponse> {
    Json(ContextResponse {
        capture_contracts: vec![2, 3],
        intent_contracts: vec![2],
        project_root: state.root.to_string_lossy().into_owned(),
        revision: "accepted-v1".into(),
        evidence_iris: vec![],
        governing_constraints: vec![],
        context: "Constraint: reading must precede work.".into(),
        files: request
            .files
            .into_iter()
            .map(|file| FileContext {
                file,
                dossier: "Preserve existing behavior.".into(),
                policy: moosedev::policy::PolicyDecision::Allow,
            })
            .collect(),
    })
}
async fn checkpoint() -> Json<CheckpointResponse> {
    Json(CheckpointResponse {
        conforms: true,
        durable: true,
        revision: "accepted-v1".into(),
        pending: vec![],
    })
}
/// A scripted action as one native tool call, or plain content when it is not one.
fn tool_message(answer: &Value) -> (Value, &'static str) {
    let action = answer
        .get("action")
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| {
            answer
                .get("action")
                .and_then(Value::as_str)
                .and(answer.as_object().cloned())
        });
    let Some(mut action) = action else {
        return (
            json!({"role":"assistant","content":answer.to_string()}),
            "stop",
        );
    };
    let message = answer.get("message").and_then(Value::as_str).unwrap_or("");
    let name = action.remove("action").unwrap_or(Value::Null);
    (
        json!({"role":"assistant","content":message,"tool_calls":[{
            "id":"call-0","type":"function",
            "function":{"name":name,"arguments":Value::Object(action).to_string()}
        }]}),
        "tool_calls",
    )
}

async fn model(State(state): State<Shared>, Json(request): Json<Value>) -> Json<Value> {
    let tools = request["tools"].as_array().cloned();
    let probe = tools
        .as_ref()
        .is_some_and(|tools| tools.iter().any(|tool| tool["function"]["name"] == "ready"));
    let schema = if probe {
        "harness_response_probe"
    } else {
        request["response_format"]["json_schema"]["name"]
            .as_str()
            .unwrap_or("")
    };
    let answer = if schema == "harness_response_probe" {
        state.probe_calls.fetch_add(1, Ordering::SeqCst);
        json!({"status":"ok"})
    } else if schema == "harness_capture_note" {
        json!({"note":"nothing beyond the diff"})
    } else {
        let count = state.calls.fetch_add(1, Ordering::SeqCst);
        state.prompts.lock().unwrap().push(request.to_string());
        if count == 0 {
            state.started.notify_one();
            state.release.acquire().await.unwrap().forget();
        }
        state.replies.lock().unwrap().pop_front().unwrap_or_else(||json!({"message":"I will explain the project.","action":{"action":"reply","message":format!("Actual answer {}.",count+1)}}))
    };
    if probe {
        return Json(
            json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{
            "id":"probe","type":"function","function":{"name":"ready","arguments":"{\"status\":\"ok\"}"}
        }]},"finish_reason":"tool_calls"}]}),
        );
    }
    if tools.is_some() {
        let (message, finish) = tool_message(&answer);
        return Json(json!({"choices":[{"message":message,"finish_reason":finish}]}));
    }
    Json(
        json!({"choices":[{"message":{"role":"assistant","content":answer.to_string()},"finish_reason":"stop"}]}),
    )
}
struct Fixture {
    root: PathBuf,
    url: String,
    state: Shared,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("moosedev-conversation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".moosedev")).unwrap();
        let root = root.canonicalize().unwrap();
        let state = Arc::new(StateData {
            root: root.clone(),
            calls: AtomicUsize::new(0),
            probe_calls: AtomicUsize::new(0),
            started: Notify::new(),
            release: Semaphore::new(0),
            prompts: Mutex::new(vec![]),
            models: Mutex::new(vec!["scripted-local-model".into()]),
            replies: Mutex::new(VecDeque::new()),
        });
        let app = Router::new()
            .route("/api/v1/health", get(health))
            .route("/api/v1/harness/context", post(context))
            .route("/api/v1/harness/checkpoint", post(checkpoint))
            .route("/v1/chat/completions", post(model))
            .route("/v1/models", get(models))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            root,
            url,
            state,
            server,
        }
    }
    fn controller(
        &self,
        conversation: Conversation,
    ) -> (
        mpsc::UnboundedSender<Command>,
        mpsc::UnboundedReceiver<Update>,
        tokio::task::JoinHandle<()>,
    ) {
        self.controller_configured(conversation, true)
    }
    fn controller_configured(
        &self,
        conversation: Conversation,
        configured: bool,
    ) -> (
        mpsc::UnboundedSender<Command>,
        mpsc::UnboundedReceiver<Update>,
        tokio::task::JoinHandle<()>,
    ) {
        let (input, input_rx) = mpsc::unbounded_channel();
        let (output, output_rx) = mpsc::unbounded_channel();
        let mut provider = ProviderSettings::fallback();
        provider
            .select(Some(&format!("{}/v1", self.url)), "scripted-local-model")
            .unwrap();
        if !configured {
            provider.config.model.clear();
        }
        let startup = StartupOptions {
            root: self.root.clone(),
            daemon: Some(self.url.clone()),
            daemon_exe: None,
        };
        let handle = tokio::spawn(
            Controller::new(conversation, None, startup, provider, input_rx, output).run(),
        );
        (input, output_rx, handle)
    }
}

#[tokio::test]
async fn startup_selects_a_single_model_and_offers_numbered_multiple_models() {
    let fixture = Fixture::new().await;
    let (input, mut updates, handle) =
        fixture.controller_configured(Conversation::new(fixture.root.clone()), false);
    let ready = until(&mut updates, |state| !state.busy).await;
    assert_eq!(ready.model, "scripted-local-model");
    assert!(ready
        .conversation
        .messages
        .iter()
        .any(|m| m.text.contains("Using scripted-local-model")));
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
    *fixture.state.models.lock().unwrap() = vec!["model-b".into(), "model-a".into()];
    let (input, mut updates, handle) =
        fixture.controller_configured(Conversation::new(fixture.root.clone()), false);
    let ready = until(&mut updates, |state| !state.busy).await;
    assert!(ready.model.is_empty());
    assert!(ready
        .conversation
        .messages
        .iter()
        .any(|m| m.text.contains("1. model-a\n2. model-b")));
    input.send(Command::Input("/model 2".into())).unwrap();
    let ready = until(&mut updates, |state| {
        !state.busy && state.model == "model-b"
    })
    .await;
    assert_eq!(ready.model, "model-b");
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn automatic_steps_refresh_conversation_and_repeated_answers_remain_visible() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("code.txt"), "original\n").unwrap();
    fixture.state.release.add_permits(1);
    *fixture.state.replies.lock().unwrap() = VecDeque::from([
        json!({"message":"First-step prose.","action":{"action":"read","file":"code.txt"}}),
        json!({"message":"I have read the file.","action":{"action":"reply","message":"Same answer."}}),
        json!({"message":"I have checked again.","action":{"action":"reply","message":"Same answer."}}),
    ]);
    let (input, mut updates, handle) = fixture.controller(Conversation::new(fixture.root.clone()));
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Read code.txt and explain it.".into()))
        .unwrap();
    until(&mut updates, |state| {
        !state.busy
            && state
                .conversation
                .messages
                .iter()
                .any(|message| message.role == "assistant" && message.text == "Same answer.")
    })
    .await;
    {
        let prompts = fixture.state.prompts.lock().unwrap();
        let request: Value = serde_json::from_str(&prompts[1]).unwrap();
        let prompt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let recent=prompt.split("Recent conversation (historical context; current human instructions and accepted knowledge govern):").nth(1).unwrap().split("Current human guidance:").next().unwrap();
        assert!(
            recent.contains("First-step prose."),
            "the next automatic action must receive the conversation from the preceding action"
        );
    }
    input
        .send(Command::Input("Please confirm the same answer.".into()))
        .unwrap();
    until(&mut updates, |state| {
        !state.busy
            && state
                .conversation
                .messages
                .iter()
                .filter(|message| message.role == "assistant" && message.text == "Same answer.")
                .count()
                == 2
    })
    .await;
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn until(
    updates: &mut mpsc::UnboundedReceiver<Update>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match updates.recv().await {
                Some(Update::State(state)) if predicate(&state) => return *state,
                Some(Update::Closed) | None => panic!("controller stopped early"),
                _ => {}
            }
        }
    })
    .await
    .expect("controller state timed out")
}

#[tokio::test]
async fn queued_steering_is_durable_and_delivered_before_the_next_action() {
    let fixture = Fixture::new().await;
    let conversation = Conversation::new(fixture.root.clone());
    let id = conversation.id.clone();
    let (input, mut updates, handle) = fixture.controller(conversation);
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Explain this project.".into()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.state.started.notified())
        .await
        .unwrap();
    input
        .send(Command::Input("Also explain the parser.".into()))
        .unwrap();
    until(&mut updates, |state| {
        state
            .conversation
            .queued
            .iter()
            .any(|item| item.text.contains("parser"))
    })
    .await;
    let saved = Conversation::load(&fixture.root, &id).unwrap();
    assert_eq!(saved.queued.len(), 1);
    assert!(saved.queued[0].text.contains("parser"));
    fixture.state.release.add_permits(1);
    let finished = until(&mut updates, |state| {
        !state.busy
            && state.conversation.queued.is_empty()
            && state
                .conversation
                .messages
                .iter()
                .any(|m| m.role == "assistant" && m.text == "Actual answer 2.")
    })
    .await;
    assert_eq!(finished.task.unwrap().phase, Phase::AwaitingInput);
    assert!(fixture.state.prompts.lock().unwrap()[1].contains("Also explain the parser."));
    {
        let prompts = fixture.state.prompts.lock().unwrap();
        let request: Value = serde_json::from_str(&prompts[1]).unwrap();
        let prompt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let recent = prompt.split("Recent conversation (historical context; current human instructions and accepted knowledge govern):").nth(1).unwrap_or("")
            .split("You are the coding sensor in MOOSEDev.").next().unwrap();
        assert!(
            !recent.contains("Also explain the parser."),
            "current guidance must not be repeated as history"
        );
    }
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 2);
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
    let saved = Conversation::load(&fixture.root, &id).unwrap();
    assert!(saved.queued.is_empty());
    assert_eq!(saved.tasks.len(), 1);
    assert!(saved
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .all(|m| m.task.as_ref() == saved.active_task.as_ref()));
}

#[tokio::test]
async fn interruption_and_resume_preserve_the_conversation_and_task() {
    let fixture = Fixture::new().await;
    let conversation = Conversation::new(fixture.root.clone());
    let id = conversation.id.clone();
    let (input, mut updates, handle) = fixture.controller(conversation);
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Explain this project.".into()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.state.started.notified())
        .await
        .unwrap();
    input.send(Command::Interrupt).unwrap();
    let paused = until(&mut updates, |state| {
        state
            .task
            .as_ref()
            .is_some_and(|task| task.phase == Phase::Cancelled)
    })
    .await;
    let task_id = paused.task.unwrap().id;
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
    let saved = Conversation::load(&fixture.root, &id).unwrap();
    assert_eq!(saved.active_task.as_deref(), Some(task_id.as_str()));
    let (input, mut updates, handle) = fixture.controller(saved);
    let resumed = until(&mut updates, |state| !state.busy && state.task.is_some()).await;
    assert_eq!(resumed.task.unwrap().phase, Phase::Cancelled);
    assert_eq!(
        fixture.state.calls.load(Ordering::SeqCst),
        1,
        "loading a session must not replay generation"
    );
    input
        .send(Command::Input("Explain only the parser.".into()))
        .unwrap();
    let finished = until(&mut updates, |state| {
        !state.busy && state.task.as_ref().is_some_and(|task| task.turn_finished)
    })
    .await;
    assert_eq!(finished.task.unwrap().id, task_id);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 2);
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn commands_during_a_step_are_retained_without_approving_an_unseen_gate() {
    let fixture = Fixture::new().await;
    let (input, mut updates, handle) = fixture.controller(Conversation::new(fixture.root.clone()));
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Explain this project.".into()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.state.started.notified())
        .await
        .unwrap();
    input.send(Command::Input("/help".into())).unwrap();
    until(&mut updates, |state| {
        state.status.contains("Queued command /help")
    })
    .await;
    input.send(Command::Input("/approve".into())).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Update::RestoreInput(text)) = updates.recv().await {
                assert_eq!(text, "/approve");
                break;
            }
        }
    })
    .await
    .unwrap();
    fixture.state.release.add_permits(1);
    let ready = until(&mut updates, |state| {
        !state.busy
            && state
                .conversation
                .messages
                .iter()
                .any(|m| m.text == moosedev::harness::session::HELP)
    })
    .await;
    assert_eq!(ready.task.unwrap().phase, Phase::AwaitingInput);
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn interrupting_a_completed_task_is_an_idle_noop() {
    let fixture = Fixture::new().await;
    let mut runner = moosedev::harness::runner::Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Finished work".into(),
    )
    .await
    .unwrap();
    runner.task.phase = Phase::Complete;
    let conversation = Conversation::new(fixture.root.clone());
    let mut provider = ProviderSettings::fallback();
    provider
        .select(Some(&format!("{}/v1", fixture.url)), "scripted-local-model")
        .unwrap();
    let (input, input_rx) = mpsc::unbounded_channel();
    let (output, mut updates) = mpsc::unbounded_channel();
    let handle = tokio::spawn(
        Controller::new(
            conversation,
            Some(runner),
            StartupOptions {
                root: fixture.root.clone(),
                daemon: Some(fixture.url.clone()),
                daemon_exe: None,
            },
            provider,
            input_rx,
            output,
        )
        .run(),
    );
    until(&mut updates, |state| {
        !state.busy
            && state
                .task
                .as_ref()
                .is_some_and(|t| t.phase == Phase::Complete)
    })
    .await;
    for count in 1..=2 {
        input.send(Command::Interrupt).unwrap();
        input.send(Command::Input("/help".into())).unwrap();
        let ready = until(&mut updates, |state| {
            !state.busy
                && state
                    .conversation
                    .messages
                    .iter()
                    .filter(|m| m.text == moosedev::harness::session::HELP)
                    .count()
                    == count
        })
        .await;
        assert_eq!(ready.task.unwrap().phase, Phase::Complete);
        assert!(!ready
            .conversation
            .messages
            .iter()
            .any(|m| m.text.contains("cannot be cancelled")));
    }
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn interruption_preserves_failed_cleanup_and_escape_retries_it_after_reconnect() {
    use fs2::FileExt;
    let fixture = Fixture::new().await;
    let conversation = Conversation::new(fixture.root.clone());
    let conversation_id = conversation.id.clone();
    let (input, mut updates, handle) = fixture.controller(conversation);
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Explain this project.".into()))
        .unwrap();
    let working = until(&mut updates, |state| state.busy && state.task.is_some()).await;
    tokio::time::timeout(Duration::from_secs(10), fixture.state.started.notified())
        .await
        .unwrap();
    let task_id = working.task.unwrap().id;
    let scratch = fixture
        .root
        .join(".moosedev/harness/scratch")
        .join(&task_id);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("cache-data"), "owned task cache").unwrap();
    let owner = std::fs::File::open(&scratch).unwrap();
    owner.try_lock_exclusive().unwrap();
    input.send(Command::Interrupt).unwrap();
    let stopped = until(&mut updates, |state| {
        !state.busy
            && state
                .task
                .as_ref()
                .is_some_and(|task| task.phase == Phase::Cancelled && task.cleanup_pending)
            && state.status.contains("Cancellation took effect")
    })
    .await;
    assert!(stopped
        .conversation
        .messages
        .iter()
        .any(|message| message.text.contains("scratch cleanup is pending")));
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();

    let saved = Conversation::load(&fixture.root, &conversation_id).unwrap();
    let (input, mut updates, handle) = fixture.controller(saved);
    let restored = until(&mut updates, |state| {
        !state.busy
            && state.task.as_ref().is_some_and(|task| task.cleanup_pending)
            && state.status.contains("scratch cleanup is pending")
    })
    .await;
    assert_eq!(restored.task.unwrap().phase, Phase::Cancelled);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    FileExt::unlock(&owner).unwrap();
    drop(owner);
    input.send(Command::Interrupt).unwrap();
    until(&mut updates, |state| {
        !state.busy
            && state
                .task
                .as_ref()
                .is_some_and(|task| task.phase == Phase::Cancelled && !task.cleanup_pending)
    })
    .await;
    assert!(!scratch.exists());
    assert_eq!(
        fixture.state.calls.load(Ordering::SeqCst),
        1,
        "cleanup retry must not resume model work"
    );
    input.send(Command::Input("/continue".into())).unwrap();
    until(&mut updates, |state| {
        !state.busy
            && state
                .task
                .as_ref()
                .is_some_and(|task| task.turn_finished && !task.cleanup_pending)
    })
    .await;
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 2);
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn invalid_action_is_repaired_without_another_human_message_or_reprobe() {
    let fixture = Fixture::new().await;
    fixture.state.release.add_permits(1);
    *fixture.state.replies.lock().unwrap() = VecDeque::from([
        json!({"message":"I will explain the project.","action":{"action":"invented_action"}}),
        json!({"message":"I corrected the action.","action":{"action":"reply","message":"Recovered answer."}}),
    ]);
    let (input, mut updates, handle) = fixture.controller(Conversation::new(fixture.root.clone()));
    until(&mut updates, |state| !state.busy).await;
    input
        .send(Command::Input("Explain the project.".into()))
        .unwrap();
    let finished =
        until(&mut updates, |state| {
            !state.busy
                && state.conversation.messages.iter().any(|message| {
                    message.role == "assistant" && message.text == "Recovered answer."
                })
        })
        .await;
    let task = finished.task.unwrap();
    assert!(task.last_error.is_none());
    assert!(task.recovery.is_none());
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture.state.probe_calls.load(Ordering::SeqCst),
        2,
        "validated provider should remain cached during correction"
    );
    let actions: Vec<_> = task
        .model_requests
        .iter()
        .filter(|request| request["purpose"] == "harness_action")
        .collect();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["decision_id"], actions[1]["decision_id"]);
    assert_eq!(actions[0]["attempt"], 1);
    assert_eq!(actions[1]["attempt"], 2);
    assert!(actions[0]["response"]
        .as_str()
        .unwrap()
        .contains("invented_action"));
    assert!(actions[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("Your last candidate was rejected"));
    assert!(task
        .events
        .iter()
        .any(|event| event.message.contains("Correcting action, attempt 2 of 3")));
    assert_eq!(
        finished
            .conversation
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        1
    );
    input.send(Command::Quit).unwrap();
    handle.await.unwrap();
}
