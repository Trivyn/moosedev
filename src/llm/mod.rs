//! OpenAI-compatible LLM client implementing MOOSE's `LlmClient` sensor trait.
//!
//! Local-first: points at an OpenAI-compatible endpoint (LM Studio, Ollama, …)
//! only when explicitly configured via environment variables. In MOOSE the LLM
//! is a *sensor*, not the controller; without provider config the server pins
//! assistance to pure symbolic mode.

use async_trait::async_trait;
use moose::traits::LlmClient;
use moose::types::{EngineError, LlmParams};
use serde_json::json;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

const DEFAULT_LLM_BASE_URL: &str = "http://localhost:1234/v1";
const DEFAULT_LLM_API_KEY: &str = "lm-studio";
const DEFAULT_LLM_MODEL: &str = "gemma-4-31b-it";
pub const DEFAULT_LLM_CONTEXT_WINDOW_TOKENS: usize = 32_768;
const MIN_LLM_CONTEXT_WINDOW_TOKENS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputMode {
    Auto,
    Required,
    Disabled,
}

impl StructuredOutputMode {
    fn parse(value: Option<String>) -> anyhow::Result<Self> {
        match value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None | Some("auto") => Ok(Self::Auto),
            Some("required") => Ok(Self::Required),
            Some("disabled") => Ok(Self::Disabled),
            Some(value) => anyhow::bail!(
                "MOOSEDEV_LLM_STRUCTURED_OUTPUT must be auto, required, or disabled; got {value:?}"
            ),
        }
    }
}

/// Endpoint + model selection, read from the environment. A base URL is the
/// explicit opt-in for LLM assistance; without it the server stays symbolic.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub configured: bool,
    pub context_window_tokens: usize,
    pub structured_output: StructuredOutputMode,
}

impl LlmConfig {
    /// `MOOSEDEV_LLM_BASE_URL` / `MOOSEDEV_LLM_API_KEY` / `MOOSEDEV_LLM_MODEL`.
    /// `MOOSEDEV_LLM_BASE_URL` is required to enable LLM-assisted sensors.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(
            std::env::var("MOOSEDEV_LLM_BASE_URL").ok(),
            std::env::var("MOOSEDEV_LLM_API_KEY").ok(),
            std::env::var("MOOSEDEV_LLM_MODEL").ok(),
            std::env::var("MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS").ok(),
            std::env::var("MOOSEDEV_LLM_STRUCTURED_OUTPUT").ok(),
        )
    }

    fn from_values(
        base_url: Option<String>,
        api_key: Option<String>,
        model: Option<String>,
        context_window_tokens: Option<String>,
        structured_output: Option<String>,
    ) -> anyhow::Result<Self> {
        let configured = base_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let context_window_tokens = context_window_tokens
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| {
                anyhow::anyhow!("MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS must be a positive integer")
            })?
            .unwrap_or(DEFAULT_LLM_CONTEXT_WINDOW_TOKENS);
        anyhow::ensure!(
            context_window_tokens >= MIN_LLM_CONTEXT_WINDOW_TOKENS,
            "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS must be at least {MIN_LLM_CONTEXT_WINDOW_TOKENS}"
        );
        Ok(Self {
            base_url: base_url
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string()),
            api_key: api_key
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_LLM_API_KEY.to_string()),
            model: model
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_LLM_MODEL.to_string()),
            configured,
            context_window_tokens,
            structured_output: StructuredOutputMode::parse(structured_output)?,
        })
    }
}

/// Cumulative token usage observed on a client's chat-completions responses.
#[derive(Debug, Default)]
struct UsageCounters {
    prompt: AtomicU64,
    completion: AtomicU64,
}

/// An OpenAI-compatible chat-completions client.
///
/// Token usage is accumulated (interior mutability) because MOOSE's `LlmClient`
/// trait returns only the completion text; [`with_fresh_usage`](Self::with_fresh_usage)
/// + [`take_usage`](Self::take_usage) let a caller attribute usage to one query.
#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
    usage: Arc<UsageCounters>,
    structured_output_mode: StructuredOutputMode,
    structured_output_capability: Arc<AtomicU8>,
}

impl OpenAiCompatClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::new_with_structured_output(base_url, api_key, StructuredOutputMode::Auto)
    }

    pub fn new_with_structured_output(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        structured_output_mode: StructuredOutputMode,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            http,
            usage: Arc::new(UsageCounters::default()),
            structured_output_mode,
            structured_output_capability: Arc::new(AtomicU8::new(0)),
        }
    }

    /// A clone that shares the HTTP pool and endpoint config but accumulates
    /// token usage into its own **fresh** counters — so usage can be attributed
    /// to a single query even under concurrent backend use.
    pub fn with_fresh_usage(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            http: self.http.clone(),
            usage: Arc::new(UsageCounters::default()),
            structured_output_mode: self.structured_output_mode,
            structured_output_capability: self.structured_output_capability.clone(),
        }
    }

    /// Request a strict JSON-schema response when the configured provider
    /// supports it. Auto mode remembers an explicit unsupported response and
    /// falls back to ordinary validated JSON for later calls.
    pub async fn chat_completion_json_schema(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        schema_name: &str,
        schema: serde_json::Value,
    ) -> Result<String, EngineError> {
        if self.structured_output_mode == StructuredOutputMode::Disabled
            || (self.structured_output_mode == StructuredOutputMode::Auto
                && self.structured_output_capability.load(Ordering::Acquire) == 2)
        {
            return self.chat_completion(model, prompt, params).await;
        }
        let response_format = json!({
            "type": "json_schema",
            "json_schema": {
                "name": schema_name,
                "strict": true,
                "schema": schema,
            }
        });
        match self
            .request_completion(model, prompt, params, Some(response_format))
            .await
        {
            Ok(text) => {
                self.structured_output_capability
                    .store(1, Ordering::Release);
                Ok(text)
            }
            Err(RequestError::StructuredOutputUnsupported) => {
                if self.structured_output_mode == StructuredOutputMode::Required {
                    return Err(EngineError::InternalError(
                        "LLM provider does not support required JSON-schema output".to_string(),
                    ));
                }
                self.structured_output_capability
                    .store(2, Ordering::Release);
                self.chat_completion(model, prompt, params).await
            }
            Err(RequestError::Engine(error)) => Err(error),
        }
    }

    /// `(prompt_tokens, completion_tokens)` accumulated since construction/fork,
    /// resetting the counters to zero.
    pub fn take_usage(&self) -> (u64, u64) {
        (
            self.usage.prompt.swap(0, Ordering::Relaxed),
            self.usage.completion.swap(0, Ordering::Relaxed),
        )
    }

    /// Accumulate `usage.prompt_tokens` / `usage.completion_tokens` from a
    /// chat-completions response body; absent fields count as 0.
    fn record_usage(&self, body: &serde_json::Value) {
        let prompt = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let completion = body["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        if prompt > 0 {
            self.usage.prompt.fetch_add(prompt, Ordering::Relaxed);
        }
        if completion > 0 {
            self.usage
                .completion
                .fetch_add(completion, Ordering::Relaxed);
        }
    }

    async fn request_completion(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        response_format: Option<serde_json::Value>,
    ) -> Result<String, RequestError> {
        let temperature = params.and_then(|p| p.temperature).unwrap_or(0.0);
        let mut body = json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
            "temperature": temperature,
            "stream": false,
        });
        if let Some(response_format) = response_format {
            body["response_format"] = response_format;
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                RequestError::Engine(EngineError::InternalError(format!(
                    "LLM request to {url}: {error}"
                )))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let lower = text.to_ascii_lowercase();
            if body.get("response_format").is_some()
                && matches!(status.as_u16(), 400 | 404 | 422)
                && (lower.contains("response_format")
                    || lower.contains("json_schema")
                    || lower.contains("structured"))
            {
                return Err(RequestError::StructuredOutputUnsupported);
            }
            return Err(RequestError::Engine(EngineError::InternalError(format!(
                "LLM endpoint returned HTTP {status}: {text}"
            ))));
        }
        let value: serde_json::Value = resp.json().await.map_err(|error| {
            RequestError::Engine(EngineError::InternalError(format!(
                "LLM response decode: {error}"
            )))
        })?;
        self.record_usage(&value);
        value["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                RequestError::Engine(EngineError::InternalError(
                    "LLM response missing message content".to_string(),
                ))
            })
    }
}

enum RequestError {
    StructuredOutputUnsupported,
    Engine(EngineError),
}

#[async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn chat_completion(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
    ) -> Result<String, EngineError> {
        self.request_completion(model, prompt, params, None)
            .await
            .map_err(|error| match error {
                RequestError::StructuredOutputUnsupported => EngineError::InternalError(
                    "LLM provider rejected structured output".to_string(),
                ),
                RequestError::Engine(error) => error,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router,
    };
    use serde_json::json;
    use std::sync::Mutex;

    fn client() -> OpenAiCompatClient {
        OpenAiCompatClient::new("http://localhost:1234/v1", "test")
    }

    #[test]
    fn config_requires_explicit_base_url_to_enable_llm() {
        let cfg = LlmConfig::from_values(None, None, None, None, None).unwrap();
        assert!(!cfg.configured);
        assert_eq!(cfg.base_url, DEFAULT_LLM_BASE_URL);
        assert_eq!(cfg.api_key, DEFAULT_LLM_API_KEY);
        assert_eq!(cfg.model, DEFAULT_LLM_MODEL);
        assert_eq!(cfg.context_window_tokens, DEFAULT_LLM_CONTEXT_WINDOW_TOKENS);
        assert_eq!(cfg.structured_output, StructuredOutputMode::Auto);
    }

    #[test]
    fn config_treats_nonempty_base_url_as_provider_opt_in() {
        let cfg = LlmConfig::from_values(
            Some("http://localhost:9999/v1".to_string()),
            Some("secret".to_string()),
            Some("model-a".to_string()),
            Some("131072".to_string()),
            Some("required".to_string()),
        )
        .unwrap();
        assert!(cfg.configured);
        assert_eq!(cfg.base_url, "http://localhost:9999/v1");
        assert_eq!(cfg.api_key, "secret");
        assert_eq!(cfg.model, "model-a");
        assert_eq!(cfg.context_window_tokens, 131_072);
        assert_eq!(cfg.structured_output, StructuredOutputMode::Required);
    }

    #[test]
    fn config_rejects_invalid_context_and_structured_output_values() {
        assert!(LlmConfig::from_values(None, None, None, Some("4095".to_string()), None,).is_err());
        assert!(
            LlmConfig::from_values(None, None, None, None, Some("sometimes".to_string()),).is_err()
        );
    }

    #[test]
    fn record_usage_accumulates_and_take_resets() {
        let c = client();
        c.record_usage(&json!({"usage": {"prompt_tokens": 12, "completion_tokens": 7}}));
        c.record_usage(&json!({"usage": {"prompt_tokens": 3, "completion_tokens": 1}}));
        assert_eq!(c.take_usage(), (15, 8));
        // take_usage resets the counters.
        assert_eq!(c.take_usage(), (0, 0));
    }

    #[test]
    fn record_usage_treats_missing_fields_as_zero() {
        let c = client();
        c.record_usage(&json!({ "choices": [] })); // no usage block at all
        c.record_usage(&json!({"usage": {"prompt_tokens": 5}})); // completion missing
        assert_eq!(c.take_usage(), (5, 0));
    }

    #[test]
    fn with_fresh_usage_isolates_counters() {
        let base = client();
        base.record_usage(&json!({"usage": {"prompt_tokens": 100, "completion_tokens": 100}}));
        let forked = base.with_fresh_usage();
        forked.record_usage(&json!({"usage": {"prompt_tokens": 2, "completion_tokens": 3}}));
        // The fork sees only its own usage…
        assert_eq!(forked.take_usage(), (2, 3));
        // …and the base is unaffected by the fork's calls.
        assert_eq!(base.take_usage(), (100, 100));
    }

    #[tokio::test]
    async fn structured_auto_falls_back_once_and_remembers_provider_capability() {
        #[derive(Clone, Default)]
        struct Requests(Arc<Mutex<Vec<serde_json::Value>>>);

        async fn complete(
            State(requests): State<Requests>,
            Json(body): Json<serde_json::Value>,
        ) -> impl IntoResponse {
            requests.0.lock().unwrap().push(body.clone());
            if body.get("response_format").is_some() {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error": "json_schema response_format is unsupported"})),
                );
            }
            (
                StatusCode::OK,
                Json(json!({
                    "choices": [{"message": {"content": "{\"paragraphs\":[]}"}}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                })),
            )
        }

        let requests = Requests::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(complete))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = OpenAiCompatClient::new_with_structured_output(
            format!("http://{address}/v1"),
            "test",
            StructuredOutputMode::Auto,
        );
        let schema = json!({"type": "object"});
        for _ in 0..2 {
            let text = client
                .chat_completion_json_schema("model", "prompt", None, "story", schema.clone())
                .await
                .unwrap();
            assert_eq!(text, "{\"paragraphs\":[]}");
        }
        let requests = requests.0.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].get("response_format").is_some());
        assert!(requests[1].get("response_format").is_none());
        assert!(requests[2].get("response_format").is_none());
        assert_eq!(client.take_usage(), (4, 6));
    }
}
