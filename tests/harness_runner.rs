#![cfg(feature = "harness")]
//! Orchestration tests with a scripted sensor: it never calls a memory tool.
//! Executor confinement is tested separately; completed-check fixtures below
//! isolate the human-review and daemon-durability completion gates.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use moosedev::harness::protocol::*;
use moosedev::harness::runner::{CheckResult, Mode, Phase, ReviewItem, Runner};
use moosedev::policy::{GateDisposition, PolicyDecision};
use serde_json::{json, Value};

#[path = "harness_runner/intent.rs"]
mod intent;
#[path = "harness_runner/symbolic.rs"]
mod symbolic;

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
    usage: Option<Value>,
    context: Option<String>,
    replies: VecDeque<(&'static str, Value)>,
    requests: Vec<Value>,
    capture_requests: Vec<CaptureRequest>,
    fail_capture_once: bool,
    fail_capture: bool,
    fail_context: bool,
    missing_capture_targets: bool,
    reject_capture_once: bool,
    deny_edit: bool,
    revision: String,
    checkpoint_durable: bool,
    reviewed: Vec<String>,
    revision_on_accept: Option<String>,
    attest_review: bool,
    reject_stale_review: bool,
    fail_global_checkpoint_once: bool,
    mutation_during_model: Option<String>,
    held_response: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    intent_records: Vec<CaptureTarget>,
    intent_stale_source: bool,
    capture_candidates: Vec<CaptureCandidate>,
    reconciliation_requests: Vec<ReconcileCaptureRequest>,
    intent_link_requests: Vec<moosedev::harness::daemon::intent::IntentLinkRequest>,
    intent_accepted: Vec<moosedev::harness::daemon::intent::IntentBinding>,
    capture_links_per_proposal: usize,
    purpose_scoped_entities: bool,
    purpose_scoped_name: Option<String>,
    postedit_symbol_name: Option<String>,
    postedit_conservative: bool,
    malformed_intent_response: bool,
    purpose_retrieval_override: Option<PurposeRetrieval>,
    fail_reconcile_once: bool,
    reject_stale_candidates: bool,
    purpose_candidates_status: Option<u16>,
    capture_type_reply: Option<Vec<TypedProposal>>,
    capture_type_requests: Vec<CaptureTypeRequest>,
    fail_capture_type_once: bool,
}

type Shared = Arc<Mutex<Script>>;

async fn model(State(state): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let held = if body["response_format"]["json_schema"]["name"] != "harness_response_probe" {
        state.lock().unwrap().held_response.take()
    } else {
        None
    };
    let response = model_response(state, body);
    if let Some((received, release)) = held {
        received.notify_one();
        release.notified().await;
    }
    response
}

fn model_response(state: Shared, body: Value) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    let name = body["response_format"]["json_schema"]["name"]
        .as_str()
        .unwrap_or("");
    if name == "harness_response_probe" {
        let mut response = json!({"choices":[{
            "message":{"role":"assistant","content":"{\"status\":\"ok\"}"},
            "finish_reason":"stop"
        }]});
        if let Some(usage) = &script.usage {
            response["usage"] = usage.clone();
        }
        return (StatusCode::OK, Json(response));
    }
    script
        .requests
        .push(json!({"kind":"model","schema":name,"body":body}));
    let Some((expected, mut answer)) = script.replies.pop_front() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"unexpected model invocation"})),
        );
    };
    assert_eq!(name, expected, "harness called the wrong sensor stage");
    // Existing scenarios describe which observation supports a claim. Translate
    // those fixture selectors to the actual harness-issued IDs for this request;
    // production accepts IDs only, never quotations.
    if name == "harness_capture" {
        let prompt = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .find(|text| text.contains("\nEvidence:\n"))
            .unwrap();
        let page = prompt
            .split_once("\nEvidence:\n")
            .unwrap()
            .1
            .split_once("\nRequired JSON schema:\n")
            .unwrap()
            .0;
        let page: Vec<Value> = serde_json::from_str(page).unwrap();
        if let Some(proposals) = answer.get_mut("proposals").and_then(Value::as_array_mut) {
            for proposal in proposals {
                if let Some(quotes) = proposal.as_object_mut().unwrap().remove("evidence") {
                    proposal["evidence_ids"] = json!(quotes
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|quote| {
                            page.iter()
                                .find(|entry| {
                                    entry["text"]
                                        .as_str()
                                        .unwrap()
                                        .contains(quote.as_str().unwrap())
                                })
                                .map(|entry| entry["id"].clone())
                                .unwrap_or_else(|| json!("not-on-evidence-page"))
                        })
                        .collect::<Vec<_>>());
                }
            }
        }
    }
    if name == "harness_action" {
        if let Some(content) = script.mutation_during_model.take() {
            std::fs::write(script.root.join("code.txt"), content).unwrap();
        }
    }
    let mut response = json!({"choices":[{"message":{"role":"assistant","content":answer.to_string()},"finish_reason":"stop"}]});
    if let Some(usage) = &script.usage {
        response["usage"] = usage.clone();
    }
    let response = Json(response);
    (StatusCode::OK, response)
}

async fn context(
    State(state): State<Shared>,
    Json(request): Json<ContextRequest>,
) -> (StatusCode, Json<ContextResponse>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"context","files":request.files}));
    let status = if script.fail_context {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(ContextResponse {
            capture_contracts: Some(vec![1, 2]),
            intent_contracts: Some(vec![1, 2]),
            capture_targets: (!script.missing_capture_targets).then(Default::default),
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
                    dossier: format!(
                        "COMPLETE_DOSSIER_FOR_{file}: preserve this entity's contract."
                    ),
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
        }),
    )
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
    if script.fail_capture || std::mem::take(&mut script.fail_capture_once) {
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
            links: (0..script.capture_links_per_proposal)
                .map(|link| {
                    format!(
                        "https://moosedev.dev/kg/ProposedLink/{}-{index}-{link}",
                        request.operation_id
                    )
                })
                .collect(),
            unanchored: vec![],
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::to_value(CaptureResponse { proposals }).unwrap()),
    )
}

async fn capture_v2(
    state: State<Shared>,
    Json(request): Json<CaptureV2Request>,
) -> (StatusCode, Json<Value>) {
    let (status, Json(value)) = capture(
        state,
        Json(CaptureRequest {
            operation_id: request.operation_id,
            proposals: request.proposals,
        }),
    )
    .await;
    if status.is_success() {
        (status, Json(json!({"status":"captured","capture":value})))
    } else {
        (status, Json(value))
    }
}

async fn capture_candidates(
    State(state): State<Shared>,
    Json(request): Json<CaptureCandidateRequest>,
) -> Json<CaptureCandidatePage> {
    let mut script = state.lock().unwrap();
    let revision = script.revision.clone();
    script.requests.push(
        json!({"kind":"capture_candidates","proposal":request.proposal.title,"revision":revision}),
    );
    Json(CaptureCandidatePage {
        // The page carries the project revision it was taken at, so the
        // fixture can tell a reconciliation against a stale page apart.
        revision,
        proposal_digest: format!("{}:{}", request.proposal.kind, request.proposal.title),
        candidates: script.capture_candidates.clone(),
        next_cursor: None,
    })
}

async fn reconcile_capture(
    State(state): State<Shared>,
    Json(request): Json<ReconcileCaptureRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    let pending_capture_operation = script
        .capture_candidates
        .iter()
        .find(|candidate| candidate.iri == request.candidate_iri)
        .and_then(|candidate| {
            candidate
                .origin
                .as_ref()
                .map(|origin| origin.operation_id.clone())
        });
    // Record the request before any failure so a retry can be compared
    // byte-for-byte with the interrupted attempt.
    script.reconciliation_requests.push(request.clone());
    if std::mem::take(&mut script.fail_reconcile_once) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"simulated lost reconciliation acknowledgment"})),
        );
    }
    if script.reject_stale_candidates && request.candidate_revision != script.revision {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"candidate snapshot is stale"})),
        );
    }
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(ReconcileCaptureResponse {
                operation_id: request.operation_id,
                disposition: request.disposition,
                candidate_iri: request.candidate_iri,
                requires_human_review: true,
                pending_capture_operation,
                review: None,
            })
            .unwrap(),
        ),
    )
}

async fn review_reconciliation(
    State(state): State<Shared>,
    Json(request): Json<ReconcileReviewRequest>,
) -> Json<ReconcileCaptureResponse> {
    let script = state.lock().unwrap();
    let original = script
        .reconciliation_requests
        .iter()
        .find(|original| original.operation_id == request.operation_id)
        .expect("reviewed reconciliation operation must exist");
    let pending_capture_operation = script
        .capture_candidates
        .iter()
        .find(|candidate| candidate.iri == original.candidate_iri)
        .and_then(|candidate| {
            candidate
                .origin
                .as_ref()
                .map(|origin| origin.operation_id.clone())
        });
    Json(ReconcileCaptureResponse {
        operation_id: request.operation_id,
        disposition: original.disposition.clone(),
        candidate_iri: original.candidate_iri.clone(),
        requires_human_review: false,
        pending_capture_operation,
        review: Some(request.accept),
    })
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
) -> (StatusCode, HeaderMap, Json<CheckpointResponse>) {
    let mut script = state.lock().unwrap();
    let base = script.revision.clone();
    let expected = headers
        .get("x-moosedev-expected-revision")
        .and_then(|value| value.to_str().ok());
    if script.reject_stale_review && expected.is_some_and(|revision| revision != base) {
        return (
            StatusCode::CONFLICT,
            HeaderMap::new(),
            Json(checked(&base, false, vec![request.operation_id])),
        );
    }
    if request.accept {
        if let Some(linked) = script
            .intent_link_requests
            .iter()
            .find(|linked| linked.operation_id == request.operation_id)
            .cloned()
        {
            script.intent_accepted.extend(linked.bindings);
        }
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
        StatusCode::OK,
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
    let durable = if !query.contains_key("operation_id")
        && std::mem::take(&mut script.fail_global_checkpoint_once)
    {
        false
    } else {
        script.checkpoint_durable
    };
    Json(checked(&script.revision, durable, pending))
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
            .route("/api/v1/harness/capture/v2", post(capture_v2))
            .route(
                "/api/v1/harness/capture/candidates",
                post(capture_candidates),
            )
            .route("/api/v1/harness/capture/reconcile", post(reconcile_capture))
            .route(
                "/api/v1/harness/capture/reconcile/review",
                post(review_reconciliation),
            )
            .route("/api/v1/harness/review", post(review))
            .route("/api/v1/harness/checkpoint", post(checkpoint))
            .route("/api/v1/harness/intent/resolve", post(intent::resolve))
            .route(
                "/api/v1/harness/intent/purpose/candidates",
                post(intent::purpose_candidates),
            )
            .route(
                "/api/v1/harness/intent/candidates",
                post(intent::postedit_candidates),
            )
            .route(
                "/api/v1/harness/intent/associate",
                post(symbolic::associate),
            )
            .route("/api/v1/harness/capture/type", post(symbolic::capture_type))
            .route("/api/v1/harness/intent/link", post(intent::link))
            .route("/api/v1/harness/intent/review", post(review))
            .route("/api/v1/harness/intent/abandon", post(review))
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
    fn last_model_prompt(&self, schema: &str) -> String {
        let script = self.shared.lock().unwrap();
        let request = script
            .requests
            .iter()
            .rev()
            .find(|r| r["kind"] == "model" && r["schema"] == schema)
            .expect("a model request with that schema was recorded");
        request["body"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["content"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n")
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
async fn one_batch_review_interaction_emits_each_capture_link_disposition_once() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    for ordinal in 0..2 {
        let operation_id = uuid::Uuid::new_v4().to_string();
        runner.task.reviews.push(ReviewItem {
            intent_links: None,
            capture_resolution: None,
            request: CaptureRequest {
                operation_id: operation_id.clone(),
                proposals: vec![KnowledgeProposal {
                    kind: "Lesson".into(),
                    title: format!("lesson {ordinal}"),
                    description: "claim".into(),
                    evidence: vec!["evidence".into()],
                    files: vec![],
                    components: vec![],
                    requirement: None,
                    supersedes: None,
                    retracts: None,
                    reconciled: vec![],
                }],
            },
            response: CaptureResponse {
                proposals: vec![CapturedProposal {
                    iri: format!("urn:record:{ordinal}"),
                    title: format!("lesson {ordinal}"),
                    kind: "Lesson".into(),
                    links: vec![format!("urn:link:{ordinal}")],
                    unanchored: vec![],
                }],
            },
            reason: "review fixture".into(),
        });
    }
    runner.task.phase = Phase::AwaitingReview;
    runner.review(false).await.unwrap();
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "review_interaction")
            .count(),
        1
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "record_review")
            .count(),
        2
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "link_review")
            .count(),
        2
    );
    assert!(runner.task.reviews.is_empty());
}

#[tokio::test]
async fn runner_enforces_reading_capture_review_and_recovery_without_memory_tool_calls() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_links_per_proposal = 2;
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
    let link_events: Vec<_> = runner
        .task
        .intent_events
        .iter()
        .filter(|event| event.kind == "link_review")
        .collect();
    assert_eq!(link_events.len(), 2);
    assert!(link_events.iter().all(|event| event
        .detail
        .contains("rejected https://moosedev.dev/kg/ProposedLink/")));
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "review_interaction")
            .count(),
        1
    );
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
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.confirm_no_knowledge().await.is_err());
    fixture.shared.lock().unwrap().checkpoint_durable = true;
    fixture.shared.lock().unwrap().revision = "accepted-v2".into();
    runner.advance().await.unwrap();
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
    fixture.reply("harness_capture", assessment(json!([])));
    fixture.shared.lock().unwrap().reject_capture_once = true;
    fixture.reply("harness_capture", assessment(json!([])));
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls + 3,
        "one action and two capture candidates"
    );
    assert!(runner.task.last_error.is_none());
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
async fn semantic_reuse_is_reviewed_without_creating_a_duplicate_record() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_candidates = vec![CaptureCandidate {
        iri: "urn:existing".into(),
        title: "Observed behavior".into(),
        kind: "Lesson".into(),
        status: "accepted".into(),
        assertion_digest: "digest-1".into(),
        literals: vec![CandidateLiteral {
            predicate: "hasDescription".into(),
            value: "Keep this evidenced implementation knowledge.".into(),
            datatype: None,
            language: None,
        }],
        relations: vec![],
        origin: None,
        owned_by_requester: false,
        exact_title: true,
        legal_relations: vec![],
    }];
    let mut runner = fixture.interactive().await;
    fixture
        .conversational(json!({"action":"reply","message":"The behavior is already documented."}));
    fixture.reply("harness_capture", json!({"reason":"The explanation repeats durable knowledge.","proposals":[{
        "kind":"Lesson","title":"Observed behavior","description":"Keep this evidenced implementation knowledge.",
        "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"reuse_unchanged","candidate_id":"c0","rationale":"The claim and links are unchanged.","revised_title":null,"revised_description":null}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(
        runner.task.reviews[0]
            .capture_resolution
            .as_ref()
            .unwrap()
            .reuse_unchanged
    );
    assert!(fixture.shared.lock().unwrap().capture_requests.is_empty());
    let operation = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&operation, true).await.unwrap();
    assert!(runner.task.reviews.is_empty());
    assert!(fixture.shared.lock().unwrap().capture_requests.is_empty());
}

#[tokio::test]
async fn changed_claim_is_never_merged_into_an_existing_candidate() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_candidates = vec![CaptureCandidate {
        iri: "urn:existing".into(),
        title: "Observed behavior".into(),
        kind: "Lesson".into(),
        status: "accepted".into(),
        assertion_digest: "digest-1".into(),
        literals: vec![CandidateLiteral {
            predicate: "hasDescription".into(),
            value: "The behavior was observed.".into(),
            datatype: None,
            language: None,
        }],
        relations: vec![],
        origin: None,
        owned_by_requester: false,
        exact_title: true,
        legal_relations: vec![],
    }];
    let mut runner = fixture.interactive().await;
    fixture.conversational(
        json!({"action":"reply","message":"The behavior was observed; tests were added."}),
    );
    fixture.reply("harness_capture", json!({"reason":"A changed claim requires separate review.","proposals":[{
        "kind":"Lesson","title":"Observed behavior","description":"The behavior was observed and tests were added.",
        "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"distinct_knowledge","candidate_id":"c0","rationale":"The tests-added assertion is absent from the candidate.","revised_title":"Observed behavior test coverage","revised_description":"The behavior was observed and tests were added."}));
    runner.advance().await.unwrap();
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 1);
    assert_eq!(
        captures[0].proposals[0].title,
        "Observed behavior test coverage"
    );
    assert!(captures[0].proposals[0]
        .description
        .contains("tests were added"));
    assert_eq!(
        runner.task.reviews.len(),
        1,
        "changed text remains a normal human-reviewed graph proposal"
    );
}

#[tokio::test]
async fn owned_pending_revision_survives_restart_and_requires_origin_rejection() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"First observation."}));
    fixture.reply("harness_capture", json!({"reason":"First pending lesson.","proposals":[{
        "kind":"Lesson","title":"Pending lesson","description":"Original pending claim.","evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    let origin = runner.task.reviews[0].request.operation_id.clone();
    let candidate_iri = runner.task.reviews[0].response.proposals[0].iri.clone();
    fixture.shared.lock().unwrap().capture_candidates = vec![CaptureCandidate {
        iri: candidate_iri,
        title: "Pending lesson".into(),
        kind: "Lesson".into(),
        status: "proposed".into(),
        assertion_digest: "pending-digest".into(),
        literals: vec![CandidateLiteral {
            predicate: "hasDescription".into(),
            value: "Original pending claim.".into(),
            datatype: None,
            language: None,
        }],
        relations: vec![],
        origin: Some(CandidateOrigin {
            operation_id: origin.clone(),
            owner_id: runner.task.id.clone(),
        }),
        owned_by_requester: true,
        exact_title: true,
        legal_relations: vec![],
    }];
    runner
        .submit_message("Revise the pending lesson.".into())
        .await
        .unwrap();
    fixture.conversational(json!({"action":"reply","message":"The claim needs correction."}));
    fixture.reply("harness_capture", json!({"reason":"Correct the pending claim.","proposals":[{
        "kind":"Lesson","title":"Pending lesson","description":"Corrected pending claim.","evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"revise_proposal","candidate_id":"c0","rationale":"The owned pending claim is inaccurate.","revised_title":"Pending lesson","revised_description":"Corrected pending claim."}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner
            .task
            .pending_revision
            .as_ref()
            .unwrap()
            .origin_operation_id,
        origin
    );
    let id = runner.task.id.clone();
    drop(runner);
    let journal = fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"));
    let mut persisted: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    persisted["intent_policy"] = json!("change-level-v2");
    persisted["postedit_association_contract"] = json!(1);
    persisted["purpose_selection"] = json!({
        "version":1,
        "request":{"objective":"Repair code.txt while preserving behavior","files":["code.txt"],"cursor":null,"limit":8},
        "revision":"accepted-v1",
        "plan_scope_digest":"pending-revision-checkpoint",
        "current_page":{"revision":"accepted-v1","retrieval":"page","candidates":[{
            "handle":"r0","iri":"urn:governing","kind":"Requirement","title":"Preserve behavior",
            "claim":{"literals":[]},"lifecycle":"accepted","assertion_digest":"governing-digest","relations":[],"legal_predicates":[]
        }],"next_cursor":null},
        "pages":[],"cursor":null,"selected":[],"rejected_iris":[],
        "status":"awaiting_missing_capture","attempts":0
    });
    std::fs::write(&journal, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    let paused = runner.task.clone();
    let calls = fixture.model_calls();
    runner.review_operation(&origin, false).await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "rejecting the origin resumes the durable replacement without generation"
    );
    assert_eq!(
        runner.task.capture_request.as_ref().unwrap().proposals[0].description,
        "Corrected pending claim."
    );
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture",
        "rejecting the origin resumes the durable replacement without consuming the checkpoint"
    );
    runner.task = paused;
    runner.review_operation(&origin, true).await.unwrap();
    assert!(runner.task.pending_revision.is_none());
    assert!(runner.task.capture_batch.is_none());
    assert!(runner.task.capture_request.is_none());
    let accepted = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(accepted["capture_due"], true);
    assert!(accepted["capture_end"].is_null());
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture",
        "accepting the origin reopens assessment without consuming the checkpoint"
    );
    assert_eq!(
        fixture.model_calls(),
        calls,
        "accepting the origin discards the paused replacement without generation"
    );
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
        .any(|quote| quote.contains(steering)));
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
    assert!(prompts[1].contains(&format!("Event {next_event}, bytes {next_offset}..")));
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

#[tokio::test]
async fn final_governing_review_does_not_block_remaining_reviews_with_stale_attestation() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "Observed repair");
    runner.advance().await.unwrap();
    runner.task.phase = Phase::Verifying;
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "passed".into(),
    }];
    fixture.reply("harness_capture", json!({"reason":"Final review identified a governing contract.","proposals":[{
        "kind":"Constraint","title":"Preserve the reviewed behavior","description":"Future work must preserve this observed contract.",
        "evidence":["Repair code.txt"],"files":["code.txt"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 2);
    let governing = runner.task.reviews[1].request.operation_id.clone();
    let lesson = runner.task.reviews[0].request.operation_id.clone();
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.reject_stale_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
    }
    runner.review_operation(&governing, true).await.unwrap();
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(runner.task.knowledge_revision, "accepted-v2");
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-v3".into());
    runner.review_operation(&lesson, true).await.unwrap();
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(
        runner.task.check_results.is_empty(),
        "governing changes invalidate prior verification"
    );
    let calls = fixture.model_calls();
    assert!(runner.advance().await.is_err());
    assert_eq!(
        fixture.model_calls(),
        calls,
        "new work must wait for renewed plan approval"
    );
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
}

#[tokio::test]
async fn failed_final_checkpoint_retries_without_claiming_a_no_knowledge_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let scratch = fixture
        .root
        .join(".moosedev/harness/scratch")
        .join(&runner.task.id);
    std::fs::create_dir_all(scratch.join("build")).unwrap();
    std::fs::write(scratch.join("build/artifact"), "cached build").unwrap();
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "Reviewed repair result");
    runner.advance().await.unwrap();
    runner.task.phase = Phase::Verifying;
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "passed".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    let operation = runner.task.reviews[0].request.operation_id.clone();
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
        script.fail_global_checkpoint_once = true;
    }
    assert!(runner.review_operation(&operation, true).await.is_err());
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["completion_pending"],
        true
    );
    assert!(runner.confirm_no_knowledge().await.is_err());
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(
        !scratch.exists(),
        "completed tasks must release their source and build scratch"
    );
    assert_eq!(
        fixture.model_calls(),
        calls,
        "retry only completion checks, not capture or generation"
    );
    assert!(!runner.task.events.iter().any(|event| event
        .message
        .contains("Human confirmed that no durable knowledge changed")));
    assert_eq!(fixture.shared.lock().unwrap().reviewed, [operation]);
}

#[tokio::test]
async fn cancellation_cleans_scratch_and_keeps_the_task_resumable() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let scratch = fixture
        .root
        .join(".moosedev/harness/scratch")
        .join(&runner.task.id);
    std::fs::create_dir_all(scratch.join("build")).unwrap();
    std::fs::write(scratch.join("build/artifact"), "cached build").unwrap();
    runner.cancel().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Cancelled);
    assert!(!scratch.exists());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.plan.is_some());
}

#[tokio::test]
async fn malformed_capture_stops_after_bounded_attempts_across_reload_and_resume() {
    let fixture = Fixture::new().await;
    let mut runner = fixture
        .interactive_objective("Explain the current project")
        .await;
    fixture.conversational(
        json!({"action":"reply","message":"The project has a local coding harness."}),
    );
    // Parsing and semantic validation share exactly three candidates per page,
    // including across advance/resume calls.
    for _ in 0..40 {
        fixture.reply("harness_capture", json!("not an assessment object"));
    }
    for _ in 0..12 {
        let _ = runner.advance().await;
        if runner.task.phase == Phase::AwaitingInput {
            break;
        }
        runner.resume().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    let state = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(state["capture_due"], true);
    assert_eq!(state["capture_cursor"], 0);
    assert_eq!(state["capture_offset"], 0);
    let capture_calls = fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| request["schema"] == "harness_capture")
        .count();
    assert_eq!(
        capture_calls, 3,
        "capture attempts escaped their checkpoint budget"
    );
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    for _ in 0..3 {
        runner.resume().await.unwrap();
        assert!(runner.advance().await.is_err());
    }
    assert_eq!(
        fixture.model_calls(),
        calls,
        "resumption alone must not reset failed capture attempts"
    );
    fixture.shared.lock().unwrap().replies.clear();
    runner
        .submit_message("Retry capture using the observed evidence only.".into())
        .await
        .unwrap();
    for _ in 0..4 {
        if serde_json::to_value(&runner.task).unwrap()["capture_due"] == false {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        false
    );
}

#[tokio::test]
async fn oversized_plan_summary_is_rejected_before_becoming_required_prompt_state() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"large summary ".repeat(10_000),"files":["code.txt"],"checks":["fixture-required-check"]}));
    assert!(runner.advance().await.is_err());
    assert!(runner.task.plan.is_none());
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|request| request["schema"] != "harness_capture"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn extreme_configured_context_window_saturates_without_overflow() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive_objective("Hello").await;
    let mut config = fixture.config();
    config.context_window_tokens = usize::MAX;
    runner.configure(config, None);
    fixture.conversational(json!({"action":"reply","message":"Hello."}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    let script = fixture.shared.lock().unwrap();
    for request in script
        .requests
        .iter()
        .filter(|request| request["kind"] == "model")
    {
        assert!(
            request["body"]["messages"][0]["content"]
                .as_str()
                .unwrap()
                .len()
                <= 100_000
        );
    }
}

#[tokio::test]
async fn new_guidance_journals_the_discarded_policy_edit_without_applying_it() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":null}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    assert!(runner.task.pending_edit.is_some());
    let prior_events = runner.task.events.len();
    runner
        .submit_message("Keep the file and explain it instead.".into())
        .await
        .unwrap();
    assert!(runner.task.pending_edit.is_none());
    assert_eq!(runner.task.mode, Mode::Plan);
    let id = runner.task.id.clone();
    drop(runner);
    let runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert!(
        runner.task.events[prior_events..].iter().any(|event| {
            let message = event.message.to_lowercase();
            (message.contains("discard") || message.contains("invalidat"))
                && message.contains("code.txt")
                && message.contains("original")
        }),
        "the durable journal must retain the discarded exact edit and why it was invalidated"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn headless_no_change_capture_requests_one_confirmation_for_all_pages() {
    let fixture = Fixture::new().await;
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Review code.txt".into(),
    )
    .await
    .unwrap();
    runner.configure(fixture.config(), None);
    runner.task.events.push(moosedev::harness::runner::Event {
        message: "Observed implementation detail\n".repeat(10_000),
    });
    fixture.reply("harness_action", json!({"action":"plan","summary":"Review the observed file","files":["code.txt"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Planning,
        "partial no-change pages must not repeatedly stop for confirmation"
    );
    for _ in 0..12 {
        if runner.task.phase == Phase::AwaitingReview {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        false
    );
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.confirm_no_knowledge().await.is_err());
    assert_eq!(
        runner
            .task
            .events
            .iter()
            .filter(|event| event
                .message
                .contains("Human confirmed that no durable knowledge changed"))
            .count(),
        1
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn capture_refresh_outages_preserve_page_and_model_repair_budget() {
    let fixture = Fixture::new().await;
    let mut runner = fixture
        .interactive_objective("Explain the existing implementation")
        .await;
    runner.task.events.push(moosedev::harness::runner::Event {
        message: "Observed implementation behavior\n".repeat(10_000),
    });
    fixture
        .conversational(json!({"action":"reply","message":"The existing behavior is preserved."}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    let before = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(before["capture_due"], true);
    assert!(before["capture_offset"].as_u64().unwrap() > 0);
    let calls = fixture.model_calls();
    fixture.shared.lock().unwrap().fail_context = true;
    for _ in 0..4 {
        let error = runner.advance().await.unwrap_err();
        assert!(format!("{error:#}").contains("503"));
        assert_eq!(runner.task.phase, Phase::Planning);
        let saved = serde_json::to_value(&runner.task).unwrap();
        assert_eq!(saved["capture_repairs"], 0);
        for field in [
            "capture_cursor",
            "capture_offset",
            "capture_checkpoint_end",
            "capture_files",
        ] {
            assert_eq!(saved[field], before[field], "outage changed {field}");
        }
    }
    assert_eq!(fixture.model_calls(), calls);
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    fixture.shared.lock().unwrap().fail_context = false;
    for _ in 0..12 {
        if runner.task.phase == Phase::AwaitingInput {
            break;
        }
        fixture.no_capture();
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        false
    );
    assert!(
        !runner
            .task
            .events
            .iter()
            .any(|event| event.message.starts_with("Human response:")),
        "service recovery must not require invented human guidance"
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn capture_ack_outages_retry_the_frozen_operation_without_model_repairs() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "Keep the observed repair");
    fixture.shared.lock().unwrap().fail_capture = true;
    assert!(runner.advance().await.is_err());
    let request = runner.task.capture_request.clone().unwrap();
    let before = serde_json::to_value(&runner.task).unwrap();
    let calls = fixture.model_calls();
    for _ in 0..3 {
        assert!(runner.advance().await.is_err());
    }
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_repairs"],
        0
    );
    assert_eq!(fixture.model_calls(), calls);
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    let restored = serde_json::to_value(&runner.task).unwrap();
    for field in [
        "capture_cursor",
        "capture_offset",
        "capture_end",
        "capture_end_offset",
        "capture_checkpoint_end",
    ] {
        assert_eq!(restored[field], before[field]);
    }
    fixture.shared.lock().unwrap().fail_capture = false;
    runner.advance().await.unwrap();
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        runner.task.reviews[0].request.operation_id,
        request.operation_id
    );
    assert_eq!(fixture.model_calls(), calls);
    let script = fixture.shared.lock().unwrap();
    assert_eq!(script.capture_requests.len(), 5);
    assert!(
        script
            .capture_requests
            .iter()
            .all(|sent| serde_json::to_value(sent).unwrap()
                == serde_json::to_value(&request).unwrap())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_cleanup_failure_survives_reload_and_explicit_cancel_or_resume_retry() {
    use fs2::FileExt;
    for resume_directly in [false, true] {
        let fixture = Fixture::new().await;
        let mut runner = fixture.interactive().await;
        let scratch = fixture
            .root
            .join(".moosedev/harness/scratch")
            .join(&runner.task.id);
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("cache-data"), "owned task cache").unwrap();
        let owner = std::fs::File::open(&scratch).unwrap();
        owner.try_lock_exclusive().unwrap();
        let error = runner.cancel().await.unwrap_err();
        assert!(format!("{error:#}").contains("Cancellation took effect"));
        assert_eq!(runner.task.phase, Phase::Cancelled);
        assert!(runner.task.cleanup_pending);
        assert!(runner
            .task
            .last_error
            .as_ref()
            .unwrap()
            .contains("scratch cleanup is pending"));
        assert!(runner.mode_plan().await.is_err());
        assert!(runner.request_review().is_err());
        assert!(runner
            .submit_message("Continue the work".into())
            .await
            .is_err());
        assert_eq!(runner.task.phase, Phase::Cancelled);
        let id = runner.task.id.clone();
        drop(runner);
        let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
        runner.configure(fixture.config(), None);
        assert!(runner.task.cleanup_pending);
        assert!(runner.resume().await.is_err());
        assert_eq!(runner.task.phase, Phase::Cancelled);
        assert!(scratch.join("cache-data").exists());
        FileExt::unlock(&owner).unwrap();
        drop(owner);
        if resume_directly {
            runner.resume().await.unwrap();
            assert_eq!(runner.task.phase, Phase::Planning);
        } else {
            runner.cancel().await.unwrap();
            assert_eq!(runner.task.phase, Phase::Cancelled);
        }
        assert!(!runner.task.cleanup_pending);
        assert!(!scratch.exists());
        assert!(runner.task.last_error.is_none());
        assert!(!runner
            .task
            .events
            .iter()
            .any(|event| event.message.starts_with("Human response:")));
        let mut legacy = serde_json::to_value(&runner.task).unwrap();
        legacy.as_object_mut().unwrap().remove("cleanup_pending");
        let legacy: moosedev::harness::runner::Task = serde_json::from_value(legacy).unwrap();
        assert!(!legacy.cleanup_pending);
    }
}

#[tokio::test]
async fn capture_requires_typed_daemon_capability_before_sensor_generation() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive_objective("Explain this project").await;
    fixture.shared.lock().unwrap().missing_capture_targets = true;
    fixture.conversational(json!({"action":"reply","message":"This project contains code.txt."}));
    let error = runner.advance().await.unwrap_err();
    assert!(format!("{error:#}").contains("upgrade the project daemon"));
    assert_eq!(
        fixture.model_calls(),
        1,
        "capture must stop before asking the sensor to invent missing choices"
    );
    assert!(fixture.shared.lock().unwrap().capture_requests.is_empty());
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_cursor"],
        0
    );
    fixture.shared.lock().unwrap().missing_capture_targets = false;
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert!(runner.task.last_error.is_none());
}

#[tokio::test]
async fn malformed_and_fragment_edit_share_one_budget_and_apply_once() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.shared.lock().unwrap().usage =
        Some(json!({"prompt_tokens":19,"completion_tokens":5,"total_tokens":24}));
    let start = runner.task.model_requests.len();
    fixture.reply("harness_action", json!("not an action object"));
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original","after":"changed"}),
    );
    fixture.conversational(
        json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"changed"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(runner.task.edits.len(), 1);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.last_error.is_none());
    let requests = &runner.task.model_requests[start..];
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["attempt"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(requests
        .iter()
        .all(|r| r["decision_id"] == requests[0]["decision_id"]));
    assert!(requests[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("last candidate was rejected"));
    assert!(requests[2]["prompt"]
        .as_str()
        .unwrap()
        .contains("entire supplied file"));
    let metered = metered_decision(&runner, requests[0]["decision_id"].as_str().unwrap());
    assert_eq!(metered.len(), 3, "each correction is a physical request");
    assert_eq!(
        metered
            .iter()
            .map(|r| r["context"]["candidate"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        metered
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert!(metered.iter().all(|r| r["status"] == "completed"
        && r["context"]["purpose"] == "harness_action"
        && r["tokens"]["prompt_tokens"] == 19
        && r["tokens"]["completion_tokens"] == 5));
}

fn metered_decision(runner: &Runner, decision: &str) -> Vec<Value> {
    serde_json::to_value(&runner.task).unwrap()["token_usage"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["context"]["decision_id"] == decision)
        .cloned()
        .collect()
}

#[tokio::test]
async fn invalid_replacements_exhaust_without_write_and_restart_cannot_refill() {
    use moosedev::harness::runner::RecoveryStatus;
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    for old_text in ["", "missing", "not there"] {
        fixture.conversational(
            json!({"action":"replace","file":"code.txt","old_text":old_text,"new_text":"changed"}),
        );
    }
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(
        runner.task.recovery.as_ref().unwrap().status,
        RecoveryStatus::AwaitingGuidance
    );
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert!(runner.advance().await.is_err());
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
}

#[tokio::test]
async fn full_write_uses_snapshot_and_deletion_still_requires_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(json!({"action":"write","file":"code.txt","content":"replacement\n"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits[0].before.as_deref(), Some("original\n"));
    fixture.no_capture();
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"write","file":"code.txt","content":null}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "replacement\n"
    );
    let pending = runner.task.pending_edit.as_ref().unwrap();
    assert_eq!(pending.before.as_deref(), Some("replacement\n"));
    assert_eq!(pending.after, None);
}

#[tokio::test]
async fn source_change_during_generation_requires_fresh_approval_without_edit() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.shared.lock().unwrap().mutation_during_model = Some("human change\n".into());
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"model change"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.edits.is_empty());
    assert!(runner.task.pending_edit.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "human change\n"
    );
}

#[tokio::test]
async fn cancellation_during_generation_preserves_charged_candidate_on_resume() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let received = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    fixture.shared.lock().unwrap().held_response = Some((received.clone(), release.clone()));
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"must not apply"}));
    {
        let advance = runner.advance();
        tokio::pin!(advance);
        tokio::select! {
            result = &mut advance => panic!("held generation completed before cancellation: {result:?}"),
            waited = tokio::time::timeout(std::time::Duration::from_secs(10), received.notified()) => waited.expect("fixture did not receive model request"),
        }
        // Dropping the future now interrupts an actually received request, not
        // a filesystem write or scheduling delay before the HTTP call.
    }
    release.notify_one();
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 1);
    let decision = runner.task.recovery.as_ref().unwrap().id.clone();
    runner.cancel().await.unwrap();
    let interrupted = metered_decision(&runner, &decision);
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0]["status"], "cancelled");
    assert!(interrupted[0]["tokens"]["prompt_tokens"].is_null());
    assert!(interrupted[0]["tokens"]["completion_tokens"].is_null());
    assert!(runner.task.edits.is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(metered_decision(&runner, &decision), interrupted);
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    for _ in 0..2 {
        fixture.reply("harness_action", json!("invalid action"));
    }
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    let metered = metered_decision(&runner, &decision);
    assert_eq!(metered.len(), 3, "reloading must not duplicate accounting");
    assert_eq!(
        metered
            .iter()
            .filter(|r| r["status"] == "cancelled")
            .count(),
        1
    );
    assert_eq!(
        metered
            .iter()
            .map(|r| r["context"]["candidate"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn legacy_pending_capture_rejection_starts_a_charged_repair_budget() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.captured("Lesson", "Keep the observed repair");
    fixture.shared.lock().unwrap().fail_capture = true;
    assert!(runner.advance().await.is_err());
    let original = runner.task.capture_request.clone().unwrap();
    let mut legacy = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(legacy["recovery"]["attempts"], 1);
    legacy.as_object_mut().unwrap().remove("recovery");
    let id = runner.task.id.clone();
    drop(runner);
    std::fs::write(
        fixture
            .root
            .join(".moosedev/harness/tasks")
            .join(format!("{id}.json")),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    assert!(runner.task.recovery.is_none());
    {
        let mut script = fixture.shared.lock().unwrap();
        script.fail_capture = false;
        script.reject_capture_once = true;
    }
    fixture.captured("Lesson", "Keep the corrected repair");
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls + 1);
    assert_eq!(runner.task.model_requests.last().unwrap()["attempt"], 2);
    assert!(runner.task.recovery.is_none());
    let script = fixture.shared.lock().unwrap();
    assert_eq!(script.capture_requests.len(), 3);
    assert_eq!(
        serde_json::to_value(&script.capture_requests[1]).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert_ne!(
        script.capture_requests[2].operation_id,
        original.operation_id
    );
}

#[tokio::test]
async fn invented_capture_target_is_repaired_before_any_daemon_operation() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.shared.lock().unwrap().usage = Some(json!({"prompt_tokens":31,"completion_tokens":7}));
    fixture.reply("harness_capture", json!({"reason":"The edit establishes a durable constraint.","proposals":[{
        "kind":"Constraint","title":"Preserve behavior","description":"Preserve public behavior.",
        "evidence":["Applied edit code.txt"],"files":["code.txt"],"components":["Ledger"],
        "requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.captured("Constraint", "Preserve observed behavior");
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls + 2);
    assert_eq!(runner.task.model_requests.last().unwrap()["attempt"], 2);
    let decision = runner.task.model_requests.last().unwrap()["decision_id"]
        .as_str()
        .unwrap();
    let metered = metered_decision(&runner, decision);
    assert_eq!(metered.len(), 2);
    assert!(metered
        .iter()
        .all(|r| r["context"]["purpose"] == "harness_capture"
            && r["tokens"]["prompt_tokens"] == 31
            && r["tokens"]["completion_tokens"] == 7
            && r["tokens"]["total_tokens"].is_null()));
    let script = fixture.shared.lock().unwrap();
    assert_eq!(
        script.capture_requests.len(),
        1,
        "invalid target must be rejected locally before minting an operation"
    );
    assert!(script.capture_requests[0].proposals[0]
        .components
        .is_empty());
    assert!(script.capture_requests[0].proposals[0].evidence[0].contains("Task "));
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
}

fn existing_lesson_candidate() -> CaptureCandidate {
    CaptureCandidate {
        iri: "urn:existing".into(),
        title: "Observed behavior".into(),
        kind: "Lesson".into(),
        status: "accepted".into(),
        assertion_digest: "digest-1".into(),
        literals: vec![CandidateLiteral {
            predicate: "hasDescription".into(),
            value: "Keep this evidenced implementation knowledge.".into(),
            datatype: None,
            language: None,
        }],
        relations: vec![],
        origin: None,
        owned_by_requester: false,
        exact_title: true,
        legal_relations: vec![],
    }
}

fn candidate_lookups(fixture: &Fixture) -> usize {
    fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| request["kind"] == "capture_candidates")
        .count()
}

fn resolution_judgments(runner: &Runner) -> usize {
    runner
        .task
        .model_requests
        .iter()
        .filter(|request| request["purpose"] == "harness_capture_resolution")
        .count()
}

#[tokio::test]
async fn rejected_reuse_refetches_candidates_at_current_revision() {
    let fixture = Fixture::new().await;
    {
        let mut script = fixture.shared.lock().unwrap();
        script.capture_candidates = vec![existing_lesson_candidate()];
        // The fixture daemon refuses a reconciliation against a page taken at
        // a superseded revision, as the real daemon does.
        script.reject_stale_candidates = true;
    }
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"The behavior is documented; a second detail was also observed."}));
    fixture.reply("harness_capture", json!({"reason":"One claim repeats durable knowledge; one is new.","proposals":[
        {"kind":"Lesson","title":"Observed behavior","description":"Keep this evidenced implementation knowledge.",
         "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null},
        {"kind":"Lesson","title":"Second observation","description":"A separate implementation detail was observed.",
         "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null}
    ]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"reuse_unchanged","candidate_id":"c0","rationale":"The claim and links are unchanged.","revised_title":null,"revised_description":null}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"distinct_knowledge","candidate_id":"c0","rationale":"The second detail is absent from the candidate.","revised_title":"Second observation","revised_description":"A separate implementation detail was observed."}));
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(runner.task.reviews.len(), 2);
    assert_eq!(candidate_lookups(&fixture), 2);
    let reuse = runner
        .task
        .reviews
        .iter()
        .find(|review| review.capture_resolution.is_some())
        .unwrap()
        .request
        .operation_id
        .clone();
    let capture = runner
        .task
        .reviews
        .iter()
        .find(|review| review.capture_resolution.is_none())
        .unwrap()
        .request
        .operation_id
        .clone();

    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-v2".into());
    runner.review_operation(&capture, true).await.unwrap();
    assert_eq!(runner.task.knowledge_revision, "accepted-v2");
    fixture.reply("harness_capture_resolution", json!({"disposition":"distinct_knowledge","candidate_id":"c0","rationale":"The human rejected reuse; the observation stays distinct.","revised_title":"Observed behavior clarification","revised_description":"Keep this evidenced implementation knowledge."}));
    runner.review_operation(&reuse, false).await.unwrap();
    let reseeded = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(
        reseeded["capture_resolution"]["candidate_pages"],
        json!([]),
        "the rejected reuse must not carry the judged pages forward"
    );
    assert_eq!(reseeded["capture_due"], true);

    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(
        candidate_lookups(&fixture),
        3,
        "rejected reuse must refetch candidates at the current revision"
    );
    assert!(!runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("candidate snapshot is stale")));
    let script = fixture.shared.lock().unwrap();
    assert_eq!(
        script
            .reconciliation_requests
            .last()
            .unwrap()
            .candidate_revision,
        "accepted-v2"
    );
    assert_eq!(script.capture_requests.len(), 2);
    assert_eq!(
        script.capture_requests[1].proposals[0].title,
        "Observed behavior clarification"
    );
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(runner.task.reviews[0].capture_resolution.is_none());
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn persisted_reconciliation_disposition_replays_without_second_model_call() {
    let fixture = Fixture::new().await;
    {
        let mut script = fixture.shared.lock().unwrap();
        script.capture_candidates = vec![existing_lesson_candidate()];
        script.fail_reconcile_once = true;
    }
    let mut runner = fixture.interactive().await;
    fixture
        .conversational(json!({"action":"reply","message":"The behavior is already documented."}));
    fixture.reply("harness_capture", json!({"reason":"The explanation repeats durable knowledge.","proposals":[{
        "kind":"Lesson","title":"Observed behavior","description":"Keep this evidenced implementation knowledge.",
        "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"reuse_unchanged","candidate_id":"c0","rationale":"The claim and links are unchanged.","revised_title":null,"revised_description":null}));
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("daemon HTTP 500") && error.contains("capture/reconcile"),
        "{error}"
    );
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
    assert_eq!(resolution_judgments(&runner), 1);
    let interrupted = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(
        interrupted["capture_resolution"]["disposition"]["choice"], "reuse_unchanged",
        "the judged disposition is durable before the daemon call"
    );
    assert!(runner.task.reviews.is_empty());
    let id = runner.task.id.clone();
    drop(runner);

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(
        resolution_judgments(&runner),
        1,
        "the persisted disposition replays without a second judgment"
    );
    assert_eq!(
        fixture
            .shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request["schema"] == "harness_capture_resolution")
            .count(),
        1
    );
    assert!(runner.task.events.iter().any(|event| event
        .message
        .contains("Replaying the persisted reconciliation disposition")));
    let script = fixture.shared.lock().unwrap();
    assert_eq!(script.reconciliation_requests.len(), 2);
    assert_eq!(
        serde_json::to_value(&script.reconciliation_requests[0]).unwrap(),
        serde_json::to_value(&script.reconciliation_requests[1]).unwrap(),
        "the retried reconciliation must be byte-identical to the interrupted one"
    );
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(
        runner.task.reviews[0]
            .capture_resolution
            .as_ref()
            .unwrap()
            .reuse_unchanged
    );
    assert!(serde_json::to_value(&runner.task).unwrap()["capture_resolution"].is_null());
    assert!(script.capture_requests.is_empty());
    assert!(script.replies.is_empty());
}

/// The same accepted candidate, but with a claim larger than the whole prompt
/// budget (84,992 bytes at the fixture's 32,768-token window), so semantic
/// reconciliation can never reach the model and must fall back to the
/// human-required card.
fn oversized_lesson_candidate() -> CaptureCandidate {
    let mut candidate = existing_lesson_candidate();
    candidate.literals[0].value = format!(
        "Keep this evidenced implementation knowledge. {}",
        "x".repeat(100_000)
    );
    candidate
}

fn reconcile_posts(fixture: &Fixture) -> usize {
    fixture.shared.lock().unwrap().reconciliation_requests.len()
}

fn intent_details(runner: &Runner, kind: &str) -> Vec<String> {
    runner
        .task
        .intent_events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.detail.clone())
        .collect()
}

fn reuse_proposal_reply(fixture: &Fixture) {
    fixture
        .conversational(json!({"action":"reply","message":"The behavior is already documented."}));
    fixture.reply("harness_capture", json!({"reason":"The explanation repeats durable knowledge.","proposals":[{
        "kind":"Lesson","title":"Observed behavior","description":"Keep this evidenced implementation knowledge.",
        "evidence":["Model action:"],"files":[],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
}

#[tokio::test]
async fn rejected_human_required_reuse_card_keeps_observation_unresolved() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_candidates = vec![oversized_lesson_candidate()];
    let mut runner = fixture.interactive().await;
    // No harness_capture_resolution reply is scripted: the oversized candidate
    // must never be judged by the model.
    reuse_proposal_reply(&fixture);
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    let card = runner.task.reviews[0].capture_resolution.clone().unwrap();
    assert_eq!(card.recommendation_source, "human_required");
    assert!(card.reuse_unchanged);
    assert_eq!(card.candidate_iri, "urn:existing");
    assert_eq!(resolution_judgments(&runner), 0);
    assert_eq!(candidate_lookups(&fixture), 1);
    assert_eq!(
        reconcile_posts(&fixture),
        1,
        "the runner posts capture/reconcile before pushing the card"
    );
    assert!(runner.task.capture_resolution.is_none());
    let operation = runner.task.reviews[0].request.operation_id.clone();

    runner.review_operation(&operation, false).await.unwrap();
    assert!(runner.task.reviews.is_empty());
    assert!(runner.task.capture_resolution.is_none());
    assert!(runner.task.capture_batch.is_none());
    assert_eq!(
        candidate_lookups(&fixture),
        1,
        "rejecting the oversized card must not refetch candidates"
    );
    assert_eq!(reconcile_posts(&fixture), 1);
    assert_eq!(
        intent_details(&runner, "reuse_unresolved"),
        vec![operation.clone()]
    );
    assert_eq!(
        intent_details(&runner, "reuse_review"),
        vec![format!("rejected {operation}")]
    );
    assert!(runner.task.events.iter().any(|event| event.message
        == "Observation retained unresolved after the human rejected the oversized reuse candidate urn:existing."));
    let journal = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(journal["capture_due"], false);
    assert_ne!(runner.task.phase, Phase::AwaitingReview);
    assert_ne!(runner.task.phase, Phase::Planning);
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingInput,
        "the task continues exactly as after an accepted card"
    );

    // Before the fix each rejection reseeded reconciliation, refetched the
    // same oversized candidate and minted a fresh human-required card. Now
    // the turn is over: further advances wait for the human and never post.
    for _ in 0..3 {
        let error = runner.advance().await.unwrap_err().to_string();
        assert!(error.contains("waiting for a human action"), "{error}");
        assert!(runner.task.reviews.is_empty());
        assert!(runner.task.capture_resolution.is_none());
    }
    assert_eq!(reconcile_posts(&fixture), 1);
    assert_eq!(candidate_lookups(&fixture), 1);
    assert_eq!(intent_details(&runner, "reuse_unresolved").len(), 1);

    // The conversation proceeds normally on the next human turn.
    runner
        .submit_message("Thanks; continue.".into())
        .await
        .unwrap();
    fixture.conversational(json!({"action":"reply","message":"Continuing."}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert!(runner.task.turn_finished);
    assert!(runner.task.reviews.is_empty());
    assert_eq!(reconcile_posts(&fixture), 1);
    assert_eq!(candidate_lookups(&fixture), 1);
    assert_eq!(intent_details(&runner, "reuse_unresolved").len(), 1);
    let script = fixture.shared.lock().unwrap();
    assert!(script.capture_requests.is_empty());
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn oversized_candidate_already_rejected_is_not_re_presented() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_candidates = vec![existing_lesson_candidate()];
    let mut runner = fixture.interactive().await;
    reuse_proposal_reply(&fixture);
    fixture.reply("harness_capture_resolution", json!({"disposition":"reuse_unchanged","candidate_id":"c0","rationale":"The claim and links are unchanged.","revised_title":null,"revised_description":null}));
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(runner.task.reviews.len(), 1);
    let card = runner.task.reviews[0].capture_resolution.clone().unwrap();
    assert_eq!(card.recommendation_source, "model");
    assert_eq!(card.candidate_iri, "urn:existing");
    assert_eq!(candidate_lookups(&fixture), 1);
    assert_eq!(reconcile_posts(&fixture), 1);
    let operation = runner.task.reviews[0].request.operation_id.clone();

    // A rejected model-path reuse still restarts reconciliation, seeded with
    // the rejected candidate IRI, and refetches at the current revision.
    runner.review_operation(&operation, false).await.unwrap();
    let reseeded = runner.task.capture_resolution.clone().unwrap();
    assert_eq!(reseeded.rejected_candidate_iris, vec!["urn:existing"]);
    assert!(reseeded.candidate_pages.is_empty());
    assert_ne!(reseeded.operation_id, operation);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        true
    );
    // The refetched page now carries the same record grown past the model
    // context, so the only route left is the human-required card the human
    // has already declined for this pairing.
    fixture.shared.lock().unwrap().capture_candidates = vec![oversized_lesson_candidate()];

    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(
        candidate_lookups(&fixture),
        2,
        "the restarted reconciliation refetches candidates once"
    );
    assert!(runner.task.reviews.is_empty(), "no new review card");
    assert!(runner.task.capture_resolution.is_none());
    assert!(runner.task.capture_batch.is_none(), "the batch completed");
    assert_eq!(
        reconcile_posts(&fixture),
        1,
        "capture/reconcile is not posted for the already-rejected pairing"
    );
    assert_eq!(
        resolution_judgments(&runner),
        1,
        "the oversized candidate never reaches the model"
    );
    let unresolved = intent_details(&runner, "reuse_unresolved");
    assert_eq!(unresolved.len(), 1, "{unresolved:?}");
    assert!(unresolved[0].contains("urn:existing"), "{unresolved:?}");
    assert!(
        unresolved[0].contains(&reseeded.operation_id),
        "{unresolved:?}"
    );
    assert!(runner.task.events.iter().any(|event| event.message
        == "Observation retained unresolved: the only reuse candidate urn:existing exceeds the model context and was already rejected."));
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    let script = fixture.shared.lock().unwrap();
    assert!(
        script.capture_requests.is_empty(),
        "the unresolved proposal is absent from the outgoing capture"
    );
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn journal_without_last_error_kind_deserializes() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let fresh = serde_json::to_value(&runner.task).unwrap();
    assert!(
        fresh.get("last_error_kind").is_none(),
        "an absent kind is not serialized"
    );
    runner.task.last_error = Some("fixture failure".into());
    runner.task.last_error_kind = Some("service".into());
    let mut journal = serde_json::to_value(&runner.task).unwrap();
    assert_eq!(journal["last_error_kind"], "service");
    journal.as_object_mut().unwrap().remove("last_error_kind");
    let legacy: moosedev::harness::runner::Task = serde_json::from_value(journal).unwrap();
    assert_eq!(legacy.last_error.as_deref(), Some("fixture failure"));
    assert!(legacy.last_error_kind.is_none());
}
