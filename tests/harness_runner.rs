#![cfg(feature = "harness")]
//! Orchestration tests with a scripted sensor: it never calls a memory tool.
//! Executor confinement is tested separately; completed-check fixtures below
//! isolate the human-review and daemon-durability completion gates.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use moosedev::harness::protocol::*;
use moosedev::harness::runner::{CheckResult, Mode, Phase, Runner};
use moosedev::policy::{GateDisposition, PolicyDecision};
use serde_json::{json, Value};

static ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Env {
    fn configure(url: &str) -> Self {
        let vars = [
            ("MOOSEDEV_LLM_BASE_URL", format!("{url}/v1")),
            ("MOOSEDEV_LLM_MODEL", "scripted-local-model".into()),
            ("MOOSEDEV_LLM_API_KEY", "fixture".into()),
            ("MOOSEDEV_LLM_STRUCTURED_OUTPUT", "required".into()),
            ("MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS", "32768".into()),
        ];
        let prior = vars
            .iter()
            .map(|(key, value)| {
                let old = std::env::var_os(key);
                std::env::set_var(key, value);
                (*key, old)
            })
            .collect();
        Self(prior)
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[derive(Default)]
struct Script {
    root: PathBuf,
    context: Option<String>,
    replies: VecDeque<(&'static str, Value)>,
    requests: Vec<Value>,
    capture_requests: Vec<CaptureRequest>,
    fail_capture_once: bool,
    reject_capture_once: bool,
    deny_edit: bool,
    revision: String,
    checkpoint_durable: bool,
    reviewed: Vec<String>,
    revision_on_accept: Option<String>,
    attest_review: bool,
}

type Shared = Arc<Mutex<Script>>;

async fn model(State(state): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    let name = body["response_format"]["json_schema"]["name"]
        .as_str()
        .unwrap_or("");
    script
        .requests
        .push(json!({"kind":"model","schema":name,"body":body}));
    let Some((expected, answer)) = script.replies.pop_front() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"unexpected model invocation"})),
        );
    };
    assert_eq!(name, expected, "harness called the wrong sensor stage");
    (
        StatusCode::OK,
        Json(json!({"choices":[{"message":{"role":"assistant","content":answer.to_string()}}]})),
    )
}

async fn context(
    State(state): State<Shared>,
    Json(request): Json<ContextRequest>,
) -> Json<ContextResponse> {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"context","files":request.files}));
    Json(ContextResponse {
        project_root: script.root.to_string_lossy().into_owned(),
        revision: script.revision.clone(),
        context: script.context.clone().unwrap_or_else(|| {
            "Constraint: Preserve the public behavior. Requirement: repair the implementation."
                .into()
        }),
        files: request
            .files
            .iter()
            .map(|file| FileContext {
                file: file.clone(),
                dossier: format!("COMPLETE_DOSSIER_FOR_{file}: preserve this entity's contract."),
                policy: if script.deny_edit {
                    PolicyDecision::Gate {
                        disposition: GateDisposition::Deny,
                        reason: "fixture governing constraint".into(),
                        records: vec![],
                        entities: vec![],
                    }
                } else {
                    PolicyDecision::Allow
                },
            })
            .collect(),
    })
}

async fn capture(
    State(state): State<Shared>,
    Json(request): Json<CaptureRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"capture","operation_id":request.operation_id}));
    script.capture_requests.push(request.clone());
    if std::mem::take(&mut script.reject_capture_once) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"component does not exist; capture was not persisted"})),
        );
    }
    if std::mem::take(&mut script.fail_capture_once) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"simulated lost durable acknowledgment"})),
        );
    }
    let proposals = request
        .proposals
        .iter()
        .enumerate()
        .map(|(index, proposal)| CapturedProposal {
            iri: format!(
                "https://moosedev.dev/kg/{}/{}-{index}",
                proposal.kind, request.operation_id
            ),
            title: proposal.title.clone(),
            kind: proposal.kind.clone(),
            links: vec![],
            unanchored: vec![],
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::to_value(CaptureResponse { proposals }).unwrap()),
    )
}

fn checked(revision: &str, durable: bool, pending: Vec<String>) -> CheckpointResponse {
    CheckpointResponse {
        conforms: true,
        durable,
        revision: revision.into(),
        pending,
    }
}

async fn review(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<ReviewRequest>,
) -> (HeaderMap, Json<CheckpointResponse>) {
    let mut script = state.lock().unwrap();
    let base = script.revision.clone();
    if request.accept {
        if let Some(revision) = script.revision_on_accept.take() {
            script.revision = revision;
        }
    }
    script
        .requests
        .push(json!({"kind":"review","accept":request.accept,"operation_id":request.operation_id}));
    script.reviewed.push(request.operation_id);
    let mut attestation = HeaderMap::new();
    if script.attest_review
        && headers
            .get("x-moosedev-expected-revision")
            .and_then(|v| v.to_str().ok())
            == Some(base.as_str())
    {
        attestation.insert("x-moosedev-review-base-revision", base.parse().unwrap());
        attestation.insert(
            "x-moosedev-review-result-revision",
            script.revision.parse().unwrap(),
        );
    }
    (
        attestation,
        Json(checked(&script.revision, script.checkpoint_durable, vec![])),
    )
}

async fn checkpoint(
    State(state): State<Shared>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Json<CheckpointResponse> {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"checkpoint","operation_id":query.get("operation_id")}));
    let pending = query
        .get("operation_id")
        .filter(|id| !script.reviewed.contains(id))
        .map(|id| vec![id.clone()])
        .unwrap_or_default();
    Json(checked(
        &script.revision,
        script.checkpoint_durable,
        pending,
    ))
}

struct Fixture {
    root: PathBuf,
    url: String,
    shared: Shared,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("moosedev-runner-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("code.txt"), "original\n").unwrap();
        let shared = Arc::new(Mutex::new(Script {
            root: root.clone(),
            revision: "accepted-v1".into(),
            checkpoint_durable: true,
            ..Script::default()
        }));
        let routes = Router::new()
            .route("/v1/chat/completions", post(model))
            .route("/api/v1/harness/context", post(context))
            .route("/api/v1/harness/capture", post(capture))
            .route("/api/v1/harness/review", post(review))
            .route("/api/v1/harness/checkpoint", get(checkpoint))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        Self {
            root,
            url,
            shared,
            server,
        }
    }
    fn reply(&self, schema: &'static str, reply: Value) {
        self.shared
            .lock()
            .unwrap()
            .replies
            .push_back((schema, reply));
    }
    fn no_capture(&self) {
        self.reply("harness_capture", json!({"proposals":[],"reason":"No additional durable claim is supported by these events."}));
    }
    fn edit(&self) {
        self.reply(
            "harness_action",
            json!({"action":"edit","file":"code.txt","before":"original\n","after":"changed\n"}),
        );
    }
    fn model_calls(&self) -> usize {
        self.shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["kind"] == "model")
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn runner_enforces_reading_capture_review_and_recovery_without_memory_tool_calls() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Repair code.txt while preserving behavior".into(),
    )
    .await
    .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(fixture.model_calls(), 0);
    assert_eq!(
        fixture.shared.lock().unwrap().requests[0]["kind"],
        "context"
    );

    fixture.edit();
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("Plan mode"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );

    fixture.reply("harness_action", json!({"action":"plan","summary":"Make a localized behavior-preserving repair","files":["code.txt"],"checks":["false"]}));
    fixture.reply("harness_capture", json!({"reason":"Preserve the explicit planning decision for review.","proposals":[{
        "kind":"ArchitecturalDecision","title":"Localized repair plan","description":"Keep the repair local.",
        "evidence":["Model action:"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(runner.advance().await.is_err());
    let operation = runner
        .task
        .capture_request
        .as_ref()
        .unwrap()
        .operation_id
        .clone();
    assert!(runner.task.plan.is_some());
    assert!(runner.task.pending_capture.is_none());
    let id = runner.task.id.clone();
    drop(runner);

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.resume().await.unwrap();
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "capture retry must reuse the frozen assessment"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].operation_id, operation);
    assert_eq!(
        serde_json::to_value(&captures[0]).unwrap(),
        serde_json::to_value(&captures[1]).unwrap()
    );
    assert!(
        runner.approve_plan().await.is_err(),
        "pending capture must precede plan approval"
    );
    runner.review(false).await.unwrap();
    runner.approve_plan().await.unwrap();

    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("First-edit guard")));
    assert!(runner.task.read_files.contains(&"code.txt".into()));

    fixture.shared.lock().unwrap().deny_edit = true;
    fixture.edit();
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("denied"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    fixture.shared.lock().unwrap().deny_edit = false;
    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    let requests = fixture.shared.lock().unwrap().requests.clone();
    let last_model = requests
        .iter()
        .rev()
        .find(|r| r["kind"] == "model")
        .unwrap();
    let prompt = last_model["body"]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("COMPLETE_DOSSIER_FOR_code.txt") && prompt.contains("original"));
    fixture.reply("harness_capture", json!({"reason":"Record the observed repair.","proposals":[{
        "kind":"Lesson","title":"Local repair evidence","description":"The repair changed the intended file.",
        "evidence":["Applied edit code.txt"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(runner.advance().await.is_err());
    let interrupted_operation = runner
        .task
        .capture_request
        .as_ref()
        .unwrap()
        .operation_id
        .clone();
    runner.cancel().await.unwrap();
    drop(runner);
    fixture.shared.lock().unwrap().revision = "accepted-v2".into();
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.resume().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Working,
        "capture recovery must precede approval invalidation"
    );
    assert_eq!(
        runner.task.capture_request.as_ref().unwrap().operation_id,
        interrupted_operation
    );
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.review(false).await.unwrap();
    runner.advance().await.unwrap(); // Freshness now invalidates approval without calling the model.
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"The local repair is ready for verification."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.confirm_no_knowledge().await.is_err());

    // Whatever this platform's sandbox availability, a required false command
    // cannot satisfy verification or permit completion.
    let _verification = runner.advance().await;
    assert_ne!(runner.task.phase, Phase::Complete);
    assert!(
        runner.task.check_results.is_empty()
            || runner.task.check_results.iter().any(|check| !check.success)
    );
    if !runner.task.check_results.is_empty() {
        let persisted: Value = serde_json::from_slice(
            &std::fs::read(
                fixture
                    .root
                    .join(".moosedev/harness/tasks")
                    .join(format!("{id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(persisted["intent"].is_null());
        assert_eq!(persisted["check_results"][0]["success"], false);
        assert_eq!(
            persisted["capture_due"], true,
            "command outcome and its capture obligation must commit together"
        );
    }

    // Simulate a crash after journaling a command intent but before observing
    // its outcome. Resumption must ask a human, never replay the command.
    let mut journal = serde_json::to_value(&runner.task).unwrap();
    journal["intent"] = json!({"Command":"touch duplicated-marker"});
    journal["phase"] = json!("Working");
    let path = fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"));
    drop(runner);
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    let calls = fixture.model_calls();
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("unknown"));
    assert!(!fixture.root.join("duplicated-marker").exists());
    assert_eq!(fixture.model_calls(), calls);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn completed_verification_still_requires_final_human_review_and_durable_graph() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Inspect the existing code".into(),
    )
    .await
    .unwrap();
    fixture.reply("harness_action", json!({"action":"plan","summary":"Inspect code without changes","files":["code.txt"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"Inspection complete; require verification."}),
    );
    runner.advance().await.unwrap();
    // The executor's correctness is independent; provide its completed result
    // as the starting state for this completion-gate scenario.
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "fixture: successful check already observed".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.pending_capture.is_none());
    assert_ne!(
        runner.task.phase,
        Phase::Complete,
        "model assessment cannot replace human confirmation"
    );
    fixture.shared.lock().unwrap().checkpoint_durable = false;
    assert!(runner.confirm_no_knowledge().await.is_err());
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().checkpoint_durable = true;
    fixture.shared.lock().unwrap().revision = "accepted-v2".into();
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "changed knowledge must invalidate completion approval"
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(
        runner.task.check_results.is_empty(),
        "changed governing evidence invalidates the old verification"
    );
    runner.approve_plan().await.unwrap();
    fixture.reply("harness_action", json!({"action":"finish","summary":"Reviewed the updated governing knowledge; verify again."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "fixture: repeated verification completed".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn definitely_unpersisted_capture_can_be_reassessed_after_http_400() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Plan a localized repair".into(),
    )
    .await
    .unwrap();
    fixture.reply("harness_action", json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["false"]}));
    let assessment = |components: Value| {
        json!({"reason":"Capture the planning choice.","proposals":[{
            "kind":"ArchitecturalDecision","title":"Localized repair","description":"Keep the repair local.",
            "evidence":["Model action:"],"files":["code.txt"],"components":components,"requirement":null,"supersedes":null,"retracts":null
        }]})
    };
    fixture.reply("harness_capture", assessment(json!(["missing component"])));
    fixture.shared.lock().unwrap().reject_capture_once = true;
    assert!(runner.advance().await.is_err());
    assert!(
        runner.task.capture_request.is_none(),
        "only definitely unpersisted captures may discard their request identity"
    );
    let calls = fixture.model_calls();
    fixture.reply("harness_capture", assessment(json!([])));
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls + 1,
        "validation rejection requires a new sensor assessment"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 2);
    assert_ne!(captures[0].operation_id, captures[1].operation_id);
    assert!(captures[1].proposals[0].components.is_empty());
    runner.review(false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
}

#[tokio::test]
#[ignore = "requires the explicitly configured local Gemma server at 127.0.0.1:1234"]
async fn live_local_model_reaches_enforced_plan_capture_without_editing() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    std::env::set_var("MOOSEDEV_LLM_BASE_URL", "http://127.0.0.1:1234/v1");
    std::env::set_var("MOOSEDEV_LLM_MODEL", "google/gemma-4-26b-a4b-qat");
    std::env::set_var("MOOSEDEV_LLM_STRUCTURED_OUTPUT", "auto");
    let mut runner = Runner::create(fixture.root.clone(), fixture.url.clone(),
        "Prepare a plan to replace the word original with changed in code.txt. Read the file first. The only permitted file is code.txt. Use a shell check that verifies its eventual contents. Stay in Plan mode; do not execute the change.".into()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        for _ in 0..8 {
            if matches!(
                runner.task.phase,
                Phase::AwaitingReview | Phase::AwaitingPlan
            ) {
                break;
            }
            runner.advance().await.unwrap_or_else(|error| {
                panic!("live-model step failed: {error:#}; task={:?}", runner.task)
            });
        }
    })
    .await
    .expect("live local model did not reach an enforced capture checkpoint within 120 seconds");
    assert!(
        matches!(
            runner.task.phase,
            Phase::AwaitingReview | Phase::AwaitingPlan
        ),
        "live model did not reach plan/capture review: {:?}",
        runner.task
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert!(
        runner.task.capture_reason.is_some(),
        "plan must trigger the capture sensor even with a real local model"
    );
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|request| request["kind"] == "context"
            && request["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file == "code.txt"))));
}

#[tokio::test]
#[ignore = "requires functional OS confinement; run explicitly outside a nested sandbox"]
async fn confined_successful_check_reaches_human_review_before_completion() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Verify that code.txt exists".into(),
    )
    .await
    .unwrap();
    fixture.reply("harness_action", json!({"action":"plan","summary":"Verify the existing file without editing it","files":["code.txt"],"checks":["test -f code.txt"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    runner.confirm_no_knowledge().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"Run the required filesystem check."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.check_results.len(), 1);
    assert!(
        runner.task.check_results[0].success,
        "real confined check failed: {:?}",
        runner.task.check_results[0]
    );
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_ne!(runner.task.phase, Phase::Complete);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

impl Fixture {
    fn conversational(&self, action: Value) {
        self.reply(
            "harness_action",
            json!({"message":"I am working through the request.","action":action}),
        );
    }

    fn captured(&self, kind: &str, title: &str) {
        self.reply("harness_capture", json!({"reason":"The observed edit supports this durable proposal.","proposals":[{
            "kind":kind,"title":title,"description":"Keep this evidenced implementation knowledge.",
            "evidence":["Applied edit code.txt"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
        }]}));
    }

    async fn interactive(&self) -> Runner {
        self.interactive_objective("Repair code.txt while preserving behavior")
            .await
    }

    async fn interactive_objective(&self, objective: &str) -> Runner {
        let mut runner = Runner::create(self.root.clone(), self.url.clone(), objective.into())
            .await
            .unwrap();
        runner.configure(self.config(), None);
        runner.enable_interactive().unwrap();
        runner
    }

    fn config(&self) -> moosedev::llm::LlmConfig {
        moosedev::llm::LlmConfig {
            base_url: format!("{}/v1", self.url),
            api_key: "fixture".into(),
            model: "scripted-local-model".into(),
            configured: true,
            context_window_tokens: 32768,
            structured_output: moosedev::llm::StructuredOutputMode::Required,
        }
    }

    async fn approved_interactive(&self) -> Runner {
        let mut runner = self.interactive().await;
        self.conversational(json!({"action":"read","file":"code.txt"}));
        runner.advance().await.unwrap();
        self.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["fixture-required-check"]}));
        self.no_capture();
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::AwaitingPlan);
        runner.approve_plan().await.unwrap();
        runner
    }
}

#[tokio::test]
async fn interactive_greeting_and_model_question_fit_a_populated_project() {
    let fixture = Fixture::new().await;
    let knowledge = format!(
        "GOVERNING_EVIDENCE_BEGIN\n{}GOVERNING_EVIDENCE_END\n",
        "Constraint: Preserve the complete public contract and its recorded rationale.\n"
            .repeat(640)
    );
    assert!(knowledge.len() > 48_000);
    fixture.shared.lock().unwrap().context = Some(knowledge.clone());
    let mut path_bytes = 0;
    for index in 0..1000 {
        let path = format!(
            "components/long_named_subsystem/module_{:03}/implementation_details/behavior_contract_{index:04}.rs",
            index / 50
        );
        path_bytes += path.len() + 1;
        let path = fixture.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "// fixture source\n").unwrap();
    }
    assert!(path_bytes > 80_000);

    let mut runner = fixture.interactive_objective("Hello").await;
    fixture.conversational(json!({"action":"reply","message":"Hello! How can I help?"}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert!(runner.task.plan.is_none());

    runner
        .submit_message("What model are you using".into())
        .await
        .unwrap();
    fixture.conversational(
        json!({"action":"reply","message":"The selected model is scripted-local-model."}),
    );
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.check_results.is_empty());

    let script = fixture.shared.lock().unwrap();
    let calls: Vec<_> = script
        .requests
        .iter()
        .filter(|r| r["kind"] == "model")
        .collect();
    assert_eq!(
        calls.len(),
        4,
        "both replies must undergo capture assessment"
    );
    for call in calls {
        let prompt = call["body"]["messages"][0]["content"].as_str().unwrap();
        assert!(prompt.len() <= 86_016, "sent {} bytes", prompt.len());
        assert!(
            prompt.contains(&knowledge),
            "governing evidence must remain complete"
        );
        if call["schema"] == "harness_action" {
            assert!(prompt.contains("scripted-local-model"));
            assert!(
                prompt.contains("omitted"),
                "discovery omission must be explicit"
            );
            assert!(
                prompt.contains("code.txt"),
                "some navigation should remain available"
            );
        }
    }
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn interactive_oversized_governing_evidence_blocks_before_model_call() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().context = Some(
        "Constraint: This complete governing evidence cannot be silently discarded.\n".repeat(1500),
    );
    let mut runner = fixture.interactive_objective("Hello").await;
    let error = runner.advance().await.unwrap_err().to_string();
    assert!(
        error.contains("context") && error.contains("budget"),
        "{error}"
    );
    assert_eq!(fixture.model_calls(), 0);
    assert!(runner.task.model_requests.is_empty());
    assert!(runner.task.plan.is_none());
    assert_ne!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn interactive_reply_needs_no_modification_plan_or_checks() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"The current file contains the original implementation."}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert!(runner.task.batch_capture);
    assert!(runner.task.turn_finished);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.check_results.is_empty());
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert_eq!(
        fixture.shared.lock().unwrap().requests[0]["kind"],
        "context"
    );
    runner
        .submit_message("Explain the behavior in more detail.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(!runner.task.turn_finished);
    fixture
        .conversational(json!({"action":"reply","message":"Here is the requested explanation."}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert!(runner.task.turn_finished);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn interactive_batches_edit_proposals_until_final_durable_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":"changed\n"}),
    );
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "First observed change");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.reviews.len(), 1);
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"changed\n","after":"complete\n"}),
    );
    runner.advance().await.unwrap();
    fixture.captured("Pattern", "Second observed change");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.reviews.len(), 2);
    fixture.conversational(json!({"action":"finish","summary":"Ready for required checks."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "Completed verification fixture".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.confirm_no_knowledge().await.is_err());
    let operations: Vec<_> = runner
        .task
        .reviews
        .iter()
        .map(|review| review.request.operation_id.clone())
        .collect();
    runner
        .review_operation(&operations[0], false)
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    fixture.shared.lock().unwrap().checkpoint_durable = false;
    assert!(runner.review_operation(&operations[1], true).await.is_err());
    assert_eq!(runner.task.reviews.len(), 1);
    assert_ne!(runner.task.phase, Phase::Complete);
    fixture.shared.lock().unwrap().checkpoint_durable = true;
    runner.review_operation(&operations[1], true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "complete\n"
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn interactive_governing_proposal_blocks_further_work_before_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":"changed\n"}),
    );
    runner.advance().await.unwrap();
    fixture.captured("Constraint", "Preserve the observed contract");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let calls = fixture.model_calls();
    assert!(runner.advance().await.is_err());
    assert_eq!(fixture.model_calls(), calls);
    assert!(runner.approve_plan().await.is_err());
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
}

#[tokio::test]
async fn interactive_steering_preserves_batch_and_ambiguous_capture_retry() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":"changed\n"}),
    );
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "First change awaiting review");
    runner.advance().await.unwrap();
    let first = runner.task.reviews[0].request.operation_id.clone();
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"changed\n","after":"second\n"}),
    );
    runner.advance().await.unwrap();
    fixture.captured("Pattern", "Second change awaiting acknowledgement");
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(runner.advance().await.is_err());
    let frozen = runner.task.capture_request.clone().unwrap();
    runner
        .submit_message("Stop changing code and explain the two changes.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.reviews[0].request.operation_id, first);
    assert_eq!(
        runner.task.capture_request.as_ref().unwrap().operation_id,
        frozen.operation_id
    );
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "ambiguous capture retry must reuse its frozen assessment"
    );
    assert_eq!(runner.task.reviews.len(), 2);
    assert_eq!(runner.task.phase, Phase::Planning);
    let requests = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(
        serde_json::to_value(&requests[1]).unwrap(),
        serde_json::to_value(&requests[2]).unwrap()
    );
    fixture.no_capture();
    runner.advance().await.unwrap();
    let requests = fixture.shared.lock().unwrap().requests.clone();
    let assessment = requests
        .iter()
        .rev()
        .find(|request| request["schema"] == "harness_capture")
        .unwrap();
    assert!(assessment["body"]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Stop changing code and explain the two changes."));
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"second\n","after":"forbidden\n"}),
    );
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("Plan mode"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "second\n"
    );
    assert_eq!(runner.task.reviews.len(), 2);
    runner.request_review().unwrap();
    runner.review(false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
}

#[tokio::test]
async fn interactive_message_delivery_is_idempotent_after_restart() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let message_id = uuid::Uuid::new_v4().to_string();
    let message = "Explain the existing file before proposing changes.";
    runner
        .submit_message_once(&message_id, message.into())
        .await
        .unwrap();
    let task_id = runner.task.id.clone();
    let events = runner.task.events.len();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner
        .submit_message_once(&message_id, message.into())
        .await
        .unwrap();
    assert_eq!(runner.task.events.len(), events);
    assert_eq!(
        runner
            .task
            .events
            .iter()
            .filter(|event| event.message == format!("Human response: {message}"))
            .count(),
        1
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(runner.task.batch_capture);
    assert_eq!(fixture.model_calls(), 0);
}

#[tokio::test]
async fn interactive_no_change_confirmation_preserves_later_steering_for_capture() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(json!({"action":"finish","summary":"Ready for required verification."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "Completed verification fixture".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let steering = "Preserve the public interface as a hard constraint for all future changes.";
    runner.submit_message(steering.into()).await.unwrap();
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    fixture.reply("harness_capture", json!({"reason":"The new guidance explicitly establishes a constraint.","proposals":[{
        "kind":"Constraint","title":"Preserve the public interface","description":steering,
        "evidence":[steering],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(runner.task.reviews[0].request.proposals[0]
        .evidence
        .iter()
        .any(|quote| quote == steering));
    let requests = fixture.shared.lock().unwrap().requests.clone();
    let assessment = requests
        .iter()
        .rev()
        .find(|request| request["schema"] == "harness_capture")
        .unwrap();
    assert!(assessment["body"]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(steering));
    assert_ne!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn search_reaches_paths_and_contents_beyond_navigation_preview() {
    let fixture = Fixture::new().await;
    for i in 0..2001 {
        std::fs::write(fixture.root.join(format!("file-{i:04}.txt")), "ordinary\n").unwrap();
    }
    std::fs::write(
        fixture.root.join("zz-hidden-target.txt"),
        "distinctive-content\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    for query in ["zz-hidden-target", "distinctive-content"] {
        fixture.conversational(json!({"action":"search","query":query}));
        runner.advance().await.unwrap();
        assert!(runner.task.last_response.contains("zz-hidden-target.txt"));
    }
    assert!(runner.task.last_response.contains("distinctive-content"));
}

#[tokio::test]
async fn large_observations_and_capture_pages_survive_restart() {
    let fixture = Fixture::new().await;
    let knowledge = format!(
        "Constraint: {} END_REQUIRED",
        "governing claim ".repeat(3000)
    );
    fixture.shared.lock().unwrap().context = Some(knowledge.clone());
    let mut runner = fixture.interactive().await;
    let evidence = format!(
        "Read code.txt: BEGIN_EVIDENCE\n{}\nEND_EVIDENCE",
        "λ😀\"\\\n".repeat(18000)
    );
    runner.task.events.push(moosedev::harness::runner::Event {
        message: evidence.clone(),
    });
    runner.task.last_response = "failed command observation\n".repeat(5000);
    runner.task.check_results = (0..20)
        .map(|index| CheckResult {
            command: format!("test-{index}"),
            success: false,
            output: "long failing test output\n".repeat(5000),
        })
        .collect();
    fixture.conversational(json!({"action":"plan","summary":"Inspect and repair within scope","files":["code.txt"],"checks":["test-command"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Planning,
        "capture must continue before plan approval"
    );
    let saved = serde_json::to_value(&runner.task).unwrap();
    assert!(saved["capture_offset"].as_u64().unwrap() > 0);
    assert!(runner.approve_plan().await.is_err());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    for _ in 0..30 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message == evidence));
    let script = fixture.shared.lock().unwrap();
    let requests: Vec<_> = script
        .requests
        .iter()
        .filter(|r| r["kind"] == "model")
        .collect();
    assert!(requests.len() > 3);
    for request in &requests {
        let prompt = request["body"]["messages"][0]["content"].as_str().unwrap();
        assert!(
            prompt.len() <= 86016,
            "request overflowed: {}",
            prompt.len()
        );
        assert!(
            prompt.contains(&knowledge),
            "governing evidence must remain complete"
        );
    }
    let last = requests.last().unwrap()["body"]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(
        last.contains("END_EVIDENCE")
            || requests.iter().any(|r| r["body"]["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("END_EVIDENCE"))
    );
}

#[tokio::test]
async fn inspect_pages_complete_journal_observations_without_replaying_actions() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let index = runner.task.events.len();
    runner.task.events.push(moosedev::harness::runner::Event {
        message: format!("{}hidden detail", "x".repeat(60000)),
    });
    fixture.conversational(json!({"action":"inspect","event":index,"offset":60000}));
    runner.advance().await.unwrap();
    assert!(runner.task.last_response.contains("hidden detail"));
    assert!(runner.task.plan.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn interactive_read_only_conversation_can_capture_knowledge_linked_to_read_files() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"reply","message":"The file retains its original implementation."}),
    );
    fixture.reply("harness_capture", json!({"reason":"Inspection established a file-specific lesson.","proposals":[{
        "kind":"Lesson","title":"Observed implementation contract","description":"Retain the observed behavior during future maintenance.",
        "evidence":["Read code.txt:"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.check_results.is_empty());
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        runner.task.reviews[0].request.proposals[0].files,
        vec!["code.txt"]
    );
    assert_eq!(fixture.shared.lock().unwrap().capture_requests.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn final_review_preserves_checks_only_for_attested_own_knowledge_changes() {
    for attest in [true, false] {
        let fixture = Fixture::new().await;
        let mut runner = fixture.approved_interactive().await;
        fixture.edit();
        runner.advance().await.unwrap();
        fixture.captured("Lesson", "Verified repair observation");
        runner.advance().await.unwrap();
        runner.task.phase = Phase::Verifying;
        runner.task.check_results = vec![CheckResult {
            command: "fixture-required-check".into(),
            success: true,
            output: "passed".into(),
        }];
        fixture.no_capture();
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::AwaitingReview);
        {
            let mut script = fixture.shared.lock().unwrap();
            script.attest_review = attest;
            script.revision_on_accept = Some("accepted-v2".into());
        }
        let id = runner.task.reviews[0].request.operation_id.clone();
        runner.review_operation(&id, true).await.unwrap();
        if attest {
            assert_eq!(runner.task.phase, Phase::Complete);
            assert_eq!(runner.task.check_results.len(), 1);
            assert_eq!(runner.task.knowledge_revision, "accepted-v2");
        } else {
            assert_eq!(runner.task.phase, Phase::AwaitingPlan);
            assert!(
                runner.task.check_results.is_empty(),
                "unproven graph changes must invalidate verification"
            );
        }
    }
}

#[tokio::test]
async fn interactive_governing_review_refreshes_evidence_before_single_plan_approval() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Preserve behavior while repairing the file","files":["code.txt"],"checks":["fixture-required-check"]}));
    fixture.reply("harness_capture", json!({"reason":"The plan makes an explicit governing constraint reviewable.","proposals":[{
        "kind":"Constraint","title":"Preserve the repair contract","description":"Retain the public behavior throughout the repair.",
        "evidence":["Model action:"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.knowledge_revision, "accepted-v1");
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-v2".into());
    let operation = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.knowledge_revision, "accepted-v2");
    assert_eq!(runner.task.mode, Mode::Plan);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(
        runner
            .task
            .events
            .iter()
            .filter(|event| event.message.contains("Human approved the plan"))
            .count(),
        1
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn headless_partial_capture_import_preserves_remaining_evidence_after_restart() {
    let fixture = Fixture::new().await;
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Review a repair plan for code.txt".into(),
    )
    .await
    .unwrap();
    runner.configure(fixture.config(), None);
    runner.task.events.push(moosedev::harness::runner::Event {
        message: format!(
            "PREFIX_ONLY\n{}",
            "observed implementation line\n".repeat(10_000)
        ),
    });
    fixture.reply("harness_action", json!({"action":"plan","summary":"Review the implementation","files":["code.txt"],"checks":["fixture-required-check"]}));
    fixture.reply("harness_capture", json!({"reason":"An observed implementation detail warrants review.","proposals":[{
        "kind":"Lesson","title":"Review the observed implementation","description":"Preserve the observed implementation behavior.",
        "evidence":["observed implementation line"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.pending_capture.is_some());
    let before = serde_json::to_value(&runner.task).unwrap();
    let next_event = before["capture_end"].as_u64().unwrap();
    let next_offset = before["capture_end_offset"].as_u64().unwrap();
    assert!(next_offset > 0, "the proposal must cover a partial event");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.enable_interactive().unwrap();
    let imported = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(imported["capture_cursor"], next_event);
    assert_eq!(imported["capture_offset"], next_offset);
    assert_eq!(
        imported["capture_checkpoint_end"],
        before["capture_checkpoint_end"]
    );
    assert_eq!(imported["capture_due"], true);
    assert!(runner.task.capture_request.is_none());
    assert!(runner.task.pending_capture.is_none());
    assert_eq!(runner.task.reviews.len(), 1);
    let operation = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&operation, false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.approve_plan().await.is_err());
    for _ in 0..12 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    let script = fixture.shared.lock().unwrap();
    let prompts: Vec<_> = script
        .requests
        .iter()
        .filter(|request| request["kind"] == "model" && request["schema"] == "harness_capture")
        .map(|request| request["body"]["messages"][0]["content"].as_str().unwrap())
        .collect();
    assert!(prompts.len() > 2);
    assert!(prompts[1].contains(&format!("Event {next_event}, byte {next_offset}:")));
    assert!(
        prompts
            .iter()
            .skip(1)
            .all(|prompt| !prompt.contains("PREFIX_ONLY")),
        "already captured bytes must not be replayed"
    );
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn steering_during_read_only_capture_keeps_file_links_after_restart() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive_objective("Explain code.txt").await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    runner.task.events.push(moosedev::harness::runner::Event {
        message: format!(
            "Read code.txt: {}",
            "OBSERVED_FILE_BEHAVIOR\n".repeat(10_000)
        ),
    });
    fixture.conversational(
        json!({"action":"reply","message":"The file preserves the observed behavior."}),
    );
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert!(runner.task.plan.is_none());
    let before = serde_json::to_value(&runner.task).unwrap();
    assert!(before["capture_offset"].as_u64().unwrap() > 0);
    assert_eq!(before["capture_files"], json!(["code.txt"]));
    runner
        .submit_message("Explain only the behavior now.".into())
        .await
        .unwrap();
    assert!(runner.task.read_files.is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    fixture.reply("harness_capture", json!({"reason":"The remaining inspection evidence supports a file-linked lesson.","proposals":[{
        "kind":"Lesson","title":"Observed file behavior","description":"Keep the observed file behavior explicit during future maintenance.",
        "evidence":["OBSERVED_FILE_BEHAVIOR"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        runner.task.reviews[0].request.proposals[0].files,
        ["code.txt"]
    );
    for _ in 0..12 {
        if serde_json::to_value(&runner.task).unwrap()["capture_due"] == false {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        false
    );
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.plan.is_none());
    let script = fixture.shared.lock().unwrap();
    assert!(
        script
            .requests
            .iter()
            .any(|request| request["kind"] == "model"
                && request["schema"] == "harness_capture"
                && request["body"]["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Human response: Explain only the behavior now.")),
        "new guidance needs its own subsequent capture assessment"
    );
    assert!(script.replies.is_empty());
}
