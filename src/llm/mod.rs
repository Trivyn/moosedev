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

/// An OpenAI-compatible chat-completions client.
#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
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

        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| EngineError::InternalError(format!("LLM response missing content: {v}")))
    }
}
