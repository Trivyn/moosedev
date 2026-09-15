//! The native tool-call action contract: request shape per mode, decoding of
//! native, several, malformed, text-embedded and missing tool calls, the
//! tool_choice fallback, and the json_schema contract kept selectable.
use super::*;
use moosedev::harness::response::ActionContract;

const PLANNING: [&str; 6] = ["inspect", "plan", "question", "read", "reply", "search"];

fn action_requests(fixture: &Fixture) -> Vec<Value> {
    requests_of_kind(fixture, "model")
        .into_iter()
        .filter(|request| request["schema"] == "harness_action")
        .collect()
}

fn tool_names(request: &Value) -> Vec<String> {
    let mut names: Vec<String> = request["body"]["tools"]
        .as_array()
        .expect("tool definitions")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort_unstable();
    names
}

/// A scripted raw tool-contract answer: text content and native tool calls.
fn tool_answer(content: &str, calls: &[(&str, &str)]) -> Value {
    json!({
        "content": content,
        "tool_calls": calls
            .iter()
            .enumerate()
            .map(|(index, (name, arguments))| json!({
                "id": format!("call-{index}"),
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }))
            .collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn tool_requests_offer_the_mode_tools_and_require_one_call() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    let actions = action_requests(&fixture);
    assert_eq!(
        actions.len(),
        3,
        "read and plan while planning, then one edit"
    );
    for request in &actions[..2] {
        assert_eq!(tool_names(request), PLANNING);
    }
    assert_eq!(
        tool_names(&actions[2]),
        [
            "command", "finish", "inspect", "question", "read", "replace", "replan", "reply",
            "search", "write"
        ]
    );
    for request in &actions {
        let body = &request["body"];
        assert!(body.get("response_format").is_none(), "{body}");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
        for tool in body["tools"].as_array().unwrap() {
            assert_eq!(tool["type"], "function");
            assert!(tool["function"]["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty()));
            assert_eq!(tool["function"]["parameters"]["type"], "object");
            assert!(tool["function"]["parameters"]["properties"]
                .get("action")
                .is_none());
        }
        let prompt = body["messages"][0]["content"].as_str().unwrap();
        assert!(prompt.contains("Call exactly one tool"), "{prompt}");
        assert!(!prompt.contains("Required JSON schema"));
        assert!(!prompt.contains("Return one JSON object"));
    }
    let read = &actions[0]["body"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "read")
        .unwrap()["function"]["parameters"];
    assert_eq!(read["required"], json!(["file"]));
    assert_eq!(read["additionalProperties"], false);
    assert!(runner
        .task
        .model_requests
        .iter()
        .filter(|request| request["purpose"] == "harness_action")
        .all(|request| request["contract"] == "tools"));
    assert_eq!(
        runner.task.response_receipt.as_ref().unwrap().contract,
        Some(ActionContract::Tools)
    );
}

#[tokio::test]
async fn only_the_first_of_several_tool_calls_runs_and_the_rest_are_journaled() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("other.txt"), "other\n").unwrap();
    let mut runner = fixture.interactive().await;
    fixture.reply(
        "harness_action",
        tool_answer(
            "Reading both files.",
            &[
                ("read", r#"{"file":"code.txt"}"#),
                ("read", r#"{"file":"other.txt"}"#),
            ],
        ),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    let ignored = intent_details(&runner, "extra_tool_calls_ignored");
    assert_eq!(ignored.len(), 1, "{ignored:?}");
    assert!(ignored[0].contains("read"), "{ignored:?}");
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("one action runs per step")));
    let request = runner.task.model_requests.last().unwrap();
    assert_eq!(request["tool_calls"].as_array().unwrap().len(), 2);
    let response: Value = serde_json::from_str(request["response"].as_str().unwrap()).unwrap();
    assert_eq!(
        response,
        json!({"message":"Reading both files.","action":{"action":"read","file":"code.txt"}})
    );
    fixture.conversational(json!({"action":"read","file":"other.txt"}));
    runner.advance().await.unwrap();
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("one action runs per step"));
}

#[tokio::test]
async fn malformed_tool_arguments_are_repaired_or_spend_a_repair_attempt() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.reply(
        "harness_action",
        tool_answer("", &[("read", r#"{"file": "code.txt""#)]),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    let repaired = intent_details(&runner, "tool_arguments_repaired");
    assert_eq!(repaired.len(), 1, "{repaired:?}");
    assert!(repaired[0].contains("read"), "{repaired:?}");
    assert!(runner.task.recovery.is_none());

    let start = runner.task.model_requests.len();
    fixture.reply("harness_action", tool_answer("", &[("search", "[1, 2]")]));
    fixture.conversational(json!({"action":"search","query":"code"}));
    runner.advance().await.unwrap();
    assert!(runner.task.recovery.is_none());
    let requests = &runner.task.model_requests[start..];
    assert_eq!(
        requests
            .iter()
            .map(|r| r["attempt"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2],
        "unusable arguments spend one repair attempt"
    );
    assert!(requests[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("last candidate was rejected"));
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.starts_with("Correcting action")
            && event.message.contains("arguments")));
}

#[tokio::test]
async fn a_response_without_a_tool_call_is_repaired_not_halted() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    // Prose only, then an empty answer that still claims finish_reason tool_calls.
    fixture.reply(
        "harness_action",
        json!({"content":"I should read code.txt first."}),
    );
    fixture.reply(
        "harness_action",
        json!({"content":"", "finish_reason":"tool_calls"}),
    );
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    assert!(runner.task.recovery.is_none());
    assert_ne!(runner.task.phase, Phase::AwaitingInput);
    let corrections: Vec<&str> = runner
        .task
        .events
        .iter()
        .map(|event| event.message.as_str())
        .filter(|message| message.starts_with("Correcting action"))
        .collect();
    assert_eq!(corrections.len(), 2, "{corrections:?}");
    assert!(corrections
        .iter()
        .all(|message| message.contains("call exactly one tool; use reply to answer in text")));
    let prompts: Vec<String> = action_requests(&fixture)
        .iter()
        .map(|request| {
            request["body"]["messages"][0]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(prompts.len(), 3);
    assert!(!prompts[0].contains("last candidate was rejected"));
    assert!(
        prompts[1..]
            .iter()
            .all(|prompt| prompt.contains("last candidate was rejected")),
        "a repair never re-sends the request without a correction"
    );
}

#[tokio::test]
async fn a_tool_call_written_as_text_runs_and_is_journaled() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.reply(
        "harness_action",
        json!({"content":"{\"type\": \"function\", \"name\": \"read\", \"parameters\": {\"file\": \"code.txt\"}}",
               "finish_reason":"tool_calls"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    let recovered = intent_details(&runner, "tool_call_from_content");
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert!(recovered[0].contains("read"), "{recovered:?}");
    // A text call naming a tool this mode does not offer is repaired instead.
    let start = runner.task.model_requests.len();
    fixture.reply(
        "harness_action",
        json!({"content":"{\"type\": \"function\", \"name\": \"bash\", \"parameters\": {\"command\": \"ls\"}}"}),
    );
    fixture.conversational(json!({"action":"search","query":"code"}));
    runner.advance().await.unwrap();
    assert_eq!(intent_details(&runner, "tool_call_from_content").len(), 1);
    assert_eq!(runner.task.model_requests.len() - start, 2);
    assert!(runner.task.recovery.is_none());
}

#[tokio::test]
async fn a_tool_the_mode_does_not_offer_is_corrected_with_the_allowed_names() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.reply(
        "harness_action",
        tool_answer(
            "",
            &[(
                "replace",
                r#"{"file":"code.txt","old_text":"original","new_text":"changed"}"#,
            )],
        ),
    );
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    let correction = runner
        .task
        .events
        .iter()
        .find(|event| event.message.starts_with("Correcting action"))
        .expect("a correction")
        .message
        .clone();
    assert!(correction.contains("replace"), "{correction}");
    for name in ["read", "search", "inspect", "question", "reply", "plan"] {
        assert!(correction.contains(name), "{correction}");
    }
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
}

#[tokio::test]
async fn a_refused_required_tool_choice_falls_back_to_auto_once() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().reject_required_tool_choice = true;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"search","query":"code"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    let fallbacks = intent_details(&runner, "tool_choice_fallback");
    assert_eq!(fallbacks.len(), 1, "{fallbacks:?}");
    assert!(fallbacks[0].contains("tool_choice"), "{fallbacks:?}");
    let choices: Vec<Value> = action_requests(&fixture)
        .iter()
        .map(|request| request["body"]["tool_choice"].clone())
        .collect();
    assert_eq!(
        choices,
        vec![json!("required"), json!("auto"), json!("auto")]
    );
    assert!(runner.task.recovery.is_none());
}

#[tokio::test]
async fn the_json_schema_contract_remains_selectable() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    runner.set_action_contract(ActionContract::JsonSchema);
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    for request in action_requests(&fixture) {
        let body = &request["body"];
        assert!(body.get("tools").is_none(), "{body}");
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "harness_action"
        );
        let prompt = body["messages"][0]["content"].as_str().unwrap();
        assert!(prompt.contains("Return one JSON object"));
        assert!(prompt.contains("Required JSON schema"));
    }
    assert!(runner
        .task
        .model_requests
        .iter()
        .all(|request| request["contract"] == "json_schema"));
    assert_eq!(
        runner.task.response_receipt.as_ref().unwrap().contract,
        Some(ActionContract::JsonSchema)
    );
}

#[tokio::test]
async fn a_tools_task_reads_plans_edits_and_finishes() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(runner.task.phase, Phase::Verifying);
    let actions = action_requests(&fixture);
    assert!(actions.len() >= 4, "{}", actions.len());
    assert!(actions
        .iter()
        .all(|request| request["body"]["tools"].is_array()));
    assert!(runner.task.recovery.is_none());
    fixture.note("A localized repair keeps the public behavior.");
    fixture.typed_one("ArchitecturalDecision", "Localized repair");
    runner.advance().await.unwrap();
    assert!(
        matches!(runner.task.phase, Phase::AwaitingReview | Phase::Complete),
        "{:?}",
        runner.task.phase
    );
}
