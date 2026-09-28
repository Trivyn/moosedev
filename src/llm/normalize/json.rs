//! The generic JSON shapes several families write a call in, e.g. llama's
//! `{"type":"function","name":"read","parameters":{...}}`.
use super::Dialect;
use crate::llm::completion::{first_json_object, strip_code_fence};
use crate::llm::ToolCall;
use serde_json::Value;

pub struct Json;

impl Dialect for Json {
    fn name(&self) -> &'static str {
        "json"
    }

    /// The whole text (after stripping a code fence) or its first balanced
    /// JSON object is tried. Accepted shapes: `{name, parameters}`,
    /// `{name, arguments}` (an object or a JSON-encoded string) and
    /// `{function: {name, arguments}}`.
    fn text_call(&self, content: &str) -> Option<ToolCall> {
        let text = strip_code_fence(content.trim());
        [Some(text), first_json_object(text)]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                serde_json::from_str::<Value>(candidate)
                    .ok()
                    .and_then(|value| call_from_value(&value))
            })
    }
}

fn call_from_value(value: &Value) -> Option<ToolCall> {
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
        Value::Object(_) => arguments.to_string(),
        Value::String(text) => {
            let parsed: Value = serde_json::from_str(text).ok()?;
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
