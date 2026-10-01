//! The native tool-call action contract (AD 84dfd153): tool definitions derived
//! from the mode's action schema, and a tool-contract response decoded back into
//! the action JSON the dispatcher validates, so validation, dispatch and repair
//! budgets are the same for both contracts.
use crate::llm::normalize::{CallSource, NormalCall, Normalized};
use serde_json::{json, Map, Value};

/// The correction every response without a usable tool call receives.
const ONE_TOOL: &str = "call exactly one tool; use reply to answer in text";

/// The action variants of a single-action or conversational schema.
fn variants(schema: &Value) -> &[Value] {
    schema["oneOf"]
        .as_array()
        .or_else(|| schema["properties"]["action"]["oneOf"].as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn variant_name(variant: &Value) -> &str {
    variant["properties"]["action"]["const"]
        .as_str()
        .unwrap_or("")
}

/// The tool names the schema offers, in schema order.
pub(super) fn names(schema: &Value) -> Vec<String> {
    variants(schema)
        .iter()
        .map(|variant| variant_name(variant).to_owned())
        .collect()
}

fn description(name: &str) -> String {
    if name == "plan" {
        return plan_description();
    }
    match name {
        "read" => "Read a project file together with its governing knowledge.",
        "search" => "Search accepted project knowledge first, then repository matches.",
        "inspect" => "Page the complete output of a journal event.",
        "replace" => "Replace exactly one unique literal occurrence of old_text in a file.",
        "write" => "Write a file's whole UTF-8 content, creating missing parent directories; null content requests deletion.",
        "apply_fix" => "Apply a quick fix the language server offered, by the number listed under an error or lint.",
        "command" => "Run a shell command in the read-only source snapshot.",
        "request_permission" => {
            "Ask the human to grant explicit external paths or network access for an exact command."
        }
        "question" => "Ask the human a question.",
        "reply" => "Say something to the human in prose. then: \"wait\" when this reply answers the human and the turn should end; \"continue\" when you are about to act and the harness should ask for your next action.",
        "replan" => "Say why the approved files or checks must change.",
        "finish" => "Declare the requested changes applied; the harness runs the required checks.",
        "edit" => "Legacy whole-file edit.",
        _ => "A harness action.",
    }
    .to_owned()
}

/// The plan tool's description, naming each optional field the schema
/// offers now.
fn plan_description() -> String {
    let mut fields = Vec::new();
    if super::rule_state::plan_satisfied_enabled() {
        fields.push("those the existing code already satisfies unchanged (satisfied)");
    }
    if super::symbolic::plan_stubs_enabled() {
        fields.push("any planned files it deliberately leaves as stubs for a later task (stubs)");
    }
    if super::symbolic::plan_unchanged_enabled() {
        fields
            .push("any planned files it lists only for reference, which need no edit (unchanged)");
    }
    match fields.split_last() {
        None => "Propose the plan: a summary, the permitted files, the required checks and the project rules it implements (addresses).".to_owned(),
        Some((last, [])) => format!("Propose the plan: a summary, the permitted files, the required checks, the project rules it implements (addresses) and {last}."),
        Some((last, rest)) => format!(
            "Propose the plan: a summary, the permitted files, the required checks, the project rules it implements (addresses), {}, and {last}.",
            rest.join(", ")
        ),
    }
}

/// One OpenAI-compatible function per action variant, with that variant's
/// argument schema (the `action` discriminator removed).
pub(super) fn definitions(schema: &Value) -> Value {
    Value::Array(
        variants(schema)
            .iter()
            .map(|variant| {
                let name = variant_name(variant);
                let mut properties = variant["properties"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                properties.remove("action");
                let required: Vec<Value> = variant["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|field| *field != "action")
                    .cloned()
                    .collect();
                json!({"type":"function","function":{
                    "name": name,
                    "description": description(name),
                    "parameters": {
                        "type": "object",
                        "properties": properties,
                        "required": required,
                        "additionalProperties": false
                    }
                }})
            })
            .collect(),
    )
}

/// A normalized tool-contract response turned into action JSON, with what
/// was journal-worthy.
pub(super) struct Decoded {
    pub(super) name: String,
    pub(super) text: String,
    /// Names of native calls after the first, which do not run.
    pub(super) ignored: Vec<String>,
    /// A `reply` call sent beside an action: its text became the message and
    /// the action ran.
    pub(super) reply_as_message: bool,
    /// Calls ahead of the one that ran that the harness would have refused,
    /// so a later call ran instead (a provider that ignores
    /// `parallel_tool_calls: false`).
    pub(super) passed_over: Vec<String>,
}

/// Whether several calls in one response run the first, as before the harness
/// chose the first it would not refuse: `MOOSEDEV_HARNESS_MULTI_CALL=first`.
pub(super) fn first_call_only() -> bool {
    std::env::var("MOOSEDEV_HARNESS_MULTI_CALL").is_ok_and(|value| value.trim() == "first")
}

/// Decode the first native call, or a call written as text that names an
/// offered tool, into the action JSON shape the json_schema contract produced:
/// `{"message", "action"}` for conversational tasks, the bare action otherwise.
/// Only harness policy lives here; the model's syntax was read by
/// [`normalize`](crate::llm::normalize::normalize). An `Err` is the correction
/// for an unusable response; it spends a repair.
///
/// Of several calls, the first that `refused` (the harness's own refusal
/// checks, by tool name and arguments) would not turn away runs; when every
/// one would be, the first does, and meets its refusal. Without that, a
/// provider that returns several reads with a redundant one first had the
/// harness refuse the same read until it parked (badciv on OpenRouter,
/// 2026-09-29).
pub(super) fn decode(
    normalized: &Normalized,
    schema: &Value,
    conversational: bool,
    refused: &dyn Fn(&str, &Map<String, Value>) -> bool,
) -> Result<Decoded, String> {
    let allowed = names(schema);
    let listing = allowed.join(", ");
    // A text call is recognised by its shape, which ordinary content can
    // share, so one naming a tool this mode does not offer is not a call.
    let mut calls: Vec<&NormalCall> = normalized
        .calls
        .iter()
        .filter(|call| call.source == CallSource::Native || allowed.contains(&call.name))
        .collect();
    // A `reply` beside an action is the model narrating what it is about to
    // do: run the action and show the reply as its message. Running the reply
    // alone ended the turn with "I will …" and dropped the action (badciv
    // a2e43815: "ran reply; ignored plan", twice).
    let mut narration: Option<String> = None;
    if calls.len() > 1 {
        if let Some(position) = calls
            .iter()
            .position(|call| call.name != "reply" && allowed.contains(&call.name))
        {
            let replies: Vec<String> = calls
                .iter()
                .filter(|call| call.name == "reply")
                .filter_map(|call| {
                    call.arguments
                        .as_ref()
                        .ok()?
                        .get("message")?
                        .as_str()
                        .map(str::to_owned)
                })
                .collect();
            if !replies.is_empty() {
                narration = Some(replies.join("\n\n"));
                let action = calls.remove(position);
                calls.retain(|call| call.name != "reply");
                calls.insert(0, action);
            }
        }
    }
    let reply_as_message = narration.is_some();
    let mut passed_over = Vec::new();
    if calls.len() > 1 {
        let runs = |call: &&NormalCall| {
            allowed.contains(&call.name)
                && call
                    .arguments
                    .as_ref()
                    .is_ok_and(|arguments| !refused(&call.name, arguments))
        };
        if let Some(position) = calls.iter().position(runs).filter(|position| *position > 0) {
            passed_over = calls[..position]
                .iter()
                .map(|call| call.name.clone())
                .collect();
            let chosen = calls.remove(position);
            calls.insert(0, chosen);
        }
    }
    let Some((call, rest)) = calls.split_first() else {
        return Err(format!(
            "the response contained no tool call; {ONE_TOOL} (tools now: {listing})"
        ));
    };
    if !allowed.contains(&call.name) {
        return Err(format!(
            "tool {:?} is not available now; {ONE_TOOL} (tools now: {listing})",
            call.name
        ));
    }
    let arguments = call
        .arguments
        .as_ref()
        .map_err(|error| format!("{error}; {ONE_TOOL} with a JSON object of arguments"))?;
    let mut action = Map::new();
    action.insert("action".into(), json!(call.name));
    for (key, value) in arguments {
        if key != "action" {
            action.insert(key.clone(), value.clone());
        }
    }
    let action = Value::Object(action);
    let text = if conversational {
        let content = normalized.content.trim();
        let message = match &narration {
            Some(narration) if content.is_empty() => narration.trim().to_owned(),
            Some(narration) => format!("{content}\n\n{}", narration.trim()),
            None => content.to_owned(),
        };
        json!({"message": message, "action": action}).to_string()
    } else {
        action.to_string()
    };
    Ok(Decoded {
        name: call.name.clone(),
        text,
        ignored: rest.iter().map(|call| call.name.clone()).collect(),
        reply_as_message,
        passed_over,
    })
}

/// Reads one response may batch: the model asked for several files at once
/// (badciv orC–orE: planning responses opened with 3–21 reads).
const DEFAULT_READ_BATCH: usize = 4;

/// `MOOSEDEV_HARNESS_CALL_STOP=off` reads every streamed response to its end,
/// as before the stop.
pub(super) fn call_stop_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_CALL_STOP").map_or(true, |value| value.trim() != "off")
}

/// The most leading reads one response runs (`MOOSEDEV_HARNESS_READ_BATCH`,
/// default [`DEFAULT_READ_BATCH`]; `1` runs a single read, as before).
pub(super) fn read_batch_limit() -> usize {
    std::env::var("MOOSEDEV_HARNESS_READ_BATCH")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map_or(DEFAULT_READ_BATCH, |limit| limit.max(1))
}

/// One call of a response as the stop rule sees it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CallSeen {
    pub(super) name: String,
    /// The call's whole text, so a repeat is the same call.
    key: String,
    /// A read's file.
    pub(super) file: Option<String>,
}

impl CallSeen {
    fn new(name: &str, arguments: &Map<String, Value>) -> Self {
        Self {
            name: name.to_owned(),
            key: format!("{name} {}", Value::Object(arguments.clone())),
            file: (name == "read")
                .then(|| arguments.get("file").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned),
        }
    }

    /// A call written as a JSON object: the json_schema action
    /// (`{"action": "read", ...}`), the conversational shape (`{"message",
    /// "action": {...}}`), or a text tool call (`{"name", "arguments"}`).
    /// Anything else is a call of no known kind, which never batches.
    pub(super) fn from_object(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let action = match &value["action"] {
            Value::Object(inner) => Some(inner.clone()),
            Value::String(_) => value.as_object().cloned(),
            _ => None,
        };
        if let Some(action) = action {
            if let Some(name) = action.get("action").and_then(Value::as_str) {
                return Self::new(name, &action);
            }
        }
        if let Some(name) = value["name"].as_str() {
            let arguments = match &value["arguments"] {
                Value::Object(arguments) => arguments.clone(),
                Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
                _ => Map::new(),
            };
            return Self::new(name, &arguments);
        }
        Self {
            name: String::new(),
            key: text.to_owned(),
            file: None,
        }
    }

    pub(super) fn normal(call: &NormalCall) -> Self {
        let arguments = call.arguments.clone().unwrap_or_default();
        Self::new(&call.name, &arguments)
    }

    pub(super) fn native(call: &crate::llm::ToolCall) -> Self {
        let arguments = serde_json::from_str(&call.arguments).unwrap_or_default();
        Self::new(&call.name, &arguments)
    }
}

/// The calls among a response's JSON objects, each with where it ends. An
/// object of no known call shape (a fenced example, prose) is not a call and
/// never counts toward a stop.
pub(super) fn text_calls(
    content: &str,
    objects: &[std::ops::Range<usize>],
) -> Vec<(usize, CallSeen)> {
    objects
        .iter()
        .map(|range| (range.end, CallSeen::from_object(&content[range.clone()])))
        .filter(|(_, call)| !call.name.is_empty())
        .collect()
}

/// How many leading calls the harness runs: a run of distinct reads up to
/// `batch`, else the first call alone.
pub(super) fn kept_calls(calls: &[CallSeen], batch: usize) -> usize {
    let Some(first) = calls.first() else {
        return 0;
    };
    if first.file.is_none() {
        return 1;
    }
    let mut kept = 0;
    for (index, call) in calls.iter().enumerate().take(batch) {
        if call.file.is_none() || calls[..index].iter().any(|earlier| earlier.key == call.key) {
            break;
        }
        kept += 1;
    }
    kept
}

/// Where to stop a response: once a complete call falls outside the calls
/// the harness will run, or once those are complete and can grow no further
/// while another call has started. `None` while the response may still add
/// a call the harness runs.
pub(super) fn stop_after(calls: &[CallSeen], started_next: bool, batch: usize) -> Option<usize> {
    let kept = kept_calls(calls, batch);
    if kept == 0 {
        return None;
    }
    let full = if calls[0].file.is_some() { batch } else { 1 };
    (calls.len() > kept || (kept == full && started_next)).then_some(kept)
}

/// Where to cut a stream, given how many calls it keeps.
type Cut = Box<dyn Fn(usize) -> crate::llm::StreamCut>;

/// The harness's [`crate::llm::StreamStop`]: cut the response after the
/// calls it will run, recording what was kept in `record` for the journal.
pub(super) fn stream_stop(
    record: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> crate::llm::StreamStop {
    let batch = read_batch_limit();
    crate::llm::StreamStop(std::sync::Arc::new(
        move |view: &crate::llm::StreamView<'_>| {
            let (seen, started, cut): (Vec<CallSeen>, bool, Cut) = if view.call_started {
                let seen = view.calls.iter().map(CallSeen::native).collect();
                let content = view.content.len();
                (
                    seen,
                    true,
                    Box::new(move |kept| crate::llm::StreamCut {
                        content,
                        calls: kept,
                    }),
                )
            } else {
                let (objects, started) = crate::llm::normalize::json_objects(view.content);
                let (ends, seen): (Vec<usize>, Vec<CallSeen>) =
                    text_calls(view.content, &objects).into_iter().unzip();
                (
                    seen,
                    started,
                    Box::new(move |kept| crate::llm::StreamCut {
                        content: ends[kept - 1],
                        calls: 0,
                    }),
                )
            };
            let kept = stop_after(&seen, started, batch)?;
            let names: Vec<&str> = seen[..kept].iter().map(|call| call.name.as_str()).collect();
            if let Ok(mut record) = record.lock() {
                *record = Some(format!(
                    "kept {kept} call(s) ({}); stopped at {} content bytes",
                    names.join(", "),
                    view.content.len()
                ));
            }
            Some(cut(kept))
        },
    ))
}

/// The files of the leading read batch in a finished response's calls,
/// after the first (which runs as the step's action).
pub(super) fn batched_reads(calls: &[CallSeen]) -> Vec<String> {
    let kept = kept_calls(calls, read_batch_limit());
    calls[..kept]
        .iter()
        .skip(1)
        .filter_map(|call| call.file.clone())
        .collect()
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    fn read(file: &str) -> CallSeen {
        CallSeen::from_object(&format!("{{\"action\":\"read\",\"file\":\"{file}\"}}"))
    }

    fn other(name: &str) -> CallSeen {
        CallSeen::from_object(&format!("{{\"action\":\"{name}\",\"query\":\"x\"}}"))
    }

    #[test]
    fn reads_batch_up_to_the_limit_and_anything_else_runs_alone() {
        assert_eq!(stop_after(&[read("a")], true, 4), None);
        assert_eq!(stop_after(&[read("a"), read("b")], false, 4), None);
        assert_eq!(
            stop_after(&[read("a"), read("b"), other("search")], false, 4),
            Some(2)
        );
        assert_eq!(stop_after(&[read("a"), read("a")], false, 4), Some(1));
        let four = [read("a"), read("b"), read("c"), read("d")];
        assert_eq!(stop_after(&four, false, 4), None);
        assert_eq!(stop_after(&four, true, 4), Some(4));
        assert_eq!(stop_after(&[other("write")], false, 4), None);
        assert_eq!(stop_after(&[other("write")], true, 4), Some(1));
        assert_eq!(stop_after(&[other("search"), read("a")], false, 4), Some(1));
        assert_eq!(stop_after(&[read("a")], true, 1), Some(1));
        assert_eq!(
            batched_reads(&[read("a"), read("b"), other("search")]),
            vec!["b"]
        );
    }

    /// The harness's stop on a stream as the llm layer shows it: text calls
    /// are cut after the kept reads, native calls after the kept count, and
    /// the cut is recorded for the journal.
    #[test]
    fn the_stream_stop_cuts_after_the_calls_that_run() {
        let record = std::sync::Arc::new(std::sync::Mutex::new(None));
        let stop = stream_stop(record.clone());
        let content = "<tool_call>\n{\"action\":\"read\",\"file\":\"a\"}\n<tool_call>\n{\"action\":\"read\",\"file\":\"b\"}\n<tool_call>\n{\"action\":\"search\",\"query\":\"q\"}";
        let view = crate::llm::StreamView {
            content,
            calls: &[],
            call_started: false,
        };
        let cut = (stop.0)(&view).unwrap();
        assert_eq!(
            &content[..cut.content],
            "<tool_call>\n{\"action\":\"read\",\"file\":\"a\"}\n<tool_call>\n{\"action\":\"read\",\"file\":\"b\"}"
        );
        assert!(record
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .starts_with("kept 2 call(s) (read, read)"));
        let partial = crate::llm::StreamView {
            content: &content[..40],
            calls: &[],
            call_started: false,
        };
        assert!((stop.0)(&partial).is_none());

        let write = crate::llm::ToolCall {
            id: None,
            name: "write".into(),
            arguments: "{\"file\":\"a\",\"content\":\"x\"}".into(),
        };
        let native = crate::llm::StreamView {
            content: "",
            calls: std::slice::from_ref(&write),
            call_started: true,
        };
        assert_eq!((stop.0)(&native).unwrap().calls, 1);
    }

    /// An object that is not a call (a fenced example) never counts: the
    /// read after it is the first call, and nothing is cut at the example.
    #[test]
    fn an_example_object_is_not_a_call() {
        let stop = stream_stop(std::sync::Arc::new(std::sync::Mutex::new(None)));
        let content = "Like this: ```{\"example\":1}```\n<tool_call>\n{\"action\":\"read\",\"file\":\"a\"}\n<tool_call>\n{\"action\":\"search\",\"query\":\"q\"}";
        let view = crate::llm::StreamView {
            content,
            calls: &[],
            call_started: false,
        };
        let cut = (stop.0)(&view).unwrap();
        assert!(content[..cut.content].ends_with("\"file\":\"a\"}"));
    }

    #[test]
    fn every_call_shape_is_recognised() {
        let conversational = CallSeen::from_object(
            "{\"message\":\"m\",\"action\":{\"action\":\"read\",\"file\":\"a\"}}",
        );
        assert_eq!(conversational.file.as_deref(), Some("a"));
        let tool = CallSeen::from_object("{\"name\":\"read\",\"arguments\":{\"file\":\"a\"}}");
        assert_eq!(tool.file.as_deref(), Some("a"));
        let unknown = CallSeen::from_object("{\"x\":1}");
        assert!(unknown.name.is_empty() && unknown.file.is_none());
    }
}
