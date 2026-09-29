//! Source by scope (context plan item 5): the files a step works on join the
//! working set as preloaded source, a read outside the scope is served
//! without joining it, and `MOOSEDEV_HARNESS_SOURCE_SCOPE=off` restores the
//! working set of reads alone.
use super::mock::*;
use moosedev::harness::protocol::{ApprovedSpecStatus, GoverningRule};
use moosedev::harness::runner::{Phase, Runner};
use serde_json::json;

const LIB: &str = "pub fn parse(text: &str) -> usize {\n    text.len()\n}\n";
const NOTES: &str = "fn other() {}\n";

/// A project with an approved spec in play: `map.md`, covering `map/`, with
/// a rule governing `map/src/lib.rs` that comes only when a request names
/// that file.
fn spec_project(fixture: &Fixture, approved: bool) {
    std::fs::create_dir_all(fixture.root.join("map/src")).unwrap();
    std::fs::create_dir_all(fixture.root.join("other")).unwrap();
    std::fs::write(fixture.root.join("map.md"), "# Map\nParse maps.\n").unwrap();
    std::fs::write(fixture.root.join("map/src/lib.rs"), LIB).unwrap();
    std::fs::write(fixture.root.join("other/notes.rs"), NOTES).unwrap();
    let mut script = fixture.shared.lock().unwrap();
    let rule: GoverningRule = serde_json::from_value(json!({
        "iri": "urn:fixture:parse-rule",
        "label": "Parsing never panics",
        "kind": "Constraint",
        "claim": "The parser returns an error for malformed input.",
        "via": "via: map/src/lib.rs"
    }))
    .unwrap();
    script.file_rules = vec![("map/src/lib.rs".into(), rule)];
    if approved {
        script.approved_specs = vec![ApprovedSpecStatus {
            path: "map.md".into(),
            stale: false,
            record_count: 1,
            open_rules: Some(vec!["Parse maps".into()]),
            covers: vec!["map/".into()],
        }];
    }
}

async fn planner(fixture: &Fixture) -> Runner {
    fixture
        .interactive_objective("Build what map.md describes")
        .await
}

#[tokio::test]
async fn plan_mode_preloads_the_spec_in_play_with_rules_not_dossiers() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    spec_project(&fixture, true);
    let mut runner = planner(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();

    // The step's refresh asks for the preloaded files' rules, not dossiers.
    let step = requests_of_kind(&fixture, "context").pop().unwrap();
    assert_eq!(step["files"], json!([]));
    assert_eq!(step["rule_files"], json!(["map.md", "map/src/lib.rs"]));
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(
        prompt.contains(&format!("\"map/src/lib.rs\":{}", json!(LIB))),
        "{prompt}"
    );
    assert!(prompt.contains("Parsing never panics"), "{prompt}");
    assert!(
        prompt.contains("Files already read with dossiers: []"),
        "{prompt}"
    );
    assert!(runner.task.read_files.is_empty());
    let journal = journal_value(&runner);
    assert_eq!(
        journal["source_preloaded"],
        json!(["map.md", "map/src/lib.rs"])
    );
    assert!(journal.get("source_recency").is_none());
    assert_eq!(
        intent_details(&runner, "scope_preload"),
        ["2 preloaded: map.md, map/src/lib.rs; 0 not loaded for space: none"]
    );

    // The read outside the scope is served, and the working set is the
    // scope's.
    assert_eq!(
        runner.task.last_response,
        format!("Current text of `other/notes.rs` (outside this step's scope; not added to the working set):\n{NOTES}")
    );
    assert_eq!(
        intent_details(&runner, "read_served_outside_scope"),
        [format!(
            "other/notes.rs: bytes 0..{} of {}",
            NOTES.len(),
            NOTES.len()
        )]
    );
    let source = journal_value(&runner)["source"].clone();
    assert!(source.get("other/notes.rs").is_none(), "{source}");
    assert!(runner.task.read_files.is_empty());

    // A repeat while only looking is refused, then parks.
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    assert!(
        runner
            .task
            .last_response
            .starts_with("Not read again: `other/notes.rs` is unchanged and already served as the Last result at event "),
        "{}",
        runner.task.last_response
    );
    assert_eq!(
        intent_details(&runner, "read_served_outside_scope").len(),
        1
    );
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);

    // The preload set did not change, so it was journaled once.
    assert_eq!(intent_details(&runner, "scope_preload").len(), 1);
}

/// A read outside the scope of a file that changed on disk since it was
/// served is served again, with the new text: it is not a repeat of an
/// unchanged file.
#[tokio::test]
async fn a_read_outside_the_scope_of_a_file_changed_since_it_was_served_is_served_again() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    spec_project(&fixture, true);
    let mut runner = planner(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    assert_eq!(
        intent_details(&runner, "read_served_outside_scope").len(),
        1
    );

    let changed = "fn other() {}\nfn another() {}\n";
    std::fs::write(fixture.root.join("other/notes.rs"), changed).unwrap();
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.last_response,
        format!("Current text of `other/notes.rs` (outside this step's scope; not added to the working set):\n{changed}")
    );
    assert_eq!(
        intent_details(&runner, "read_served_outside_scope"),
        [
            format!("other/notes.rs: bytes 0..{0} of {0}", NOTES.len()),
            format!("other/notes.rs: bytes 0..{0} of {0}", changed.len()),
        ]
    );
    assert!(runner.task.read_files.is_empty());

    // Unchanged since that second serve, the next read is a repeat.
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    assert!(
        runner
            .task
            .last_response
            .starts_with("Not read again: `other/notes.rs` is unchanged"),
        "{}",
        runner.task.last_response
    );
}

#[tokio::test]
async fn a_read_of_a_preloaded_file_shown_in_full_is_not_repeated() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    spec_project(&fixture, true);
    let mut runner = planner(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"map/src/lib.rs"}));
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_response.starts_with(
            "Not read again: map/src/lib.rs is shown in full under Source and is current"
        ),
        "{}",
        runner.task.last_response
    );
    assert!(runner.task.read_files.is_empty());
}

#[tokio::test]
async fn an_edit_to_a_preloaded_file_shown_in_full_proceeds() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    // Planned without reading code.txt: approved, it is preloaded.
    fixture.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    let calls = fixture.model_calls();
    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls + 1);
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        intent_details(&runner, "first_edit_satisfied_preloaded"),
        ["code.txt"]
    );
    assert!(!runner
        .task
        .events
        .iter()
        .any(|event| event.message.starts_with("First-edit guard")));
    // Read in by the guard: an ordinary working-set file now.
    assert_eq!(runner.task.read_files, ["code.txt"]);
}

/// The prompts of one scripted planning step and a read.
async fn planning_prompts(fixture: &Fixture) -> Vec<String> {
    let mut runner = planner(fixture).await;
    fixture.conversational(json!({"action":"read","file":"other/notes.rs"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"reply","message":"Read it."}));
    runner.advance().await.unwrap();
    requests_of_kind(fixture, "model")
        .iter()
        .filter(|request| request["schema"] == "harness_action")
        .map(|request| {
            request["body"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["content"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

#[tokio::test]
async fn switched_off_the_prompts_are_those_of_a_task_with_no_scope() {
    let _env_lock = ENVIRONMENT.lock().await;
    // Today's behaviour: nothing in scope (no spec approved).
    let unscoped = Fixture::new().await;
    spec_project(&unscoped, false);
    let expected = planning_prompts(&unscoped).await;
    assert!(requests_of_kind(&unscoped, "context")
        .iter()
        .all(|request| request["rule_files"] == json!([])));

    // A spec in play, switched off: byte for byte the same prompts.
    let _off = SourceScopeOff::new();
    let fixture = Fixture::new().await;
    spec_project(&fixture, true);
    let prompts = planning_prompts(&fixture).await;
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts, expected);
    assert!(requests_of_kind(&fixture, "context")
        .iter()
        .all(|request| request["rule_files"] == json!([])));
    let mut runner = planner(&fixture).await;
    fixture.conversational(json!({"action":"reply","message":"Nothing yet."}));
    runner.advance().await.unwrap();
    assert!(journal_value(&runner).get("source_preloaded").is_none());
    assert!(intent_details(&runner, "scope_preload").is_empty());
}

#[tokio::test]
async fn a_prompt_the_scopes_rules_overflow_is_built_without_them() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    spec_project(&fixture, true);
    // A rule of a preloaded file too large for any prompt, at any claim
    // budget.
    fixture.shared.lock().unwrap().file_rules[0].1.claim = "x".repeat(120_000);
    let mut runner = planner(&fixture).await;
    fixture.conversational(json!({"action":"reply","message":"Planning."}));
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(
        requests_of_kind(&fixture, "model")
            .iter()
            .filter(|request| request["schema"] == "harness_action")
            .count(),
        1
    );
    let step = requests_of_kind(&fixture, "context").pop().unwrap();
    assert_eq!(step["rule_files"], json!([]));
    let preloads = intent_details(&runner, "scope_preload");
    // The rules alone left no room for the outlines, and then overflowed.
    assert_eq!(
        preloads,
        [
            "0 preloaded: none; 2 not loaded for space: map.md, map/src/lib.rs",
            "withdrawn: the prompt overflowed with the scope's rules, so it is built without them"
        ]
    );
    assert!(journal_value(&runner).get("source_preloaded").is_none());
    assert!(!fixture
        .last_model_prompt("harness_action")
        .contains("Parsing never panics"));
}
