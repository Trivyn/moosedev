//! Context plan receipts (context plan item 1): every step-action request
//! journals what each prompt section took, whole on its `model_requests`
//! entry and compact as one `context_plan` intent event.
use super::mock::*;
use moosedev::harness::protocol::GoverningRule;
use moosedev::harness::response::ActionContract;
use moosedev::harness::runner::Runner;
use serde_json::{json, Value};

/// The fixture's prompt budget: a 32,768-token window less 4,096 tokens, at
/// three bytes a token, less the 1 KB repair reserve.
const BUDGET: u64 = (32_768 - 4_096) * 3 - 1_024;

fn rule(iri: &str, label: &str, kind: &str) -> GoverningRule {
    serde_json::from_value(json!({
        "iri": iri,
        "label": label,
        "kind": kind,
        "claim": format!("{label}, always."),
        "via": "via: code.txt"
    }))
    .unwrap()
}

/// The step prompt an entry carries, without the output schema or a repair
/// note.
fn step_prompt(entry: &Value) -> &str {
    let request = entry["prompt"].as_str().unwrap();
    let end = [
        "\nRequired JSON schema:\n",
        "\nYour last candidate was rejected:",
    ]
    .iter()
    .filter_map(|marker| request.find(marker))
    .min()
    .unwrap_or(request.len());
    &request[..end]
}

/// The project-rules section of a step prompt: from its header to the output
/// instruction after it; empty when there are no rules.
fn rules_section(prompt: &str) -> &str {
    let Some(start) = prompt.find("\nProject rules (") else {
        return "";
    };
    let end = [
        "Return one JSON object",
        "Return exactly one JSON action.",
        "Call exactly one tool",
    ]
    .iter()
    .filter_map(|marker| prompt[start..].find(marker))
    .min()
    .unwrap();
    &prompt[start..start + end]
}

fn action_requests(runner: &Runner) -> Vec<Value> {
    runner
        .task
        .model_requests
        .iter()
        .filter(|entry| entry["purpose"] == "harness_action")
        .cloned()
        .collect()
}

fn usize_at(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} in {value}"))
}

/// Every step-action entry's receipt against its own prompt: the sections
/// add up to the step prompt, and with the output schema and any repair note
/// to the prompt sent (`total`); the rules section is as long as it says, and
/// one compact intent event, within the intent cap, was journaled per
/// request.
fn assert_receipts(runner: &Runner) -> Vec<Value> {
    let requests = action_requests(runner);
    assert!(!requests.is_empty());
    let mut plans = Vec::new();
    for entry in &requests {
        let plan = &entry["context_plan"];
        assert!(plan.is_object(), "no context_plan on {entry}");
        let prompt = step_prompt(entry);
        let sent = entry["prompt"].as_str().unwrap();
        assert_eq!(usize_at(plan, "total"), sent.len() as u64, "{plan}");
        let schema = sent[prompt.len()..]
            .find("\nYour last candidate was rejected:")
            .unwrap_or(sent.len() - prompt.len());
        assert_eq!(usize_at(plan, "schema_bytes"), schema as u64, "{plan}");
        assert_eq!(
            usize_at(plan, "repair_bytes"),
            (sent.len() - prompt.len() - schema) as u64,
            "{plan}"
        );
        let sections: u64 = [
            "head_bytes",
            "navigation_bytes",
            "observations_bytes",
            "state_bytes",
        ]
        .iter()
        .map(|key| usize_at(plan, key))
        .sum::<u64>()
            + usize_at(&plan["source"], "bytes")
            + usize_at(&plan["history"], "bytes");
        assert_eq!(sections, prompt.len() as u64, "{plan}");
        assert_eq!(
            sections + usize_at(plan, "schema_bytes") + usize_at(plan, "repair_bytes"),
            usize_at(plan, "total"),
            "{plan}"
        );
        assert_eq!(
            usize_at(&plan["rules"], "bytes"),
            rules_section(prompt).len() as u64,
            "{plan}"
        );
        assert!(usize_at(&plan["rules"], "bytes") < usize_at(plan, "head_bytes"));
        assert_eq!(usize_at(plan, "budget"), BUDGET);
        assert_eq!(plan["source"]["budget"], entry["source_budget"]);
        assert_eq!(plan["rules"]["decided_by_supported"], true);
        plans.push(plan.clone());
    }
    let events = intent_details(runner, "context_plan");
    assert_eq!(events.len(), requests.len(), "{events:?}");
    for event in &events {
        assert!(event.len() <= 2000, "{} bytes", event.len());
        assert!(event.starts_with("rules "), "{event}");
    }
    // Other requests carry no receipt.
    assert!(runner
        .task
        .model_requests
        .iter()
        .filter(|entry| entry["purpose"] != "harness_action")
        .all(|entry| entry["context_plan"].is_null()));
    plans
}

#[tokio::test]
async fn every_step_action_request_carries_its_context_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().governing_rules = vec![
        rule("urn:fixture:c", "Code stays plain text", "Constraint"),
        rule("urn:fixture:r", "Repairs keep behavior", "Requirement"),
    ];
    let mut runner = fixture.interactive().await;
    runner.set_action_contract(ActionContract::JsonSchema);
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    // A rejected candidate and its repair are two requests, each with its
    // own receipt.
    fixture.reply("harness_action", json!("not an action object"));
    fixture.conversational(json!({"action":"reply","message":"Read it.","then":"wait"}));
    runner.advance().await.unwrap();
    let requests = action_requests(&runner);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["attempt"], 2);

    let plans = assert_receipts(&runner);
    // Under the json_schema contract every request appends the schema; only
    // the repair appends the rejection note, and its receipt counts it.
    assert!(plans.iter().all(|plan| usize_at(plan, "schema_bytes") > 0));
    assert_eq!(usize_at(&plans[1], "repair_bytes"), 0);
    assert!(usize_at(&plans[2], "repair_bytes") > 0, "{}", plans[2]);
    let rules = &plans[0]["rules"];
    assert_eq!(rules["full"], json!({"Constraint": 1, "Requirement": 1}));
    assert_eq!(rules["one_line"], json!({}));
    assert_eq!(rules["settled"], json!({}));
    // After the read, code.txt is shown in full; nothing is in scope yet.
    let source = &plans[1]["source"];
    assert_eq!(source["full"], 1);
    assert_eq!(source["nonscope_full"], 1);
    assert_eq!(source["scope_full"], 0);
    assert_eq!(plans[1]["scope"], json!([]));
    assert_eq!(plans[1]["history"]["earlier_tasks"], 0);

    // The journal round-trips with the receipts on.
    let journal = journal_value(&runner);
    let reloaded: moosedev::harness::runner::Task =
        serde_json::from_value(journal.clone()).unwrap();
    assert_eq!(serde_json::to_value(&reloaded).unwrap(), journal);
}

#[tokio::test]
async fn the_receipt_shows_the_scope_and_what_it_preloaded() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    // Planned without reading code.txt: approved, it is preloaded.
    fixture.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.edit();
    runner.advance().await.unwrap();

    let plans = assert_receipts(&runner);
    let plan = plans.last().unwrap();
    assert_eq!(plan["scope"], json!(["code.txt"]));
    assert_eq!(plan["preloaded"], json!(["code.txt"]));
    assert_eq!(plan["preload_skipped"], json!([]));
    assert_eq!(plan["source"]["full"], 1);
    assert_eq!(plan["source"]["scope_full"], 1);
    assert_eq!(plan["source"]["nonscope_full"], 0);
    let compact = intent_details(&runner, "context_plan").pop().unwrap();
    assert!(compact.contains("; scope 1 pre 1 skip 0;"), "{compact}");
    assert!(compact.contains(" (in scope 1, out 0);"), "{compact}");
}

#[tokio::test]
async fn the_receipt_counts_the_earlier_tasks_the_history_shows() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    runner.set_conversation_context(
        "Earlier tasks in this conversation (history; where it disagrees with the current source above, the source is right):\n1 earlier task(s) omitted.\n- Fix the parser → Fixed it.\n- Add a test → Added one.\n\nuser: Now repair code.txt".into(),
    );
    fixture.conversational(json!({"action":"reply","message":"On it.","then":"wait"}));
    runner.advance().await.unwrap();

    let plans = assert_receipts(&runner);
    assert_eq!(plans[0]["history"]["earlier_tasks"], 2);
    assert!(usize_at(&plans[0]["history"], "bytes") > 0);
    let compact = intent_details(&runner, "context_plan").pop().unwrap();
    assert!(compact.contains("(2 earlier)"), "{compact}");
}
