//! The Hermes/Qwen text form: a JSON call inside `<tool_call>` tags,
//! `<tool_call>\n{"name":"read","arguments":{"file":"a.rs"}}\n</tool_call>`.
//! Qwen models served without a tool parser write their calls this way, one
//! block per call.
use super::json::call_from_value;
use super::Dialect;
use crate::llm::completion::first_json_object;
use crate::llm::ToolCall;
use serde_json::Value;

const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

pub struct Hermes;

impl Dialect for Hermes {
    fn name(&self) -> &'static str {
        "hermes"
    }

    fn text_call(&self, content: &str) -> Option<ToolCall> {
        self.text_calls(content).into_iter().next()
    }

    /// Each `<tool_call>` block's JSON call, in the shapes the json dialect
    /// accepts, in order. A tag counts only where it begins a line outside a
    /// code fence, so prose quoting the markup (inline, or in a fenced
    /// example) is not a call. A missing closing tag ends the block at the
    /// next tag or the end of the text.
    fn text_calls(&self, content: &str) -> Vec<ToolCall> {
        let starts = tag_starts(content);
        starts
            .iter()
            .enumerate()
            .filter_map(|(index, &start)| {
                let from = start + OPEN.len();
                let until = starts.get(index + 1).copied().unwrap_or(content.len());
                let rest = &content[from..until];
                let block = rest.find(CLOSE).map_or(rest, |end| &rest[..end]).trim();
                let json = first_json_object(block).unwrap_or(block);
                serde_json::from_str::<Value>(json)
                    .ok()
                    .and_then(|value| call_from_value(&value))
            })
            .collect()
    }
}

/// Byte offsets of the tags that begin a line (after optional indentation)
/// outside a ``` fence.
fn tag_starts(content: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut fenced = false;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && trimmed.starts_with(OPEN) {
            starts.push(offset + (line.len() - trimmed.len()));
        }
        offset += line.len();
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::super::{normalize, CallSource, Note};
    use super::*;
    use crate::llm::ToolCompletion;

    fn text(content: &str) -> ToolCompletion {
        ToolCompletion {
            content: content.to_owned(),
            ..ToolCompletion::default()
        }
    }

    #[test]
    fn a_tagged_call_is_read_and_named_hermes() {
        // The shape qwen3.8-27b wrote on OpenRouter (2026-09-29).
        let content = "<tool_call>\n{\"name\": \"read\", \"arguments\": {\"file\": \"badciv-map.md\"}}\n</tool_call>";
        let normalized = normalize(&text(content));
        assert_eq!(normalized.calls.len(), 1);
        assert_eq!(normalized.calls[0].name, "read");
        assert_eq!(
            normalized.calls[0].source,
            CallSource::Text { dialect: "hermes" }
        );
        assert_eq!(
            normalized.calls[0].arguments.as_ref().unwrap()["file"],
            "badciv-map.md"
        );
        assert_eq!(normalized.notes, [Note::TextCall { dialect: "hermes" }]);
        assert!(normalized.content.is_empty());
    }

    #[test]
    fn every_block_is_a_call_and_an_unclosed_block_ends_at_the_next_tag() {
        let content = "I will read both.\n<tool_call>\n{\"name\":\"read\",\"arguments\":\"{\\\"file\\\":\\\"a.rs\\\"}\"}\n<tool_call>{\"name\":\"read\",\"arguments\":{\"file\":\"b.rs\"}}</tool_call>";
        let calls = Hermes.text_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, "{\"file\":\"a.rs\"}");
        assert_eq!(calls[1].arguments, "{\"file\":\"b.rs\"}");
        let normalized = normalize(&text(content));
        assert_eq!(
            normalized.calls.len(),
            2,
            "both reach the one-action policy"
        );
    }

    #[test]
    fn quoted_markup_and_blocks_without_a_call_are_not_calls() {
        let inline = "Qwen writes calls like <tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call> when unparsed.";
        assert!(Hermes.text_calls(inline).is_empty());
        let fenced = "For example:\n```\n<tool_call>\n{\"name\":\"read\",\"arguments\":{}}\n</tool_call>\n```\n";
        assert!(Hermes.text_calls(fenced).is_empty());
        assert!(Hermes
            .text_call("{\"name\":\"read\",\"arguments\":{}}")
            .is_none());
        assert!(Hermes
            .text_call("<tool_call>\nnot json\n</tool_call>")
            .is_none());
        assert!(Hermes
            .text_call("<tool_call>{\"action\":\"read\"}</tool_call>")
            .is_none());
    }
}
