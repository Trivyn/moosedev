//! Provider-reported accounting for individual HTTP requests. These receipts
//! are observations, not token estimates or claims about provider billing.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Arc, time::Instant};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageContext {
    pub purpose: String,
    pub decision_id: Option<String>,
    pub candidate: Option<u8>,
}

/// Missing, invalid, and unreported counts remain unknown. Do not add detail
/// fields to prompt/completion totals without provider-specific semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl TokenUsage {
    fn from_provider(value: &Value) -> Self {
        Self {
            prompt_tokens: value["prompt_tokens"].as_u64(),
            completion_tokens: value["completion_tokens"].as_u64(),
            total_tokens: value["total_tokens"].as_u64(),
            cached_input_tokens: value["prompt_tokens_details"]["cached_tokens"].as_u64(),
            cache_write_tokens: value["cache_creation_input_tokens"].as_u64(),
            reasoning_tokens: value["completion_tokens_details"]["reasoning_tokens"].as_u64(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
}

/// A started and terminal snapshot share one UUID; consumers must replace by ID
/// when totaling. A schema fallback has its own ID even within one candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsage {
    pub id: String,
    pub model: String,
    /// Endpoint with user information, query, and fragment removed.
    pub endpoint: String,
    pub streaming: bool,
    pub context: UsageContext,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub status: RequestStatus,
    pub http_status: Option<u16>,
    /// Only the provider's usage value, never the surrounding response.
    pub raw_usage: Option<Value>,
    pub tokens: TokenUsage,
    /// The JSON body was fully received or SSE reached its valid completion marker.
    /// Independent of content validity and of whether usage was reported.
    pub response_complete: bool,
}

/// Called synchronously before send and on termination (including future Drop).
/// Observers should not panic; persistence failures belong to their owner.
pub type UsageObserver = Arc<dyn Fn(RequestUsage) + Send + Sync>;

#[derive(Clone)]
pub(super) struct UsageBinding {
    pub observer: UsageObserver,
    pub context: UsageContext,
}

impl std::fmt::Debug for UsageBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageBinding")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

fn sanitized_endpoint(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else {
        return "invalid endpoint".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

struct ActiveRequest {
    observer: UsageObserver,
    receipt: RequestUsage,
    started: Instant,
}

/// One scoped receipt; no observer means no retained ledger or global state.
pub(super) struct RequestObservation(Option<ActiveRequest>);

impl RequestObservation {
    pub fn start(
        binding: Option<&UsageBinding>,
        endpoint: &str,
        model: &str,
        streaming: bool,
    ) -> Self {
        Self(binding.map(|binding| {
            let receipt = RequestUsage {
                id: uuid::Uuid::new_v4().to_string(),
                model: model.to_owned(),
                endpoint: sanitized_endpoint(endpoint),
                streaming,
                context: binding.context.clone(),
                started_at: chrono::Utc::now().to_rfc3339(),
                finished_at: None,
                elapsed_ms: None,
                status: RequestStatus::Started,
                http_status: None,
                raw_usage: None,
                tokens: TokenUsage::default(),
                response_complete: false,
            };
            let started = Instant::now();
            (binding.observer)(receipt.clone());
            ActiveRequest {
                observer: binding.observer.clone(),
                receipt,
                started,
            }
        }))
    }

    pub fn http_status(&mut self, status: reqwest::StatusCode) {
        if let Some(active) = &mut self.0 {
            active.receipt.http_status = Some(status.as_u16());
        }
    }

    /// Optional correlation for a recording proxy. Invalid attribution is
    /// omitted rather than turning accounting into a generation failure.
    pub fn attribute(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(active) = &self.0 {
            let candidate = active
                .receipt
                .context
                .candidate
                .map(|value| value.to_string());
            for (name, value) in [
                ("x-moosedev-request-id", Some(active.receipt.id.as_str())),
                (
                    "x-moosedev-purpose",
                    Some(active.receipt.context.purpose.as_str()),
                ),
                (
                    "x-moosedev-decision-id",
                    active.receipt.context.decision_id.as_deref(),
                ),
                ("x-moosedev-candidate", candidate.as_deref()),
            ] {
                if let Some(value) =
                    value.and_then(|value| reqwest::header::HeaderValue::from_str(value).ok())
                {
                    request = request.header(name, value);
                }
            }
        }
        request
    }

    pub fn observe(&mut self, body: &Value) {
        if let Some(active) = &mut self.0 {
            if let Some(usage) = body.get("usage").filter(|usage| !usage.is_null()) {
                // Stream snapshots are cumulative. Replace, never sum them.
                active.receipt.tokens = TokenUsage::from_provider(usage);
                active.receipt.raw_usage = Some(usage.clone());
            }
        }
    }

    pub fn response_complete(&mut self) {
        if let Some(active) = &mut self.0 {
            active.receipt.response_complete = true;
        }
    }

    pub fn finish(&mut self, success: bool) {
        self.emit_terminal(if success {
            RequestStatus::Completed
        } else {
            RequestStatus::Failed
        });
    }

    fn emit_terminal(&mut self, status: RequestStatus) {
        if let Some(mut active) = self.0.take() {
            active.receipt.status = status;
            active.receipt.finished_at = Some(chrono::Utc::now().to_rfc3339());
            active.receipt.elapsed_ms =
                Some(u64::try_from(active.started.elapsed().as_millis()).unwrap_or(u64::MAX));
            (active.observer)(active.receipt);
        }
    }
}

impl Drop for RequestObservation {
    fn drop(&mut self) {
        self.emit_terminal(RequestStatus::Cancelled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{completion::CompletionStream, OpenAiCompatClient, StructuredOutputMode};
    use axum::{
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use moose::traits::LlmClient;
    use serde_json::json;
    use std::sync::Mutex;

    type Receipts = Arc<Mutex<Vec<RequestUsage>>>;

    fn observed_client(endpoint: &str) -> (OpenAiCompatClient, Receipts) {
        let receipts = Receipts::default();
        let sink = receipts.clone();
        let client = OpenAiCompatClient::new(endpoint, "private-api-key").with_usage_observer(
            Arc::new(move |receipt| sink.lock().unwrap().push(receipt)),
            UsageContext {
                purpose: "capture".into(),
                decision_id: Some("decision-1".into()),
                candidate: Some(2),
            },
        );
        (client, receipts)
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/v1"), handle)
    }

    fn event(value: Value) -> String {
        format!("data: {value}\r\n\r\n")
    }

    fn stream_body() -> String {
        event(
            json!({"choices":[{"delta":{"content":"héllo 🫎"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
        ) + &event(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
            + &event(json!({"choices":[],"usage":{
                "prompt_tokens":10,"completion_tokens":4,"total_tokens":14,
                "prompt_tokens_details":{"cached_tokens":8},
                "completion_tokens_details":{"reasoning_tokens":2},
                "cache_creation_input_tokens":0,"provider_extension":99
            }}))
            + "data: [DONE]\r\n\r\n"
    }

    #[tokio::test]
    async fn physical_schema_fallback_receipts_match_headers_and_last_sse_usage() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push((headers, body.clone()));
                    assert_eq!(body["stream_options"]["include_usage"], true);
                    if body.get("response_format").is_some() {
                        return (StatusCode::UNPROCESSABLE_ENTITY, "json_schema unsupported")
                            .into_response();
                    }
                    ([("content-type", "text/event-stream")], stream_body()).into_response()
                }
            }),
        );
        let (endpoint, server) = serve(app).await;
        let (client, receipts) = observed_client(&endpoint);
        let result = client
            .chat_completion_json_schema_streaming_checked(
                "model",
                "private prompt",
                None,
                "shape",
                json!({}),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(result, "héllo 🫎");
        assert_eq!(client.take_usage(), (10, 4));
        let receipts = receipts.lock().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(receipts.len(), 4);
        assert_ne!(receipts[0].id, receipts[2].id);
        for (pair, (headers, _)) in receipts.as_chunks::<2>().0.iter().zip(requests.iter()) {
            assert_eq!(pair[0].id, pair[1].id);
            assert_eq!(headers["x-moosedev-request-id"], pair[0].id);
            assert_eq!(headers["x-moosedev-purpose"], "capture");
            assert_eq!(headers["x-moosedev-decision-id"], "decision-1");
            assert_eq!(headers["x-moosedev-candidate"], "2");
            assert_eq!(pair[0].status, RequestStatus::Started);
            assert_eq!(pair[0].tokens, TokenUsage::default());
            assert!(pair[1].finished_at.is_some());
        }
        assert_eq!(receipts[1].status, RequestStatus::Failed);
        assert_eq!(receipts[1].http_status, Some(422));
        assert_eq!(receipts[3].status, RequestStatus::Completed);
        assert!(receipts[3].response_complete);
        assert_eq!(
            receipts[3].tokens,
            TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(4),
                total_tokens: Some(14),
                cached_input_tokens: Some(8),
                cache_write_tokens: Some(0),
                reasoning_tokens: Some(2),
            }
        );
        assert_eq!(
            receipts[3].raw_usage.as_ref().unwrap()["provider_extension"],
            99
        );
        let serialized = serde_json::to_string(&*receipts).unwrap();
        for forbidden in [
            "private-api-key",
            "private prompt",
            "héllo",
            "choices",
            "messages",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "receipt leaked {forbidden}"
            );
        }
        server.abort();
    }

    #[test]
    fn cumulative_usage_survives_every_sse_byte_boundary() {
        let body = stream_body();
        for size in 1..body.len() {
            let mut stream = CompletionStream::default();
            for chunk in body.as_bytes().chunks(size) {
                stream.feed(chunk, &|_| {}).unwrap();
            }
            stream.finish().unwrap();
            assert_eq!(stream.usage.unwrap()["usage"]["prompt_tokens"], 10);
        }
    }

    #[tokio::test]
    async fn concurrent_plain_helpers_keep_unknown_zero_partial_and_failure_distinct() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let mut value =
                    json!({"choices":[{"message":{"content":"secret generated content"}}]});
                match body["model"].as_str().unwrap() {
                    "zero" => value["usage"] = json!({"prompt_tokens":0,"completion_tokens":0}),
                    "partial" => value["usage"] = json!({"prompt_tokens":7,"completion_tokens":-1}),
                    "failed" => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(
                                json!({"error":"private failure","usage":{"completion_tokens":2}}),
                            ),
                        )
                            .into_response()
                    }
                    "malformed" => {
                        return (StatusCode::OK, "invalid json private text").into_response()
                    }
                    _ => (),
                }
                Json(value).into_response()
            }),
        );
        let (endpoint, server) = serve(app).await;
        let (client, receipts) = observed_client(&endpoint);
        let mut handles = Vec::new();
        for model in ["missing", "zero", "partial", "failed", "malformed"] {
            let client = client.with_fresh_usage();
            handles.push(tokio::spawn(async move {
                let result = client.chat_completion(model, "private prompt", None).await;
                assert_eq!(result.is_err(), matches!(model, "failed" | "malformed"));
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 10);
        let terminal = |model: &str| {
            receipts
                .iter()
                .find(|r| r.model == model && r.status != RequestStatus::Started)
                .unwrap()
        };
        assert_eq!(terminal("missing").tokens, TokenUsage::default());
        assert_eq!(terminal("missing").raw_usage, None);
        assert_eq!(terminal("zero").tokens.prompt_tokens, Some(0));
        assert_eq!(terminal("zero").tokens.completion_tokens, Some(0));
        assert_eq!(terminal("zero").tokens.total_tokens, None);
        assert_eq!(terminal("partial").tokens.prompt_tokens, Some(7));
        assert_eq!(terminal("partial").tokens.completion_tokens, None);
        assert_eq!(terminal("failed").tokens.completion_tokens, Some(2));
        assert_eq!(terminal("failed").status, RequestStatus::Failed);
        assert!(terminal("failed").response_complete);
        assert!(terminal("malformed").response_complete);
        assert_eq!(terminal("malformed").status, RequestStatus::Failed);
        for receipt in receipts
            .iter()
            .filter(|r| r.status == RequestStatus::Started)
        {
            assert_eq!(receipts.iter().filter(|r| r.id == receipt.id).count(), 2);
        }
        server.abort();
    }

    #[tokio::test]
    async fn sse_failure_keeps_observed_usage_without_executing_partial_response() {
        let app = Router::new().route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
            let first = event(json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}}));
            let last = match body["model"].as_str().unwrap() {
                "malformed" => "data: broken json\n\n".to_string(),
                "finish" => event(json!({"choices":[{"delta":{},"finish_reason":"length"}],"usage":{"prompt_tokens":3,"completion_tokens":2}})),
                "error" => event(json!({"error":"unavailable","usage":{"prompt_tokens":3,"completion_tokens":4}})),
                _ => String::new(), // EOF without finish marker
            };
            ([("content-type", "text/event-stream")], first + &last)
        }));
        let (endpoint, server) = serve(app).await;
        let (client, receipts) = observed_client(&endpoint);
        for (model, expected) in [("malformed", 1), ("finish", 2), ("error", 4), ("eof", 1)] {
            assert!(client
                .chat_completion_json_schema_streaming_checked(
                    model,
                    "private",
                    None,
                    "shape",
                    json!({}),
                    |_| {}
                )
                .await
                .is_err());
            let receipts = receipts.lock().unwrap();
            let receipt = receipts.last().unwrap();
            assert_eq!(receipt.status, RequestStatus::Failed);
            assert_eq!(receipt.tokens.completion_tokens, Some(expected));
            assert!(!receipt.response_complete);
        }
        assert_eq!(receipts.lock().unwrap().len(), 8);
        server.abort();
    }

    #[tokio::test]
    async fn usage_option_rejection_is_retried_once_and_remembered() {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body.clone());
                    if body.get("stream_options").is_some() {
                        return (StatusCode::BAD_REQUEST, "unknown field stream_options")
                            .into_response();
                    }
                    let value =
                        json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]});
                    Json(value).into_response()
                }
            }),
        );
        let (endpoint, server) = serve(app).await;
        let (client, receipts) = observed_client(&endpoint);
        for _ in 0..2 {
            assert_eq!(
                client
                    .chat_completion_json_schema_streaming_checked(
                        "m",
                        "p",
                        None,
                        "s",
                        json!({}),
                        |_| {}
                    )
                    .await
                    .unwrap(),
                "ok"
            );
        }
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3);
        assert!(bodies[0].get("stream_options").is_some());
        assert!(bodies[1].get("stream_options").is_none());
        assert!(bodies[2].get("stream_options").is_none());
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 6);
        assert_eq!(receipts[1].status, RequestStatus::Failed);
        assert_eq!(receipts[3].status, RequestStatus::Completed);
        assert_eq!(receipts[3].tokens, TokenUsage::default());
        assert!(receipts[3].streaming); // requested SSE; provider returned JSON
        server.abort();
    }

    #[tokio::test]
    async fn cancelled_stream_preserves_usage_and_terminal_receipt() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            let data = event(
                json!({"choices":[{"delta":{"content":"partial"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}),
            );
            socket
                .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let (client, receipts) = observed_client(&endpoint);
        let observed = Arc::new(tokio::sync::Notify::new());
        let consumed = observed.clone();
        let generation = tokio::spawn(async move {
            client
                .chat_completion_json_schema_streaming_checked(
                    "model",
                    "private",
                    None,
                    "shape",
                    json!({}),
                    move |_| consumed.notify_one(),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), observed.notified())
            .await
            .unwrap();
        generation.abort();
        assert!(generation.await.unwrap_err().is_cancelled());
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[1].status, RequestStatus::Cancelled);
        assert_eq!(receipts[1].tokens.prompt_tokens, Some(5));
        assert_eq!(receipts[1].tokens.completion_tokens, Some(2));
        assert!(!receipts[1].response_complete);
        server.abort();
    }

    #[tokio::test]
    async fn transport_failure_and_unreserved_request_have_distinct_receipt_counts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        drop(listener);
        let (client, receipts) = observed_client(&endpoint);
        assert!(client.chat_completion("m", "p", None).await.is_err());
        {
            let receipts = receipts.lock().unwrap();
            assert_eq!(receipts.len(), 2);
            assert_eq!(receipts[1].status, RequestStatus::Failed);
            assert_eq!(receipts[1].http_status, None);
            assert_eq!(receipts[1].raw_usage, None);
        }
        let client =
            client.with_request_allowance(Some(Arc::new(std::sync::atomic::AtomicU8::new(0))));
        assert!(client.chat_completion("m", "p", None).await.is_err());
        assert_eq!(receipts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn interrupted_error_body_is_not_reported_complete() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 999\r\n\r\n{\"error\":").await.unwrap();
        });
        let (client, receipts) = observed_client(&endpoint);
        assert!(client.chat_completion("m", "p", None).await.is_err());
        server.await.unwrap();
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[1].status, RequestStatus::Failed);
        assert_eq!(receipts[1].http_status, Some(400));
        assert!(!receipts[1].response_complete);
        assert_eq!(receipts[1].raw_usage, None);
    }

    #[tokio::test]
    async fn usage_fallback_consumes_request_allowance_and_other_errors_do_not_retry() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                match body["model"].as_str().unwrap() {
                    "unsupported" => (StatusCode::BAD_REQUEST, "stream_options is unsupported"),
                    "transient" => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "stream_options is unsupported",
                    ),
                    _ => (StatusCode::BAD_REQUEST, "stream_options backend failed"),
                }
            }),
        );
        let (endpoint, server) = serve(app).await;
        for model in ["unsupported", "transient", "invalid"] {
            let (client, receipts) = observed_client(&endpoint);
            let client =
                client.with_request_allowance(Some(Arc::new(std::sync::atomic::AtomicU8::new(1))));
            let error = client
                .chat_completion_json_schema_streaming_checked(
                    model,
                    "p",
                    None,
                    "s",
                    json!({}),
                    |_| {},
                )
                .await
                .unwrap_err();
            assert_eq!(receipts.lock().unwrap().len(), 2);
            assert_eq!(
                error.to_string().contains("allowance"),
                model == "unsupported"
            );
            assert_eq!(
                client
                    .stream_usage_unsupported
                    .load(std::sync::atomic::Ordering::Acquire)
                    != 0,
                model == "unsupported"
            );
        }
        server.abort();
    }

    #[test]
    fn receipt_endpoint_strips_credentials_and_bad_headers_are_omitted() {
        assert_eq!(
            sanitized_endpoint("https://user:password@example.test/v1?token=secret#private"),
            "https://example.test/v1"
        );
        assert_eq!(sanitized_endpoint("broken secret"), "invalid endpoint");
        let binding = UsageBinding {
            observer: Arc::new(|_| {}),
            context: UsageContext {
                purpose: "invalid\r\nAuthorization: secret".into(),
                ..Default::default()
            },
        };
        let mut observation =
            RequestObservation::start(Some(&binding), "https://example.test", "model", false);
        let request = observation
            .attribute(reqwest::Client::new().post("https://example.test"))
            .build()
            .unwrap();
        assert!(!request.headers().contains_key("x-moosedev-purpose"));
        assert!(request.headers().contains_key("x-moosedev-request-id"));
        observation.finish(true);
        // Fresh plain clients retain no callback or global request journal.
        let client = OpenAiCompatClient::new_with_structured_output(
            "https://example.test",
            "secret",
            StructuredOutputMode::Disabled,
        );
        assert!(client.usage_binding.is_none());
    }
}
