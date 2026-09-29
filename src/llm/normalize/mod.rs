//! One model-agnostic layer every raw tool-contract completion passes through
//! before harness logic sees it: native calls pass through, a call a model
//! wrote as text is recovered by the first [`Dialect`] that recognises it, and
//! each call's arguments are parsed into a JSON object (repaired when
//! slightly broken). Model-family quirks plug in behind it as dialects, so the
//! harness keeps only its own policy (which tools are offered, one action per
//! step) and never learns a family's syntax. What normalization did is
//! returned as [`Note`]s for the journal.
//!
//! The json_schema contract's answers, whose schema travelled in the prompt,
//! get schema-driven shape repairs in [`json_schema`] (a flattened nested
//! object folded back into place).
//!
//! Candidates for later migration here, left in place because they depend on
//! harness state: the `old_text` junk trimming in
//! `harness::runner::actions::repair_literal_span` (needs the file's source)
//! and `parse_model_json`'s content recovery for the json_schema contract
//! (validated against the caller's type).

pub mod gemma;
pub mod json;
pub mod json_schema;

use super::{ToolCall, ToolCompletion};
use serde_json::{Map, Value};

/// A completion after normalization.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    /// The assistant's text. Empty when the text was itself the call.
    pub content: String,
    /// The native calls in order, else the one call written as text.
    pub calls: Vec<NormalCall>,
    /// What normalization did, for the journal.
    pub notes: Vec<Note>,
}

/// One call with its arguments parsed. Arguments are per call rather than
/// failing the whole completion, because which call runs is harness policy:
/// an ignored extra call with broken arguments must not cost the step.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalCall {
    pub name: String,
    pub arguments: Result<Map<String, Value>, NormalizeError>,
    pub source: CallSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSource {
    Native,
    Text { dialect: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The call was written as content in this dialect.
    TextCall { dialect: &'static str },
    /// Malformed arguments were repaired; `detail` is the parse error and the
    /// raw text.
    ArgumentsRepaired { tool: String, detail: String },
}

/// Arguments that could not be made into a JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    NotJson { tool: String, error: String },
    NotObject { tool: String },
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson { tool, error } => {
                write!(f, "tool {tool} arguments are not valid JSON ({error})")
            }
            Self::NotObject { tool } => write!(f, "tool {tool} arguments must be a JSON object"),
        }
    }
}
impl std::error::Error for NormalizeError {}

/// A way some model family writes a tool call as text content.
pub trait Dialect: Sync {
    fn name(&self) -> &'static str;
    /// The call `content` holds, arguments as JSON text; `None` when it is
    /// not one. The name is not checked against any offered tools.
    fn text_call(&self, content: &str) -> Option<ToolCall>;
}

/// Tried in order; the first dialect that recognises the content wins.
pub static DIALECTS: &[&dyn Dialect] = &[&json::Json, &gemma::Gemma];

/// The call `content` holds in any supported dialect, with that dialect's name.
pub fn text_call(content: &str) -> Option<(&'static str, ToolCall)> {
    DIALECTS
        .iter()
        .find_map(|dialect| Some((dialect.name(), dialect.text_call(content)?)))
}

/// Normalize a tool-contract completion. Text is read as a call only when no
/// native call came back: beside a native call, text is the model talking.
pub fn normalize(completion: &ToolCompletion) -> Normalized {
    let mut notes = Vec::new();
    if completion.tool_calls.is_empty() {
        if let Some((dialect, call)) = text_call(&completion.content) {
            notes.push(Note::TextCall { dialect });
            let call = normal_call(&call, CallSource::Text { dialect }, &mut notes);
            return Normalized {
                content: String::new(),
                calls: vec![call],
                notes,
            };
        }
    }
    let calls = completion
        .tool_calls
        .iter()
        .map(|call| normal_call(call, CallSource::Native, &mut notes))
        .collect();
    Normalized {
        content: completion.content.clone(),
        calls,
        notes,
    }
}

fn normal_call(call: &ToolCall, source: CallSource, notes: &mut Vec<Note>) -> NormalCall {
    NormalCall {
        name: call.name.clone(),
        arguments: arguments(&call.name, &call.arguments, notes),
        source,
    }
}

/// Empty arguments are an empty object; malformed JSON gets one `jsonrepair`
/// attempt, recorded as a note.
fn arguments(
    tool: &str,
    raw: &str,
    notes: &mut Vec<Note>,
) -> Result<Map<String, Value>, NormalizeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Map::new());
    }
    let value = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(error) => {
            let repaired = jsonrepair::repair_json(trimmed, &jsonrepair::Options::default())
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .ok_or_else(|| NormalizeError::NotJson {
                    tool: tool.to_owned(),
                    error: error.to_string(),
                })?;
            notes.push(Note::ArgumentsRepaired {
                tool: tool.to_owned(),
                detail: format!("{error}; repaired {trimmed}"),
            });
            repaired
        }
    };
    match value {
        Value::Object(arguments) => Ok(arguments),
        _ => Err(NormalizeError::NotObject {
            tool: tool.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn native(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: Some("call-0".into()),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn completion(content: &str, tool_calls: Vec<ToolCall>) -> ToolCompletion {
        ToolCompletion {
            content: content.into(),
            tool_calls,
            tool_choice_fallback: None,
        }
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn native_calls_pass_through_untouched_and_content_stays_prose() {
        let normalized = normalize(&completion(
            "Reading both. reply{message:<|\"|>hi<|\"|>}",
            vec![native("read", r#"{"file":"a.rs"}"#), native("read", "")],
        ));
        assert_eq!(
            normalized,
            Normalized {
                content: "Reading both. reply{message:<|\"|>hi<|\"|>}".into(),
                calls: vec![
                    NormalCall {
                        name: "read".into(),
                        arguments: Ok(object(json!({"file":"a.rs"}))),
                        source: CallSource::Native,
                    },
                    NormalCall {
                        name: "read".into(),
                        arguments: Ok(Map::new()),
                        source: CallSource::Native,
                    },
                ],
                notes: vec![],
            }
        );
    }

    #[test]
    fn a_text_call_names_its_dialect_and_leaves_no_content() {
        let json = normalize(&completion(
            r#"{"type":"function","name":"read","parameters":{"file":"a.rs"}}"#,
            vec![],
        ));
        assert_eq!(json.notes, vec![Note::TextCall { dialect: "json" }]);
        assert_eq!(json.calls[0].source, CallSource::Text { dialect: "json" });
        assert_eq!(json.content, "");
        let gemma = normalize(&completion(
            "reply{message:<|\"|>done<|\"|>,then:<|\"|>wait<|\"|>}<tool_call|>",
            vec![],
        ));
        assert_eq!(gemma.notes, vec![Note::TextCall { dialect: "gemma" }]);
        assert_eq!(gemma.calls[0].name, "reply");
        assert_eq!(
            gemma.calls[0].arguments,
            Ok(object(json!({"message":"done","then":"wait"})))
        );
        let prose = normalize(&completion("I should read a.rs first.", vec![]));
        assert!(prose.calls.is_empty() && prose.notes.is_empty());
        assert_eq!(prose.content, "I should read a.rs first.");
    }

    #[test]
    fn arguments_are_repaired_with_a_note_or_fail_per_call() {
        let normalized = normalize(&completion(
            "",
            vec![
                native("read", r#"{"file": "a.rs""#),
                native("search", "[1, 2]"),
            ],
        ));
        assert_eq!(
            normalized.calls[0].arguments,
            Ok(object(json!({"file":"a.rs"})))
        );
        assert!(matches!(
            &normalized.notes[..],
            [Note::ArgumentsRepaired { tool, detail }]
                if tool == "read" && detail.contains(r#"repaired {"file": "a.rs""#)
        ));
        assert_eq!(
            normalized.calls[1]
                .arguments
                .as_ref()
                .unwrap_err()
                .to_string(),
            "tool search arguments must be a JSON object"
        );
        assert_eq!(
            NormalizeError::NotJson {
                tool: "write".into(),
                error: "EOF while parsing".into()
            }
            .to_string(),
            "tool write arguments are not valid JSON (EOF while parsing)"
        );
    }
}
