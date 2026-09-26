//! The scripted daemon and sensor every runner test drives: one axum router
//! answers the model endpoint and every harness route from a `Script`, and
//! `Fixture` builds runners against it. Helpers here are shared by the core,
//! link-review and symbolic test modules; nothing in this file is a test.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use moosedev::harness::daemon::intent::{
    IntentBinding, IntentEntity, IntentLinkRequest, IntentLinkResponse, IntentResolveRequest,
    IntentResolveResponse,
};
use moosedev::harness::digest::sha256_hex;
use moosedev::harness::protocol::*;
use moosedev::harness::response::ActionContract;
use moosedev::harness::runner::{CheckResult, Phase, Runner};
use moosedev::policy::{GateDisposition, PolicyDecision};
use serde_json::{json, Value};

/// Serializes the tests that mutate process-wide state (the environment,
/// the working directory). A runner's model comes from `Fixture::config`,
/// never from the environment.
pub(super) static ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
pub(super) struct Script {
    pub(super) root: PathBuf,
    pub(super) usage: Option<Value>,
    pub(super) context: Option<String>,
    /// Governing rules the full context returns.
    pub(super) governing_rules: Vec<GoverningRule>,
    /// Records the context route returns, with the claims it supplied.
    pub(super) context_records: Vec<ContextRecord>,
    /// Governing-rule claims come only with a rule-claim budget, as claims
    /// past the daemon's fixed floor do.
    pub(super) rule_claims_need_budget: bool,
    /// Rules the full context adds only when its request names the file.
    pub(super) file_rules: Vec<(String, GoverningRule)>,
    /// Approved specs the full context reports.
    pub(super) approved_specs: Vec<ApprovedSpecStatus>,
    /// What `ground` answers (default: nothing to ground), and what it was asked.
    pub(super) ground_response: Option<GroundResponse>,
    pub(super) ground_requests: Vec<GroundRequest>,
    /// What `ground/plan` answers (default: nothing defined), and what it was asked.
    pub(super) plan_ground_response: Option<GroundResponse>,
    pub(super) plan_ground_requests: Vec<PlanGroundRequest>,
    /// Accepted knowledge an evidence-only (search) request returns.
    pub(super) search_knowledge: Option<String>,
    pub(super) replies: VecDeque<(&'static str, Value)>,
    pub(super) requests: Vec<Value>,
    pub(super) capture_requests: Vec<CaptureV2Request>,
    pub(super) fail_capture_once: bool,
    pub(super) fail_capture: bool,
    pub(super) fail_context: bool,
    /// One definite pre-persistence rejection (HTTP 400) of the next capture.
    pub(super) reject_capture_once: bool,
    /// Every capture is rejected before persistence.
    pub(super) reject_capture: bool,
    /// The next capture answers with a title collision instead of persisting.
    pub(super) collide_capture_once: bool,
    pub(super) deny_edit: bool,
    pub(super) revision: String,
    pub(super) checkpoint_durable: bool,
    pub(super) reviewed: Vec<String>,
    pub(super) revision_on_accept: Option<String>,
    pub(super) attest_review: bool,
    pub(super) reject_stale_review: bool,
    /// The expected-revision header of every review request, in order.
    pub(super) review_headers: Vec<Option<String>>,
    pub(super) fail_global_checkpoint_once: bool,
    pub(super) mutation_during_model: Option<String>,
    pub(super) held_response: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    pub(super) intent_records: Vec<CaptureTarget>,
    pub(super) intent_stale_source: bool,
    pub(super) intent_link_requests: Vec<moosedev::harness::daemon::intent::IntentLinkRequest>,
    pub(super) intent_accepted: Vec<moosedev::harness::daemon::intent::IntentBinding>,
    pub(super) capture_links_per_proposal: usize,
    pub(super) malformed_intent_response: bool,
    /// The next `intent/link` answers with this status instead of a receipt.
    pub(super) intent_link_status: Option<u16>,
    /// Every `intent/associate` answers with this status instead of a page.
    pub(super) associate_status: Option<u16>,
    pub(super) capture_type_reply: Option<Vec<TypedProposal>>,
    pub(super) capture_type_requests: Vec<CaptureTypeRequest>,
    /// Every review request received, in order.
    pub(super) review_requests: Vec<ReviewRequest>,
    pub(super) fail_capture_type_once: bool,
    pub(super) fail_capture_type: bool,
    /// Every `capture/type` answers with this status instead of a typing.
    pub(super) capture_type_status: Option<u16>,
    /// Action requests with `tool_choice: "required"` are refused (HTTP 400).
    pub(super) reject_required_tool_choice: bool,
}

pub(super) type Shared = Arc<Mutex<Script>>;

pub(super) async fn model(
    State(state): State<Shared>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let held = if request_schema(&body) != "harness_response_probe" {
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

/// The sensor stage a model request is for: the probe's `ready` tool, other
/// tool requests (actions), or the JSON schema name.
pub(super) fn request_schema(body: &Value) -> String {
    match body["tools"].as_array() {
        Some(tools) if tools.iter().any(|tool| tool["function"]["name"] == "ready") => {
            "harness_response_probe".into()
        }
        Some(_) => "harness_action".into(),
        None => body["response_format"]["json_schema"]["name"]
            .as_str()
            .unwrap_or("")
            .into(),
    }
}

/// The tool-contract form of a scripted answer. A raw `{content, tool_calls?,
/// finish_reason?}` answer is sent as given; an action (conversational or single)
/// becomes one native tool call; anything else is plain text content.
pub(super) fn tool_message(answer: &Value) -> (Value, String) {
    if let Some(raw) = answer
        .as_object()
        .filter(|raw| raw.contains_key("content") || raw.contains_key("tool_calls"))
    {
        let calls = raw.get("tool_calls").cloned().unwrap_or(json!([]));
        let finish = raw
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if calls.as_array().is_some_and(|calls| !calls.is_empty()) {
                    "tool_calls".into()
                } else {
                    "stop".into()
                }
            });
        let content = raw.get("content").cloned().unwrap_or(json!(""));
        return (
            json!({"role":"assistant","content":content,"tool_calls":calls}),
            finish,
        );
    }
    let (message, action) = match answer.get("action") {
        Some(Value::Object(action)) => (
            answer.get("message").and_then(Value::as_str).unwrap_or(""),
            Some(action.clone()),
        ),
        Some(Value::String(_)) => ("", answer.as_object().cloned()),
        _ => ("", None),
    };
    let Some(mut action) = action else {
        return (
            json!({"role":"assistant","content":answer.to_string()}),
            "stop".into(),
        );
    };
    let name = action
        .remove("action")
        .and_then(|name| name.as_str().map(str::to_owned))
        .unwrap_or_default();
    (
        json!({"role":"assistant","content":message,"tool_calls":[{
            "id":"call-0","type":"function",
            "function":{"name":name,"arguments":Value::Object(action).to_string()}
        }]}),
        "tool_calls".into(),
    )
}

pub(super) fn model_response(state: Shared, body: Value) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    let schema = request_schema(&body);
    let name = schema.as_str();
    let tools = body["tools"].is_array();
    if name == "harness_response_probe" {
        let mut response = if tools {
            json!({"choices":[{
                "message":{"role":"assistant","content":"","tool_calls":[{
                    "id":"probe","type":"function",
                    "function":{"name":"ready","arguments":"{\"status\":\"ok\"}"}
                }]},
                "finish_reason":"tool_calls"
            }]})
        } else {
            json!({"choices":[{
                "message":{"role":"assistant","content":"{\"status\":\"ok\"}"},
                "finish_reason":"stop"
            }]})
        };
        if let Some(usage) = &script.usage {
            response["usage"] = usage.clone();
        }
        return (StatusCode::OK, Json(response));
    }
    let refuse = tools && script.reject_required_tool_choice && body["tool_choice"] == "required";
    script
        .requests
        .push(json!({"kind":"model","schema":name,"body":body}));
    if refuse {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"tool_choice 'required' is not supported"})),
        );
    }
    let Some((expected, answer)) = script.replies.pop_front() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"unexpected model invocation"})),
        );
    };
    assert_eq!(name, expected, "harness called the wrong sensor stage");
    if name == "harness_action" {
        if let Some(content) = script.mutation_during_model.take() {
            std::fs::write(script.root.join("code.txt"), content).unwrap();
        }
    }
    let mut response = if tools {
        let (message, finish) = tool_message(&answer);
        json!({"choices":[{"message":message,"finish_reason":finish}]})
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":answer.to_string()},"finish_reason":"stop"}]})
    };
    if let Some(usage) = &script.usage {
        response["usage"] = usage.clone();
    }
    (StatusCode::OK, Json(response))
}

pub(super) async fn context(
    State(state): State<Shared>,
    Json(request): Json<ContextRequest>,
) -> (StatusCode, Json<ContextResponse>) {
    let mut script = state.lock().unwrap();
    if request.evidence_only {
        script.requests.push(json!({
            "kind":"knowledge_search",
            "topic": request.topic,
            "max_bytes": request.max_bytes,
        }));
        let knowledge = script.search_knowledge.clone();
        let delivery_receipt = knowledge.as_ref().map(|context| ContextDeliveryReceipt {
            max_bytes: request.max_bytes,
            context_bytes: context.len(),
            records: vec![ContextRecordDelivery {
                iri: "urn:fixture:search-knowledge".into(),
                kind: "Constraint".into(),
                tier: ContextRecordDeliveryTier::FullClaim,
                reason: "fixture record fit within caller byte budget".into(),
            }],
        });
        return (
            StatusCode::OK,
            Json(ContextResponse {
                capture_contracts: vec![2, 3],
                intent_contracts: vec![2],
                context_contracts: vec![1, 2],
                project_root: script.root.to_string_lossy().into_owned(),
                revision: script.revision.clone(),
                evidence_iris: knowledge
                    .iter()
                    .map(|_| "urn:fixture:search-knowledge".to_string())
                    .collect(),
                delivery_receipt,
                context: knowledge.unwrap_or_default(),
                files: vec![],
                records: vec![],
                governing_rules: vec![],
                approved_specs: vec![],
            }),
        );
    }
    script
        .requests
        .push(json!({"kind":"context","files":request.files,"rule_files":request.rule_files,"rule_claim_bytes":request.rule_claim_bytes}));
    let status = if script.fail_context {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(ContextResponse {
            capture_contracts: vec![2, 3],
            intent_contracts: vec![2],
            context_contracts: vec![1, 2],
            project_root: script.root.to_string_lossy().into_owned(),
            revision: script.revision.clone(),
            evidence_iris: vec![],
            delivery_receipt: None,
            records: script.context_records.clone(),
            governing_rules: script
                .governing_rules
                .iter()
                .cloned()
                .map(|mut rule| {
                    if script.rule_claims_need_budget && request.rule_claim_bytes.is_none() {
                        rule.claim.clear();
                    }
                    rule
                })
                .chain(
                    script
                        .file_rules
                        .iter()
                        .filter(|(file, _)| {
                            request.files.contains(file) || request.rule_files.contains(file)
                        })
                        .map(|(_, rule)| rule.clone()),
                )
                .collect(),
            approved_specs: script.approved_specs.clone(),
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

pub(super) async fn capture_v2(
    State(state): State<Shared>,
    Json(request): Json<CaptureV2Request>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"capture","operation_id":request.operation_id}));
    script.capture_requests.push(request.clone());
    if script.reject_capture || std::mem::take(&mut script.reject_capture_once) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"component does not exist; capture was not persisted"})),
        );
    }
    if std::mem::take(&mut script.collide_capture_once) {
        return (
            StatusCode::OK,
            Json(
                serde_json::to_value(CaptureV2Response::Collision {
                    collisions: vec![CaptureCollision {
                        proposal_index: 0,
                        candidate_iris: vec!["urn:existing-title".into()],
                    }],
                })
                .unwrap(),
            ),
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
            // One definition anchor per proposal file the capture changed.
            anchors: proposal
                .files
                .iter()
                .filter(|file| request.changed.iter().any(|changed| &changed.file == *file))
                .map(|file| CaptureAnchor {
                    file: file.clone(),
                    symbol: fixture_symbol(file, "changed"),
                    name: None,
                    basis: AnchorBasis::Definition,
                })
                .collect(),
            anchor_notes: vec![],
        })
        .collect();
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(CaptureV2Response::Captured {
                capture: CaptureResponse {
                    proposals,
                    // One queued link per restated record.
                    restated: request
                        .restated
                        .iter()
                        .enumerate()
                        .map(|(index, candidate)| RestatedLinks {
                            candidate_iri: candidate.candidate_iri.clone(),
                            links: vec![format!(
                                "https://moosedev.dev/kg/ProposedLink/{}-restated-{index}",
                                request.operation_id
                            )],
                            anchors: vec![],
                            anchor_notes: vec![],
                        })
                        .collect(),
                },
            })
            .unwrap(),
        ),
    )
}

pub(super) fn checked(revision: &str, durable: bool, pending: Vec<String>) -> CheckpointResponse {
    CheckpointResponse {
        conforms: true,
        durable,
        revision: revision.into(),
        pending,
    }
}

pub(super) async fn review(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<ReviewRequest>,
) -> (StatusCode, HeaderMap, Json<CheckpointResponse>) {
    let mut script = state.lock().unwrap();
    let base = script.revision.clone();
    let expected = headers
        .get("x-moosedev-expected-revision")
        .and_then(|value| value.to_str().ok());
    script.review_headers.push(expected.map(str::to_owned));
    script.review_requests.push(request.clone());
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

pub(super) async fn checkpoint(
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

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) url: String,
    pub(super) shared: Shared,
    pub(super) server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub(super) async fn new() -> Self {
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
            .route("/api/v1/harness/capture/v2", post(capture_v2))
            .route("/api/v1/harness/review", post(review))
            .route("/api/v1/harness/checkpoint", post(checkpoint))
            .route("/api/v1/harness/intent/resolve", post(resolve))
            .route("/api/v1/harness/ground", post(ground))
            .route("/api/v1/harness/ground/plan", post(ground_plan))
            .route("/api/v1/harness/intent/associate", post(associate))
            .route("/api/v1/harness/capture/type", post(capture_type))
            .route("/api/v1/harness/intent/link", post(link))
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
    pub(super) fn reply(&self, schema: &'static str, reply: Value) {
        self.shared
            .lock()
            .unwrap()
            .replies
            .push_back((schema, reply));
    }
    /// The one final capture note the model is asked for.
    pub(super) fn note(&self, text: &str) {
        self.reply("harness_capture_note", json!({"note":text}));
    }
    /// What the scripted daemon types the next note into.
    pub(super) fn typed(&self, proposals: Vec<TypedProposal>) {
        self.shared.lock().unwrap().capture_type_reply = Some(proposals);
    }
    pub(super) fn typed_one(&self, kind: &str, title: &str) {
        self.typed(vec![distinct_proposal(kind, title)]);
    }
    pub(super) fn edit(&self) {
        self.reply(
            "harness_action",
            json!({"action":"replace","file":"code.txt","old_text":"original\n","new_text":"changed\n"}),
        );
    }
    pub(super) fn last_model_prompt(&self, schema: &str) -> String {
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
    pub(super) fn model_calls(&self) -> usize {
        requests_of_kind(self, "model").len()
    }
    pub(super) fn note_calls(&self) -> usize {
        requests_of_kind(self, "model")
            .iter()
            .filter(|r| r["schema"] == "harness_capture_note")
            .count()
    }
    pub(super) fn capture_ids(&self) -> Vec<String> {
        self.shared
            .lock()
            .unwrap()
            .capture_requests
            .iter()
            .map(|r| r.operation_id.clone())
            .collect()
    }
    pub(super) fn typing_ids(&self) -> Vec<String> {
        self.shared
            .lock()
            .unwrap()
            .capture_type_requests
            .iter()
            .map(|r| r.operation_id.clone())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(super) fn distinct_proposal(kind: &str, title: &str) -> TypedProposal {
    TypedProposal {
        proposal: KnowledgeProposal {
            kind: kind.into(),
            title: title.into(),
            description: "Keep this evidenced implementation knowledge.".into(),
            evidence: vec!["Event 0: capture note".into()],
            files: vec!["code.txt".into()],
            components: vec![],
            requirement: None,
            motivated_by: Vec::new(),
            supersedes: None,
            retracts: None,
            learned_from: None,
            reconciled: vec![],
        },
        origin: ProposalOrigin::SymbolicDecision,
        disposition: TypedDisposition::Distinct {
            nearest_iri: None,
            score: None,
            receipt_operation_id: "fixture-receipt".into(),
        },
        resolved_by: "symbolic".into(),
        derived: vec![],
        names_rules: vec![],
    }
}

pub(super) fn passed_check() -> CheckResult {
    CheckResult {
        command: "true".into(),
        success: true,
        output: "fixture: successful check already observed".into(),
    }
}

pub(super) fn intent_details(runner: &Runner, kind: &str) -> Vec<String> {
    runner
        .task
        .intent_events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.detail.clone())
        .collect()
}

pub(super) fn journal_value(runner: &Runner) -> Value {
    serde_json::to_value(&runner.task).unwrap()
}

pub(super) fn metered_decision(runner: &Runner, decision: &str) -> Vec<Value> {
    journal_value(runner)["token_usage"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["context"]["decision_id"] == decision)
        .cloned()
        .collect()
}

impl Fixture {
    pub(super) fn conversational(&self, action: Value) {
        self.reply(
            "harness_action",
            json!({"message":"I am working through the request.","action":action}),
        );
    }

    pub(super) async fn interactive(&self) -> Runner {
        self.interactive_objective("Repair code.txt while preserving behavior")
            .await
    }

    pub(super) async fn interactive_objective(&self, objective: &str) -> Runner {
        let mut runner = Runner::create(self.root.clone(), self.url.clone(), objective.into())
            .await
            .unwrap();
        runner.configure(self.config(), None);
        runner.enable_interactive().unwrap();
        runner
    }

    pub(super) fn config(&self) -> moosedev::llm::LlmConfig {
        moosedev::llm::LlmConfig {
            base_url: format!("{}/v1", self.url),
            api_key: "fixture".into(),
            model: "scripted-local-model".into(),
            configured: true,
            context_window_tokens: 32768,
            structured_output: moosedev::llm::StructuredOutputMode::Required,
            timeouts: Default::default(),
        }
    }

    /// Plan approved for `code.txt` in interactive mode. The plan checkpoint
    /// journals without a model call and lands directly on plan approval.
    pub(super) async fn approved_interactive(&self) -> Runner {
        self.approved_interactive_with(ActionContract::Tools).await
    }

    /// The same approved task under an explicit action contract.
    pub(super) async fn approved_interactive_with(&self, contract: ActionContract) -> Runner {
        let mut runner = self.interactive().await;
        runner.set_action_contract(contract);
        self.conversational(json!({"action":"read","file":"code.txt"}));
        runner.advance().await.unwrap();
        self.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["true"]}));
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::AwaitingPlan);
        runner.approve_plan().await.unwrap();
        runner
    }

    /// One applied edit to the ungoverned `code.txt`, finish, and a passing
    /// required check: the next advance runs the final capture checkpoint.
    pub(super) async fn ready_for_final(&self) -> Runner {
        let mut runner = self.approved_interactive().await;
        self.edit();
        runner.advance().await.unwrap();
        assert_eq!(runner.task.edits.len(), 1);
        let calls = self.model_calls();
        runner.advance().await.unwrap();
        assert_eq!(
            self.model_calls(),
            calls,
            "an intermediate checkpoint journals without a model call"
        );
        assert_eq!(runner.task.phase, Phase::Working);
        assert!(runner.task.reviews.is_empty());
        self.conversational(json!({"action":"finish","summary":"Ready for required checks."}));
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::Verifying);
        runner.task.check_results = vec![passed_check()];
        runner
    }
}

/// The fixture index: every `def` in a Python file is a Function, by line.
/// Non-code fixture bodies still expose one synthetic indexed function.
pub(super) fn python_defs(content: &str) -> Vec<(usize, String)> {
    let mut names: Vec<(usize, String)> = content
        .lines()
        .enumerate()
        .filter_map(|(line, text)| {
            text.strip_prefix("def ")?
                .split_once('(')
                .map(|(name, _)| (line, name.to_owned()))
        })
        .collect();
    if names.is_empty() {
        names.push((0, "render_name".into()));
    }
    names
}

/// The normalized SCIP symbol the fixture index assigns to a definition.
pub(super) fn fixture_symbol(file: &str, name: &str) -> String {
    format!("scip-python python fixture . {file}/{name}().")
}

/// Records the human has already accepted as linked to this definition.
pub(super) fn existing_links(script: &Script, file: &str, symbol: &str) -> Vec<String> {
    script
        .intent_accepted
        .iter()
        .filter(|binding| binding.file == file && binding.symbol == symbol)
        .map(|binding| binding.record_iri.clone())
        .collect()
}

/// Mock of `ground`: the scripted answer, recording each request.
pub(super) async fn ground(
    State(state): State<Shared>,
    Json(request): Json<GroundRequest>,
) -> Json<GroundResponse> {
    let mut script = state.lock().unwrap();
    script.ground_requests.push(request);
    Json(script.ground_response.clone().unwrap_or_default())
}

/// Mock of `ground/plan`: the scripted answer, recording each request.
pub(super) async fn ground_plan(
    State(state): State<Shared>,
    Json(request): Json<PlanGroundRequest>,
) -> Json<GroundResponse> {
    let mut script = state.lock().unwrap();
    script.plan_ground_requests.push(request);
    Json(script.plan_ground_response.clone().unwrap_or_default())
}

/// Mock of `intent/resolve`: every `def` in a plan file is an indexed Function
/// whose dossier records are the bindings the human has accepted for it.
pub(super) async fn resolve(
    State(state): State<Shared>,
    Json(request): Json<IntentResolveRequest>,
) -> Json<IntentResolveResponse> {
    let mut script = state.lock().unwrap();
    script.requests.push(json!({"kind":"intent_resolve","refresh_index":request.refresh_index,"files":request.files}));
    let mut entities = Vec::new();
    for file in &request.files {
        let Ok(content) = std::fs::read_to_string(script.root.join(file)) else {
            continue;
        };
        for (_, name) in python_defs(&content) {
            let symbol = fixture_symbol(file, &name);
            entities.push(IntentEntity {
                handle: format!("entity_{}", entities.len()),
                symbol: symbol.clone(),
                file: file.clone(),
                name,
                source_digest: if script.intent_stale_source {
                    "stale-digest".into()
                } else {
                    sha256_hex(&content)
                },
                dossier_records: existing_links(&script, file, &symbol),
            });
        }
    }
    Json(IntentResolveResponse {
        revision: script.revision.clone(),
        records: script.intent_records.clone(),
        entities,
        unresolved: Vec::new(),
    })
}

/// Mock of `intent/link`: one proposed link per binding, recorded before any
/// scripted failure so a retry can be compared with the interrupted attempt.
pub(super) async fn link(
    State(state): State<Shared>,
    Json(request): Json<IntentLinkRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"intent_link","operation_id":request.operation_id}));
    script.intent_link_requests.push(request.clone());
    if let Some(status) = script.intent_link_status.take() {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":"scripted link failure"})),
        );
    }
    let mut links: Vec<_> = request
        .bindings
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                "https://moosedev.dev/kg/ProposedLink/{}-{index}",
                request.operation_id
            )
        })
        .collect();
    if script.malformed_intent_response && links.len() > 1 {
        links[1] = links[0].clone();
    }
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(IntentLinkResponse {
                links,
                resolved: request.bindings,
                unresolved: Vec::new(),
            })
            .unwrap(),
        ),
    )
}

pub(super) fn journal_path(fixture: &Fixture, id: &str) -> PathBuf {
    fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"))
}

pub(super) fn reload(fixture: &Fixture, id: &str) -> Runner {
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), id).unwrap();
    runner.configure(fixture.config(), None);
    runner
}

/// Approved symbolic plan with the helper edit applied and its intermediate
/// checkpoint journaled: the next `finish` derives the association batch.
pub(super) async fn edited_symbolic_runner(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    add_helper(fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    runner
}

pub(super) fn finish(fixture: &Fixture) {
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
}

pub(super) fn link_operation_ids(fixture: &Fixture) -> Vec<String> {
    fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .iter()
        .map(|request| request.operation_id.clone())
        .collect()
}

/// Mock of `intent/associate`: every `def` in a changed file is a Function
/// definition; each governing record not already linked to it binds with the
/// predicate its kind allows.
pub(super) async fn associate(
    State(state): State<Shared>,
    Json(request): Json<AssociateRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"intent_associate","request":request}));
    if let Some(status) = script.associate_status {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":"scripted association failure"})),
        );
    }
    let mut bindings = Vec::new();
    let mut skipped = Vec::new();
    let mut ungoverned = Vec::new();
    for changed in &request.files {
        let content = std::fs::read_to_string(script.root.join(&changed.file)).unwrap_or_default();
        let governing = request
            .governing
            .get(&changed.file)
            .cloned()
            .unwrap_or_default();
        if governing.is_empty() {
            ungoverned.push(changed.file.clone());
            continue;
        }
        for (line, name) in python_defs(&content) {
            let symbol = fixture_symbol(&changed.file, &name);
            let existing = existing_links(&script, &changed.file, &symbol);
            for (ordinal, iri) in governing.iter().enumerate() {
                let kind = script
                    .intent_records
                    .iter()
                    .find(|record| &record.iri == iri)
                    .map(|record| record.kind.clone())
                    .unwrap_or_else(|| "Requirement".into());
                if existing.contains(iri) {
                    skipped.push(SkippedScope {
                        file: changed.file.clone(),
                        symbol: symbol.clone(),
                        kind: Some("Function".into()),
                        reason: SkipReason::AlreadyLinked,
                        record_iri: Some(iri.clone()),
                    });
                    continue;
                }
                bindings.push(DerivedBinding {
                    file: changed.file.clone(),
                    symbol: symbol.clone(),
                    name: Some(name.clone()),
                    kind: Some("Function".into()),
                    definition_range: HarnessSourceRange {
                        start: HarnessSourcePosition {
                            line: line as u32,
                            col: 4,
                        },
                        end: HarnessSourcePosition {
                            line: line as u32,
                            col: 4 + name.len() as u32,
                        },
                    },
                    scope_basis: IntentScopeBasis::ChangedDefinition,
                    source_digest: sha256_hex(&content),
                    record_iri: iri.clone(),
                    record_kind: kind.clone(),
                    assertion_digest: format!("digest-{ordinal}"),
                    predicate: if kind == "Constraint" {
                        "constrains"
                    } else {
                        "concerns"
                    }
                    .into(),
                    basis: DerivedBasis::Obligation,
                    candidate_digest: sha256_hex(format!("{}|{symbol}|{iri}", changed.file)),
                });
            }
        }
    }
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(AssociatePage {
                knowledge_revision: script.revision.clone(),
                index: IntentIndexSnapshot {
                    revision: Some("fixture-index".into()),
                    producer: Some("scip-python".into()),
                    status: IntentIndexStatus::Current,
                    refresh_action: IntentRefreshAction::NotRequested,
                },
                scope_digest: "fixture-scope".into(),
                bindings,
                skipped,
                ungoverned,
                unresolved: Vec::new(),
            })
            .unwrap(),
        ),
    )
}

/// Mock of `capture/type`: by default the note becomes one distinct
/// ArchitecturalDecision titled by the plan summary; a scripted reply
/// overrides that.
pub(super) async fn capture_type(
    State(state): State<Shared>,
    Json(request): Json<CaptureTypeRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"capture_type","operation_id":request.operation_id}));
    script.capture_type_requests.push(request.clone());
    if let Some(status) = script.capture_type_status {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":"scripted typing rejection"})),
        );
    }
    if script.fail_capture_type || std::mem::take(&mut script.fail_capture_type_once) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"simulated lost typing acknowledgment"})),
        );
    }
    let proposals = match &script.capture_type_reply {
        Some(proposals) => proposals.clone(),
        None => vec![TypedProposal {
            proposal: KnowledgeProposal {
                kind: "ArchitecturalDecision".into(),
                title: request.plan_summary.clone(),
                description: request.note.clone(),
                evidence: request.note_evidence.clone(),
                files: request.changed_files.clone(),
                components: vec![],
                requirement: None,
                motivated_by: Vec::new(),
                supersedes: None,
                retracts: None,
                learned_from: None,
                reconciled: vec![],
            },
            origin: ProposalOrigin::SymbolicDecision,
            disposition: TypedDisposition::Distinct {
                nearest_iri: None,
                score: None,
                receipt_operation_id: format!("{}-r0", request.operation_id),
            },
            resolved_by: "symbolic".into(),
            derived: vec![],
            names_rules: vec![],
        }],
    };
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(CaptureTypeResponse {
                revision: script.revision.clone(),
                typing_mode: TypingMode::SymbolicOnly,
                typing_note: Some("fixture typing".into()),
                thresholds: ReconcileThresholds::default(),
                dropped: vec![],
                proposals,
            })
            .unwrap(),
        ),
    )
}

pub(super) const PRESERVE: &str = "https://moosedev.dev/kg/Requirement/preserve-labels";
pub(super) const UNLINKED: &str = "https://moosedev.dev/kg/Constraint/unlinked";
pub(super) const RENDER_NAME: &str = "scip-python python fixture . labels.py/render_name().";
pub(super) const NORMALIZE: &str = "scip-python python fixture . labels.py/normalize().";

/// Two accepted records exist; only one is linked to a definition in the plan
/// file. The direct dossier rule makes that one the obligation.
pub(super) async fn symbolic_fixture() -> Fixture {
    let fixture = Fixture::new().await;
    std::fs::write(
        fixture.root.join("labels.py"),
        "def render_name(name):\n    return name\n",
    )
    .unwrap();
    let mut script = fixture.shared.lock().unwrap();
    script.intent_records = vec![
        CaptureTarget {
            iri: PRESERVE.into(),
            label: "Preserve display label behavior".into(),
            kind: "Requirement".into(),
        },
        CaptureTarget {
            iri: UNLINKED.into(),
            label: "Labels never exceed one line".into(),
            kind: "Constraint".into(),
        },
    ];
    script.intent_accepted = vec![IntentBinding {
        record_iri: PRESERVE.into(),
        file: "labels.py".into(),
        symbol: RENDER_NAME.into(),
        source_digest: None,
    }];
    drop(script);
    fixture
}

pub(super) async fn planned_symbolic_runner(fixture: &Fixture) -> Runner {
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner
}

/// The edit every association scenario applies: a new `normalize` helper
/// next to the governed `render_name`.
pub(super) fn add_helper(fixture: &Fixture) {
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return normalize(name)\n\ndef normalize(value):\n    return value.strip()\n"}));
}

/// Every recorded daemon or model request of one kind, in order.
pub(super) fn requests_of_kind(fixture: &Fixture, kind: &str) -> Vec<Value> {
    fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r["kind"] == kind)
        .cloned()
        .collect()
}

pub(super) fn request_kinds(fixture: &Fixture) -> Vec<String> {
    fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .map(|r| {
            if r["kind"] == "model" {
                format!("model:{}", r["schema"].as_str().unwrap_or(""))
            } else {
                r["kind"].as_str().unwrap_or("").to_string()
            }
        })
        .collect()
}

/// Drive a symbolic task through plan, approval, one edit that adds a helper,
/// link review and passing checks, up to the final capture checkpoint.
pub(super) async fn symbolic_task_ready_for_final_capture(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    add_helper(fixture);
    runner.advance().await.unwrap();
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "intermediate checkpoint journals only"
    );
    assert_eq!(intent_details(&runner, "capture_deferred").len(), 2);
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.task.check_results = vec![passed_check()];
    runner
        .task
        .symbolic
        .as_mut()
        .unwrap()
        .check_history
        .push(CheckOutcome {
            command: "true".into(),
            success: true,
            after_edit: true,
        });
    runner
}

pub(super) fn model_schemas(fixture: &Fixture) -> Vec<String> {
    requests_of_kind(fixture, "model")
        .iter()
        .map(|r| r["schema"].as_str().unwrap_or("").to_string())
        .collect()
}

pub(super) fn journal_snapshots(dir: &std::path::Path) -> Vec<(PathBuf, Value)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let journal = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some((path, journal))
        })
        .collect()
}

pub(super) fn awaiting_plan_with_capture_due(snapshots: &[(PathBuf, Value)]) -> Vec<String> {
    snapshots
        .iter()
        .filter(|(_, journal)| journal["phase"] == "AwaitingPlan" && journal["capture_due"] == true)
        .map(|(path, journal)| {
            format!(
                "{}: phase={} capture_due={} mode={}",
                path.display(),
                journal["phase"],
                journal["capture_due"],
                journal["mode"]
            )
        })
        .collect()
}

pub(super) fn assert_journal_invariant(fixture: &Fixture, step: &str) {
    let snapshots = journal_snapshots(&fixture.root.join(".moosedev/harness/tasks"));
    assert!(!snapshots.is_empty(), "no journal persisted after {step}");
    let violations = awaiting_plan_with_capture_due(&snapshots);
    assert!(violations.is_empty(), "after {step}: {violations:?}");
}

pub(super) struct JournalWatch {
    pub(super) stop: Arc<std::sync::atomic::AtomicBool>,
    pub(super) observed: Arc<std::sync::atomic::AtomicUsize>,
    pub(super) violations: Arc<Mutex<Vec<String>>>,
    pub(super) handle: Option<std::thread::JoinHandle<()>>,
}

impl JournalWatch {
    pub(super) fn start(fixture: &Fixture) -> Self {
        let dir = fixture.root.join(".moosedev/harness/tasks");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let violations = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let stop = stop.clone();
            let observed = observed.clone();
            let violations = violations.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let snapshots = journal_snapshots(&dir);
                    observed.fetch_add(snapshots.len(), std::sync::atomic::Ordering::SeqCst);
                    violations
                        .lock()
                        .unwrap()
                        .extend(awaiting_plan_with_capture_due(&snapshots));
                    std::thread::sleep(std::time::Duration::from_micros(250));
                }
            })
        };
        Self {
            stop,
            observed,
            violations,
            handle: Some(handle),
        }
    }

    pub(super) fn finish(mut self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
        assert!(
            self.observed.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "the poller never observed a persisted journal"
        );
        let mut violations = self.violations.lock().unwrap().clone();
        violations.dedup();
        violations
    }
}
