use moose::types::EngineError;

/// A provider delivery failure, distinct from invalid model JSON or action arguments.
#[derive(Debug)]
pub enum CompletionError {
    StructuredOutputUnsupported,
    UsageReportingUnsupported,
    ReasoningOnly,
    MissingContent,
    /// The provider refused `tool_choice: "required"` (or `parallel_tool_calls`).
    ToolChoiceUnsupported(String),
    Incomplete(String),
    InvalidResponse(String),
    /// The connection failed or the provider went silent before a usable
    /// response arrived. Nothing executable was produced, so the same request
    /// may be sent again.
    Transport(String),
    /// A tool call was announced but the provider never delivered its arguments
    /// within the tool-argument bound. Unlike a transport failure this is not
    /// worth resending: the provider buffers the arguments, so the same request
    /// spends the same generation and meets the same bound.
    ToolArgumentsIncomplete(String),
    Provider(EngineError),
}

impl std::fmt::Display for CompletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UsageReportingUnsupported => f.write_str("LLM provider does not support optional stream usage reporting"),
            Self::StructuredOutputUnsupported => f.write_str("LLM provider does not support required JSON-schema output"),
            Self::ReasoningOnly => f.write_str("LLM completed with reasoning only and no executable message content; check the model response policy"),
            Self::MissingContent => f.write_str("LLM response missing message content"),
            Self::ToolChoiceUnsupported(message) => {
                write!(f, "LLM provider rejected a required tool choice: {message}")
            }
            Self::Incomplete(message)
            | Self::InvalidResponse(message)
            | Self::Transport(message)
            | Self::ToolArgumentsIncomplete(message) => f.write_str(message),
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
    /// answer or an invalid response. A tool call whose arguments never arrived
    /// is deliberately excluded: resending it repeats the same generation.
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

/// One function call a model requested; `arguments` is the raw JSON text it sent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: String,
}

/// A finished tool-contract response: prose content and every tool call, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ToolCompletion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// The provider message that made this request fall back from
    /// `tool_choice: "required"` to `"auto"`, when it did.
    pub tool_choice_fallback: Option<String>,
}

/// Finish reasons that end a tool-contract response successfully.
fn tool_finish(reason: &str) -> bool {
    matches!(reason, "stop" | "tool_calls" | "function_call")
}

fn tool_arguments(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A non-streaming tool-contract response. Content may be empty; a response with
/// neither content nor calls is returned empty (the caller repairs it), unless it
/// carried only reasoning.
pub(super) fn complete_tool_message(
    value: &serde_json::Value,
) -> Result<ToolCompletion, CompletionError> {
    if let Some(error) = value.get("error") {
        return Err(CompletionError::InvalidResponse(format!(
            "LLM response error: {error}"
        )));
    }
    let choice = &value["choices"][0];
    let message = &choice["message"];
    if !choice["finish_reason"].as_str().is_some_and(tool_finish) {
        return Err(CompletionError::Incomplete(format!(
            "LLM completion ended without successful stop: {}",
            choice["finish_reason"]
        )));
    }
    if has_payload(&message["refusal"]) {
        return Err(CompletionError::InvalidResponse(
            "LLM returned a refusal instead of content".into(),
        ));
    }
    let content = message["content"].as_str().unwrap_or("").to_owned();
    let tool_calls: Vec<ToolCall> = message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| ToolCall {
            id: item["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(Into::into),
            name: item["function"]["name"].as_str().unwrap_or("").to_owned(),
            arguments: tool_arguments(&item["function"]["arguments"]),
        })
        .collect();
    if content.trim().is_empty()
        && tool_calls.is_empty()
        && (has_payload(&message["reasoning_content"]) || has_payload(&message["reasoning"]))
    {
        return Err(CompletionError::ReasoningOnly);
    }
    Ok(ToolCompletion {
        content,
        tool_calls,
        tool_choice_fallback: None,
    })
}

/// A tool call some models write as text instead of a native call, e.g.
/// `{"type":"function","name":"read","parameters":{...}}`. The whole text (after
/// stripping a code fence) or its first balanced JSON object is tried. Accepted
/// shapes: `{name, parameters}`, `{name, arguments}` (an object or a JSON-encoded
/// string) and `{function: {name, arguments}}`. The name is not checked here.
pub fn tool_call_from_text(text: &str) -> Option<ToolCall> {
    let text = strip_code_fence(text.trim());
    [Some(text), first_json_object(text)]
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            serde_json::from_str::<serde_json::Value>(candidate)
                .ok()
                .and_then(|value| call_from_value(&value))
        })
}

fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let body = rest.split_once('\n').map_or("", |(_, body)| body);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

fn first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (offset, byte) in text.as_bytes()[start..].iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn call_from_value(value: &serde_json::Value) -> Option<ToolCall> {
    let object = value.as_object()?;
    let (name, arguments) = match object.get("function").and_then(|f| f.as_object()) {
        Some(function) => (
            function.get("name")?,
            function
                .get("arguments")
                .or_else(|| function.get("parameters"))?,
        ),
        None => (
            object.get("name")?,
            object
                .get("parameters")
                .or_else(|| object.get("arguments"))?,
        ),
    };
    let name = name.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = match arguments {
        serde_json::Value::Object(_) => arguments.to_string(),
        serde_json::Value::String(text) => {
            let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
            parsed.is_object().then(|| parsed.to_string())?
        }
        _ => return None,
    };
    Some(ToolCall {
        id: None,
        name: name.to_owned(),
        arguments,
    })
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
    /// Tool-contract streams accumulate `delta.tool_calls` by index instead of refusing them.
    tools: bool,
    tool_calls: std::collections::BTreeMap<u64, ToolCall>,
}

impl CompletionStream {
    pub(super) fn for_tools() -> Self {
        Self {
            tools: true,
            ..Self::default()
        }
    }

    pub(super) fn into_tool_completion(self) -> ToolCompletion {
        ToolCompletion {
            content: self.content,
            tool_calls: self.tool_calls.into_values().collect(),
            tool_choice_fallback: None,
        }
    }

    /// A tool call has been announced but its arguments have not arrived. A
    /// provider may send the call's header and then stay silent for the whole
    /// of argument generation, so this interval is bounded as generation rather
    /// than as a stall. Openness is `id` or `name`, because either may come
    /// first. Any pending call counts: calls interleave, and one already filled
    /// says nothing about the one still being written. `stopped` ends the state
    /// — after a finish reason no further arguments can legitimately arrive, so
    /// the trailing usage chunk is a stall like any other.
    pub(super) fn tool_arguments_pending(&self) -> bool {
        self.tools
            && !self.stopped
            && self.tool_calls.values().any(|call| {
                (call.id.is_some() || !call.name.is_empty()) && call.arguments.is_empty()
            })
    }

    /// Argument bytes accumulated so far, for diagnosing a call that never finished.
    pub(super) fn tool_arguments_len(&self) -> usize {
        self.tool_calls
            .values()
            .map(|call| call.arguments.len())
            .sum()
    }

    fn accumulate_tool_calls(&mut self, deltas: &serde_json::Value) {
        for (position, delta) in deltas.as_array().into_iter().flatten().enumerate() {
            let index = delta["index"].as_u64().unwrap_or(position as u64);
            let call = self.tool_calls.entry(index).or_insert_with(|| ToolCall {
                id: None,
                name: String::new(),
                arguments: String::new(),
            });
            if let Some(id) = delta["id"].as_str().filter(|id| !id.is_empty()) {
                call.id = Some(id.to_owned());
            }
            if let Some(name) = delta["function"]["name"].as_str() {
                call.name.push_str(name);
            }
            match &delta["function"]["arguments"] {
                serde_json::Value::String(piece) => call.arguments.push_str(piece),
                serde_json::Value::Null => {}
                other => call.arguments = other.to_string(),
            }
        }
    }

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
            if self.tools {
                if has_payload(&choice["delta"]["refusal"]) {
                    return Err("LLM stream returned a refusal instead of content".into());
                }
                if has_payload(&choice["delta"]["tool_calls"]) {
                    self.accumulate_tool_calls(&choice["delta"]["tool_calls"]);
                }
            } else if has_payload(&choice["delta"]["tool_calls"])
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
                if reason != "stop" && !(self.tools && tool_finish(reason)) {
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
        if self.content.trim().is_empty() && self.tool_calls.is_empty() {
            if self.saw_reasoning {
                return Err(CompletionError::ReasoningOnly);
            }
            // An empty tool-contract response is returned as such: the caller
            // repairs it rather than re-sending the identical request.
            if !self.tools {
                return Err(CompletionError::MissingContent);
            }
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
