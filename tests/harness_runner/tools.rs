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
            "command",
            "finish",
            "inspect",
            "question",
            "read",
            "replace",
            "replan",
            "reply",
            "request_permission",
            "search",
            "write"
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
    // A read and a search: only leading reads batch, so the search does not
    // run beside it.
    fixture.reply(
        "harness_action",
        tool_answer(
            "Reading both files.",
            &[
                ("read", r#"{"file":"code.txt"}"#),
                ("search", r#"{"query":"other"}"#),
            ],
        ),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    let ignored = intent_details(&runner, "extra_tool_calls_ignored");
    assert_eq!(ignored.len(), 1, "{ignored:?}");
    assert!(ignored[0].contains("search"), "{ignored:?}");
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

/// A provider that ignores `parallel_tool_calls: false` (OpenRouter, 2026-09-29)
/// sent several reads with an already-read file first; running the first had
/// the harness refuse the same read until it parked. The first call the
/// harness would not refuse runs instead.
#[tokio::test]
async fn of_several_calls_the_first_the_harness_would_not_refuse_runs() {
    let _env_lock = ENVIRONMENT.lock().await;
    // These cover the refusal of a file shown in full, which a first read
    // of it now gets served instead (MOOSEDEV_HARNESS_SERVE_SHOWN).
    let _serve_off = ServeShownOff::set();
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("other.txt"), "other\n").unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    let both = || {
        tool_answer(
            "",
            &[
                ("read", r#"{"file":"code.txt"}"#),
                ("read", r#"{"file":"other.txt"}"#),
            ],
        )
    };
    fixture.reply("harness_action", both());
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.read_files,
        vec!["code.txt".to_string(), "other.txt".to_string()]
    );
    assert_eq!(
        intent_details(&runner, "tool_calls_passed_over"),
        ["ran read; passed over read, which the harness would refuse"]
    );
    let multi = intent_details(&runner, "provider_multi_call");
    assert_eq!(multi.len(), 1, "{multi:?}");
    assert!(multi[0].starts_with("2 calls in one response"), "{multi:?}");
    assert!(runner.task.events.iter().any(|event| event
        .message
        .contains("the first the harness would not refuse")));

    // Every call would be refused: the first runs and meets its refusal, and
    // the multiple calls are journaled once per task.
    fixture.reply("harness_action", both());
    runner.advance().await.unwrap();
    assert_eq!(intent_details(&runner, "tool_calls_passed_over").len(), 1);
    assert_eq!(intent_details(&runner, "provider_multi_call").len(), 1);
    assert!(!intent_details(&runner, "read_repeat_refused").is_empty());

    // Switched off, the first call runs whatever the harness would do with it.
    std::env::set_var("MOOSEDEV_HARNESS_MULTI_CALL", "first");
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("other.txt"), "other\n").unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.reply("harness_action", both());
    let result = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_MULTI_CALL");
    result.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    assert!(intent_details(&runner, "tool_calls_passed_over").is_empty());
}

/// A routing endpoint that serves requests from a new upstream provider is
/// journaled each time, so a run's evidence shows mixed backends.
#[tokio::test]
async fn a_new_upstream_provider_is_journaled() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let serve = |fixture: &Fixture, provider: &str| {
        fixture.shared.lock().unwrap().provider = Some(provider.into());
    };
    serve(&fixture, "CoreWeave");
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"search","query":"code"}));
    runner.advance().await.unwrap();
    assert!(intent_details(&runner, "provider_changed").is_empty());
    serve(&fixture, "DeepInfra");
    fixture.conversational(json!({"action":"search","query":"other"}));
    runner.advance().await.unwrap();
    serve(&fixture, "CoreWeave");
    fixture.conversational(json!({"action":"search","query":"more"}));
    runner.advance().await.unwrap();
    assert_eq!(
        intent_details(&runner, "provider_changed"),
        ["CoreWeave -> DeepInfra"]
    );
}

/// A response past the content limit is a runaway: the same request would
/// repeat it, so the step parks with what happened instead of being resent.
#[tokio::test]
async fn a_response_past_the_size_limit_parks() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let huge = "x".repeat(5 * 1024 * 1024);
    fixture.reply(
        "harness_action",
        tool_answer("", &[("read", &format!("{{\"file\":\"{huge}\"}}"))]),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "response_size_exceeded").len(), 1);
    assert!(
        runner.task.last_response.contains("passed the size limit"),
        "{}",
        runner.task.last_response
    );
}

/// Every action request carries the output cap, so a runaway generation
/// stops there (badciv orC: 30-105k-token responses ran 9-45 minutes),
/// planning included, and no request asks for more than the room its prompt
/// leaves in the window.
#[tokio::test]
async fn action_requests_carry_an_output_cap_sized_to_the_window() {
    let _env_lock = ENVIRONMENT.lock().await;
    let default = u64::from(moosedev::harness::response::DEFAULT_MAX_OUTPUT_TOKENS);
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.edit();
    runner.advance().await.unwrap();
    let actions = action_requests(&fixture);
    // Planning and working alike: the configured cap.
    for request in [&actions[0], actions.last().unwrap()] {
        assert_eq!(request["body"]["max_tokens"].as_u64(), Some(default));
    }
    assert_eq!(
        runner.task.model_requests.last().unwrap()["max_output_tokens"],
        default
    );

    // A small window: the cap shrinks to the room the prompt leaves.
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let mut config = fixture.config();
    config.context_window_tokens = 12_000;
    runner.configure(config, None);
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    let request = action_requests(&fixture).pop().unwrap();
    let tokens = request["body"]["max_tokens"].as_u64().unwrap();
    let prompt = request["body"]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .len() as u64;
    assert!(tokens < 12_000 && tokens + prompt / 3 < 12_000, "{tokens}");
}

/// An action the provider stopped at the output cap is repaired with how to
/// split the work, not rejected and resent unchanged; the repair budget
/// bounds it.
#[tokio::test]
async fn an_action_stopped_at_the_output_cap_is_repaired_then_parks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let cut = || json!({"content":"","tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"{\"file\":\"code"}}],"finish_reason":"length"});
    fixture.reply("harness_action", cut());
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    assert_eq!(intent_details(&runner, "output_limit_reached").len(), 1);
    let repaired = action_requests(&fixture);
    assert!(
        repaired[1]["body"]
            .to_string()
            .contains("stopped at the output limit"),
        "the correction names the limit and how to split the work"
    );

    // Three in a row spend the repair budget and stop for the human.
    for _ in 0..3 {
        fixture.reply("harness_action", cut());
    }
    let result = runner.advance().await;
    assert!(result.is_err() || runner.task.phase == Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "output_limit_reached").len(), 4);
    assert!(runner.task.edits.is_empty());
}

/// Headless runs stream actions only when asked, so a provider that stalls
/// mid-response meets the idle timeout instead of the whole-request bound.
#[tokio::test]
async fn action_streaming_always_streams_a_headless_action() {
    use moosedev::harness::response::ActionStreaming;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    runner.task.batch_capture = false;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    runner.set_action_streaming(ActionStreaming::Always);
    fixture.conversational(json!({"action":"search","query":"code"}));
    runner.advance().await.unwrap();
    runner.set_action_streaming(ActionStreaming::Never);
    runner.task.batch_capture = true;
    fixture.conversational(json!({"action":"search","query":"other"}));
    runner.advance().await.unwrap();
    let streamed: Vec<bool> = action_requests(&fixture)
        .iter()
        .map(|request| request["body"]["stream"] == true)
        .collect();
    assert_eq!(streamed, [false, true, false]);
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
    assert!(recovered[0].contains("read (json)"), "{recovered:?}");
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
    // Gemma's own call syntax as content (badciv e3c533b4) runs too, and the
    // journal names its dialect.
    fixture.reply(
        "harness_action",
        json!({"content":"search{query:<|\"|>code<|\"|>}<tool_call|>"}),
    );
    runner.advance().await.unwrap();
    let recovered = intent_details(&runner, "tool_call_from_content");
    assert_eq!(recovered.len(), 2, "{recovered:?}");
    assert!(recovered[1].contains("search (gemma)"), "{recovered:?}");
    let response: Value = serde_json::from_str(
        runner.task.model_requests.last().unwrap()["response"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        response,
        json!({"message":"","action":{"action":"search","query":"code"}})
    );
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

/// Qwen3.5-9B under the json_schema contract flattened the conversational
/// envelope (`{"message":…,"action":"write","file":…}`) three times running;
/// the schema-driven repair folds it back instead of spending the repairs.
#[tokio::test]
async fn a_flattened_json_schema_answer_is_folded_back_into_its_envelope() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    runner.set_action_contract(ActionContract::JsonSchema);
    fixture.reply(
        "harness_action",
        json!({"message":"Reading it first.","action":"read","file":"code.txt"}),
    );
    runner.advance().await.unwrap();
    assert!(runner.task.recovery.is_none(), "no repair was spent");
    assert!(runner.task.read_files.iter().any(|file| file == "code.txt"));
    assert!(runner
        .task
        .intent_events
        .iter()
        .any(|event| event.kind == "json_unflattened"));
}

/// The same model also keyed a variant by its tag
/// (`{"action":{"read":{"file":…}}}`); that is rebuilt too.
#[tokio::test]
async fn a_tag_keyed_json_schema_answer_is_rebuilt() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    runner.set_action_contract(ActionContract::JsonSchema);
    fixture.reply(
        "harness_action",
        json!({"message":"Reading it first.","action":{"read":{"file":"code.txt"}}}),
    );
    runner.advance().await.unwrap();
    assert!(runner.task.recovery.is_none(), "no repair was spent");
    assert!(runner.task.read_files.iter().any(|file| file == "code.txt"));
    assert!(runner
        .task
        .intent_events
        .iter()
        .any(|event| event.kind == "json_unflattened"));
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

/// A `reply` sent beside an action is the model saying what it is about to
/// do. The action runs and the reply becomes its message; running the reply
/// alone ended the turn on "I will …" with the action dropped.
#[tokio::test]
async fn a_reply_beside_an_action_becomes_its_message_and_the_action_runs() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.reply(
        "harness_action",
        tool_answer(
            "",
            &[
                ("reply", r#"{"message":"I will read code.txt first."}"#),
                ("read", r#"{"file":"code.txt"}"#),
            ],
        ),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.read_files, vec!["code.txt".to_string()]);
    assert_eq!(intent_details(&runner, "reply_as_message"), vec!["read"]);
    assert!(intent_details(&runner, "extra_tool_calls_ignored").is_empty());
    let request = runner.task.model_requests.last().unwrap();
    let response: Value = serde_json::from_str(request["response"].as_str().unwrap()).unwrap();
    assert_eq!(
        response,
        json!({"message":"I will read code.txt first.","action":{"action":"read","file":"code.txt"}})
    );
}

/// A response that opens with several reads (badciv orC-orE: 3-21 reads,
/// then searches) runs its leading distinct reads together: the first as the
/// step's action, the rest into the working set, and nothing after them.
#[tokio::test]
async fn leading_reads_in_one_response_run_together() {
    let _env_lock = ENVIRONMENT.lock().await;
    let call = |name: &str, arguments: Value| {
        format!(
            "<tool_call>\n{}\n</tool_call>",
            json!({"name": name, "arguments": arguments})
        )
    };
    let raw = [
        call("read", json!({"file":"code.txt"})),
        call("read", json!({"file":"notes.txt"})),
        call("read", json!({"file":"notes.txt"})),
        call("read", json!({"file":"more.txt"})),
        call("search", json!({"query":"x"})),
    ]
    .join("\n");
    for batch in [true, false] {
        let fixture = Fixture::new().await;
        std::fs::write(fixture.root.join("notes.txt"), "notes\n").unwrap();
        std::fs::write(fixture.root.join("more.txt"), "more\n").unwrap();
        let mut runner = fixture.interactive().await;
        if !batch {
            std::env::set_var("MOOSEDEV_HARNESS_READ_BATCH", "1");
        }
        fixture.reply("harness_action", json!({"content": raw}));
        runner.advance().await.unwrap();
        std::env::remove_var("MOOSEDEV_HARNESS_READ_BATCH");
        let read = |file: &str| runner.task.read_files.contains(&file.to_string());
        assert!(!read("more.txt"));
        if batch {
            assert_eq!(
                intent_details(&runner, "read_batch"),
                ["read 1; not read 0"]
            );
            assert!(read("notes.txt"));
            assert!(
                runner.task.last_response.ends_with(
                    "Also read from the same response, now in the working set with their governing knowledge: `notes.txt`."
                ),
                "{}",
                runner.task.last_response
            );
        } else {
            assert!(intent_details(&runner, "read_batch").is_empty());
            assert!(!read("notes.txt"));
        }
    }
}

/// `moosedev code render`: the request it builds is the one the next real
/// step sends, byte for byte (prompt, tools, output cap), and building it
/// neither asks the model nor writes the journal.
#[tokio::test]
async fn a_rendered_request_is_the_request_the_next_step_sends() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    let id = runner.task.id.clone();
    drop(runner);
    let journal = journal_path(&fixture, &id);
    let before = std::fs::read(&journal).unwrap();
    let sent_before = action_requests(&fixture).len();

    let mut renderer = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    renderer.configure(fixture.config(), None);
    let rendered = renderer.render_next_request(4).await.unwrap();
    drop(renderer);
    assert_eq!(
        std::fs::read(&journal).unwrap(),
        before,
        "render wrote the journal"
    );
    assert_eq!(
        action_requests(&fixture).len(),
        sent_before,
        "render asked the model"
    );

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    let sent = action_requests(&fixture).pop().unwrap();
    let body = &rendered["body"];
    assert_eq!(body["messages"], sent["body"]["messages"]);
    assert_eq!(body["tools"], sent["body"]["tools"]);
    assert_eq!(body["max_tokens"], sent["body"]["max_tokens"]);
    assert_eq!(body["temperature"], sent["body"]["temperature"]);
    let entry = runner
        .task
        .model_requests
        .iter()
        .rev()
        .find(|entry| entry["purpose"] == "harness_action")
        .unwrap();
    assert_eq!(body["messages"][0]["content"], entry["prompt"]);
    assert_eq!(rendered["task"], json!(id));

    // A task at a human gate has no request to render.
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    runner.task.phase = Phase::AwaitingInput;
    let error = runner.render_next_request(4).await.unwrap_err();
    assert!(
        error.to_string().contains("no model request to render"),
        "{error}"
    );
}

/// A provider that refuses for payment stops the step once, saying what to
/// fix, and nothing is sent again until a human answers (badciv orG2: HTTP 402
/// on every request, the compatibility probe re-sent 600+ times a replicate).
#[tokio::test]
async fn a_refusal_for_payment_stops_the_step_without_retrying() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.shared.lock().unwrap().refuse_status = Some(402);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(
        runner.task.last_response.contains("HTTP 402"),
        "{}",
        runner.task.last_response
    );
    assert!(runner.task.last_response.contains("add credit"));
    assert_eq!(intent_details(&runner, "provider_refused"), ["HTTP 402"]);
    let sent = fixture.shared.lock().unwrap().refused_requests;
    assert!(sent >= 1);
    let _ = runner.advance().await;
    assert_eq!(fixture.shared.lock().unwrap().refused_requests, sent);
}

/// An insertion the file already holds is not applied again: it is a no-op,
/// not an edit (badciv orL: one replace applied 30-77 times, each counted as
/// progress, so the loop detector never saw the failure come back).
#[tokio::test]
async fn an_insertion_the_file_already_holds_is_not_applied_again() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let insert = json!({"action":"replace","file":"code.txt","old_text":"original\n","new_text":"original\nadded\n"});
    fixture.conversational(insert.clone());
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\nadded\n"
    );
    let edits = runner.task.edits.len();
    fixture.conversational(insert);
    for _ in 0..3 {
        if fixture.shared.lock().unwrap().replies.is_empty() {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\nadded\n"
    );
    assert_eq!(runner.task.edits.len(), edits);
    assert_eq!(intent_details(&runner, "reapplied_insertion").len(), 1);
}
