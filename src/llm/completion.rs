use moose::types::EngineError;

/// A provider delivery failure, distinct from invalid model JSON or action arguments.
#[derive(Debug)]
pub enum CompletionError {
    StructuredOutputUnsupported,
    UsageReportingUnsupported,
    ReasoningOnly,
    MissingContent,
    Incomplete(String),
    InvalidResponse(String),
    /// The connection failed or the provider went silent before a usable
    /// response arrived. Nothing executable was produced, so the same request
    /// may be sent again.
    Transport(String),
    Provider(EngineError),
}

impl std::fmt::Display for CompletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UsageReportingUnsupported => f.write_str("LLM provider does not support optional stream usage reporting"),
            Self::StructuredOutputUnsupported => f.write_str("LLM provider does not support required JSON-schema output"),
            Self::ReasoningOnly => f.write_str("LLM completed with reasoning only and no executable message content; check the model response policy"),
            Self::MissingContent => f.write_str("LLM response missing message content"),
            Self::Incomplete(message) | Self::InvalidResponse(message) | Self::Transport(message) => {
                f.write_str(message)
            }
            Self::Provider(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for CompletionError {}

impl CompletionError {
    pub(super) fn message(message: impl Into<String>) -> Self {
        Self::Provider(EngineError::InternalError(message.into()))
    }

    pub(super) fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }

    /// A connection, first-output or idle-read failure, as opposed to a provider
    /// answer or an invalid response.
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport(_))
    }

    pub(super) fn into_engine(self) -> EngineError {
        match self {
            Self::Provider(error) => error,
            error => EngineError::InternalError(error.to_string()),
        }
    }
}

fn has_payload(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

pub(super) fn complete_content(
    value: &serde_json::Value,
    strict: bool,
) -> Result<&str, CompletionError> {
    if let Some(error) = value.get("error") {
        return Err(CompletionError::InvalidResponse(format!(
            "LLM response error: {error}"
        )));
    }
    let choice = &value["choices"][0];
    let message = &choice["message"];
    if strict {
        if choice["finish_reason"].as_str() != Some("stop") {
            return Err(CompletionError::Incomplete(format!(
                "LLM completion ended without successful stop: {}",
                choice["finish_reason"]
            )));
        }
        if has_payload(&message["tool_calls"]) || has_payload(&message["refusal"]) {
            return Err(CompletionError::InvalidResponse(
                "LLM returned tools or refusal instead of content".into(),
            ));
        }
    }
    let content = message["content"].as_str();
    if content.is_none_or(|text| text.trim().is_empty()) {
        if has_payload(&message["reasoning_content"]) || has_payload(&message["reasoning"]) {
            return Err(CompletionError::ReasoningOnly);
        }
        return Err(CompletionError::MissingContent);
    }
    Ok(content.unwrap())
}

pub(super) const MAX_STREAM_BYTES: usize = 4 * 1024 * 1024;

/// Parse SSE on byte boundaries, including CRLF and UTF-8 split across chunks.
#[derive(Default)]
pub(super) struct CompletionStream {
    pending: Vec<u8>,
    data: String,
    pub(super) content: String,
    received: usize,
    stopped: bool,
    pub(super) done: bool,
    pub(super) usage: Option<serde_json::Value>,
    saw_reasoning: bool,
}

impl CompletionStream {
    pub(super) fn feed(
        &mut self,
        bytes: &[u8],
        on_delta: &impl Fn(&str),
    ) -> Result<(), CompletionError> {
        self.received = self.received.saturating_add(bytes.len());
        if self.received > MAX_STREAM_BYTES {
            return Err("LLM stream exceeds size limit".into());
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            let line =
                std::str::from_utf8(&line).map_err(|_| "LLM stream contains invalid UTF-8")?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.event(on_delta)?;
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
            if self.done {
                break;
            }
        }
        Ok(())
    }

    fn event(&mut self, on_delta: &impl Fn(&str)) -> Result<(), CompletionError> {
        let data = std::mem::take(&mut self.data);
        if data == "[DONE]" {
            if !self.stopped {
                return Err(CompletionError::Incomplete(
                    "LLM stream ended without successful finish_reason".into(),
                ));
            }
            self.done = true;
            return Ok(());
        }
        let value: serde_json::Value = serde_json::from_str(&data)
            .map_err(|error| format!("LLM stream event decode: {error}"))?;
        if value.get("usage").is_some_and(|usage| !usage.is_null()) {
            self.usage = Some(value.clone());
        }
        if let Some(error) = value.get("error") {
            return Err(CompletionError::InvalidResponse(format!(
                "LLM stream error: {error}"
            )));
        }
        let choices = value["choices"]
            .as_array()
            .ok_or("LLM stream event missing choices")?;
        for choice in choices {
            if choice["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            self.saw_reasoning |= has_payload(&choice["delta"]["reasoning_content"])
                || has_payload(&choice["delta"]["reasoning"]);
            if has_payload(&choice["delta"]["tool_calls"])
                || has_payload(&choice["delta"]["refusal"])
            {
                return Err("LLM stream returned tools or refusal instead of content".into());
            }
            if let Some(content) = choice["delta"]["content"].as_str() {
                if self.stopped && !content.is_empty() {
                    return Err(CompletionError::Incomplete(
                        "LLM stream content after finish_reason".into(),
                    ));
                }
                self.content.push_str(content);
                if !content.is_empty() {
                    on_delta(content);
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if reason != "stop" {
                    return Err(CompletionError::Incomplete(format!(
                        "LLM stream ended with {reason}"
                    )));
                }
                self.stopped = true;
            }
        }
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<(), CompletionError> {
        if !self.done || !self.stopped {
            return Err(CompletionError::Incomplete(
                "LLM stream interrupted before completion".into(),
            ));
        }
        if self.content.trim().is_empty() {
            return Err(if self.saw_reasoning {
                CompletionError::ReasoningOnly
            } else {
                CompletionError::MissingContent
            });
        }
        Ok(())
    }
}

impl From<&str> for CompletionError {
    fn from(message: &str) -> Self {
        Self::InvalidResponse(message.to_owned())
    }
}
impl From<String> for CompletionError {
    fn from(message: String) -> Self {
        Self::InvalidResponse(message)
    }
}
