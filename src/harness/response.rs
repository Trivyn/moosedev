//! Negotiate the content channel before a model can participate in a task.
//! Probes are neutral, bounded, and cancellation-safe: dropping this future drops
//! its HTTP request. No probe response is dispatched as a harness action.
use crate::llm::{
    tool_call_from_text, CompletionError, LlmConfig, OpenAiCompatClient, StructuredOutputMode,
    UsageContext, UsageObserver,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{atomic::AtomicU8, Arc};
use std::time::Duration;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponsePolicy {
    #[default]
    Auto,
    ProviderDefault,
    ReasoningOff,
}

impl ResponsePolicy {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::parse(
            std::env::var("MOOSEDEV_HARNESS_RESPONSE_POLICY")
                .ok()
                .as_deref(),
        )
    }

    pub(super) fn parse(value: Option<&str>) -> anyhow::Result<Option<Self>> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(None),
            Some("auto") => Ok(Some(Self::Auto)),
            Some("provider-default") => Ok(Some(Self::ProviderDefault)),
            Some("reasoning-off") => Ok(Some(Self::ReasoningOff)),
            Some(value) => anyhow::bail!("MOOSEDEV_HARNESS_RESPONSE_POLICY must be auto, provider-default, or reasoning-off; got {value:?}"),
        }
    }
}

/// How the coding model returns its step action: native tool calls (the default)
/// or one JSON object constrained by a response schema.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionContract {
    #[default]
    Tools,
    JsonSchema,
}

impl ActionContract {
    /// `MOOSEDEV_HARNESS_ACTION_CONTRACT`: `tools` (default) or `json_schema`.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(
            std::env::var("MOOSEDEV_HARNESS_ACTION_CONTRACT")
                .ok()
                .as_deref(),
        )
    }

    pub(super) fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("tools") => Ok(Self::Tools),
            Some("json_schema") => Ok(Self::JsonSchema),
            Some(value) => anyhow::bail!(
                "MOOSEDEV_HARNESS_ACTION_CONTRACT must be tools or json_schema; got {value:?}"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::JsonSchema => "json_schema",
        }
    }
}

/// This in-memory key contains credentials. Never serialize it into a receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct ResponseKey {
    base_url: String,
    model: String,
    api_key: String,
    context_window_tokens: usize,
    structured_output: StructuredOutputMode,
    policy: ResponsePolicy,
    contract: ActionContract,
}

pub fn cache_key(
    config: &LlmConfig,
    policy: ResponsePolicy,
    contract: ActionContract,
) -> ResponseKey {
    ResponseKey {
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        api_key: config.api_key.clone(),
        context_window_tokens: config.context_window_tokens,
        structured_output: config.structured_output,
        policy,
        contract,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeAttempt {
    pub stream: bool,
    pub reasoning_off: bool,
    pub status: String,
    pub diagnostic: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseReceipt {
    pub requested: ResponsePolicy,
    pub resolved: Option<ResponsePolicy>,
    pub attempts: Vec<ProbeAttempt>,
    /// The action contract the probes verified; absent in receipts from earlier builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ActionContract>,
}

pub struct PreparedResponse {
    pub client: OpenAiCompatClient,
    pub receipt: ResponseReceipt,
}

#[derive(Debug)]
pub struct ProbeError {
    pub receipt: ResponseReceipt,
    pub cause: CompletionError,
}
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Model response compatibility check failed: {}. Select a compatible model or adjust MOOSEDEV_HARNESS_RESPONSE_POLICY and reconnect.", self.cause)
    }
}
impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Require both final-content paths to work. Auto retries only the observed
/// reasoning-only incompatibility, never arbitrary provider or model failures.
pub async fn prepare(
    config: &LlmConfig,
    policy: ResponsePolicy,
) -> Result<PreparedResponse, ProbeError> {
    prepare_with_observer(config, policy, None).await
}

/// The same compatibility negotiation, with physical requests accounted separately
/// from the legacy per-probe receipt (which must not be added to these totals).
pub async fn prepare_with_observer(
    config: &LlmConfig,
    policy: ResponsePolicy,
    observer: Option<UsageObserver>,
) -> Result<PreparedResponse, ProbeError> {
    prepare_for_contract(config, policy, ActionContract::JsonSchema, observer).await
}

/// Probe the path the coding model's actions will use. Under the tools contract
/// the probe asks for one call of a trivial `ready` tool; a thinking model under
/// the provider default gets room (1024 tokens) to finish, reasoning-off keeps 128.
/// A tool_choice fallback costs an extra request, so that contract's allowance is 6.
pub async fn prepare_for_contract(
    config: &LlmConfig,
    policy: ResponsePolicy,
    contract: ActionContract,
    observer: Option<UsageObserver>,
) -> Result<PreparedResponse, ProbeError> {
    let mut receipt = ResponseReceipt {
        requested: policy,
        resolved: None,
        attempts: vec![],
        contract: Some(contract),
    };
    let mut reasoning_off = policy == ResponsePolicy::ReasoningOff;
    let allowance = Arc::new(AtomicU8::new(match contract {
        ActionContract::Tools => 6,
        ActionContract::JsonSchema => 4,
    }));
    let mut base_client = OpenAiCompatClient::new_with_structured_output(
        config.base_url.clone(),
        config.api_key.clone(),
        config.structured_output,
    )
    .with_timeouts(config.timeouts);
    if let Some(observer) = observer {
        base_client = base_client.with_usage_observer(
            observer,
            UsageContext {
                purpose: "harness_response_probe".into(),
                decision_id: None,
                candidate: None,
            },
        );
    }
    loop {
        let limit = probe_output_limit(contract, reasoning_off);
        let client = base_client
            .clone()
            .for_harness(reasoning_off)
            .with_output_limit(limit)
            .with_request_allowance(Some(allowance.clone()));
        let mut failure = None;
        for stream in [false, true] {
            let result = match contract {
                ActionContract::JsonSchema => probe(&client, &config.model, stream).await,
                ActionContract::Tools => probe_tools(&client, &config.model, stream, limit).await,
            };
            let usage = client.take_usage_observation();
            let prompt_tokens = usage.map(|value| value.0);
            let completion_tokens = usage.map(|value| value.1);
            receipt.attempts.push(ProbeAttempt {
                stream,
                reasoning_off,
                status: if result.is_ok() { "passed" } else { "failed" }.into(),
                diagnostic: result.as_ref().err().map(|error| match error {
                    // Delivery failures are summarised rather than quoted: this
                    // receipt is persisted, and their messages name the endpoint.
                    CompletionError::Provider(_)
                    | CompletionError::Transport(_)
                    | CompletionError::ToolArgumentsIncomplete(_) => {
                        "Provider request failed during the neutral probe".into()
                    }
                    error => error.to_string().replace(&config.api_key, "[redacted]"),
                }),
                prompt_tokens,
                completion_tokens,
            });
            if let Err(error) = result {
                failure = Some(error);
                break;
            }
        }
        match failure {
            None => {
                receipt.resolved = Some(if reasoning_off {
                    ResponsePolicy::ReasoningOff
                } else {
                    ResponsePolicy::ProviderDefault
                });
                return Ok(PreparedResponse {
                    client: client.without_output_limit().with_request_allowance(None),
                    receipt,
                });
            }
            Some(CompletionError::ReasoningOnly)
                if policy == ResponsePolicy::Auto && !reasoning_off =>
            {
                reasoning_off = true;
            }
            Some(cause) => return Err(ProbeError { receipt, cause }),
        }
    }
}

fn probe_output_limit(contract: ActionContract, reasoning_off: bool) -> u32 {
    match contract {
        ActionContract::Tools if !reasoning_off => 1024,
        _ => 128,
    }
}

/// The wall-clock bound on one probe: 60 s for a 128-token answer, 300 s when a
/// thinking model may spend up to 1024 tokens on a large local model.
fn probe_time_bound(limit: u32) -> Duration {
    Duration::from_secs(if limit > 128 { 300 } else { 60 })
}

async fn probe_tools(
    client: &OpenAiCompatClient,
    model: &str,
    stream: bool,
    limit: u32,
) -> Result<(), CompletionError> {
    const PROMPT: &str =
        "This is a neutral connection test. Call the ready tool once with status \"ok\".";
    let tools = json!([{"type":"function","function":{
        "name":"ready",
        "description":"Confirm the connection works.",
        "parameters":{"type":"object","additionalProperties":false,"properties":{"status":{"type":"string","enum":["ok"]}},"required":["status"]}
    }}]);
    let bound = probe_time_bound(limit);
    let completion = tokio::time::timeout(
        bound,
        client.chat_completion_tools_checked(model, PROMPT, tools, stream, |_| {}),
    )
    .await
    .map_err(|_| {
        CompletionError::Incomplete(format!(
            "Neutral response probe exceeded {} seconds",
            bound.as_secs()
        ))
    })??;
    // A model that writes its call as text is still usable: the runner reads
    // such calls too.
    let call = completion
        .tool_calls
        .first()
        .cloned()
        .or_else(|| tool_call_from_text(&completion.content))
        .ok_or_else(|| {
            CompletionError::InvalidResponse(
                "Neutral response probe did not call the ready tool".into(),
            )
        })?;
    let arguments: serde_json::Value = serde_json::from_str(&call.arguments).map_err(|_| {
        CompletionError::InvalidResponse(
            "Neutral response probe tool arguments were not valid JSON".into(),
        )
    })?;
    if call.name != "ready" || arguments != json!({"status":"ok"}) {
        return Err(CompletionError::InvalidResponse(
            "Neutral response probe did not call ready with status ok".into(),
        ));
    }
    Ok(())
}

async fn probe(
    client: &OpenAiCompatClient,
    model: &str,
    stream: bool,
) -> Result<(), CompletionError> {
    const PROMPT: &str =
        "This is a neutral connection test. Return exactly the JSON object {\"status\":\"ok\"}.";
    let schema = json!({"type":"object","additionalProperties":false,"properties":{"status":{"type":"string","enum":["ok"]}},"required":["status"]});
    let request = async {
        if stream {
            client
                .chat_completion_json_schema_streaming_checked(
                    model,
                    PROMPT,
                    None,
                    "harness_response_probe",
                    schema,
                    |_| {},
                )
                .await
        } else {
            client
                .chat_completion_json_schema_checked(
                    model,
                    PROMPT,
                    None,
                    "harness_response_probe",
                    schema,
                )
                .await
        }
    };
    let text = tokio::time::timeout(Duration::from_secs(60), request)
        .await
        .map_err(|_| {
            CompletionError::Incomplete("Neutral response probe exceeded 60 seconds".into())
        })??;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        CompletionError::InvalidResponse("Neutral response probe did not return valid JSON".into())
    })?;
    if value != json!({"status":"ok"}) {
        return Err(CompletionError::InvalidResponse(
            "Neutral response probe did not match its required schema".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Stub {
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        reasoning_only: bool,
        streaming_reasoning_only: bool,
        reject_off: bool,
        reject_schema: bool,
        malformed: bool,
        stall: bool,
        stream_usage: bool,
    }
    async fn complete(State(stub): State<Stub>, Json(body): Json<serde_json::Value>) -> Response {
        stub.requests.lock().unwrap().push(body.clone());
        if stub.stall {
            std::future::pending::<()>().await;
        }
        if stub.reject_schema && body.get("response_format").is_some() {
            return (StatusCode::UNPROCESSABLE_ENTITY, "json_schema unsupported").into_response();
        }
        if stub.reject_off && body["reasoning_effort"] == "none" {
            return (StatusCode::BAD_REQUEST, "reasoning_effort unsupported").into_response();
        }
        let streaming = body["stream"] == true;
        let only_reasoning = (stub.reasoning_only || (streaming && stub.streaming_reasoning_only))
            && body["reasoning_effort"] != "none";
        let text = if stub.malformed {
            "{}"
        } else {
            "{\"status\":\"ok\"}"
        };
        let content = if only_reasoning { "" } else { text };
        if streaming {
            let delta = if only_reasoning {
                json!({"reasoning_content":text})
            } else {
                json!({"content":content})
            };
            let usage = if stub.stream_usage {
                format!(
                    "data: {}\n\n",
                    json!({"choices":[],"usage":{"prompt_tokens":13,"completion_tokens":5,"total_tokens":18}})
                )
            } else {
                String::new()
            };
            let events = format!(
                "data: {}\n\ndata: {}\n\n{usage}data: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":delta}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            );
            ([("content-type", "text/event-stream")], events).into_response()
        } else {
            Json(json!({"choices":[{"message":{"content":content,"reasoning_content":if only_reasoning {text} else {""}},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":7}})).into_response()
        }
    }
    async fn server(stub: Stub) -> (LlmConfig, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = LlmConfig {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            api_key: "test-secret".into(),
            model: "fixture-model".into(),
            configured: true,
            context_window_tokens: 32768,
            structured_output: StructuredOutputMode::Auto,
            timeouts: Default::default(),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(complete))
                    .with_state(stub),
            )
            .await
            .unwrap()
        });
        (config, task)
    }

    #[test]
    fn policy_parsing_and_cache_identity_are_explicit() {
        assert_eq!(ResponsePolicy::parse(None).unwrap(), None);
        assert_eq!(
            ResponsePolicy::parse(Some("reasoning-off")).unwrap(),
            Some(ResponsePolicy::ReasoningOff)
        );
        assert!(ResponsePolicy::parse(Some("none")).is_err());
        assert_eq!(
            serde_json::from_str::<ResponsePolicy>("\"provider-default\"").unwrap(),
            ResponsePolicy::ProviderDefault
        );
        let mut config = LlmConfig::from_env().unwrap();
        let tools = ActionContract::Tools;
        let key = cache_key(&config, ResponsePolicy::Auto, tools);
        assert!(key == cache_key(&config, ResponsePolicy::Auto, tools));
        assert!(key != cache_key(&config, ResponsePolicy::ProviderDefault, tools));
        assert!(key != cache_key(&config, ResponsePolicy::Auto, ActionContract::JsonSchema));
        config.api_key.push('x');
        assert!(key != cache_key(&config, ResponsePolicy::Auto, tools));
        config.api_key.pop();
        config.model.push('x');
        assert!(key != cache_key(&config, ResponsePolicy::Auto, tools));
        config.model.pop();
        config.base_url.push('x');
        assert!(key != cache_key(&config, ResponsePolicy::Auto, tools));
        config.base_url.pop();
        config.structured_output = StructuredOutputMode::Required;
        assert!(key != cache_key(&config, ResponsePolicy::Auto, tools));
        assert_eq!(ActionContract::parse(None).unwrap(), ActionContract::Tools);
        assert_eq!(
            ActionContract::parse(Some(" json_schema ")).unwrap(),
            ActionContract::JsonSchema
        );
        assert!(ActionContract::parse(Some("json"))
            .unwrap_err()
            .to_string()
            .contains("MOOSEDEV_HARNESS_ACTION_CONTRACT"));
    }

    #[tokio::test]
    async fn auto_requires_both_paths_before_adopting_reasoning_off() {
        let stub = Stub {
            streaming_reasoning_only: true,
            ..Stub::default()
        };
        let requests = stub.requests.clone();
        let (config, task) = server(stub).await;
        let prepared = prepare(&config, ResponsePolicy::Auto).await.unwrap();
        assert_eq!(
            prepared.receipt.resolved,
            Some(ResponsePolicy::ReasoningOff)
        );
        assert_eq!(prepared.receipt.attempts.len(), 4);
        assert_eq!(prepared.receipt.attempts[0].prompt_tokens, Some(11));
        assert_eq!(prepared.receipt.attempts[1].prompt_tokens, None);
        assert!(!serde_json::to_string(&prepared.receipt)
            .unwrap()
            .contains("test-secret"));
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 4);
            for request in requests.iter() {
                assert_eq!(request["max_tokens"], 128);
                assert_eq!(
                    request["response_format"]["json_schema"]["name"],
                    "harness_response_probe"
                );
            }
            assert!(requests[0].get("reasoning_effort").is_none());
            assert_eq!(requests[2]["reasoning_effort"], "none");
        }
        // Prepared client retains the selected mode, but not probe limits.
        prepared
            .client
            .chat_completion_json_schema_checked(
                &config.model,
                "task",
                None,
                "task",
                json!({"type":"object"}),
            )
            .await
            .unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[4].get("max_tokens").is_none());
        assert_eq!(requests[4]["reasoning_effort"], "none");
        task.abort();
    }

    #[tokio::test]
    async fn explicit_modes_never_silently_negotiate() {
        let stub = Stub {
            reasoning_only: true,
            ..Stub::default()
        };
        let (config, task) = server(stub).await;
        let error = prepare(&config, ResponsePolicy::ProviderDefault)
            .await
            .err()
            .unwrap();
        assert!(matches!(error.cause, CompletionError::ReasoningOnly));
        assert_eq!(error.receipt.attempts.len(), 1);
        task.abort();
        let stub = Stub {
            reject_off: true,
            ..Stub::default()
        };
        let (config, task) = server(stub).await;
        let error = prepare(&config, ResponsePolicy::ReasoningOff)
            .await
            .err()
            .unwrap();
        assert!(matches!(error.cause, CompletionError::Provider(_)));
        assert_eq!(error.receipt.attempts.len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn schema_fallback_preserves_reasoning_policy_and_request_bound() {
        let stub = Stub {
            reasoning_only: true,
            reject_schema: true,
            ..Stub::default()
        };
        let requests = stub.requests.clone();
        let (config, task) = server(stub).await;
        let prepared = prepare(&config, ResponsePolicy::Auto).await.unwrap();
        assert_eq!(
            prepared.receipt.resolved,
            Some(ResponsePolicy::ReasoningOff)
        );
        assert_eq!(requests.lock().unwrap().len(), 4);
        task.abort();
    }

    #[tokio::test]
    async fn malformed_probe_is_not_a_reason_to_change_model_settings() {
        let stub = Stub {
            malformed: true,
            ..Stub::default()
        };
        let (config, task) = server(stub).await;
        let error = prepare(&config, ResponsePolicy::Auto).await.err().unwrap();
        assert!(matches!(error.cause, CompletionError::InvalidResponse(_)));
        assert_eq!(error.receipt.attempts.len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn dropping_a_probe_does_not_launch_retries() {
        let stub = Stub {
            stall: true,
            ..Stub::default()
        };
        let requests = stub.requests.clone();
        let (config, task) = server(stub).await;
        let preparation = tokio::spawn(async move { prepare(&config, ResponsePolicy::Auto).await });
        tokio::time::timeout(Duration::from_secs(10), async {
            while requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        preparation.abort();
        assert!(preparation.await.err().unwrap().is_cancelled());
        tokio::task::yield_now().await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        task.abort();
    }
    #[derive(Clone, Default)]
    struct ToolStub {
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        text_call: bool,
    }

    async fn tool_complete(
        State(stub): State<ToolStub>,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        stub.requests.lock().unwrap().push(body.clone());
        let text = "{\"name\":\"ready\",\"parameters\":{\"status\":\"ok\"}}";
        let call = json!({"id":"probe","type":"function","function":{"name":"ready","arguments":"{\"status\":\"ok\"}"}});
        if body["stream"] == true {
            let delta = if stub.text_call {
                json!({"content":text})
            } else {
                json!({"tool_calls":[{"index":0,"id":"probe","type":"function","function":{"name":"ready","arguments":"{\"status\":\"ok\"}"}}]})
            };
            let events = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":delta}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
            );
            ([("content-type", "text/event-stream")], events).into_response()
        } else {
            let message = if stub.text_call {
                json!({"content":text})
            } else {
                json!({"content":"","tool_calls":[call]})
            };
            Json(json!({"choices":[{"message":message,"finish_reason":"tool_calls"}]}))
                .into_response()
        }
    }

    async fn tool_server(stub: ToolStub) -> (LlmConfig, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = LlmConfig {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            api_key: "test-secret".into(),
            model: "fixture-model".into(),
            configured: true,
            context_window_tokens: 32768,
            structured_output: StructuredOutputMode::Auto,
            timeouts: Default::default(),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(tool_complete))
                    .with_state(stub),
            )
            .await
            .unwrap()
        });
        (config, task)
    }

    #[tokio::test]
    async fn tool_contract_probes_call_the_ready_tool_within_policy_bounds() {
        for (policy, limit) in [
            (ResponsePolicy::ReasoningOff, 128),
            (ResponsePolicy::ProviderDefault, 1024),
        ] {
            let stub = ToolStub::default();
            let requests = stub.requests.clone();
            let (config, task) = tool_server(stub).await;
            let prepared = prepare_for_contract(&config, policy, ActionContract::Tools, None)
                .await
                .unwrap();
            assert_eq!(prepared.receipt.contract, Some(ActionContract::Tools));
            assert_eq!(prepared.receipt.resolved, Some(policy));
            assert_eq!(prepared.receipt.attempts.len(), 2);
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for request in requests.iter() {
                assert_eq!(request["max_tokens"], limit, "{policy:?}");
                assert!(request.get("response_format").is_none());
                assert_eq!(request["tool_choice"], "required");
                assert_eq!(request["tools"][0]["function"]["name"], "ready");
                assert_eq!(
                    request.get("reasoning_effort").is_some(),
                    policy == ResponsePolicy::ReasoningOff
                );
            }
            task.abort();
        }
        // A model that writes the call as text (Llama on LM Studio) still passes.
        let (config, task) = tool_server(ToolStub {
            text_call: true,
            ..ToolStub::default()
        })
        .await;
        let prepared = prepare_for_contract(
            &config,
            ResponsePolicy::ReasoningOff,
            ActionContract::Tools,
            None,
        )
        .await
        .unwrap();
        assert_eq!(prepared.receipt.attempts.len(), 2);
        task.abort();
    }

    fn accounting_observer() -> (UsageObserver, Arc<Mutex<Vec<crate::llm::RequestUsage>>>) {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let destination = receipts.clone();
        (
            Arc::new(move |receipt| destination.lock().unwrap().push(receipt)),
            receipts,
        )
    }

    #[tokio::test]
    async fn physical_probe_receipts_include_schema_fallback_and_stream_usage() {
        use crate::llm::RequestStatus;
        let (config, task) = server(Stub {
            reject_schema: true,
            stream_usage: true,
            ..Stub::default()
        })
        .await;
        let (observer, receipts) = accounting_observer();
        prepare_with_observer(&config, ResponsePolicy::Auto, Some(observer))
            .await
            .unwrap();
        let receipts = receipts.lock().unwrap();
        let finished: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt.status != RequestStatus::Started)
            .collect();
        assert_eq!(finished.len(), 3);
        assert_eq!(receipts.len(), 6);
        assert_eq!(finished[0].status, RequestStatus::Failed);
        assert_eq!(finished[0].http_status, Some(422));
        assert!(finished[0].tokens.prompt_tokens.is_none());
        assert_eq!(finished[1].tokens.prompt_tokens, Some(11));
        assert_eq!(finished[2].tokens.prompt_tokens, Some(13));
        assert!(finished[2].streaming);
        assert!(finished.iter().all(
            |receipt| receipt.context.purpose == "harness_response_probe"
                && receipt.context.decision_id.is_none()
        ));
        assert_eq!(
            finished
                .iter()
                .map(|receipt| &receipt.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        assert!(!serde_json::to_string(&*receipts)
            .unwrap()
            .contains("test-secret"));
        task.abort();
    }

    #[tokio::test]
    async fn cancelled_probe_has_terminal_receipt_without_retry_or_usage_inference() {
        use crate::llm::RequestStatus;
        let stub = Stub {
            stall: true,
            ..Stub::default()
        };
        let requests = stub.requests.clone();
        let (config, task) = server(stub).await;
        let (observer, receipts) = accounting_observer();
        let preparation = tokio::spawn(async move {
            prepare_with_observer(&config, ResponsePolicy::Auto, Some(observer)).await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        preparation.abort();
        assert!(preparation.await.err().unwrap().is_cancelled());
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[0].id, receipts[1].id);
        assert_eq!(receipts[1].status, RequestStatus::Cancelled);
        assert!(receipts[1].tokens.prompt_tokens.is_none());
        assert!(!receipts[1].response_complete);
        task.abort();
    }

    #[tokio::test]
    #[ignore = "requires the three installed study models and explicit neutral usage probe opt-in"]
    async fn native_usage_receipts() {
        use crate::harness::digest::sha256_hex;
        use crate::llm::RequestStatus;
        assert_eq!(
            std::env::var("MOOSEDEV_RUN_HARNESS_USAGE_PROBES").as_deref(),
            Ok("1")
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let directory = root.join("target/harness-usage-verification");
        std::fs::create_dir_all(&directory).unwrap();
        let destination = directory.join(format!("native-probes-{}.json", uuid::Uuid::new_v4()));
        let sources: std::collections::BTreeMap<_, _> = [
            "src/llm/mod.rs",
            "src/llm/completion.rs",
            "src/llm/usage.rs",
            "src/harness/response.rs",
        ]
        .into_iter()
        .map(|path| (path, sha256_hex(std::fs::read(root.join(path)).unwrap())))
        .collect();
        let mut observations = Vec::new();
        for model in [
            "qwen/qwen3.8-27b",
            "google/gemma-4-26b-a4b",
            "gemma-4-e4b-it-mlx",
        ] {
            let config = LlmConfig {
                base_url: "http://127.0.0.1:1234/v1".into(),
                model: model.into(),
                api_key: "lm-studio".into(),
                configured: true,
                context_window_tokens: 32768,
                structured_output: StructuredOutputMode::Auto,
                timeouts: Default::default(),
            };
            let (observer, receipts) = accounting_observer();
            let result = prepare_with_observer(&config, ResponsePolicy::Auto, Some(observer)).await;
            let (passed, response_receipt) = match result {
                Ok(prepared) => (true, prepared.receipt),
                Err(error) => (false, error.receipt),
            };
            let receipts = receipts.lock().unwrap();
            let finished: Vec<_> = receipts
                .iter()
                .filter(|receipt| receipt.status != RequestStatus::Started)
                .collect();
            let complete_pairs = finished.len() * 2 == receipts.len()
                && finished.iter().all(|receipt| {
                    receipt.context.purpose == "harness_response_probe"
                        && receipt.context.decision_id.is_none()
                });
            let reported_usage = [false, true].into_iter().all(|stream| {
                finished.iter().any(|receipt| {
                    receipt.streaming == stream
                        && receipt.status == RequestStatus::Completed
                        && receipt.response_complete
                        && receipt.raw_usage.is_some()
                        && receipt.tokens.prompt_tokens.is_some()
                        && receipt.tokens.completion_tokens.is_some()
                })
            });
            observations.push(json!({"model":model,"passed":passed,"complete_pairs":complete_pairs,"reported_usage_both_paths":reported_usage,"response_receipt":response_receipt,"request_snapshots":&*receipts}));
            let manifest = json!({"purpose":"Native neutral usage accounting; no coding task or output execution", "timestamp":chrono::Utc::now().to_rfc3339(), "source_sha256":sources,
                "request_options":{"temperature":0,"max_tokens":128,"timeout_seconds":60,"maximum_wire_requests_per_model":4,"policy":"auto"},"observations":observations});
            std::fs::write(&destination, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        }
        eprintln!("Native usage probe receipts: {}", destination.display());
        assert!(observations
            .iter()
            .all(|item| item["passed"] == true && item["complete_pairs"] == true));
        // Preserve provider non-reporting as a measured result, never synthesize
        // counts. The saved manifest exposes whether both paths supply usage.
    }

    /// Deliberately separate from ordinary tests: real local generation is an
    /// explicit development observation, with a fresh receipt directory.
    #[tokio::test]
    #[ignore = "requires the three installed study models and a running LM Studio"]
    async fn native_neutral_contracts() {
        use crate::harness::digest::sha256_hex;
        assert_eq!(
            std::env::var("MOOSEDEV_RUN_HARNESS_RESPONSE_PROBES").as_deref(),
            Ok("1")
        );
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/harness-fix-probes")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let sources = [
            "src/llm/mod.rs",
            "src/llm/completion.rs",
            "src/harness/response.rs",
        ];
        let source_hashes: std::collections::BTreeMap<_, _> = sources
            .into_iter()
            .map(|path| {
                let bytes =
                    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
                        .unwrap();
                (path, sha256_hex(bytes))
            })
            .collect();
        let mut observations = Vec::new();
        for model in [
            "qwen/qwen3.8-27b",
            "google/gemma-4-26b-a4b",
            "gemma-4-e4b-it-mlx",
        ] {
            let config = LlmConfig {
                base_url: "http://127.0.0.1:1234/v1".into(),
                model: model.into(),
                api_key: "lm-studio".into(),
                configured: true,
                context_window_tokens: 32768,
                structured_output: StructuredOutputMode::Auto,
                timeouts: Default::default(),
            };
            let result = prepare(&config, ResponsePolicy::Auto).await;
            let (passed, receipt) = match result {
                Ok(prepared) => (true, prepared.receipt),
                Err(error) => (false, error.receipt),
            };
            observations.push(json!({"model":model,"endpoint":config.base_url,"passed":passed,"response_receipt":receipt}));
            let manifest = json!({"purpose":"Native neutral response compatibility; no coding task or output execution","timestamp":chrono::Utc::now().to_rfc3339(),"source_sha256":source_hashes,"request_options":{"temperature":0,"max_tokens":128,"schema_name":"harness_response_probe","schema_mode":"auto","timeout_seconds":60,"maximum_wire_requests":4,"reasoning_off_wire_value":{"reasoning_effort":"none"}},"observations":observations});
            std::fs::write(
                directory.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .unwrap();
        }
        eprintln!("Native response probe receipts: {}", directory.display());
        assert!(observations.iter().all(|value| value["passed"] == true));
    }
}
