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

fn description(name: &str) -> &'static str {
    match name {
        "read" => "Read a project file together with its governing knowledge.",
        "search" => "Search accepted project knowledge first, then repository matches.",
        "inspect" => "Page the complete output of a journal event.",
        "plan" if super::rule_state::plan_satisfied_enabled() => "Propose the plan: a summary, the permitted files, the required checks, the project rules it implements (addresses) and those the existing code already satisfies unchanged (satisfied).",
        "plan" => "Propose the plan: a summary, the permitted files, the required checks and the project rules it implements (addresses).",
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
}

/// Decode the first native call, or a call written as text that names an
/// offered tool, into the action JSON shape the json_schema contract produced:
/// `{"message", "action"}` for conversational tasks, the bare action otherwise.
/// Only harness policy lives here; the model's syntax was read by
/// [`normalize`](crate::llm::normalize::normalize). An `Err` is the correction
/// for an unusable response; it spends a repair.
pub(super) fn decode(
    normalized: &Normalized,
    schema: &Value,
    conversational: bool,
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
    })
}
