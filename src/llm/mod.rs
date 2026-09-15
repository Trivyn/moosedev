//! OpenAI-compatible LLM client implementing MOOSE's `LlmClient` sensor trait.
//!
//! Local-first: points at an OpenAI-compatible endpoint (LM Studio, Ollama, …)
//! only when explicitly configured via environment variables. In MOOSE the LLM
//! is a *sensor*, not the controller; without provider config the server pins
//! assistance to pure symbolic mode.

mod completion;
mod usage;
pub use completion::CompletionError;
use completion::{complete_content, CompletionStream, MAX_STREAM_BYTES};
use usage::{RequestObservation, UsageBinding};
pub use usage::{RequestStatus, RequestUsage, TokenUsage, UsageContext, UsageObserver};

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

const DEFAULT_LLM_CONNECT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_LLM_FIRST_CHUNK_TIMEOUT_SECS: u64 = 300;
const DEFAULT_LLM_IDLE_TIMEOUT_SECS: u64 = 120;
const MAX_LLM_TIMEOUT_SECS: u64 = 86_400;

/// Bounds on one provider request. A streaming completion has no total bound,
/// because a slow local model may legitimately generate for minutes. Instead its
/// response must begin within `first_chunk` (which covers prompt prefill) and
/// then keep producing data, with no gap longer than `idle`. A non-streaming
/// request produces nothing until generation ends, so `first_chunk` bounds it
/// whole. `connect` bounds establishing the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmTimeouts {
    pub connect: std::time::Duration,
    pub first_chunk: std::time::Duration,
    pub idle: std::time::Duration,
}

impl Default for LlmTimeouts {
    fn default() -> Self {
        Self {
            connect: std::time::Duration::from_secs(DEFAULT_LLM_CONNECT_TIMEOUT_SECS),
            first_chunk: std::time::Duration::from_secs(DEFAULT_LLM_FIRST_CHUNK_TIMEOUT_SECS),
            idle: std::time::Duration::from_secs(DEFAULT_LLM_IDLE_TIMEOUT_SECS),
        }
    }
}

impl LlmTimeouts {
    /// `MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS`, `MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS`
    /// and `MOOSEDEV_LLM_IDLE_TIMEOUT_SECS`: whole seconds in 1..=86400. An invalid
    /// value is an error, never a silent default.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(
            std::env::var("MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS").ok(),
            std::env::var("MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS").ok(),
            std::env::var("MOOSEDEV_LLM_IDLE_TIMEOUT_SECS").ok(),
        )
    }

    fn from_values(
        connect: Option<String>,
        first_chunk: Option<String>,
        idle: Option<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            connect: timeout_seconds(
                "MOOSEDEV_LLM_CONNECT_TIMEOUT_SECS",
                connect,
                DEFAULT_LLM_CONNECT_TIMEOUT_SECS,
            )?,
            first_chunk: timeout_seconds(
                "MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS",
                first_chunk,
                DEFAULT_LLM_FIRST_CHUNK_TIMEOUT_SECS,
            )?,
            idle: timeout_seconds(
                "MOOSEDEV_LLM_IDLE_TIMEOUT_SECS",
                idle,
                DEFAULT_LLM_IDLE_TIMEOUT_SECS,
            )?,
        })
    }
}

fn timeout_seconds(
    name: &str,
    value: Option<String>,
    default: u64,
) -> anyhow::Result<std::time::Duration> {
    let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(std::time::Duration::from_secs(default));
    };
    let seconds = value
        .parse::<u64>()
        .ok()
        .filter(|seconds| (1..=MAX_LLM_TIMEOUT_SECS).contains(seconds))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{name} must be a whole number of seconds in 1..={MAX_LLM_TIMEOUT_SECS}; got {value:?}"
            )
        })?;
    Ok(std::time::Duration::from_secs(seconds))
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
    /// Request bounds; see [`LlmTimeouts`].
    pub timeouts: LlmTimeouts,
}

impl LlmConfig {
    /// `MOOSEDEV_LLM_BASE_URL` / `MOOSEDEV_LLM_API_KEY` / `MOOSEDEV_LLM_MODEL`.
    /// `MOOSEDEV_LLM_BASE_URL` is required to enable LLM-assisted sensors.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::from_values(
            std::env::var("MOOSEDEV_LLM_BASE_URL").ok(),
            std::env::var("MOOSEDEV_LLM_API_KEY").ok(),
            std::env::var("MOOSEDEV_LLM_MODEL").ok(),
            std::env::var("MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS").ok(),
            std::env::var("MOOSEDEV_LLM_STRUCTURED_OUTPUT").ok(),
        )?;
        config.timeouts = LlmTimeouts::from_env()?;
        Ok(config)
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
            timeouts: LlmTimeouts::default(),
        })
    }
}

/// Cumulative token usage observed on a client's chat-completions responses.
#[derive(Debug, Default)]
struct UsageCounters {
    prompt: AtomicU64,
    completion: AtomicU64,
    reported: AtomicU8,
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
    reasoning_off: bool,
    strict_content: bool,
    max_output_tokens: Option<u32>,
    request_allowance: Option<Arc<AtomicU8>>,
    usage_binding: Option<UsageBinding>,
    stream_usage_unsupported: Arc<AtomicU8>,
    timeouts: LlmTimeouts,
}

/// No total timeout: reqwest's would also cut a streamed body mid-generation.
/// Output bounds are applied per request instead (see [`LlmTimeouts`]).
fn http_client(timeouts: LlmTimeouts) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(timeouts.connect)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
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
        let timeouts = LlmTimeouts::default();
        let http = http_client(timeouts);
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            http,
            usage: Arc::new(UsageCounters::default()),
            structured_output_mode,
            structured_output_capability: Arc::new(AtomicU8::new(0)),
            reasoning_off: false,
            strict_content: false,
            max_output_tokens: None,
            request_allowance: None,
            usage_binding: None,
            stream_usage_unsupported: Arc::new(AtomicU8::new(0)),
            timeouts,
        }
    }

    /// Apply configured request bounds. This rebuilds the connection pool, so
    /// apply it before cloning the client for concurrent use.
    pub fn with_timeouts(mut self, timeouts: LlmTimeouts) -> Self {
        self.http = http_client(timeouts);
        self.timeouts = timeouts;
        self
    }

    /// Observe each physical HTTP request with caller-owned attribution. The
    /// callback is preserved by clones, including `with_fresh_usage`.
    pub fn with_usage_observer(mut self, observer: UsageObserver, context: UsageContext) -> Self {
        self.usage_binding = Some(UsageBinding { observer, context });
        self
    }

    /// Scope stricter completion semantics to a harness client, without changing
    /// daemon sensors or provider-wide defaults. Reasoning is never executable.
    pub fn for_harness(mut self, reasoning_off: bool) -> Self {
        self.reasoning_off = reasoning_off;
        self.strict_content = true;
        self
    }

    /// Bound neutral connection probes independently of ordinary action output.
    pub fn with_output_limit(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    pub fn without_output_limit(mut self) -> Self {
        self.max_output_tokens = None;
        self
    }

    pub fn with_request_allowance(mut self, allowance: Option<Arc<AtomicU8>>) -> Self {
        self.request_allowance = allowance;
        self
    }

    fn reserve_request(&self) -> Result<(), CompletionError> {
        if let Some(allowance) = &self.request_allowance {
            allowance
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                    left.checked_sub(1)
                })
                .map_err(|_| {
                    CompletionError::InvalidResponse(
                        "Neutral response probes exhausted their four-request allowance".into(),
                    )
                })?;
        }
        Ok(())
    }

    fn apply_request_options(&self, body: &mut serde_json::Value) {
        if self.reasoning_off {
            body["reasoning_effort"] = json!("none");
        }
        if let Some(tokens) = self.max_output_tokens {
            body["max_tokens"] = json!(tokens);
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
            reasoning_off: self.reasoning_off,
            strict_content: self.strict_content,
            max_output_tokens: self.max_output_tokens,
            request_allowance: self.request_allowance.clone(),
            usage_binding: self.usage_binding.clone(),
            stream_usage_unsupported: self.stream_usage_unsupported.clone(),
            timeouts: self.timeouts,
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
        self.chat_completion_json_schema_checked(model, prompt, params, schema_name, schema)
            .await
            .map_err(CompletionError::into_engine)
    }

    pub async fn chat_completion_json_schema_checked(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        schema_name: &str,
        schema: serde_json::Value,
    ) -> Result<String, CompletionError> {
        if self.structured_output_mode == StructuredOutputMode::Disabled
            || (self.structured_output_mode == StructuredOutputMode::Auto
                && self.structured_output_capability.load(Ordering::Acquire) == 2)
        {
            return self.request_completion(model, prompt, params, None).await;
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
            Err(CompletionError::StructuredOutputUnsupported) => {
                if self.structured_output_mode == StructuredOutputMode::Required {
                    return Err(CompletionError::StructuredOutputUnsupported);
                }
                self.structured_output_capability
                    .store(2, Ordering::Release);
                self.request_completion(model, prompt, params, None).await
            }
            Err(error) => Err(error),
        }
    }

    /// Stream content observations while returning only a completely finished
    /// response. Callers must validate the returned JSON before using it.
    pub async fn chat_completion_json_schema_streaming(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        schema_name: &str,
        schema: serde_json::Value,
        on_delta: impl Fn(&str) + Send + Sync,
    ) -> Result<String, EngineError> {
        self.chat_completion_json_schema_streaming_checked(
            model,
            prompt,
            params,
            schema_name,
            schema,
            on_delta,
        )
        .await
        .map_err(CompletionError::into_engine)
    }

    pub async fn chat_completion_json_schema_streaming_checked(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        schema_name: &str,
        schema: serde_json::Value,
        on_delta: impl Fn(&str) + Send + Sync,
    ) -> Result<String, CompletionError> {
        let use_schema = self.structured_output_mode != StructuredOutputMode::Disabled
            && !(self.structured_output_mode == StructuredOutputMode::Auto
                && self.structured_output_capability.load(Ordering::Acquire) == 2);
        let format = use_schema.then(|| {
            json!({
                "type": "json_schema",
                "json_schema": {"name": schema_name, "strict": true, "schema": schema}
            })
        });
        match self
            .request_stream(model, prompt, params, format, &on_delta)
            .await
        {
            Ok(text) => {
                if use_schema {
                    self.structured_output_capability
                        .store(1, Ordering::Release);
                }
                Ok(text)
            }
            Err(CompletionError::StructuredOutputUnsupported)
                if self.structured_output_mode == StructuredOutputMode::Auto =>
            {
                self.structured_output_capability
                    .store(2, Ordering::Release);
                self.request_stream(model, prompt, params, None, &on_delta)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn request_stream(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        response_format: Option<serde_json::Value>,
        on_delta: &(impl Fn(&str) + Send + Sync),
    ) -> Result<String, CompletionError> {
        match self
            .request_stream_once(model, prompt, params, response_format.clone(), on_delta)
            .await
        {
            Err(CompletionError::UsageReportingUnsupported) => {
                self.stream_usage_unsupported.store(1, Ordering::Release);
                self.request_stream_once(model, prompt, params, response_format, on_delta)
                    .await
            }
            result => result,
        }
    }

    async fn request_stream_once(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
        response_format: Option<serde_json::Value>,
        on_delta: &(impl Fn(&str) + Send + Sync),
    ) -> Result<String, CompletionError> {
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": params.and_then(|p| p.temperature).unwrap_or(0.0),
            "stream": true,
        });
        if self.stream_usage_unsupported.load(Ordering::Acquire) == 0 {
            body["stream_options"] = json!({"include_usage": true});
        }
        if let Some(format) = response_format {
            body["response_format"] = format;
        }
        self.reserve_request()?;
        self.apply_request_options(&mut body);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut observation =
            RequestObservation::start(self.usage_binding.as_ref(), &url, model, true);
        let first_bound = self.timeouts.first_chunk;
        let silent = |url: &str| {
            CompletionError::transport(format!(
                "LLM request to {url}: no model output within {}s (MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS)",
                first_bound.as_secs()
            ))
        };
        let result = async {
            let request = self.http.post(&url).bearer_auth(&self.api_key).json(&body);
            // The response must begin, headers and first body data alike, within the
            // first-chunk bound; after that only gaps between chunks are bounded.
            let first_deadline = tokio::time::Instant::now() + first_bound;
            let mut response =
                tokio::time::timeout_at(first_deadline, observation.attribute(request).send())
                    .await
                    .map_err(|_| silent(&url))?
                    .map_err(|error| {
                        CompletionError::transport(format!("LLM request to {url}: {error}"))
                    })?;
            observation.http_status(response.status());
            if !response.status().is_success() {
                let status = response.status();
                let received = tokio::time::timeout(self.timeouts.idle, response.text())
                    .await
                    .ok()
                    .and_then(Result::ok);
                if received.is_some() {
                    observation.response_complete();
                }
                let text = received.unwrap_or_default();
                if let Ok(value) = serde_json::from_str(&text) {
                    observation.observe(&value);
                }
                let lower = text.to_ascii_lowercase();
                if body.get("stream_options").is_some()
                    && matches!(status.as_u16(), 400 | 422)
                    && (lower.contains("stream_options") || lower.contains("include_usage"))
                    && [
                        "unsupported",
                        "not supported",
                        "unrecognized",
                        "unknown",
                        "not permitted",
                        "not allowed",
                        "unexpected",
                        "extra inputs",
                    ]
                    .iter()
                    .any(|reason| lower.contains(reason))
                {
                    return Err(CompletionError::UsageReportingUnsupported);
                }
                if body.get("response_format").is_some()
                    && matches!(status.as_u16(), 400 | 404 | 422)
                    && (lower.contains("response_format")
                        || lower.contains("json_schema")
                        || lower.contains("structured"))
                {
                    return Err(CompletionError::StructuredOutputUnsupported);
                }
                return Err(CompletionError::message(format!(
                    "LLM endpoint returned HTTP {status}: {text}"
                )));
            }
            let is_json = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json"));
            let mut stream = CompletionStream::default();
            let mut json_body = Vec::new();
            let mut started = false;
            loop {
                let next = if started {
                    tokio::time::timeout(self.timeouts.idle, response.chunk()).await
                } else {
                    tokio::time::timeout_at(first_deadline, response.chunk()).await
                };
                let chunk = match next {
                    Err(_) if started => {
                        return Err(CompletionError::transport(format!(
                            "LLM stream stalled: no data for {}s (MOOSEDEV_LLM_IDLE_TIMEOUT_SECS)",
                            self.timeouts.idle.as_secs()
                        )))
                    }
                    Err(_) => return Err(silent(&url)),
                    Ok(Err(error)) => {
                        return Err(CompletionError::transport(format!(
                            "LLM stream read: {error}"
                        )))
                    }
                    Ok(Ok(None)) => break,
                    Ok(Ok(Some(chunk))) => chunk,
                };
                started = true;
                if is_json {
                    if json_body.len().saturating_add(chunk.len()) > MAX_STREAM_BYTES {
                        return Err(CompletionError::message("LLM response exceeds size limit"));
                    }
                    json_body.extend_from_slice(&chunk);
                } else {
                    let fed = stream.feed(&chunk, on_delta);
                    if let Some(usage) = &stream.usage {
                        observation.observe(usage);
                    }
                    if stream.done {
                        observation.response_complete();
                    }
                    fed?;
                    if stream.done {
                        break;
                    }
                }
            }
            if is_json {
                observation.response_complete();
                // Some compatible servers ignore stream=true. Their complete JSON
                // response remains usable; it simply produces one observation.
                let value: serde_json::Value =
                    serde_json::from_slice(&json_body).map_err(|error| {
                        CompletionError::message(format!("LLM response decode: {error}"))
                    })?;
                observation.observe(&value);
                self.record_usage(&value);
                let text = complete_content(&value, self.strict_content)?;
                on_delta(text);
                return Ok(text.to_owned());
            }
            if let Some(usage) = &stream.usage {
                self.record_usage(usage);
            }
            stream.finish()?;
            Ok(stream.content)
        }
        .await;
        observation.finish(result.is_ok());
        result
    }

    /// `(prompt_tokens, completion_tokens)` accumulated since construction/fork,
    /// resetting the counters to zero.
    pub fn take_usage(&self) -> (u64, u64) {
        (
            self.usage.prompt.swap(0, Ordering::Relaxed),
            self.usage.completion.swap(0, Ordering::Relaxed),
        )
    }

    /// Unlike legacy sensor counters, a probe receipt must distinguish absent
    /// usage from a measured zero.
    pub fn take_usage_observation(&self) -> Option<(u64, u64)> {
        let reported = self.usage.reported.swap(0, Ordering::AcqRel) != 0;
        let usage = self.take_usage();
        reported.then_some(usage)
    }

    /// Accumulate `usage.prompt_tokens` / `usage.completion_tokens` from a
    /// chat-completions response body; absent fields count as 0.
    fn record_usage(&self, body: &serde_json::Value) {
        if body["usage"]["prompt_tokens"].is_u64() && body["usage"]["completion_tokens"].is_u64() {
            self.usage.reported.store(1, Ordering::Release);
        }
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
    ) -> Result<String, CompletionError> {
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
        self.reserve_request()?;
        self.apply_request_options(&mut body);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut observation =
            RequestObservation::start(self.usage_binding.as_ref(), &url, model, false);
        let bound = self.timeouts.first_chunk;
        let result = match tokio::time::timeout(
            bound,
            async {
                let request = self.http.post(&url).bearer_auth(&self.api_key).json(&body);
                let mut resp = observation
                    .attribute(request)
                    .send()
                    .await
                    .map_err(|error| {
                        CompletionError::transport(format!("LLM request to {url}: {error}"))
                    })?;
                observation.http_status(resp.status());
                if !resp.status().is_success() {
                    let status = resp.status();
                    let received = resp.text().await;
                    if received.is_ok() {
                        observation.response_complete();
                    }
                    let text = received.unwrap_or_default();
                    if let Ok(value) = serde_json::from_str(&text) {
                        observation.observe(&value);
                    }
                    let lower = text.to_ascii_lowercase();
                    if body.get("response_format").is_some()
                        && matches!(status.as_u16(), 400 | 404 | 422)
                        && (lower.contains("response_format")
                            || lower.contains("json_schema")
                            || lower.contains("structured"))
                    {
                        return Err(CompletionError::StructuredOutputUnsupported);
                    }
                    return Err(CompletionError::Provider(EngineError::InternalError(
                        format!("LLM endpoint returned HTTP {status}: {text}"),
                    )));
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = resp.chunk().await.map_err(|error| {
                    CompletionError::transport(format!("LLM response read: {error}"))
                })? {
                    if bytes.len().saturating_add(chunk.len()) > MAX_STREAM_BYTES {
                        return Err(CompletionError::InvalidResponse(
                            "LLM response exceeds size limit".into(),
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                observation.response_complete();
                let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                    CompletionError::InvalidResponse(format!("LLM response decode: {error}"))
                })?;
                observation.observe(&value);
                self.record_usage(&value);
                complete_content(&value, self.strict_content).map(str::to_owned)
            },
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(CompletionError::transport(format!(
                "LLM request to {url}: no complete response within {}s (MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS bounds non-streaming requests)",
                bound.as_secs()
            ))),
        };
        observation.finish(result.is_ok());
        result
    }
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
            .map_err(CompletionError::into_engine)
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

    fn sse(content: &str, finish: &str) -> String {
        format!(
            "data: {}\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({"choices":[{"index":0,"delta":{"content":content}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
        )
    }

    #[test]
    fn streaming_decodes_every_byte_boundary_and_unicode() {
        let body = sse("{\"message\":\"héllo 🫎\"}", "stop");
        for size in 1..body.len() {
            let mut stream = CompletionStream::default();
            let observed = Mutex::new(String::new());
            for chunk in body.as_bytes().chunks(size) {
                stream
                    .feed(chunk, &|text| observed.lock().unwrap().push_str(text))
                    .unwrap();
            }
            stream.finish().unwrap();
            assert_eq!(stream.content, "{\"message\":\"héllo 🫎\"}");
            assert_eq!(stream.content, *observed.lock().unwrap());
        }
    }

    #[test]
    fn streaming_rejects_incomplete_malformed_and_unsuccessful_output() {
        for body in [
            "data: [DONE]\n\n".to_owned(),
            "data: {broken}\n\n".to_owned(),
            "data: {\"error\":\"failed\"}\n\n".to_owned(),
            sse("partial", "length"),
            sse("blocked", "content_filter"),
            sse("partial", "stop").replace("data: [DONE]\r\n\r\n", ""),
        ] {
            let mut stream = CompletionStream::default();
            assert!(
                stream
                    .feed(body.as_bytes(), &|_| {})
                    .and_then(|_| stream.finish())
                    .is_err(),
                "accepted {body}"
            );
        }
    }

    #[tokio::test]
    async fn streaming_auto_fallback_preserves_observations_and_usage() {
        async fn complete(Json(body): Json<serde_json::Value>) -> axum::response::Response {
            assert_eq!(body["stream"], true);
            if body.get("response_format").is_some() {
                return (StatusCode::UNPROCESSABLE_ENTITY, "json_schema unsupported")
                    .into_response();
            }
            let events = sse("{\"message\":\"hello\"}", "stop").replace(
                "data: [DONE]",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\r\n\r\ndata: [DONE]",
            );
            ([("content-type", "text/event-stream")], events).into_response()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(complete)),
            )
            .await
            .unwrap();
        });
        let client = OpenAiCompatClient::new(format!("http://{address}/v1"), "test");
        let observed = Mutex::new(String::new());
        let result = client
            .chat_completion_json_schema_streaming(
                "model",
                "prompt",
                None,
                "response",
                json!({"type":"object"}),
                |text| observed.lock().unwrap().push_str(text),
            )
            .await
            .unwrap();
        assert_eq!(result, "{\"message\":\"hello\"}");
        assert_eq!(result, *observed.lock().unwrap());
        assert_eq!(
            client.structured_output_capability.load(Ordering::Acquire),
            2
        );
        assert_eq!(client.take_usage(), (2, 3));
        server.abort();
    }

    fn short_timeouts() -> LlmTimeouts {
        LlmTimeouts {
            connect: std::time::Duration::from_secs(5),
            first_chunk: std::time::Duration::from_secs(1),
            idle: std::time::Duration::from_secs(1),
        }
    }

    /// Serve one connection: write `prefix`, then either finish with `tail`
    /// after paced pieces or hold the connection open forever.
    async fn raw_stub(
        prefix: &'static str,
        pieces: Vec<String>,
        pace: std::time::Duration,
        tail: Option<String>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 16384];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(prefix.as_bytes()).await.unwrap();
            for piece in pieces {
                tokio::time::sleep(pace).await;
                socket
                    .write_all(format!("{:x}\r\n{piece}\r\n", piece.len()).as_bytes())
                    .await
                    .unwrap();
            }
            match tail {
                Some(tail) => socket
                    .write_all(format!("{:x}\r\n{tail}\r\n0\r\n\r\n", tail.len()).as_bytes())
                    .await
                    .unwrap(),
                None => std::future::pending::<()>().await,
            }
        });
        (endpoint, server)
    }

    const SSE_HEADERS: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

    fn sse_piece(content: &str) -> String {
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"index":0,"delta":{"content":content}}]})
        )
    }

    #[test]
    fn llm_timeouts_default_parse_and_reject_invalid_values() {
        let defaults = LlmTimeouts::from_values(None, None, None).unwrap();
        assert_eq!(defaults, LlmTimeouts::default());
        assert_eq!(defaults.connect, std::time::Duration::from_secs(10));
        assert_eq!(defaults.first_chunk, std::time::Duration::from_secs(300));
        assert_eq!(defaults.idle, std::time::Duration::from_secs(120));
        let chosen =
            LlmTimeouts::from_values(Some("7".into()), Some(" 600 ".into()), Some("".into()))
                .unwrap();
        assert_eq!(chosen.connect, std::time::Duration::from_secs(7));
        assert_eq!(chosen.first_chunk, std::time::Duration::from_secs(600));
        assert_eq!(chosen.idle, std::time::Duration::from_secs(120));
        for invalid in ["0", "-1", "abc", "86401"] {
            let error = LlmTimeouts::from_values(None, None, Some(invalid.into()))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("MOOSEDEV_LLM_IDLE_TIMEOUT_SECS"),
                "{invalid}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn a_stream_that_outlasts_every_single_bound_still_completes() {
        let pieces = ["{\"message\":", "\"slow", " but", " steady", "\"}"]
            .iter()
            .map(|piece| sse_piece(piece))
            .collect();
        let tail = format!(
            "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        let (endpoint, server) = raw_stub(
            SSE_HEADERS,
            pieces,
            std::time::Duration::from_millis(450),
            Some(tail),
        )
        .await;
        let client = OpenAiCompatClient::new(endpoint, "test").with_timeouts(short_timeouts());
        let started = std::time::Instant::now();
        let text = client
            .chat_completion_json_schema_streaming_checked(
                "model",
                "prompt",
                None,
                "shape",
                json!({}),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(text, "{\"message\":\"slow but steady\"}");
        assert!(
            started.elapsed() > std::time::Duration::from_secs(2),
            "the stream must outlast both the first-chunk and the idle bound"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_and_silent_providers_fail_as_bounded_transport_errors() {
        let cases = [
            (
                SSE_HEADERS,
                vec![sse_piece("{\"message\":")],
                true,
                "MOOSEDEV_LLM_IDLE_TIMEOUT_SECS",
            ),
            (SSE_HEADERS, vec![], true, "MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS"),
            (
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"cho",
                vec![],
                false,
                "MOOSEDEV_LLM_FIRST_CHUNK_TIMEOUT_SECS",
            ),
        ];
        for (prefix, pieces, streaming, bound) in cases {
            let (endpoint, server) =
                raw_stub(prefix, pieces, std::time::Duration::from_millis(10), None).await;
            let client = OpenAiCompatClient::new(endpoint, "test").with_timeouts(short_timeouts());
            let started = std::time::Instant::now();
            let error = if streaming {
                client
                    .chat_completion_json_schema_streaming_checked(
                        "model",
                        "prompt",
                        None,
                        "shape",
                        json!({}),
                        |_| {},
                    )
                    .await
                    .unwrap_err()
            } else {
                client
                    .chat_completion_json_schema_checked(
                        "model",
                        "prompt",
                        None,
                        "shape",
                        json!({}),
                    )
                    .await
                    .unwrap_err()
            };
            assert!(error.is_transport(), "{bound}: {error}");
            assert!(error.to_string().contains(bound), "{bound}: {error}");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{bound} took {:?}",
                started.elapsed()
            );
            server.abort();
        }
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
    #[test]
    fn checked_content_never_uses_reasoning_or_incomplete_results() {
        let make = |message: serde_json::Value, finish: serde_json::Value| json!({"choices":[{"message":message,"finish_reason":finish}]});
        assert!(matches!(
            complete_content(
                &make(
                    json!({"content":"","reasoning_content":"{\"action\":\"finish\"}"}),
                    json!("stop")
                ),
                true
            ),
            Err(CompletionError::ReasoningOnly)
        ));
        assert!(matches!(
            complete_content(&make(json!({"content":"{}"}), json!("length")), true),
            Err(CompletionError::Incomplete(_))
        ));
        assert!(matches!(
            complete_content(
                &make(json!({"content":"{}"}), serde_json::Value::Null),
                true
            ),
            Err(CompletionError::Incomplete(_))
        ));
        for message in [
            json!({"content":"{}","tool_calls":[{}]}),
            json!({"content":"{}","refusal":"no"}),
        ] {
            assert!(matches!(
                complete_content(&make(message, json!("stop")), true),
                Err(CompletionError::InvalidResponse(_))
            ));
        }
        let value = make(
            json!({"content":"{}","reasoning_content":"discard me","tool_calls":[]}),
            json!("stop"),
        );
        assert_eq!(complete_content(&value, true).unwrap(), "{}");
        assert!(matches!(
            complete_content(&make(json!({"content":"  "}), json!("stop")), true),
            Err(CompletionError::MissingContent)
        ));
    }

    #[test]
    fn reasoning_deltas_are_observed_only_as_presence() {
        let reasoning = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"reasoning_content":"{\"action\":\"finish\"}"}}]})
        );
        let mut stream = CompletionStream::default();
        let mut observed = String::new();
        stream
            .feed(reasoning.as_bytes(), &|_| {
                panic!("reasoning must not be emitted")
            })
            .unwrap();
        stream.feed(sse("", "stop").as_bytes(), &|_| {}).unwrap();
        assert!(matches!(
            stream.finish().unwrap_err(),
            CompletionError::ReasoningOnly
        ));
        let mut stream = CompletionStream::default();
        let deltas = Mutex::new(String::new());
        stream
            .feed(reasoning.as_bytes(), &|text| {
                deltas.lock().unwrap().push_str(text)
            })
            .unwrap();
        stream
            .feed(sse("{}", "stop").as_bytes(), &|text| {
                deltas.lock().unwrap().push_str(text)
            })
            .unwrap();
        stream.finish().unwrap();
        observed.push_str(&deltas.into_inner().unwrap());
        assert_eq!(observed, "{}");
    }

    #[test]
    fn harness_request_options_do_not_change_daemon_clients() {
        let base = client();
        let mut body = json!({});
        base.apply_request_options(&mut body);
        assert_eq!(body, json!({}));
        let harness = base
            .clone()
            .for_harness(true)
            .with_output_limit(128)
            .with_fresh_usage();
        harness.apply_request_options(&mut body);
        assert_eq!(body, json!({"reasoning_effort":"none","max_tokens":128}));
        let mut later = json!({});
        harness
            .without_output_limit()
            .apply_request_options(&mut later);
        assert_eq!(later, json!({"reasoning_effort":"none"}));
        let mut unchanged = json!({});
        base.apply_request_options(&mut unchanged);
        assert_eq!(unchanged, json!({}));
    }
}
