//! OpenAI-compatible LLM client implementing MOOSE's `LlmClient` sensor trait.
//!
//! Local-first: points at any OpenAI-compatible endpoint (LM Studio, Ollama, …)
//! configured via environment variables. In MOOSE the LLM is a *sensor*, not the
//! controller — the default assist level is kept low for determinism, and many
//! queries never call it at all.

use async_trait::async_trait;
use moose::traits::LlmClient;
use moose::types::{EngineError, LlmParams};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Endpoint + model selection, read from the environment with local-first defaults.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl LlmConfig {
    /// `MOOSEDEV_LLM_BASE_URL` / `MOOSEDEV_LLM_API_KEY` / `MOOSEDEV_LLM_MODEL`.
    /// Defaults target a local OpenAI-compatible server (LM Studio's port).
    pub fn from_env() -> Self {
        Self {
            base_url: env_or("MOOSEDEV_LLM_BASE_URL", "http://localhost:1234/v1"),
            api_key: env_or("MOOSEDEV_LLM_API_KEY", "lm-studio"),
            model: env_or("MOOSEDEV_LLM_MODEL", "gemma-4-31b-it"),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
}

impl OpenAiCompatClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            http,
            usage: Arc::new(UsageCounters::default()),
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
}

#[async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn chat_completion(
        &self,
        model: &str,
        prompt: &str,
        params: Option<&LlmParams>,
    ) -> Result<String, EngineError> {
        let temperature = params.and_then(|p| p.temperature).unwrap_or(0.0);
        let body = json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
            "temperature": temperature,
            "stream": false,
        });
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::InternalError(format!("LLM request to {url}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(EngineError::InternalError(format!(
                "LLM endpoint returned HTTP {status}: {text}"
            )));
        }

        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| EngineError::InternalError(format!("LLM response decode: {e}")))?;

        self.record_usage(&v);

        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| EngineError::InternalError(format!("LLM response missing content: {v}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn client() -> OpenAiCompatClient {
        OpenAiCompatClient::new("http://localhost:1234/v1", "test")
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
}
