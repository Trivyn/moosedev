//! The harness's own decisions: the coding model answers only `harness_action`
//! and one final `harness_capture_note`; obligations, associations and capture
//! typing come from the scripted daemon. Same scripted sensor and filesystem
//! as the other runner tests.
use super::*;

#[tokio::test]
async fn schema_1_journal_is_refused_with_start_a_new_task() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let runner = fixture.interactive().await;
    let id = runner.task.id.clone();
    drop(runner);
    let journal = fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"));
    let mut persisted: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    assert_eq!(
        persisted["schema"],
        json!(moosedev::harness::runner::SCHEMA)
    );
    assert_eq!(persisted["schema"], json!(2));
    assert!(persisted.get("intent_policy").is_none());
    assert!(persisted.get("capture_contract").is_none());
    Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    persisted["schema"] = json!(1);
    std::fs::write(&journal, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
    let error = Runner::load(fixture.root.clone(), fixture.url.clone(), &id)
        .err()
        .expect("a schema 1 journal must not load");
    assert_eq!(
        error.to_string(),
        "task journal schema 1 is unsupported by this build; start a new task"
    );
}

#[tokio::test]
async fn job_text_names_the_single_final_note() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"Hello."}));
    runner.advance().await.unwrap();
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(prompt.contains("Your job: read, edit, run checks, finish."));
    assert!(prompt.contains("one plain question"));
    assert!(!prompt.contains("change_intent"));
    assert!(!prompt.contains("\"associate\""));
}

#[tokio::test]
async fn symbolic_approval_derives_obligations_from_direct_dossier_records() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    let calls = fixture.model_calls();
    runner.approve_plan().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "approval asks the model nothing"
    );
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(intent_details(&runner, "obligations_derived").len(), 1);

    let scope = runner.task.approved_change_scope.clone().unwrap();
    assert_eq!(scope.version, 2);
    assert_eq!(scope.obligation_iris, vec![PRESERVE.to_string()]);
    assert_eq!(scope.knowledge_revision, "accepted-v1");
    assert_eq!(scope.checks, vec!["true".to_string()]);
    assert_eq!(scope.definition_scopes.len(), 1);
    assert_eq!(scope.definition_scopes[0].file, "labels.py");
    assert_eq!(scope.definition_scopes[0].symbol, RENDER_NAME);
    let state = runner.task.symbolic.clone().unwrap();
    assert_eq!(
        state.obligations.get("labels.py").unwrap(),
        &vec![PRESERVE.to_string()]
    );
    assert_eq!(state.knowledge_revision, "accepted-v1");
    assert!(!state.obligations_digest.is_empty());
    assert_eq!(state.scope_escapes, 0);

    let id = runner.task.id.clone();
    drop(runner);
    let runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    let reloaded = runner.task.symbolic.clone().unwrap();
    assert_eq!(reloaded.obligations, state.obligations);
    assert_eq!(reloaded.obligations_digest, state.obligations_digest);
    assert_eq!(
        runner
            .task
            .approved_change_scope
            .as_ref()
            .unwrap()
            .obligation_iris,
        vec![PRESERVE.to_string()]
    );
}

#[tokio::test]
async fn symbolic_real_replan_keeps_working_set_rederives_obligations_and_counters() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().scope_escapes = 2;
    // A command result is new evidence, so the replan after it is real.
    fixture.conversational(json!({"action":"command","command":"true"}));
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    fixture
        .conversational(json!({"action":"replan","reason":"Narrow the change to the helper only"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        runner.task.read_files,
        vec!["labels.py".to_string()],
        "a real replan keeps the working set"
    );
    assert!(journal_value(&runner)["source"].get("labels.py").is_some());
    assert_eq!(
        intent_details(&runner, "model_replan"),
        vec!["Narrow the change to the helper only"]
    );
    assert!(intent_details(&runner, "replan_continuation").is_empty());
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "the checkpoint journals without a model call"
    );
    assert_eq!(runner.task.phase, Phase::Planning);
    // Human links the second record while the plan is being revised.
    fixture.shared.lock().unwrap().intent_accepted.push(
        moosedev::harness::daemon::intent::IntentBinding {
            record_iri: UNLINKED.into(),
            file: "labels.py".into(),
            symbol: RENDER_NAME.into(),
            source_digest: None,
        },
    );
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior in the helper","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.approved_change_scope.is_none());
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    let state = runner.task.symbolic.clone().unwrap();
    assert_eq!(state.scope_escapes, 2, "task-wide bounds survive a replan");
    assert!(
        state.unchanged_since_approval,
        "approval opens a new window"
    );
    assert_eq!(
        state.obligations.get("labels.py").unwrap(),
        &vec![UNLINKED.to_string(), PRESERVE.to_string()]
    );
    assert_eq!(intent_details(&runner, "obligations_derived").len(), 2);
}

#[tokio::test]
async fn symbolic_replan_with_nothing_new_continues_the_approved_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let approved = journal_value(&runner)["approved_revision"].clone();
    assert!(approved.is_string());
    let cycles = intent_details(&runner, "cycle_ended").len();
    // Reads do not end the unchanged window.
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"replan","reason":"Reconsider the helper name"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(journal_value(&runner)["approved_revision"], approved);
    assert!(runner.task.approved_change_scope.is_some());
    assert_eq!(journal_value(&runner)["capture_due"], false);
    assert!(
        runner.task.last_response.starts_with("Replan not needed"),
        "{}",
        runner.task.last_response
    );
    assert!(runner.task.last_response.contains("labels.py"));
    assert_eq!(
        intent_details(&runner, "replan_continuation"),
        vec!["1: Reconsider the helper name"]
    );
    assert!(intent_details(&runner, "model_replan").is_empty());
    assert_eq!(intent_details(&runner, "cycle_ended").len(), cycles);
    assert_eq!(runner.task.read_files, vec!["labels.py".to_string()]);
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(prompt.contains("A replan with nothing new since approval does not reopen planning."));
    assert!(prompt.contains(
        "Use replan when an edit, a check result or a human answer shows the approved files or checks must change."
    ));
    fixture.conversational(json!({"action":"replan","reason":"Reconsider again"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().replan_continuations,
        2
    );

    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    let state = runner.task.symbolic.clone().unwrap();
    assert!(state.unchanged_since_approval);
    assert_eq!(state.replan_continuations, 2);
    fixture.conversational(json!({"action":"replan","reason":"And once more"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(intent_details(&runner, "replan_continuation").len(), 3);
}

#[tokio::test]
async fn symbolic_replan_after_an_edit_is_a_real_replan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().intent_accepted.clear();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name.strip()"}));
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert!(
        !runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unchanged_since_approval
    );
    fixture.conversational(json!({"action":"replan","reason":"The strip belongs in a helper"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(journal_value(&runner)["approved_revision"].is_null());
    assert_eq!(
        intent_details(&runner, "model_replan"),
        vec!["The strip belongs in a helper"]
    );
    assert!(intent_details(&runner, "replan_continuation").is_empty());
    assert_eq!(runner.task.read_files, vec!["labels.py".to_string()]);
}

#[tokio::test]
async fn symbolic_replan_after_a_human_answer_or_command_is_a_real_replan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(
        json!({"action":"question","question":"Should the helper also lower-case?"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    runner.answer("Yes, lower-case too.".into()).await.unwrap();
    assert!(
        !runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unchanged_since_approval
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    fixture.conversational(json!({"action":"replan","reason":"Lower-casing changes the checks"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan, "an answer is new evidence");
    assert_eq!(intent_details(&runner, "model_replan").len(), 1);
    assert!(intent_details(&runner, "replan_continuation").is_empty());

    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"command","command":"true"}));
    runner.advance().await.unwrap();
    assert!(
        !runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unchanged_since_approval
    );
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"replan","reason":"The command output changes the approach"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.mode,
        Mode::Plan,
        "a command result is new evidence"
    );
    assert_eq!(intent_details(&runner, "model_replan").len(), 1);
    assert!(intent_details(&runner, "replan_continuation").is_empty());
}

/// The action names a recorded `harness_action` request offered as tools.
fn offered_actions(request: &Value) -> Vec<String> {
    let mut names: Vec<String> = request["body"]["tools"]
        .as_array()
        .expect("tool definitions")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn standing_guidance_sections_reach_only_their_own_mode() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    // Written before the runner exists: guidance is snapshotted at creation.
    std::fs::create_dir_all(fixture.root.join(".moosedev")).unwrap();
    std::fs::write(
        fixture.root.join(".moosedev/GUIDANCE.md"),
        "Shared everywhere.\n\n## Plan\nOnly while planning.\n\n## Implement\nOnly once approved.\n",
    )
    .unwrap();

    let mut runner = planned_symbolic_runner(&fixture).await;
    let prompts = |fixture: &Fixture, from: usize| {
        requests_of_kind(fixture, "model")[from..]
            .iter()
            .map(|request| {
                request["body"]["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let planning = prompts(&fixture, 0);
    assert!(!planning.is_empty());
    for prompt in &planning {
        assert!(
            prompt.contains("Shared everywhere.\n\nOnly while planning."),
            "{prompt}"
        );
        assert!(!prompt.contains("Only once approved."), "{prompt}");
    }

    let before = requests_of_kind(&fixture, "model").len();
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    let working = prompts(&fixture, before);
    assert!(!working.is_empty());
    for prompt in &working {
        assert!(
            prompt.contains("Shared everywhere.\n\nOnly once approved."),
            "{prompt}"
        );
        assert!(!prompt.contains("Only while planning."), "{prompt}");
    }
}

#[tokio::test]
async fn model_requests_carry_the_action_schema_for_the_current_mode() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();

    let actions: Vec<Value> = requests_of_kind(&fixture, "model")
        .into_iter()
        .filter(|request| request["schema"] == "harness_action")
        .collect();
    assert_eq!(
        actions.len(),
        3,
        "read and plan while planning, then one edit"
    );
    let planning = ["inspect", "plan", "question", "read", "reply", "search"];
    for request in &actions[..2] {
        assert_eq!(offered_actions(request), planning);
        let prompt = request["body"]["messages"][0]["content"].as_str().unwrap();
        assert!(
            prompt.contains("Allowed actions now: read, search, inspect, question, reply, plan."),
            "the planning prompt promises exactly the planning actions"
        );
    }
    assert_eq!(
        offered_actions(&actions[2]),
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
}

#[tokio::test]
async fn symbolic_replan_in_plan_mode_is_a_noop() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    // A replan while planning is refused only for providers that ignore the schema.
    runner.set_action_contract(moosedev::harness::response::ActionContract::JsonSchema);
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    let cycles = intent_details(&runner, "cycle_ended").len();
    fixture.conversational(json!({"action":"replan","reason":"Start over"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(journal_value(&runner)["capture_due"], false);
    assert!(
        runner.task.last_response.starts_with("Already planning"),
        "{}",
        runner.task.last_response
    );
    assert_eq!(intent_details(&runner, "replan_noop"), vec!["Start over"]);
    assert!(intent_details(&runner, "model_replan").is_empty());
    assert_eq!(intent_details(&runner, "cycle_ended").len(), cycles);
    assert_eq!(runner.task.read_files, vec!["labels.py".to_string()]);
}

#[tokio::test]
async fn symbolic_scope_escape_replans_naming_the_file_then_edits_after_approval() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    // The off switch keeps the automatic replan.
    std::env::set_var("MOOSEDEV_HARNESS_SCOPE_CHOICE", "off");
    let advanced = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_SCOPE_CHOICE");
    advanced.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("other.py")).unwrap(),
        "x = 1\n"
    );
    assert!(runner.task.last_response.contains("other.py"));
    assert!(runner.task.last_response.contains("labels.py"));
    assert_eq!(
        intent_details(&runner, "scope_escape_replan"),
        vec!["other.py: escape 1 of 3"]
    );
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 1);
    assert!(
        intent_details(&runner, "replan_continuation").is_empty(),
        "a scope escape is never continued"
    );
    assert!(intent_details(&runner, "model_replan").is_empty());
    assert_eq!(
        runner.task.read_files,
        vec!["labels.py".to_string()],
        "the working set is kept"
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior and update the constant","files":["labels.py","other.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    fixture.conversational(json!({"action":"read","file":"other.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(runner.task.edits[0].file, "other.py");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("other.py")).unwrap(),
        "x = 2\n"
    );
}

/// badciv P5: a4b sent `write` with file `command` and a shell script as its
/// content. That is a misrouted action, corrected by the repair budget, not an
/// escape from the plan that replans.
#[tokio::test]
async fn symbolic_a_write_to_an_action_name_is_corrected_not_a_scope_escape() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"write","file":"command","content":"mkdir -p badciv-map/tests\ntouch badciv-map/tests/tiny_fixture.rs"}));
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    let calls = fixture.model_calls();
    for _ in 0..3 {
        if fixture.model_calls() < calls + 2 {
            runner.advance().await.unwrap();
        }
    }
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(intent_details(&runner, "scope_escape_replan").is_empty());
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 0);
    assert!(!fixture.root.join("command").exists());
    assert!(
        runner.task.events.iter().any(|event| {
            event
                .message
                .starts_with("Correcting action, attempt 2 of 3")
                && event.message.contains(
                    "`command` is an action, not a file. To run a shell command use the command action; write creates missing parent directories itself.",
                )
        }),
        "{:?}",
        runner
            .task
            .events
            .iter()
            .rev()
            .take(4)
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

/// An action name the plan itself names is a file: a planned `command`
/// script is written, not refused as a misrouted action.
#[tokio::test]
async fn a_planned_file_named_like_an_action_is_written() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    act(&fixture, &mut runner, json!({"action":"plan","summary":"Preserve display behavior and add a command script","files":["command"],"checks":["true"]})).await;
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    runner.approve_plan().await.unwrap();
    act(
        &fixture,
        &mut runner,
        json!({"action":"write","file":"command","content":"#!/bin/sh\necho ok\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("command")).unwrap(),
        "#!/bin/sh\necho ok\n"
    );
    assert!(!runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("is an action, not a file")));
}

#[tokio::test]
async fn symbolic_scope_escapes_are_bounded_per_task_and_park_for_guidance() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().scope_escapes = 3;
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    let calls = fixture.model_calls();
    // The bound belongs to the automatic replan, which the off switch keeps.
    std::env::set_var("MOOSEDEV_HARNESS_SCOPE_CHOICE", "off");
    let advanced = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_SCOPE_CHOICE");
    advanced.unwrap();
    assert_eq!(fixture.model_calls(), calls + 1);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.edits.is_empty());
    assert!(runner.task.last_response.contains("other.py"));
    assert!(runner.task.last_response.contains("Provide guidance"));
    assert_eq!(
        intent_details(&runner, "scope_escape_exhausted"),
        vec!["other.py: escape 4, bound 3"]
    );
    assert!(intent_details(&runner, "scope_escape_replan").is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 4);
    assert!(runner.advance().await.is_err(), "parked until a human acts");
    runner
        .submit_message("Add other.py to the plan and continue.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
}

#[tokio::test]
async fn symbolic_noop_edit_is_repaired_while_the_language_server_reports_errors() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let mut snapshot = diagnostics(2);
    snapshot.errors[0].file = "labels.py".into();
    runner.task.diagnostics = Some(snapshot);
    // Three different no-ops: identical ones are narrowed or stopped at two
    // (the repair lever).
    for span in ["return name", "def render_name", "render_name(name)"] {
        fixture.conversational(
            json!({"action":"replace","file":"labels.py","old_text":span,"new_text":span}),
        );
    }
    let error = runner.advance().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("the language server reports 2 error(s) in the current source, the first at labels.py:1: mismatched types 0"),
        "{rendered}"
    );
    assert!(intent_details(&runner, "noop_edit_continuation").is_empty());
    assert_ne!(runner.task.phase, Phase::Verifying, "not treated as done");

    // An unsettled result proves nothing: the no-op runs the checks.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let mut unsettled = diagnostics(2);
    unsettled.settled = false;
    runner.task.diagnostics = Some(unsettled);
    // Nothing is edited: the first no-op is sent back once for the unedited
    // planned file, and the repeat asks the human,
    // who says it needs no change.
    for _ in 0..2 {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"}));
        runner.advance().await.unwrap();
    }
    verify_unedited(&mut runner).await;
    assert_eq!(runner.task.phase, Phase::Verifying);
}

#[tokio::test]
async fn a_finish_with_stubs_in_planned_files_is_sent_back_once() {
    // badciv be128e71: a4b stubbed parse_map with unimplemented!() and
    // finished; an ignored test let the checks pass.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    raise NotImplementedError\n"}));
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    let calls = fixture.model_calls();
    while fixture.model_calls() < calls + 2 {
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        intent_details(&runner, "finish_refused_stubs"),
        vec!["labels.py:2 raise NotImplementedError"]
    );
    assert!(runner
        .task
        .last_response
        .starts_with("Not finished: planned files still hold code"));

    // The same source finished again goes on to the checks, which decide.
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    runner.advance().await.unwrap();
    assert_ne!(runner.task.phase, Phase::Working);
    assert_eq!(intent_details(&runner, "finish_refused_stubs").len(), 1);
}

/// badciv run 15: the scaffold plan asked for `parse_map` as a "minimal
/// stub", the gate refused the finish, and the builder wrote the whole parser
/// in the scaffold step. Stubs in a file the approved plan names in `stubs`
/// are the plan's to leave; review evidence still names them.
#[tokio::test]
async fn a_finish_keeps_the_stubs_the_approved_plan_leaves() {
    let _env_lock = ENVIRONMENT.lock().await;
    // The scaffold's check runs no tests: with stubs left on purpose there is
    // nothing to test yet, so it is not returned for a test.
    let plan = json!({"action":"plan","summary":"Scaffold: render_name stays a stub for the next task","files":["labels.py"],"checks":["echo 'running 0 tests'"],"stubs":["./labels.py","missing.py"]});
    let stub_and_finish = |fixture: &Fixture| {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    raise NotImplementedError\n"}));
        fixture.conversational(json!({"action":"finish","summary":"Scaffold done."}));
    };

    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("Its stubs lists the planned files"));
    fixture.conversational(plan.clone());
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.plan.as_ref().unwrap().stubs, ["labels.py"]);
    assert_eq!(intent_details(&runner, "plan_stubs"), ["labels.py"]);
    assert_eq!(
        intent_details(&runner, "plan_stubs_unresolved"),
        ["missing.py"]
    );
    runner.approve_plan().await.unwrap();
    stub_and_finish(&fixture);
    fixture.note("Scaffolded render_name.");
    fixture.typed(vec![]);
    for _ in 0..16 {
        if runner.task.phase == Phase::AwaitingReview {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert!(intent_details(&runner, "finish_refused_stubs").is_empty());
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(intent_details(&runner, "check_vacuous").len(), 1);
    assert!(intent_details(&runner, "check_vacuous_returned").is_empty());
    let evidence = runner
        .task
        .symbolic
        .as_ref()
        .and_then(|state| state.capture_note.as_ref())
        .map(|note| note.evidence.clone())
        .unwrap();
    assert!(
        evidence
            .iter()
            .any(|fact| fact == "Stubs the plan leaves: labels.py:2 raise NotImplementedError."),
        "{evidence:?}"
    );
    assert!(
        !evidence
            .iter()
            .any(|fact| fact.starts_with("Stubs left in planned files")),
        "{evidence:?}"
    );

    // Switched off, the field is not offered and every stub is judged.
    std::env::set_var("MOOSEDEV_HARNESS_PLAN_STUBS", "off");
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    let prompt = fixture.last_model_prompt("harness_action");
    fixture.conversational(plan);
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    let kept = runner.task.plan.as_ref().unwrap().stubs.clone();
    stub_and_finish(&fixture);
    let calls = fixture.model_calls();
    while fixture.model_calls() < calls + 2 {
        runner.advance().await.unwrap();
    }
    std::env::remove_var("MOOSEDEV_HARNESS_PLAN_STUBS");
    assert!(!prompt.contains("Its stubs lists"), "{prompt}");
    assert!(kept.is_empty());
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        intent_details(&runner, "finish_refused_stubs"),
        vec!["labels.py:2 raise NotImplementedError"]
    );
}

#[tokio::test]
async fn a_replace_already_applied_is_a_noop_that_runs_the_checks() {
    // badciv e3c533b4: a4b re-sent the `#[ignore]` it had already added;
    // "found 0" three times spent the repairs and parked the task.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let edit = json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"});
    fixture.conversational(edit.clone());
    fixture.conversational(edit);
    let calls = fixture.model_calls();
    while fixture.model_calls() < calls + 2 {
        runner.advance().await.unwrap();
    }
    assert!(
        runner.task.recovery.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(intent_details(&runner, "noop_edit_continuation").len(), 1);
    assert_ne!(runner.task.phase, Phase::Working);
}

#[tokio::test]
async fn the_noop_repair_names_planned_files_not_written_yet() {
    // badciv e3c533b4: lib.rs declared modules whose planned files did not
    // exist, and a repair naming only lib.rs sent a4b back to it twice.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper module","files":["labels.py","codes.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    let mut snapshot = diagnostics(1);
    snapshot.errors[0].file = "labels.py".into();
    runner.task.diagnostics = Some(snapshot);
    // Three different no-ops: identical ones are narrowed or stopped at two
    // (the repair lever).
    for span in ["return name", "def render_name", "render_name(name)"] {
        fixture.conversational(
            json!({"action":"replace","file":"labels.py","old_text":span,"new_text":span}),
        );
    }
    let rendered = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        rendered.contains("planned files codes.py do not exist. Write them next"),
        "{rendered}"
    );
}

#[tokio::test]
async fn symbolic_noop_edit_runs_checks_unless_this_source_already_failed() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    // Nothing is edited: the first no-op is sent back once for the unedited
    // planned file, without spending a repair, and the repeat asks the human,
    // who says it needs no change.
    for _ in 0..2 {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"}));
        runner.advance().await.unwrap();
        assert!(runner.task.recovery.is_none());
    }
    verify_unedited(&mut runner).await;
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.edits.is_empty());
    assert_eq!(intent_details(&runner, "noop_edit_continuation").len(), 2);
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished").len(),
        1
    );
    assert_eq!(runner.task.symbolic.as_ref().unwrap().noop_continuations, 2);

    // Exactly this source already failed a check: retesting it proves
    // nothing, so the no-op is repaired and the failure is named.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().last_failure = Some(FailedRun {
        command: "cargo check".into(),
        edits: 0,
        denied: false,
        ungrantable: false,
    });
    // Three different no-ops: identical ones are narrowed or stopped at two
    // (the repair lever).
    for span in ["return name", "def render_name", "render_name(name)"] {
        fixture.conversational(
            json!({"action":"replace","file":"labels.py","old_text":span,"new_text":span}),
        );
    }
    let error = runner.advance().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("edit makes no change"), "{rendered}");
    assert!(
        rendered.contains("`cargo check` failed against exactly this source"),
        "{rendered}"
    );
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));
    assert!(intent_details(&runner, "noop_edit_continuation").is_empty());
    assert_eq!(intent_details(&runner, "repair_exhausted").len(), 1);

    // A fix applied after the failure has not been tested: the no-op that
    // follows it runs the checks, however many no-ops the task has had.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    {
        let state = runner.task.symbolic.as_mut().unwrap();
        state.noop_continuations = 1;
        state.last_failure = Some(FailedRun {
            command: "cargo check".into(),
            edits: 0,
            denied: false,
            ungrantable: false,
        });
    }
    add_helper(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return value.strip()","new_text":"return value.strip()"}));
    runner.advance().await.unwrap();
    assert!(runner.task.recovery.is_none());
    assert_eq!(runner.task.symbolic.as_ref().unwrap().noop_continuations, 2);
    assert_eq!(intent_details(&runner, "noop_edit_continuation").len(), 1);
    // The finish derives the helper's association first, then verifies.
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    // The check passes, which clears the remembered failure.
    runner.advance().await.unwrap();
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .last_failure
        .is_none());
}

/// A finish reruns the required checks; with nothing edited since one of
/// them failed, the rerun would only repeat the failure. badciv 301f7887
/// looped nine finish/deny cycles on `cargo run` of a TUI before it was
/// stopped by hand.
#[tokio::test]
async fn a_finish_never_reruns_a_required_check_the_source_already_failed() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    // A check the sandbox appears to block, naming no path the model could
    // request.
    runner.task.plan.as_mut().unwrap().checks =
        vec!["sh -c 'echo /etc/hosts: Operation not permitted; exit 1'".into()];
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    let check_runs = |runner: &Runner| {
        runner
            .task
            .events
            .iter()
            .filter(|event| event.message.starts_with("Command: sh -c"))
            .count()
    };
    runner.advance().await.unwrap();
    assert_eq!(check_runs(&runner), 1);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(intent_details(&runner, "sandbox_denial").len(), 1);
    // The capture checkpoint the failed check made due.
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().last_failure,
        Some(FailedRun {
            command: "sh -c 'echo /etc/hosts: Operation not permitted; exit 1'".into(),
            edits: 1,
            denied: true,
            ungrantable: false,
        })
    );

    // Finishing again changes nothing: the check is not rerun, the finish is
    // repaired naming the check and the permission request, and the third
    // refusal parks the task for the human.
    for n in 0..3 {
        fixture.conversational(
            json!({"action":"finish","summary":format!("The helper is implemented ({n}).")}),
        );
    }
    let error = runner.advance().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains(
            "required check `sh -c 'echo /etc/hosts: Operation not permitted; exit 1'` was blocked by the sandbox against exactly this source"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("request_permission"), "{rendered}");
    assert_eq!(check_runs(&runner), 1);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));
    assert_eq!(intent_details(&runner, "finish_retest_refused").len(), 3);
    assert_eq!(intent_details(&runner, "repair_exhausted").len(), 1);

    // The human's answer re-arms exactly one rerun.
    runner
        .answer("That check cannot run here; finish anyway.".into())
        .await
        .unwrap();
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .last_failure
        .is_none());
    // The capture checkpoint the answer made due.
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.advance().await.unwrap();
    assert_eq!(check_runs(&runner), 2);
    assert_eq!(runner.task.phase, Phase::Working);
    runner.advance().await.unwrap();
    for n in 0..3 {
        fixture.conversational(
            json!({"action":"finish","summary":format!("The helper is implemented ({n}).")}),
        );
    }
    runner.advance().await.unwrap_err();
    assert_eq!(check_runs(&runner), 2);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "finish_retest_refused").len(), 6);
}

/// A failed command the model chose to run is not a required check; the
/// plan's checks decide completion, so the finish runs them.
#[tokio::test]
async fn a_check_denied_without_anything_grantable_parks_for_the_human_without_a_model_call() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    // The badciv shape: a denial signature with no path outside the project
    // and no network need (a TUI dying on the terminal).
    let check = "sh -c 'echo Operation not permitted; exit 1'";
    runner.task.plan.as_mut().unwrap().checks = vec![check.into()];
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);

    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls, "the park asks no model");
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.last_error.is_none());
    assert_eq!(
        intent_details(&runner, "check_ungrantable"),
        vec![check.to_string()]
    );
    assert_eq!(
        intent_details(&runner, "sandbox_denial_ungrantable"),
        vec![check.to_string()]
    );
    assert!(intent_details(&runner, "sandbox_denial").is_empty());
    assert!(
        runner.task.last_response.starts_with(&format!(
            "Required check `{check}` was blocked by the sandbox, and its output names no path"
        )),
        "{}",
        runner.task.last_response
    );
    assert!(runner
        .task
        .last_response
        .contains("/plan to change the plan's checks"));
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().last_failure,
        Some(FailedRun {
            command: check.into(),
            edits: 1,
            denied: true,
            ungrantable: true,
        })
    );

    // The human's guidance returns the task to planning and forgets the old
    // plan's failed check, so the next plan's finish is not refused for it.
    runner
        .submit_message("Use `true` as the check; the TUI cannot run here.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .last_failure
        .is_none());
}

#[tokio::test]
async fn a_failed_free_command_does_not_refuse_the_finish() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"command","command":"false"}));
    runner.advance().await.unwrap();
    // The capture checkpoint the command made due.
    runner.advance().await.unwrap();
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .last_failure
        .is_none());
    // Nothing is edited: the first finish is sent back once for the unedited
    // planned file, and the repeat asks the human,
    // who says it needs no change.
    for _ in 0..2 {
        fixture.conversational(json!({"action":"finish","summary":"Nothing to change."}));
        runner.advance().await.unwrap();
    }
    verify_unedited(&mut runner).await;
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(intent_details(&runner, "finish_retest_refused").is_empty());
}

#[tokio::test]
async fn symbolic_associations_are_derived_and_ratified_without_a_model() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    let calls = fixture.model_calls();
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls + 1,
        "finish is the only model call"
    );
    let kinds = request_kinds(&fixture);
    assert!(kinds.contains(&"intent_associate".to_string()));
    assert!(
        kinds
            .iter()
            .all(|kind| !kind.starts_with("model:") || kind == "model:harness_action"),
        "{kinds:?}"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let links = fixture.shared.lock().unwrap().intent_link_requests.clone();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].bindings.len(), 1);
    assert_eq!(links[0].bindings[0].record_iri, PRESERVE);
    assert_eq!(links[0].bindings[0].symbol, NORMALIZE);
    let derived = intent_details(&runner, "association_derived");
    assert_eq!(derived.len(), 1);
    assert!(derived[0].contains("normalize") && derived[0].contains("concerns"));
    assert!(intent_details(&runner, "association_none").is_empty());
    assert_eq!(intent_details(&runner, "association_skipped").len(), 1);
    let association = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .association
        .clone()
        .unwrap();
    assert_eq!(association.status, "awaiting_review");
    assert_eq!(
        association.link_operation_id.as_deref(),
        Some(links[0].operation_id.as_str())
    );
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        runner.task.reviews[0].response.proposals[0].kind,
        "DerivedAssociation"
    );
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .association
            .as_ref()
            .unwrap()
            .status,
        "resolved"
    );
    assert_eq!(intent_details(&runner, "link_review").len(), 1);
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
}

#[tokio::test]
async fn symbolic_ungoverned_edit_journals_and_proceeds_to_checks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().intent_accepted.clear();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    assert!(runner
        .task
        .approved_change_scope
        .as_ref()
        .unwrap()
        .obligation_iris
        .is_empty());
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name.strip()"}));
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"finish","summary":"Stripped the name."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.task.reviews.is_empty());
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    assert_eq!(
        intent_details(&runner, "association_none"),
        vec!["labels.py"]
    );
    assert_eq!(
        runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .association
            .as_ref()
            .unwrap()
            .status,
        "resolved"
    );
}

#[tokio::test]
async fn only_action_and_capture_note_schemas_are_ever_requested() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    // The rules governing the task's files at capture travel to typing.
    fixture.shared.lock().unwrap().governing_rules = vec![GoverningRule {
        iri: "urn:rule:names".into(),
        label: "Display names stay stable".into(),
        kind: "Requirement".into(),
        claim: "hasDescription: A rendered name never changes between views.\n".into(),
        via: "via: component Labels".into(),
        decided_by: Vec::new(),
    }];
    fixture.note(
        "The helper strips whitespace so labels compare equal; keep normalization in one place.",
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let schemas = model_schemas(&fixture);
    assert!(
        schemas
            .iter()
            .all(|schema| schema == "harness_action" || schema == "harness_capture_note"),
        "{schemas:?}"
    );
    assert_eq!(
        schemas
            .iter()
            .filter(|schema| *schema == "harness_capture_note")
            .count(),
        1
    );
    let typing = fixture.shared.lock().unwrap().capture_type_requests.clone();
    assert_eq!(typing.len(), 1);
    assert_eq!(typing[0].owner_id, runner.task.id);
    assert_eq!(typing[0].changed_files, vec!["labels.py".to_string()]);
    assert_eq!(
        typing[0].plan_summary,
        "Preserve display behavior while adding a helper"
    );
    assert_eq!(typing[0].knowledge_revision, "accepted-links");
    assert_eq!(
        typing[0].governing_labels,
        vec!["Display names stay stable".to_string()],
        "the daemon is told which rules governed the task, to flag a claim that names one"
    );
    assert_eq!(typing[0].check_history.len(), 1);
    assert!(typing[0].check_history[0].after_edit);
    assert!(typing[0].note.starts_with("The helper strips whitespace"));
    let note_event = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .as_ref()
        .unwrap()
        .note_event;
    assert_eq!(
        typing[0].note_evidence,
        vec![format!("Event {note_event}: capture note")]
    );
    assert!(runner.task.events[note_event]
        .message
        .starts_with("Capture note: The helper"));
    assert_eq!(intent_details(&runner, "capture_note").len(), 1);
    assert_eq!(intent_details(&runner, "capture_typed").len(), 1);
    assert_eq!(intent_details(&runner, "reconciled_distinct").len(), 1);
    assert_eq!(runner.task.reviews.len(), 1);
    let proposal = &runner.task.reviews[0].request.proposals[0];
    assert_eq!(proposal.kind, "ArchitecturalDecision");
    assert_eq!(
        proposal.title,
        "Preserve display behavior while adding a helper"
    );
    assert!(proposal
        .description
        .starts_with("The helper strips whitespace"));
    assert_eq!(proposal.files, vec!["labels.py".to_string()]);
    assert_eq!(
        runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .capture_note
            .as_ref()
            .unwrap()
            .status,
        "captured",
        "a successful capture marks the note captured"
    );
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(fixture.shared.lock().unwrap().capture_requests.len(), 1);
}

#[tokio::test]
async fn symbolic_capture_note_survives_restart_before_and_after_typing() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    fixture.shared.lock().unwrap().fail_capture_type_once = true;
    fixture.note("Keep normalization in one helper.");
    assert!(
        runner.advance().await.is_err(),
        "typing acknowledgment was lost"
    );
    let note = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .clone()
        .unwrap();
    assert_eq!(note.status, "asked");
    assert!(note.response.is_none());
    let id = runner.task.id.clone();
    drop(runner);

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(
        runner.advance().await.is_err(),
        "capture acknowledgment was lost"
    );
    let typed = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .clone()
        .unwrap();
    assert_eq!(typed.status, "typed");
    assert_eq!(typed.operation_id, note.operation_id);
    assert_eq!(typed.note, note.note);
    assert!(runner.task.capture_request.is_some());
    drop(runner);

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(
        fixture.typing_ids(),
        vec![note.operation_id.clone(), note.operation_id.clone()]
    );
    assert_eq!(
        fixture.capture_ids(),
        vec![
            note.capture_operation_id.clone(),
            note.capture_operation_id.clone()
        ]
    );
    assert_eq!(
        fixture.note_calls(),
        1,
        "the note is asked once for the whole task"
    );
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn symbolic_restated_note_completes_without_new_knowledge() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    // A restatement that names no changed file has nothing to link.
    fixture.typed(vec![TypedProposal {
        proposal: KnowledgeProposal {
            kind: "Requirement".into(),
            title: "Preserve display label behavior".into(),
            description: "Labels keep their display form.".into(),
            evidence: vec!["Event 1: capture note".into()],
            files: vec![],
            components: vec![],
            requirement: None,
            motivated_by: Vec::new(),
            supersedes: None,
            retracts: None,
            learned_from: None,
            reconciled: vec![],
        },
        origin: ProposalOrigin::SymbolicDecision,
        disposition: TypedDisposition::Restates {
            candidate_iri: PRESERVE.into(),
            score: 0.93,
            confidence: 0.93,
            receipt_operation_id: "receipt-r0".into(),
        },
        resolved_by: "symbolic".into(),
        derived: vec![],
        names_rules: vec![],
    }]);
    fixture.note("Labels keep their display form.");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.reviews.is_empty());
    assert!(runner.task.capture_request.is_none());
    assert_eq!(intent_details(&runner, "reconciled_restates").len(), 1);
    assert!(intent_details(&runner, "reconciled_restates")[0].contains(PRESERVE));
    assert!(fixture.shared.lock().unwrap().capture_requests.is_empty());
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn symbolic_capture_submits_hunk_ranges_and_journals_its_anchors_once() {
    use moosedev::harness::protocol::{HarnessSourcePosition, HarnessSourceRange};
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    fixture.shared.lock().unwrap().fail_capture_once = true;
    fixture.note("Keep normalization in one helper.");
    assert!(
        runner.advance().await.is_err(),
        "the first capture acknowledgment is lost"
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 2);
    let lines = |start: u32, end: u32| HarnessSourceRange {
        start: HarnessSourcePosition {
            line: start,
            col: 0,
        },
        end: HarnessSourcePosition { line: end, col: 0 },
    };
    // The helper edit replaced one line with four: one hunk on each side.
    assert_eq!(captures[1].changed.len(), 1);
    assert_eq!(captures[1].changed[0].file, "labels.py");
    assert_eq!(captures[1].changed[0].changed_ranges, vec![lines(1, 5)]);
    assert_eq!(captures[1].changed[0].before_ranges, vec![lines(1, 2)]);
    assert_eq!(
        serde_json::to_value(&captures[0]).unwrap(),
        serde_json::to_value(&captures[1]).unwrap(),
        "the retried submission carries the frozen geometry"
    );
    assert_eq!(
        intent_details(&runner, "capture_anchored"),
        vec![
            "1 definition anchors, 0 module anchors, 0 unanchored files, 0 anchor notes, 0 restated links"
                .to_string()
        ]
    );
}

#[tokio::test]
async fn symbolic_restated_note_links_the_existing_record_through_one_capture() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    fixture.typed(vec![TypedProposal {
        proposal: KnowledgeProposal {
            kind: "Requirement".into(),
            title: "Preserve display label behavior".into(),
            description: "Labels keep their display form.".into(),
            evidence: vec!["Event 1: capture note".into()],
            files: vec!["labels.py".into()],
            components: vec![],
            requirement: None,
            motivated_by: Vec::new(),
            supersedes: None,
            retracts: None,
            learned_from: None,
            reconciled: vec![],
        },
        origin: ProposalOrigin::SymbolicDecision,
        disposition: TypedDisposition::Restates {
            candidate_iri: PRESERVE.into(),
            score: 0.93,
            confidence: 0.93,
            receipt_operation_id: "receipt-r0".into(),
        },
        resolved_by: "symbolic".into(),
        derived: vec![],
        names_rules: vec![],
    }]);
    fixture.note("Labels keep their display form.");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 1, "a restated-only note submits a capture");
    assert!(captures[0].proposals.is_empty());
    assert_eq!(
        captures[0].restated,
        vec![RestatedCandidate {
            candidate_iri: PRESERVE.into(),
            receipt_operation_id: "receipt-r0".into(),
            files: vec!["labels.py".into()],
        }]
    );
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        intent_details(&runner, "capture_anchored"),
        vec!["0 definition anchors, 0 module anchors, 0 unanchored files, 0 anchor notes, 1 restated links".to_string()]
    );
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    // The earlier association review journaled its own link_review events.
    let link_reviews = intent_details(&runner, "link_review");
    assert_eq!(
        link_reviews
            .iter()
            .filter(|detail| detail.contains("-restated-0"))
            .count(),
        1,
        "{link_reviews:?}"
    );
}

/// A plan approved over `labels.py` whose summary compares `account.segment`
/// with a segment the code does not define; `accounts.py` holds the segment
/// table and is never read.
async fn disputed_plan_runner(fixture: &Fixture) -> Runner {
    std::fs::write(
        fixture.root.join("accounts.py"),
        "SEGMENTS = {\"retail\": \"Retail\", \"charity\": \"Registered charity\"}\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Render no label when `account.segment` is 'non-profit'; otherwise keep the name","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    runner
}

fn segment_grounding() -> GroundResponse {
    GroundResponse {
        keys: vec![GroundKey {
            attribute: "segment".into(),
            literals: vec!["non-profit".into()],
        }],
        definitions: vec![GroundDefinition {
            key: "segment".into(),
            name: "SEGMENTS".into(),
            file: "accounts.py".into(),
            symbol: "scip-python python fixture . accounts.py/SEGMENTS.".into(),
            role: "declaration".into(),
            preview: "SEGMENTS = {\"retail\": \"Retail\", \"charity\": \"Registered charity\"}"
                .into(),
        }],
        mismatches: vec![GroundMismatch {
            key: "segment".into(),
            literal: "non-profit".into(),
            file: "accounts.py".into(),
            definition: "SEGMENTS".into(),
        }],
    }
}

async fn dispute(fixture: &Fixture, runner: &mut Runner, reason: &str) {
    fixture.conversational(json!({"action":"replan","reason":reason}));
    runner.advance().await.unwrap();
}

fn plan_ground_requests(fixture: &Fixture) -> Vec<PlanGroundRequest> {
    fixture.shared.lock().unwrap().plan_ground_requests.clone()
}

#[tokio::test]
async fn a_second_replan_dispute_reads_what_the_plan_compares_against_then_replans_for_real() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = disputed_plan_runner(&fixture).await;
    fixture.shared.lock().unwrap().plan_ground_response = Some(segment_grounding());

    dispute(&fixture, &mut runner, "The NP-7 check may be wrong").await;
    assert!(
        runner.task.last_response.starts_with("Replan not needed"),
        "{}",
        runner.task.last_response
    );
    assert!(
        plan_ground_requests(&fixture).is_empty(),
        "the first continuation in an approval cycle is unchanged"
    );
    assert!(intent_details(&runner, "plan_grounding").is_empty());

    dispute(
        &fixture,
        &mut runner,
        "The segment check against non-profit looks wrong",
    )
    .await;
    let requests = plan_ground_requests(&fixture);
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .text
            .contains("`account.segment` is 'non-profit'"),
        "{}",
        requests[0].text
    );
    assert!(requests[0]
        .text
        .contains("The segment check against non-profit looks wrong"));
    assert_eq!(requests[0].files, vec!["labels.py".to_string()]);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.read_files.contains(&"accounts.py".to_string()));
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.recovery.is_none());
    let note = runner.task.last_response.clone();
    assert!(note.contains("SEGMENTS in accounts.py"), "{note}");
    assert!(
        note.contains("'non-profit' does not appear among the values of SEGMENTS"),
        "{note}"
    );
    assert_eq!(intent_details(&runner, "plan_grounding").len(), 1);
    assert!(
        !runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unchanged_since_approval,
        "the grounding note is new evidence"
    );

    dispute(
        &fixture,
        &mut runner,
        "SEGMENTS has no non-profit segment; the plan must use charity",
    )
    .await;
    assert_eq!(runner.task.mode, Mode::Plan, "the next replan is real");
    assert_eq!(
        intent_details(&runner, "model_replan"),
        vec!["SEGMENTS has no non-profit segment; the plan must use charity"]
    );
    assert_eq!(intent_details(&runner, "replan_continuation").len(), 2);
    assert_eq!(intent_details(&runner, "plan_grounding").len(), 1);
    assert!(
        runner.task.read_files.contains(&"accounts.py".to_string()),
        "a real replan keeps the working set"
    );
}

#[tokio::test]
async fn a_second_replan_dispute_with_nothing_defined_keeps_the_continuation() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = disputed_plan_runner(&fixture).await;
    fixture.shared.lock().unwrap().plan_ground_response = Some(GroundResponse {
        keys: segment_grounding().keys,
        ..GroundResponse::default()
    });
    dispute(&fixture, &mut runner, "Reconsider").await;
    dispute(&fixture, &mut runner, "Reconsider the segment").await;
    assert_eq!(plan_ground_requests(&fixture).len(), 1);
    assert!(
        runner.task.last_response.starts_with("Replan not needed"),
        "{}",
        runner.task.last_response
    );
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(
        runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unchanged_since_approval,
        "nothing was shown, so the window stays open"
    );
    assert!(!runner.task.read_files.contains(&"accounts.py".to_string()));
    let events = intent_details(&runner, "plan_grounding");
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("no definitions"), "{events:?}");

    dispute(&fixture, &mut runner, "Reconsider once more").await;
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(
        plan_ground_requests(&fixture).len(),
        1,
        "at most one plan grounding per approval cycle"
    );
    assert_eq!(intent_details(&runner, "replan_continuation").len(), 3);
    assert_eq!(intent_details(&runner, "plan_grounding").len(), 1);
}

#[tokio::test]
async fn plan_grounding_resets_when_a_new_plan_is_approved() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = disputed_plan_runner(&fixture).await;
    fixture.shared.lock().unwrap().plan_ground_response = Some(segment_grounding());
    dispute(&fixture, &mut runner, "Reconsider").await;
    dispute(&fixture, &mut runner, "Reconsider the segment").await;
    dispute(&fixture, &mut runner, "Use the charity segment").await;
    assert_eq!(runner.task.mode, Mode::Plan);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Render no label when `account.segment` is 'charity'; otherwise keep the name","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    let state = runner.task.symbolic.clone().unwrap();
    assert_eq!(state.cycle_replan_continuations, 0);
    assert!(!state.plan_grounded);

    dispute(&fixture, &mut runner, "Reconsider again").await;
    assert_eq!(plan_ground_requests(&fixture).len(), 1);
    dispute(&fixture, &mut runner, "Reconsider the segment again").await;
    assert_eq!(
        plan_ground_requests(&fixture).len(),
        2,
        "a new approval cycle may ground its plan once"
    );
    assert_eq!(intent_details(&runner, "plan_grounding").len(), 2);

    let id = runner.task.id.clone();
    drop(runner);
    let runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    let state = runner.task.symbolic.clone().unwrap();
    assert!(state.plan_grounded, "the memo survives resume");
    assert_eq!(state.cycle_replan_continuations, 2);
}

/// Two approved plans in one task: the second (after a replan) does not erase
/// the first. The capture note is asked about both plans and the first plan's
/// edit, and the capture carries every rule either plan said it implements,
/// resolved to IRIs; an entry naming no rule is dropped and journaled.
#[tokio::test]
async fn capture_sees_every_approved_plan_and_carries_the_rules_they_address() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![
        GoverningRule {
            iri: PRESERVE.into(),
            label: "Preserve display label behavior".into(),
            kind: "Requirement".into(),
            claim: "hasDescription: Display labels render as before.\n".into(),
            via: "via: linked to labels.py".into(),
            decided_by: Vec::new(),
        },
        GoverningRule {
            iri: UNLINKED.into(),
            label: "Labels never exceed one line".into(),
            kind: "Constraint".into(),
            claim: "hasDescription: A label is a single line.\n".into(),
            via: "via: linked to labels.py".into(),
            decided_by: Vec::new(),
        },
    ];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display label behavior while adding a normalize helper; labels never exceed one line is kept as is","files":["labels.py"],"checks":["true"],"addresses":["Preserve display label behavior","No such rule"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().addresses,
        vec![PRESERVE.to_string()]
    );
    assert_eq!(
        intent_details(&runner, "plan_addresses_unresolved"),
        vec!["No such rule"]
    );
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);

    fixture.conversational(json!({"action":"replan","reason":"Labels must also be single-line"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Collapse newlines in normalize so labels never exceed one line; preserve display label behavior otherwise","files":["labels.py"],"checks":["true"],"addresses":["[Constraint] Labels never exceed one line"]}));
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        match runner.task.phase {
            Phase::AwaitingReview => runner.confirm_no_knowledge().await.unwrap(),
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.approved_plans.len(), 2);
    assert_eq!(runner.task.approved_plans[1].edit_start, 1);
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return value.strip()\n","new_text":"    return \" \".join(value.split())\n"}));
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 2);

    fixture.conversational(
        json!({"action":"finish","summary":"Normalize strips and collapses whitespace."}),
    );
    for _ in 0..4 {
        match runner.task.phase {
            Phase::AwaitingReview => runner.review(true).await.unwrap(),
            Phase::Verifying => break,
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.task.check_results = vec![passed_check()];
    fixture.note("Normalization lives in one helper so both rules are enforced in one place.");
    runner.advance().await.unwrap();

    let prompt = fixture.last_model_prompt("harness_capture_note");
    assert!(prompt.contains(
        "Approved plan 1 of 2:\nPreserve display label behavior while adding a normalize helper"
    ));
    assert!(prompt.contains("Approved plan 2 of 2:\nCollapse newlines in normalize"));
    assert!(
        prompt.contains("def normalize(value):\n    return value.strip()"),
        "the first plan's edit is shown: {prompt}"
    );
    let request = fixture
        .shared
        .lock()
        .unwrap()
        .capture_type_requests
        .last()
        .cloned()
        .expect("the note was typed");
    assert_eq!(
        request.addressed_rules,
        vec![PRESERVE.to_string(), UNLINKED.to_string()]
    );

    // Completing the task is not completing the spec: the completion says how
    // much of it recorded decisions have taken up.
    fixture.shared.lock().unwrap().approved_specs = vec![ApprovedSpecStatus {
        path: "spec.md".into(),
        stale: false,
        record_count: 3,
        open_rules: Some(vec!["Labels are localized".into()]),
        covers: Vec::new(),
    }];
    for _ in 0..4 {
        match runner.task.phase {
            Phase::Complete => break,
            Phase::AwaitingReview => runner.review(true).await.unwrap(),
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::Complete);
    let complete = &runner.task.events.last().unwrap().message;
    assert!(
        complete.ends_with("Approved spec spec.md: 2 of 3 rule(s) addressed by recorded decisions; 1 open: Labels are localized."),
        "{complete}"
    );
}

/// A plan returned for an unmentioned rule names the rule by its own kind.
#[tokio::test]
async fn a_returned_plan_names_each_rule_by_its_kind() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![GoverningRule {
        iri: PRESERVE.into(),
        label: "Preserve display label behavior".into(),
        kind: "Requirement".into(),
        claim: "hasDescription: Display labels render exactly as before.\n".into(),
        via: "via: linked to labels.py".into(),
        decided_by: Vec::new(),
    }];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Add a helper","files":["labels.py"],"checks":["true"],"addresses":[]}));
    runner.advance().await.unwrap();
    assert!(runner.task.plan.is_none(), "the plan was returned");
    assert!(
        runner.task.last_response.contains(&format!(
            "[Requirement] Preserve display label behavior ({PRESERVE})"
        )),
        "{}",
        runner.task.last_response
    );
}

/// What the project and the human pushed back with reaches typing as
/// numbered support events: a failed command and a human steer after
/// approval. The harness's own mechanics (a repair, a scope escape) and a
/// successful command do not.
#[tokio::test]
async fn capture_typing_receives_the_tasks_failures_and_corrections() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    for message in [
        "Command: true\nPermission grants: none\nSuccess: false\nlabel kept its padding",
        "Correcting harness_action, attempt 2 of 3: model output failed validation",
        "Scope escape: the model proposed an edit to x.py outside the plan files [labels.py]; replanning (1 of 3).",
        "Human response: strip only trailing whitespace",
        "Command: ls\nPermission grants: none\nSuccess: true\nSuccess: false printed by the tool",
    ] {
        runner.task.events.push(moosedev::harness::runner::Event {
            message: message.into(),
        });
    }
    let first = runner.task.events.len() - 5;
    fixture.note("Normalization lives in one helper.");
    runner.advance().await.unwrap();
    let request = fixture
        .shared
        .lock()
        .unwrap()
        .capture_type_requests
        .last()
        .cloned()
        .expect("the note was typed");
    let support: Vec<(usize, &str, &str)> = request
        .support_events
        .iter()
        .map(|event| (event.event, event.kind.as_str(), event.summary.as_str()))
        .collect();
    assert_eq!(
        support,
        vec![
            (
                first,
                "command_failed",
                "Command: true | Permission grants: none | Success: false | label kept its padding"
            ),
            (
                first + 3,
                "human_steer",
                "Human response: strip only trailing whitespace"
            ),
        ]
    );
}

/// `/drop` leaves one numbered proposal out of the capture the human
/// accepts: the review carries it as a rejected entry and the journal names
/// it.
#[tokio::test]
async fn a_dropped_proposal_is_rejected_within_the_accepted_capture() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    fixture.typed(vec![
        distinct_proposal("ArchitecturalDecision", "One normalize helper"),
        distinct_proposal("Lesson", "Resolver two is critical"),
    ]);
    fixture.note("Normalization lives in one helper.");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.set_proposal_dropped(None, 3, true).is_err());
    assert_eq!(
        runner.set_proposal_dropped(None, 2, true).unwrap(),
        "Resolver two is critical"
    );
    runner.review(true).await.unwrap();
    let review = fixture
        .shared
        .lock()
        .unwrap()
        .review_requests
        .last()
        .cloned()
        .unwrap();
    assert!(review.accept);
    assert_eq!(review.rejected, vec![1]);
    assert!(runner.task.review_drops.is_empty());
    assert!(runner.task.events.iter().any(|event| event
        .message
        .starts_with("Human accepted captured knowledge; dropped: Resolver two is critical.")));
    assert!(intent_details(&runner, "record_review")
        .iter()
        .any(|detail| detail.starts_with("rejected ")));
}

/// An exact repeat of a command whose output cannot have changed is not run:
/// the model is pointed at the earlier result, and a second repeat parks the
/// task for the human.
#[tokio::test]
async fn an_unchanged_repeat_of_a_command_is_refused_then_parks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let runs = |runner: &Runner| {
        runner
            .task
            .events
            .iter()
            .filter(|event| event.message.starts_with("Command: ls labels.py\n"))
            .count()
    };
    async fn step(fixture: &Fixture, runner: &mut Runner) {
        fixture.conversational(json!({"action":"command","command":"ls labels.py"}));
        for _ in 0..3 {
            if fixture.shared.lock().unwrap().replies.is_empty() {
                return;
            }
            runner.advance().await.unwrap();
        }
    }
    step(&fixture, &mut runner).await;
    assert_eq!(runs(&runner), 1);
    step(&fixture, &mut runner).await;
    assert_eq!(runs(&runner), 1, "the repeat did not run");
    assert!(runner
        .task
        .last_response
        .starts_with("Not run: this exact command ran at event"));
    assert_eq!(intent_details(&runner, "command_repeat_refused").len(), 1);
    step(&fixture, &mut runner).await;
    assert_eq!(runs(&runner), 1);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("Guidance is needed"));
}

/// A reply marked `then: continue` says the model is about to act: the turn
/// continues once, so the plan follows without the human saying "ok"; a
/// second such reply before the human speaks hands the turn back. A reply
/// that waits (the default) is an answer and ends the turn.
#[tokio::test]
async fn a_reply_that_continues_asks_for_the_next_action_once() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"I have read the file. I will now add the helper.","then":"continue"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning, "the turn continues");
    assert_eq!(intent_details(&runner, "reply_continued").len(), 1);
    assert!(runner.task.last_response.contains(
        "(Continuing: you said you are about to act, so take that action now; in Plan mode the next action is plan.)"
    ));
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["true"],"addresses":[]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);

    // Continuing twice in a row gives the turn back.
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(
        json!({"action":"reply","message":"I will begin by adding the helper.","then":"continue"}),
    );
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"reply","message":"Starting with labels.py.","then":"continue"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "reply_continued").len(), 1);

    // A reply that waits is an answer.
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(
        json!({"action":"reply","message":"It returns the name unchanged.","then":"wait"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(intent_details(&runner, "reply_continued").is_empty());
}

/// Apply `edit` in an approved symbolic task, then give it the clean, settled
/// language-server result the harness arms auto-verify on. The fixture has no
/// language server, so the arm is set as `arm_auto_verify` would set it.
async fn clean_edit(fixture: &Fixture, runner: &mut Runner, edit: Value) {
    fixture.conversational(edit);
    let calls = fixture.model_calls();
    while fixture.model_calls() == calls {
        runner.advance().await.unwrap();
    }
    runner.task.diagnostics = Some(diagnostics(0));
    let at = runner.task.edits.len();
    runner.task.symbolic.as_mut().unwrap().auto_verify_armed = Some(at);
}

/// Advance while the task works without calling the model; true when it left
/// Working that way. A queued reply answers if the model is asked.
async fn leaves_working_without_the_model(fixture: &Fixture, runner: &mut Runner) -> bool {
    fixture.conversational(json!({"action":"reply","message":"Working.","then":"wait"}));
    let calls = fixture.model_calls();
    for _ in 0..6 {
        if runner.task.phase != Phase::Working || fixture.model_calls() > calls {
            break;
        }
        runner.advance().await.unwrap();
    }
    fixture.model_calls() == calls && runner.task.phase != Phase::Working
}

#[tokio::test]
async fn a_clean_edit_covering_every_planned_file_runs_the_checks_without_a_model_call() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    assert!(leaves_working_without_the_model(&fixture, &mut runner).await);
    assert_eq!(intent_details(&runner, "auto_verify").len(), 1);
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.starts_with("All planned files are edited")));
    // Auto-verify fires only on finished work, so the unfinished-plan gate
    // never sends it back.
    assert!(intent_details(&runner, "finish_refused_unfinished").is_empty());
}

#[tokio::test]
async fn auto_verify_waits_for_every_planned_file_and_for_a_clean_result() {
    let _env_lock = ENVIRONMENT.lock().await;
    // A planned file not written yet: the model keeps working.
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper module","files":["labels.py","codes.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    assert!(!leaves_working_without_the_model(&fixture, &mut runner).await);
    assert!(intent_details(&runner, "auto_verify").is_empty());

    // Any finding, or a stub left in a planned file: the model keeps working.
    for (after, findings) in [
        ("    return name.strip()\n", 1),
        ("    raise NotImplementedError\n", 0),
    ] {
        let fixture = symbolic_fixture().await;
        let mut runner = planned_symbolic_runner(&fixture).await;
        runner.approve_plan().await.unwrap();
        clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":after})).await;
        runner.task.diagnostics = Some(diagnostics(findings));
        assert!(
            !leaves_working_without_the_model(&fixture, &mut runner).await,
            "{after}"
        );
        assert!(intent_details(&runner, "auto_verify").is_empty());
    }
}

#[tokio::test]
async fn a_failed_auto_verify_says_so_and_does_not_rerun_on_the_same_source() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.task.plan.as_mut().unwrap().checks = vec!["false".into()];
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    assert!(leaves_working_without_the_model(&fixture, &mut runner).await);
    while runner.task.phase == Phase::Verifying {
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(
        runner
            .task
            .last_response
            .starts_with("The harness ran the plan's required checks"),
        "{}",
        runner.task.last_response
    );
    // The arm was taken: the next step is the model's.
    assert!(!leaves_working_without_the_model(&fixture, &mut runner).await);
    assert_eq!(intent_details(&runner, "auto_verify").len(), 1);
}

#[tokio::test]
async fn auto_verify_is_bounded_disarmed_by_the_human_and_can_be_switched_off() {
    let _env_lock = ENVIRONMENT.lock().await;
    // At the limit: journaled once, and the model finishes.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    runner.task.symbolic.as_mut().unwrap().auto_verifications = 3;
    assert!(!leaves_working_without_the_model(&fixture, &mut runner).await);
    assert_eq!(intent_details(&runner, "auto_verify_exhausted").len(), 1);

    // A human message takes the arm: the next word is the model's.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    runner
        .submit_message("Keep the helper private.".into())
        .await
        .unwrap();
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().auto_verify_armed,
        None
    );

    // Switched off for a study variant.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    std::env::set_var("MOOSEDEV_HARNESS_AUTO_VERIFY", "off");
    let fired = leaves_working_without_the_model(&fixture, &mut runner).await;
    std::env::remove_var("MOOSEDEV_HARNESS_AUTO_VERIFY");
    assert!(!fired);
    assert!(intent_details(&runner, "auto_verify").is_empty());
}

#[tokio::test]
async fn a_new_approval_drops_the_arm_and_counts_only_its_own_edits() {
    // codex: an arm or an edit from before a re-approval must not let the
    // harness run the checks under the new approval.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    // The source changes outside the harness: the approval is withdrawn.
    std::fs::write(
        fixture.root.join("labels.py"),
        "def render_name(name):\n    return name.lower()\n",
    )
    .unwrap();
    for _ in 0..4 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    let state = runner.task.symbolic.as_ref().unwrap();
    assert_eq!(state.auto_verify_armed, None);
    runner.approve_plan().await.unwrap();
    let state = runner.task.symbolic.as_ref().unwrap();
    assert_eq!(state.auto_verify_armed, None);
    assert_eq!(state.cycle_edit_start, runner.task.edits.len());
    // With no edit under this approval, a stray arm would not fire either.
    let at = runner.task.edits.len();
    runner.task.symbolic.as_mut().unwrap().auto_verify_armed = Some(at);
    runner.task.diagnostics = Some(diagnostics(0));
    assert!(!leaves_working_without_the_model(&fixture, &mut runner).await);
    assert!(intent_details(&runner, "auto_verify").is_empty());
}

/// An approved plan over labels.py whose action repair parked early: the
/// model sent the same rejected replace twice (badciv P5).
async fn repeat_parked_runner(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    let miss =
        json!({"action":"replace","file":"labels.py","old_text":"no such text","new_text":"x"});
    fixture.conversational(miss.clone());
    fixture.conversational(miss);
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "repair_repeat_parked").len(), 1);
    assert!(runner.task.plan_stands_park);
    runner
}

/// Advance until the model is asked once (checkpoints journal first).
async fn until_model_called(fixture: &Fixture, runner: &mut Runner) {
    let calls = fixture.model_calls();
    for _ in 0..6 {
        if fixture.model_calls() > calls {
            return;
        }
        runner.advance().await.unwrap();
    }
    panic!("the model was never asked");
}

#[tokio::test]
async fn a_hint_answering_a_repeat_park_continues_the_approved_plan() {
    // badciv P5: "The enums are defined in codes.rs…" answered a repeat park
    // and cost a ~5-minute replan and a plan approval, twice.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message("The function body in labels.py is `return name`; replace that.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.recovery.is_none(), "a fresh repair budget");
    assert!(!runner.task.plan_stands_park);
    assert_eq!(
        intent_details(&runner, "message_disposition"),
        vec!["unclear"]
    );
    // The approval stands: the next edit applies without a new plan.
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}));
    until_model_called(&fixture, &mut runner).await;
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(intent_details(&runner, "plan_approved").len() == 1);
}

/// A park answer explains with negations ("does not re-export"), which
/// must not read as stopping; turning words still replan.
#[tokio::test]
async fn a_negated_hint_continues_a_park_but_a_turning_word_replans() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message("labels.py does not strip the name; don't change anything else.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);

    // A quoted hint (`▎`, as a terminal pastes one) that negates mid-sentence
    // still explains.
    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message(
            "▎ The render_name body is in labels.py, and labels.py does not strip the name.".into(),
        )
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);

    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message("Stop and lowercase it instead.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        intent_details(&runner, "message_disposition"),
        vec!["replan: your message says \"stop\", which may change or stop the approved work."]
    );

    // An answer that opens with a refusal refuses the work (it used to
    // continue: neither "not" nor "wait" turned a park answer).
    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message("Do not make this change; wait.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        intent_details(&runner, "message_disposition"),
        vec!["replan: your message says \"do not\", which may change or stop the approved work."]
    );
}

#[tokio::test]
async fn a_park_answer_naming_a_file_outside_the_plan_replans() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = repeat_parked_runner(&fixture).await;
    runner
        .submit_message("Put the helper in src/other.py.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(!runner.task.plan_stands_park);
    assert_eq!(
        intent_details(&runner, "message_disposition"),
        vec!["replan: your message names src/other.py, which the approved plan does not cover."]
    );
}

/// Keep the automatic scope-escape replan for a test about what follows it;
/// the switch is cleared on drop, even when the test fails.
struct ScopeChoiceOff;

impl ScopeChoiceOff {
    fn new() -> Self {
        std::env::set_var("MOOSEDEV_HARNESS_SCOPE_CHOICE", "off");
        Self
    }
}

impl Drop for ScopeChoiceOff {
    fn drop(&mut self) {
        std::env::remove_var("MOOSEDEV_HARNESS_SCOPE_CHOICE");
    }
}

/// A park that questions the plan is not judged: the answer replans.
#[tokio::test]
async fn answering_a_scope_exhausted_park_still_replans() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _scope_choice = ScopeChoiceOff::new();
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().scope_escapes = 3;
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(!runner.task.plan_stands_park);
    runner.submit_message("continue".into()).await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(intent_details(&runner, "message_disposition").is_empty());
}

/// Headless `answer` and a message in the conversation take the same path:
/// a park answer continues the plan in both, and a scope-exhausted park's
/// answer replans in both (headless `answer` used to continue it).
#[tokio::test]
async fn headless_answer_and_a_conversation_message_agree() {
    use moosedev::harness::tui::{execute, Action};
    let _env_lock = ENVIRONMENT.lock().await;
    let _scope_choice = ScopeChoiceOff::new();
    let hint = "The body is `return name`; replace exactly that.";
    let mut outcomes = Vec::new();
    for headless in [false, true] {
        let fixture = symbolic_fixture().await;
        let mut runner = repeat_parked_runner(&fixture).await;
        if headless {
            execute(&mut runner, Action::Answer(hint.into()))
                .await
                .unwrap();
        } else {
            runner.submit_message(hint.into()).await.unwrap();
        }
        outcomes.push((
            runner.task.mode,
            runner.task.phase,
            intent_details(&runner, "message_disposition"),
        ));
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0].1, Phase::Working);

    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().scope_escapes = 3;
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    execute(&mut runner, Action::Answer("continue".into()))
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    // Headless answer still needs something to answer.
    assert!(execute(&mut runner, Action::Answer("continue".into()))
        .await
        .is_err());
}

#[tokio::test]
async fn answering_a_stall_park_or_a_read_repeat_park_continues_the_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = stalled_failure_runner(&fixture).await;
    for variant in ["a", "b", "c", "d"] {
        act(&fixture, &mut runner, failing_test_command(variant)).await;
    }
    assert_eq!(intent_details(&runner, "stalled_failure_parked").len(), 1);
    assert!(runner.task.plan_stands_park);
    runner
        .submit_message("Strip the name before returning it.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);

    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    for _ in 0..2 {
        act(
            &fixture,
            &mut runner,
            json!({"action":"read","file":"labels.py"}),
        )
        .await;
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("Guidance is needed"));
    assert!(runner.task.plan_stands_park);
    runner.submit_message("go ahead".into()).await.unwrap();
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        intent_details(&runner, "message_disposition"),
        vec!["continue"]
    );
}

/// A reply may continue the turn again once the model made progress since the
/// last continued one; with nothing done in between it hands the turn back.
#[tokio::test]
async fn a_reply_after_an_edit_continues_again() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let reply = json!({"action":"reply","message":"Now the next change."});
    act(&fixture, &mut runner, reply.clone()).await;
    assert_eq!(intent_details(&runner, "reply_continued").len(), 1);
    act(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    assert_eq!(runner.task.edits.len(), 1);
    act(&fixture, &mut runner, reply.clone()).await;
    assert_eq!(intent_details(&runner, "reply_continued").len(), 2);
    assert_eq!(runner.task.phase, Phase::Working);
    act(&fixture, &mut runner, reply).await;
    assert_eq!(intent_details(&runner, "reply_continued").len(), 2);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
}

/// badciv P5: a scope-escape replan that only added a file reset the cycle,
/// so auto-verify waited for every planned file to be edited again.
#[tokio::test]
async fn an_additive_reapproval_keeps_the_edits_made_under_the_earlier_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _scope_choice = ScopeChoiceOff::new();
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    act(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    assert_eq!(runner.task.edits.len(), 1);
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    )
    .await;
    assert_eq!(runner.task.mode, Mode::Plan);
    act(&fixture, &mut runner, json!({"action":"plan","summary":"Preserve display behavior and update the constant","files":["labels.py","other.py"],"checks":["true"]})).await;
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    runner.approve_plan().await.unwrap();
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().cycle_edit_start,
        0,
        "the earlier plan's edit still counts"
    );
    act(
        &fixture,
        &mut runner,
        json!({"action":"read","file":"other.py"}),
    )
    .await;
    clean_edit(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    )
    .await;
    assert!(leaves_working_without_the_model(&fixture, &mut runner).await);
    assert_eq!(intent_details(&runner, "auto_verify").len(), 1);
}

#[tokio::test]
async fn a_reapproval_that_drops_a_file_counts_afresh() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    act(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    runner.mode_plan().await.unwrap();
    act(&fixture, &mut runner, json!({"action":"plan","summary":"Preserve display behavior and update the constant","files":["other.py"],"checks":["true"]})).await;
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    runner.approve_plan().await.unwrap();
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().cycle_edit_start,
        runner.task.edits.len()
    );
}

/// A replan the human asked for (`/plan`) re-approves different work even
/// over the same files: only the very same plan (files and summary) keeps
/// the edits made under the earlier approval.
#[tokio::test]
async fn a_human_replan_over_the_same_files_keeps_coverage_unless_switched_off() {
    // badciv run 17: a replan that only corrected a check reset coverage, and
    // the unfinished-plan gate then refused 8 finishes until every planned
    // file was edited again. A plan that keeps the earlier plan's files keeps
    // the edits made to them; switched off, only the same plan again does.
    let _env_lock = ENVIRONMENT.lock().await;
    for (summary, off, kept) in [
        ("Preserve display behavior and strip each name", false, true),
        (
            "Preserve display behavior while adding a helper",
            false,
            true,
        ),
        ("Preserve display behavior and strip each name", true, false),
        (
            "Preserve display behavior while adding a helper",
            true,
            true,
        ),
    ] {
        if off {
            std::env::set_var("MOOSEDEV_HARNESS_KEEP_COVERAGE", "off");
        } else {
            std::env::remove_var("MOOSEDEV_HARNESS_KEEP_COVERAGE");
        }
        let fixture = symbolic_fixture().await;
        let mut runner = planned_symbolic_runner(&fixture).await;
        runner.approve_plan().await.unwrap();
        act(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
        assert_eq!(runner.task.edits.len(), 1);
        runner.mode_plan().await.unwrap();
        act(
            &fixture,
            &mut runner,
            json!({"action":"plan","summary":summary,"files":["labels.py"],"checks":["true"]}),
        )
        .await;
        for _ in 0..3 {
            if runner.task.phase == Phase::AwaitingPlan {
                break;
            }
            runner.advance().await.unwrap();
        }
        runner.approve_plan().await.unwrap();
        let start = runner.task.symbolic.as_ref().unwrap().cycle_edit_start;
        assert_eq!(start, if kept { 0 } else { 1 }, "{summary} (off: {off})");
    }
    std::env::remove_var("MOOSEDEV_HARNESS_KEEP_COVERAGE");
}

/// An approved task that edited labels.py to a stub, passed its checks and
/// captured one proposal: the final review badciv P5 could only reject.
async fn at_final_review(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    raise NotImplementedError\n"}));
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    fixture.note("Rendering is stubbed for now.");
    fixture.typed_one("Lesson", "Rendering is stubbed");
    for _ in 0..16 {
        if runner.task.at_final_review() {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert!(runner.task.at_final_review());
    assert_eq!(runner.task.reviews.len(), 1);
    runner
}

#[tokio::test]
async fn rework_at_the_final_review_returns_to_work_and_captures_again() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = at_final_review(&fixture).await;
    let rejected = runner.task.reviews[0].request.operation_id.clone();
    let notes = fixture.note_calls();
    runner
        .rework("render_name is still a stub; implement it.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(!runner.task.at_final_review());
    assert!(runner.task.reviews.is_empty());
    assert!(runner.task.check_results.is_empty());
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .is_none());
    assert_eq!(
        intent_details(&runner, "review_rework"),
        vec!["render_name is still a stub; implement it."]
    );
    assert_eq!(
        intent_details(&runner, "review_interaction"),
        vec![format!("rejected batch [{rejected}]")]
    );
    assert!(intent_details(&runner, "record_review")
        .iter()
        .all(|detail| detail.starts_with("rejected ")));
    assert!(runner
        .task
        .last_response
        .starts_with("The human sent the work back at review:"));
    // The work goes on under the same approval; the next finish verifies and
    // asks for the final note again.
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    raise NotImplementedError\n","new_text":"    return name.strip()\n"}));
    fixture.conversational(json!({"action":"finish","summary":"Implemented."}));
    fixture.note("render_name strips surrounding whitespace.");
    fixture.typed_one("Lesson", "Names are stripped");
    for _ in 0..16 {
        if runner.task.at_final_review() {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(fixture.note_calls(), notes + 1, "a second note was asked");
    assert_eq!(intent_details(&runner, "plan_approved").len(), 1);
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
}

/// A rework note that refuses the work is judged as a park answer is: it
/// rejects the capture and returns the task to Plan with the note as
/// guidance, as a steering message does.
#[tokio::test]
async fn a_rework_note_that_changes_the_plan_returns_to_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = at_final_review(&fixture).await;
    let rejected = runner.task.reviews[0].request.operation_id.clone();
    let note = "Do not keep the stub; render names through a lookup table.";
    runner.rework(note.into()).await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(!runner.task.at_final_review());
    assert!(runner.task.reviews.is_empty());
    assert!(journal_value(&runner)["approved_revision"].is_null());
    assert_eq!(journal_value(&runner)["guidance"], note);
    assert_eq!(runner.task.last_response, note);
    assert_eq!(
        intent_details(&runner, "review_rework"),
        vec!["replan: your message says \"do not\", which may change or stop the approved work."]
    );
    assert_eq!(
        intent_details(&runner, "review_interaction"),
        vec![format!("rejected batch [{rejected}]")]
    );
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .is_none());
    // The next plan is a plan under review, approved again.
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior through a lookup table","files":["labels.py"],"checks":["true"]}));
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
}

#[tokio::test]
async fn rework_is_refused_outside_the_final_review() {
    use moosedev::harness::tui::{execute, Action};
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    assert!(runner.rework("more".into()).await.is_err());
    runner.approve_plan().await.unwrap();
    let error = execute(&mut runner, Action::Rework("more".into()))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("final capture review"),
        "{error}"
    );
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(intent_details(&runner, "review_rework").is_empty());
}

/// A plan over labels.py and a codes.py not written yet, approved, with a
/// settled language-server error: the state badciv c83c10f8 was in.
async fn missing_module_runner(fixture: &Fixture) -> Runner {
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper module","files":["labels.py","codes.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    let mut snapshot = diagnostics(1);
    snapshot.errors[0].file = "labels.py".into();
    runner.task.diagnostics = Some(snapshot);
    runner
}

#[tokio::test]
async fn a_repeated_noop_narrows_the_offer_to_the_missing_files() {
    // badciv c83c10f8: a4b sent the identical whole-file write of lib.rs until
    // the repairs ran out; the correction line alone could not change it.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = missing_module_runner(&fixture).await;
    let noop = json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"});
    fixture.conversational(noop.clone());
    fixture.conversational(noop);
    fixture.conversational(json!({"action":"write","file":"codes.py","content":"CODES = {}\n"}));
    runner.advance().await.unwrap();
    assert_eq!(intent_details(&runner, "repair_narrowed"), vec!["codes.py"]);
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("Allowed actions now: read, write (only to codes.py), question."));
    assert_eq!(runner.task.edits.len(), 1, "the narrowed write applied");
    assert_eq!(runner.task.edits[0].file, "codes.py");
    assert!(
        runner.task.recovery.is_none(),
        "an accepted candidate clears the narrowing"
    );
}

#[tokio::test]
async fn a_narrowed_repair_refuses_a_write_elsewhere_and_other_repeats_park_early() {
    let _env_lock = ENVIRONMENT.lock().await;
    // Narrowed, then a write to a file outside the set is refused.
    let fixture = symbolic_fixture().await;
    let mut runner = missing_module_runner(&fixture).await;
    let noop = json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"});
    fixture.conversational(noop.clone());
    fixture.conversational(noop);
    fixture.conversational(json!({"action":"write","file":"labels.py","content":"x = 1\n"}));
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("only for the planned files that do not exist yet: codes.py"),
        "{error}"
    );
    assert!(runner.task.edits.is_empty());
    // Returning to Plan is guidance: the spent budget and its narrowing go,
    // or the first plan would be refused.
    assert!(runner.task.recovery.is_some());
    runner.mode_plan().await.unwrap();
    assert!(runner.task.recovery.is_none());

    // The same rejected candidate of another kind: stop spending attempts.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let miss =
        json!({"action":"replace","file":"labels.py","old_text":"no such text","new_text":"x"});
    fixture.conversational(miss.clone());
    fixture.conversational(miss);
    let calls = fixture.model_calls();
    assert!(runner.advance().await.is_err());
    assert_eq!(
        fixture.model_calls(),
        calls + 2,
        "parked after two identical rejections, not three"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "repair_repeat_parked").len(), 1);
    assert!(runner
        .task
        .last_response
        .contains("repeated the same rejected candidate"));

    // Switched off: the old behaviour, three attempts.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    for _ in 0..3 {
        fixture.conversational(
            json!({"action":"replace","file":"labels.py","old_text":"no such text","new_text":"x"}),
        );
    }
    std::env::set_var("MOOSEDEV_HARNESS_NARROW_REPAIR", "off");
    let calls = fixture.model_calls();
    let result = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_NARROW_REPAIR");
    assert!(result.is_err());
    assert_eq!(fixture.model_calls(), calls + 3);
    assert!(intent_details(&runner, "repair_repeat_parked").is_empty());
}

/// Give `file` a settled lint with one preferred fix that turns `from` into
/// `to`, offered against the file as it is now, and arm auto-fix the way a
/// fresh language-server result would.
fn offer_preferred_fix(fixture: &Fixture, runner: &mut Runner, file: &str, from: &str, to: &str) {
    let text = std::fs::read_to_string(fixture.root.join(file)).unwrap();
    let start = text.find(from).unwrap();
    let fix = moosedev::harness::runner::OfferedFix {
        id: 1,
        title: format!("replace `{from}` with `{to}`"),
        file: file.into(),
        base: moosedev::harness::digest::sha256_hex(&text),
        edits: vec![moosedev::harness::runner::FixEdit {
            start,
            end: start + from.len(),
            text: to.into(),
        }],
        preferred: true,
    };
    let mut snapshot = diagnostics(0);
    snapshot.lints = vec![moosedev::harness::runner::Finding {
        file: file.into(),
        line: 2,
        column: 5,
        message: "a lint with a machine-applicable suggestion".into(),
        detail: None,
        definition: None,
        declared: Vec::new(),
        fixes: vec![fix],
        fixes_complete: true,
    }];
    runner.task.diagnostics = Some(snapshot);
    let at = runner.task.edits.len();
    runner.task.symbolic.as_mut().unwrap().auto_fix_armed = Some(at);
}

/// Advance while the harness works without the model, at most six times.
async fn advance_without_the_model(fixture: &Fixture, runner: &mut Runner) {
    fixture.conversational(json!({"action":"reply","message":"Working.","then":"wait"}));
    let calls = fixture.model_calls();
    for _ in 0..6 {
        if fixture.model_calls() > calls || runner.task.phase != Phase::Working {
            break;
        }
        runner.advance().await.unwrap();
    }
}

#[tokio::test]
async fn the_harness_applies_the_one_preferred_fix_without_a_model_step() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    offer_preferred_fix(&fixture, &mut runner, "labels.py", "strip", "lower");
    let calls = fixture.model_calls();
    for _ in 0..4 {
        if runner.task.edits.len() == 2 {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(fixture.model_calls(), calls, "no model step");
    assert_eq!(runner.task.edits.len(), 2);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "def render_name(name):\n    return name.lower()\n"
    );
    assert_eq!(intent_details(&runner, "fix_auto_applied").len(), 1);
    assert!(runner
        .task
        .last_response
        .starts_with("The harness applied the language server's preferred fix to labels.py"));
}

#[tokio::test]
async fn an_auto_fix_is_held_capped_disarmed_and_switched_off() {
    let _env_lock = ENVIRONMENT.lock().await;
    let edit = json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"});
    // The file changed after the fix was offered: held, not applied.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, edit.clone()).await;
    offer_preferred_fix(&fixture, &mut runner, "labels.py", "strip", "lower");
    // A change on disk withdraws the approval first; a stale offer is the
    // case left for the fix itself to refuse.
    runner.task.diagnostics.as_mut().unwrap().lints[0].fixes[0].base = "stale".into();
    advance_without_the_model(&fixture, &mut runner).await;
    assert!(intent_details(&runner, "fix_auto_applied").is_empty());
    assert_eq!(
        intent_details(&runner, "fix_auto_held"),
        vec!["labels.py: the file changed since the fix was offered"]
    );

    // At the chain cap, after a human message, or switched off: not applied.
    for case in ["cap", "human", "off"] {
        let fixture = symbolic_fixture().await;
        let mut runner = planned_symbolic_runner(&fixture).await;
        runner.approve_plan().await.unwrap();
        clean_edit(&fixture, &mut runner, edit.clone()).await;
        offer_preferred_fix(&fixture, &mut runner, "labels.py", "strip", "lower");
        match case {
            "cap" => runner.task.symbolic.as_mut().unwrap().auto_fix_chain = 3,
            "human" => runner.submit_message("Keep going.".into()).await.unwrap(),
            _ => std::env::set_var("MOOSEDEV_HARNESS_AUTO_FIX", "off"),
        }
        advance_without_the_model(&fixture, &mut runner).await;
        std::env::remove_var("MOOSEDEV_HARNESS_AUTO_FIX");
        assert!(
            intent_details(&runner, "fix_auto_applied").is_empty(),
            "{case}"
        );
        assert_eq!(runner.task.edits.len(), 1, "{case}");
    }
}

/// A fix is held when it adds a panicking call to the file (rustc's
/// preferred `.try_into().unwrap()` conversion, badciv P5), judged against
/// the file's text: one that rewrites a line keeping its `.unwrap()` is
/// applied.
#[tokio::test]
async fn an_auto_fix_is_held_only_when_it_adds_a_panicking_call() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    offer_preferred_fix(
        &fixture,
        &mut runner,
        "labels.py",
        "strip()",
        "strip().unwrap()",
    );
    advance_without_the_model(&fixture, &mut runner).await;
    assert!(intent_details(&runner, "fix_auto_applied").is_empty());
    assert_eq!(
        intent_details(&runner, "fix_auto_held"),
        vec!["labels.py: the fix adds a panicking call"]
    );
    assert_eq!(runner.task.edits.len(), 1);

    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip().unwrap()\n"})).await;
    offer_preferred_fix(
        &fixture,
        &mut runner,
        "labels.py",
        "strip().unwrap()",
        "lower().unwrap()",
    );
    for _ in 0..4 {
        if runner.task.edits.len() == 2 {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(intent_details(&runner, "fix_auto_applied").len(), 1);
    assert!(intent_details(&runner, "fix_auto_held").is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "def render_name(name):\n    return name.lower().unwrap()\n"
    );
}

#[tokio::test]
async fn a_harness_fix_is_not_the_models_planned_edit_for_auto_verify() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(fixture.root.join("codes.py"), "CODES = {'a': 1}\n").unwrap();
    let mut runner = fixture.interactive().await;
    for file in ["labels.py", "codes.py"] {
        fixture.conversational(json!({"action":"read","file":file}));
        runner.advance().await.unwrap();
    }
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper module","files":["labels.py","codes.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    clean_edit(&fixture, &mut runner, json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"})).await;
    offer_preferred_fix(&fixture, &mut runner, "codes.py", "1", "2");
    for _ in 0..4 {
        if runner.task.edits.len() == 2 {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(intent_details(&runner, "fix_auto_applied").len(), 1);
    // Clean again, armed: codes.py was touched only by the harness.
    runner.task.diagnostics = Some(diagnostics(0));
    let at = runner.task.edits.len();
    runner.task.symbolic.as_mut().unwrap().auto_verify_armed = Some(at);
    assert!(!leaves_working_without_the_model(&fixture, &mut runner).await);
    assert!(intent_details(&runner, "auto_verify").is_empty());
}

#[tokio::test]
async fn the_review_shows_what_the_harness_checked_beside_the_note() {
    // badciv be128e71: a stubbed finish with only an ignored test, and a note
    // describing code that did not exist, was accepted at review.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    raise NotImplementedError\n"}));
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    fixture.conversational(json!({"action":"finish","summary":"Done."}));
    fixture
        .note("I implemented `validate_labels` as a separate pass that check_grid(labels) calls.");
    fixture.typed(vec![]);
    for _ in 0..16 {
        if runner.task.phase == Phase::AwaitingReview {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let evidence = runner
        .task
        .symbolic
        .as_ref()
        .and_then(|state| state.capture_note.as_ref())
        .map(|note| note.evidence.clone())
        .unwrap();
    assert!(
        evidence
            .iter()
            .any(|fact| fact.starts_with("Required checks passed, but no test passed")),
        "{evidence:?}"
    );
    assert!(
        evidence
            .iter()
            .any(|fact| fact
                == "Stubs left in planned files: labels.py:2 raise NotImplementedError."),
        "{evidence:?}"
    );
    assert!(
        evidence.iter().any(|fact| fact == "The note names `validate_labels`, `check_grid`, which no edit in this task added or changed."),
        "{evidence:?}"
    );
    assert_eq!(
        intent_details(&runner, "review_evidence").len(),
        evidence.len()
    );
}

/// A command that fails `test_render` as libtest reports it, made distinct by
/// a trailing comment so the exact-repeat guard never fires: badciv run 12
/// varied `| tail -30`, `| tail -40` the same way.
fn failing_test_command(variant: &str) -> Value {
    json!({"action":"command","command":format!(
        "printf \"test test_render ... FAILED\\nthread 'test_render' panicked at tests/test_labels.py:2:5:\\n\"; exit 101 # {variant}"
    )})
}

/// The symbolic fixture with a test of `render_name` that the failing
/// command's panic line points into.
async fn stalled_failure_runner(fixture: &Fixture) -> Runner {
    std::fs::create_dir_all(fixture.root.join("tests")).unwrap();
    std::fs::write(
        fixture.root.join("tests/test_labels.py"),
        "def test_render():\n    assert render_name(\" a \") == \"a\"\n",
    )
    .unwrap();
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    runner
}

async fn act(fixture: &Fixture, runner: &mut Runner, action: Value) {
    fixture.conversational(action);
    for _ in 0..4 {
        if fixture.shared.lock().unwrap().replies.is_empty() {
            return;
        }
        runner.advance().await.unwrap();
    }
    panic!("the scripted action was never requested");
}

fn stall_count(runner: &Runner) -> Option<usize> {
    runner
        .task
        .symbolic
        .as_ref()
        .and_then(|state| state.stalled_failure.as_ref())
        .map(|stall| stall.count)
}

/// badciv run 12: `grid_too_few_rows` failed again and again with no edit
/// between while qwen reread and paged. The second failure shows the test and
/// what it calls, before the output; the fourth parks. A passing command in
/// between says nothing about the failure.
#[tokio::test]
async fn the_same_failure_with_no_edit_between_is_focused_then_parks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = stalled_failure_runner(&fixture).await;
    act(&fixture, &mut runner, failing_test_command("a")).await;
    assert_eq!(stall_count(&runner), Some(1));
    assert!(runner
        .task
        .last_response
        .starts_with("test test_render ... FAILED"));
    act(
        &fixture,
        &mut runner,
        json!({"action":"command","command":"true"}),
    )
    .await;
    act(&fixture, &mut runner, failing_test_command("b")).await;
    assert_eq!(stall_count(&runner), Some(2));
    assert_eq!(intent_details(&runner, "stalled_failure_focus").len(), 1);
    let response = runner.task.last_response.clone();
    assert!(
        response.starts_with("[Harness: the same failure again with no edit since: test `test_render` (tests/test_labels.py:2). Its source and the code it calls:\n"),
        "{response}"
    );
    assert!(
        response.contains("tests/test_labels.py:1-2 `test_render` (the test):\ndef test_render():\n    assert render_name(\" a \") == \"a\"\n"),
        "{response}"
    );
    assert!(
        response.contains("labels.py:1-2 `render_name` (called by the test):\ndef render_name(name):\n    return name\n"),
        "{response}"
    );
    let (block, output) = response
        .split_once("edit the code it points at.]\n")
        .unwrap();
    assert!(block.len() <= 4_000);
    assert!(
        output.starts_with("test test_render ... FAILED"),
        "{output}"
    );
    assert_eq!(runner.task.phase, Phase::Working);

    act(&fixture, &mut runner, failing_test_command("c")).await;
    assert_eq!(stall_count(&runner), Some(3));
    assert!(runner
        .task
        .last_response
        .starts_with("test test_render ... FAILED"));
    act(&fixture, &mut runner, failing_test_command("d")).await;
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert_eq!(intent_details(&runner, "stalled_failure_parked").len(), 1);
    assert!(
        runner.task.last_response.starts_with(
            "[Harness: the same failure (test `test_render`) has come back 4 times with no edit in between"
        ),
        "{}",
        runner.task.last_response
    );
    assert!(runner.task.last_response.contains("Guidance is needed"));

    // The human's answer starts the count again.
    runner.answer("Strip the name.".into()).await.unwrap();
    assert_eq!(stall_count(&runner), None);
}

/// An applied edit is a new source: the same failure after it is the first
/// sighting again.
#[tokio::test]
async fn an_applied_edit_starts_the_count_again() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = stalled_failure_runner(&fixture).await;
    act(&fixture, &mut runner, failing_test_command("a")).await;
    act(&fixture, &mut runner, failing_test_command("b")).await;
    assert_eq!(intent_details(&runner, "stalled_failure_focus").len(), 1);
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.lower()\n"}),
    )
    .await;
    act(&fixture, &mut runner, failing_test_command("c")).await;
    assert_eq!(stall_count(&runner), Some(1));
    assert!(!runner.task.last_response.starts_with("[Harness:"));
    act(&fixture, &mut runner, failing_test_command("d")).await;
    assert_eq!(stall_count(&runner), Some(2));
    assert_eq!(intent_details(&runner, "stalled_failure_focus").len(), 2);
    // The focus shows the file's current text, not the index's.
    assert!(
        runner
            .task
            .last_response
            .contains("    return name.lower()\n"),
        "{}",
        runner.task.last_response
    );
}

/// A required check that fails the same way as the model's last command,
/// with no edit between, is the second sighting too.
#[tokio::test]
async fn a_required_check_failing_the_same_way_is_focused() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::create_dir_all(fixture.root.join("tests")).unwrap();
    std::fs::write(
        fixture.root.join("tests/test_labels.py"),
        "def test_render():\n    assert render_name(\" a \") == \"a\"\n",
    )
    .unwrap();
    let check = failing_test_command("check")["command"]
        .as_str()
        .unwrap()
        .to_string();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Strip the rendered name","files":["labels.py"],"checks":[check]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    act(&fixture, &mut runner, failing_test_command("a")).await;
    // Nothing is edited: the first finish is sent back once for the unedited
    // planned file, and the repeat asks the human, who says it needs no
    // change.
    for _ in 0..2 {
        act(
            &fixture,
            &mut runner,
            json!({"action":"finish","summary":"Done."}),
        )
        .await;
    }
    verify_unedited(&mut runner).await;
    for _ in 0..6 {
        if runner
            .task
            .check_results
            .last()
            .is_some_and(|run| !run.success)
        {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(intent_details(&runner, "stalled_failure_focus").len(), 1);
    assert!(
        runner
            .task
            .last_response
            .starts_with("[Harness: the same failure again with no edit since: test `test_render`"),
        "{}",
        runner.task.last_response
    );
    assert!(runner
        .task
        .last_response
        .contains("Required verification failed."));
}

/// A required check that fails `test_render` with its values, as libtest
/// reports an `assert_eq!`.
fn failing_check_with_values() -> String {
    "printf \"test test_render ... FAILED\\nthread 'test_render' panicked at tests/test_labels.py:2:5:\\nassertion failed\\n  left: 1\\n right: 2\\n\"; exit 101 # check".to_string()
}

/// An approved plan whose required check fails, its planned file edited and
/// a finish run, so the check has failed once.
async fn failed_required_check(fixture: &Fixture) -> Runner {
    std::fs::create_dir_all(fixture.root.join("tests")).unwrap();
    std::fs::write(
        fixture.root.join("tests/test_labels.py"),
        "def test_render():\n    assert render_name(\" a \") == \"a\"\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Strip the rendered name","files":["labels.py"],"checks":[failing_check_with_values()]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    act(
        fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    for _ in 0..6 {
        if runner
            .task
            .check_results
            .last()
            .is_some_and(|run| !run.success)
        {
            return runner;
        }
        if fixture.shared.lock().unwrap().replies.is_empty() {
            fixture.conversational(json!({"action":"finish","summary":"Done."}));
        }
        runner.advance().await.unwrap();
    }
    panic!("the required check never failed");
}

/// badciv orE, 3 of 6: the model paged a failed required check's output until
/// the inspect guard parked, before any failure came back. A required check
/// is the plan's measure of done, so its first failure shows the failing
/// test and the code it calls.
#[tokio::test]
async fn a_required_checks_first_failure_shows_where_to_look() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let runner = failed_required_check(&fixture).await;
    assert_eq!(stall_count(&runner), Some(1));
    let response = &runner.task.last_response;
    assert!(
        response.starts_with("[Harness: a required check failed: test `test_render` (tests/test_labels.py:2). Its source and the code it calls:\n"),
        "{response}"
    );
    assert!(
        response.contains("`render_name` (called by the test)"),
        "{response}"
    );
    let focus = intent_details(&runner, "stalled_failure_focus");
    assert_eq!(focus.len(), 1);
    assert!(focus[0].contains("first failure"), "{focus:?}");

    // Switched off, the first failure is its output alone.
    let fixture = symbolic_fixture().await;
    std::env::set_var("MOOSEDEV_HARNESS_FOCUS_FIRST", "off");
    let runner = failed_required_check(&fixture).await;
    std::env::remove_var("MOOSEDEV_HARNESS_FOCUS_FIRST");
    assert!(intent_details(&runner, "stalled_failure_focus").is_empty());
}

/// The steer before a park: while a required check fails, a repeated look
/// that would park gets the failing test, its values and its source once;
/// the next one parks. Switched off, the repeat parks at once.
#[tokio::test]
async fn a_look_that_would_park_while_a_check_fails_is_steered_once() {
    let _env_lock = ENVIRONMENT.lock().await;
    let read = json!({"action":"read","file":"labels.py"});
    for steer in [true, false] {
        let fixture = symbolic_fixture().await;
        let mut runner = failed_required_check(&fixture).await;
        if !steer {
            std::env::set_var("MOOSEDEV_HARNESS_STEER", "off");
        }
        act(&fixture, &mut runner, read.clone()).await;
        assert!(
            runner.task.last_response.starts_with("Not read again"),
            "{}",
            runner.task.last_response
        );
        act(&fixture, &mut runner, read.clone()).await;
        if steer {
            assert_eq!(runner.task.phase, Phase::Working);
            let response = runner.task.last_response.clone();
            assert!(
                response.starts_with("[Harness: the required check is still failing, and looking again will not change it: test `test_render` (tests/test_labels.py:2).\nIts values:\nassertion failed\nleft: 1\nright: 2\nIts source and the code it calls:\n"),
                "{response}"
            );
            assert!(
                response.ends_with("a further look parks for the human.]\n"),
                "{response}"
            );
            assert_eq!(intent_details(&runner, "steer_before_park").len(), 1);
            act(&fixture, &mut runner, read.clone()).await;
        }
        std::env::remove_var("MOOSEDEV_HARNESS_STEER");
        assert_eq!(runner.task.phase, Phase::AwaitingInput, "steer {steer}");
        assert!(runner
            .task
            .last_response
            .starts_with("The model keeps asking to read files"));
        assert_eq!(
            intent_details(&runner, "steer_before_park").len(),
            usize::from(steer)
        );
    }
}

/// `MOOSEDEV_HARNESS_LOOP_DETECTOR=off` tracks nothing, shows nothing and
/// never parks, for study variants.
#[tokio::test]
async fn the_loop_detector_can_be_switched_off() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = stalled_failure_runner(&fixture).await;
    std::env::set_var("MOOSEDEV_HARNESS_LOOP_DETECTOR", "off");
    for variant in ["a", "b", "c", "d"] {
        act(&fixture, &mut runner, failing_test_command(variant)).await;
    }
    std::env::remove_var("MOOSEDEV_HARNESS_LOOP_DETECTOR");
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(stall_count(&runner), None);
    assert!(intent_details(&runner, "stalled_failure_focus").is_empty());
    assert!(intent_details(&runner, "stalled_failure_parked").is_empty());
}

/// Long sources are cut to the focus budget on char boundaries, each cut
/// named; the output still follows the block.
#[tokio::test]
async fn the_focus_block_keeps_to_its_budget() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let padding = |name: &str| {
        format!("    # {}\n", "é".repeat(40)).repeat(30) + &format!("    return {name}\n")
    };
    std::fs::write(
        fixture.root.join("labels.py"),
        format!(
            "def render_name(name):\n{}\ndef normalize(value):\n{}\ndef trim(value):\n{}",
            padding("name"),
            padding("value"),
            padding("value")
        ),
    )
    .unwrap();
    std::fs::create_dir_all(fixture.root.join("tests")).unwrap();
    std::fs::write(
        fixture.root.join("tests/test_labels.py"),
        format!(
            "def test_render():\n{}    assert trim(normalize(render_name(\" a \"))) == \"a\"\n",
            "    # é padding line of the test body\n".repeat(60)
        ),
    )
    .unwrap();
    // Written before approval: a change outside the harness withdraws it.
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    act(&fixture, &mut runner, failing_test_command("a")).await;
    act(&fixture, &mut runner, failing_test_command("b")).await;
    let (block, output) = runner
        .task
        .last_response
        .split_once("edit the code it points at.]\n")
        .unwrap();
    assert!(block.len() <= 4_000, "{}", block.len());
    assert!(block.contains("`test_render` continues;"), "{block}");
    for callee in ["trim", "normalize", "render_name"] {
        assert!(
            block.contains(&format!("`{callee}` (called by the test)")),
            "{block}"
        );
    }
    assert!(output.starts_with("test test_render ... FAILED"));
}

/// A panic in the code under test shows the function it panicked in; the
/// test itself, in no planned or read file here, is not shown.
#[tokio::test]
async fn a_panic_outside_the_test_shows_where_it_panicked() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    let command = |variant: &str| {
        json!({"action":"command","command":format!(
            "printf \"test test_render ... FAILED\\nthread 'test_render' panicked at labels.py:2:5:\\n\"; exit 101 # {variant}"
        )})
    };
    act(&fixture, &mut runner, command("a")).await;
    act(&fixture, &mut runner, command("b")).await;
    let response = &runner.task.last_response;
    assert!(
        response
            .contains("labels.py:1-2 `render_name` (where it panicked):\ndef render_name(name):\n"),
        "{response}"
    );
    assert!(!response.contains("(the test)"), "{response}");
}

// ---- Harness questions (PendingChoice): scope escapes and missing planned
// files ask the human instead of replanning or verifying a false completion.

fn choice_keys(runner: &Runner) -> Vec<String> {
    runner
        .task
        .pending_choice
        .as_ref()
        .unwrap()
        .options
        .iter()
        .map(|option| option.key.clone())
        .collect()
}

/// The approved labels.py plan, and a model edit to other.py outside it,
/// parked on the scope question.
async fn escaped_to_other(fixture: &Fixture) -> Runner {
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    // An invalid candidate first: the escape that follows is valid output and
    // closes the repair it opened instead of spending another attempt.
    fixture.conversational(
        json!({"action":"replace","file":"labels.py","old_text":"absent text","new_text":"x"}),
    );
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    runner
}

#[tokio::test]
async fn a_scope_escape_asks_the_human_and_add_amends_the_approved_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = escaped_to_other(&fixture).await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(runner.task.recovery.is_none(), "no repair is spent");
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("other.py")).unwrap(),
        "x = 1\n"
    );
    let pending = runner.task.pending_choice.clone().unwrap();
    assert_eq!(
        pending.kind,
        moosedev::harness::runner::ChoiceKind::ScopeAdd {
            file: "other.py".into()
        }
    );
    assert_eq!(
        pending.prompt,
        "The model wants to edit `other.py`, which is outside the approved plan (labels.py)."
    );
    assert_eq!(choice_keys(&runner), ["add", "replan", "refuse"]);
    assert_eq!(pending.default, "add");
    assert_eq!(
        intent_details(&runner, "choice_asked"),
        vec!["scope_add other.py: add, replan, refuse (default add)"]
    );
    assert!(intent_details(&runner, "scope_escape_replan").is_empty());
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 0);
    assert!(
        runner.advance().await.is_err(),
        "parked until the human chooses"
    );

    // The question survives a restart.
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(runner.task.pending_choice.as_ref(), Some(&pending));

    let error = runner.choose("maybe").await.unwrap_err().to_string();
    assert!(error.contains("add, replan, refuse"), "{error}");
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);

    let approved = journal_value(&runner)["approved_revision"].clone();
    let calls = fixture.model_calls();
    runner.choose("add").await.unwrap();
    assert_eq!(fixture.model_calls(), calls, "the model is asked nothing");
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(journal_value(&runner)["approved_revision"], approved);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().files,
        ["labels.py", "other.py"]
    );
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["labels.py", "other.py"]
    );
    let scope = runner.task.approved_change_scope.as_ref().unwrap();
    assert!(scope.files.contains_key("other.py"));
    let state = runner.task.symbolic.as_ref().unwrap();
    assert!(state.obligations.contains_key("other.py"));
    assert_eq!(state.scope_escapes, 0);
    assert_eq!(intent_details(&runner, "obligations_derived").len(), 2);
    assert_eq!(intent_details(&runner, "scope_added"), vec!["other.py"]);
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["scope_add:add"]
    );
    assert_eq!(
        runner.task.last_response,
        "`other.py` was added to the approved plan. Make your edit."
    );

    // The approval stands for the amended plan: the edit applies without a
    // replan or a second approval.
    fixture.conversational(json!({"action":"read","file":"other.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("other.py")).unwrap(),
        "x = 2\n"
    );
}

#[tokio::test]
async fn choosing_replan_at_a_scope_escape_replans_and_the_planner_amends_the_approved_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = escaped_to_other(&fixture).await;
    runner.choose("replan").await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.pending_choice.is_none());
    assert!(journal_value(&runner)["approved_revision"].is_null());
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 1);
    assert_eq!(
        intent_details(&runner, "scope_escape_replan"),
        vec!["other.py: escape 1, chosen by the human"]
    );
    assert!(runner.task.symbolic.as_ref().unwrap().scope_replan);
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["scope_add:replan"]
    );
    assert_eq!(
        runner.task.last_response,
        "Edit to other.py is outside the approved plan files [labels.py]; replan with every file the change needs."
    );
    assert_eq!(
        runner.task.read_files,
        ["labels.py"],
        "the working set stays"
    );

    // The planner sees the plan it amends, whole and labelled as approved.
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior and update the constant","files":["labels.py","other.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    let prompt = fixture.last_model_prompt("harness_action");
    let plan = prompt
        .split("\nApproved plan (amend it; keep what still holds): ")
        .nth(1)
        .unwrap_or_else(|| panic!("no amend label: {prompt}"));
    assert!(
        plan.starts_with("{\"summary\":\"Preserve display behavior while adding a helper\""),
        "{plan}"
    );
    assert!(!prompt.contains("\nPlan: {"), "labelled once");

    // Once the new plan is proposed it is a plan under review, not an
    // amendment.
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"read","file":"other.py"}));
    runner.advance().await.unwrap();
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(prompt.contains("\nPlan: {"), "{prompt}");
    assert!(!prompt.contains("Approved plan (amend it"));
}

/// `add` cannot stand on an approval that never saw the rules governing the
/// added file: the amendment is undone and the task replans, naming them.
#[tokio::test]
async fn adding_a_file_governed_by_rules_the_plan_does_not_address_replans() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().file_rules = vec![(
        "other.py".into(),
        GoverningRule {
            iri: "urn:rule:frozen-constants".into(),
            label: "Module constants are frozen".into(),
            kind: "Constraint".into(),
            claim: "hasDescription: A module-level constant is never reassigned.\n".into(),
            via: "via: linked to other.py".into(),
            decided_by: Vec::new(),
        },
    )];
    let mut runner = escaped_to_other(&fixture).await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    let calls = fixture.model_calls();
    runner.choose("add").await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.pending_choice.is_none());
    assert!(journal_value(&runner)["approved_revision"].is_null());
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["labels.py"]
    );
    assert!(journal_value(&runner)["snapshots"]
        .get("other.py")
        .is_none());
    let state = runner.task.symbolic.as_ref().unwrap();
    assert!(!state.obligations.contains_key("other.py"));
    assert_eq!(state.scope_escapes, 1);
    assert!(state.scope_replan);
    assert_eq!(
        intent_details(&runner, "scope_add_needs_replan"),
        vec!["other.py: Module constants are frozen"]
    );
    assert_eq!(
        intent_details(&runner, "scope_escape_replan"),
        vec!["other.py: escape 1, rules the approved plan does not address"]
    );
    assert!(intent_details(&runner, "scope_added").is_empty());
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["scope_add:add"]
    );
    assert_eq!(
        runner.task.last_response,
        "`other.py` is governed by rules the approved plan does not address (Module constants are frozen); replanning so the plan can address them."
    );
}

#[tokio::test]
async fn choosing_refuse_keeps_the_plan_and_a_message_replaces_the_question() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = escaped_to_other(&fixture).await;
    runner.choose("refuse").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 0);
    assert_eq!(
        runner.task.last_response,
        "`other.py` is outside the approved plan and the human declined to add it. Continue within the plan files: labels.py."
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["scope_add:refuse"]
    );

    // A plain message while a question waits is new guidance: it replans and
    // the question is discarded.
    fixture.conversational(
        json!({"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    runner
        .submit_message("Leave other.py alone and rename the helper instead.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(runner.task.pending_choice.is_none());
    assert!(runner.task.events.iter().any(|event| event
        .message
        .starts_with("Discarded pending harness question")));
    assert!(runner.choose("add").await.is_err());
}

#[tokio::test]
async fn scope_choice_off_keeps_the_automatic_replan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::env::set_var("MOOSEDEV_HARNESS_SCOPE_CHOICE", "off");
    let runner = escaped_to_other(&fixture).await;
    std::env::remove_var("MOOSEDEV_HARNESS_SCOPE_CHOICE");
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.pending_choice.is_none());
    assert!(runner.task.recovery.is_none());
    assert!(intent_details(&runner, "choice_asked").is_empty());
    assert_eq!(
        intent_details(&runner, "scope_escape_replan"),
        vec!["other.py: escape 1 of 3"]
    );
}

/// code.txt and a notes.txt that does not exist, approved, with code.txt
/// edited.
async fn edited_with_notes_missing(fixture: &Fixture) -> Runner {
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Repair code.txt and record the reasoning in notes.txt","files":["code.txt","notes.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    act(
        fixture,
        &mut runner,
        json!({"action":"replace","file":"code.txt","old_text":"original\n","new_text":"changed\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 1);
    runner
}

/// A plan may list a file only for reference (`unchanged`): the unfinished
/// plan gate does not ask for it to be edited, and the approval gate shows
/// it. Switched off, the field is dropped and the file is asked for again.
#[tokio::test]
async fn a_planned_file_the_plan_leaves_unchanged_is_not_demanded() {
    let _env_lock = ENVIRONMENT.lock().await;
    for off in [false, true] {
        if off {
            std::env::set_var("MOOSEDEV_HARNESS_PLAN_UNCHANGED", "off");
        }
        let fixture = Fixture::new().await;
        std::fs::write(fixture.root.join("notes.txt"), "Earlier notes.\n").unwrap();
        let mut runner = fixture.interactive().await;
        fixture.conversational(json!({"action":"read","file":"code.txt"}));
        runner.advance().await.unwrap();
        fixture.conversational(json!({"action":"plan","summary":"Repair code.txt; notes.txt already records the reasoning","files":["code.txt","notes.txt"],"checks":["true"],"unchanged":["notes.txt"]}));
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::AwaitingPlan);
        let unchanged = runner.task.plan.as_ref().unwrap().unchanged.clone();
        runner.approve_plan().await.unwrap();
        act(
            &fixture,
            &mut runner,
            json!({"action":"replace","file":"code.txt","old_text":"original\n","new_text":"changed\n"}),
        )
        .await;
        act(
            &fixture,
            &mut runner,
            json!({"action":"finish","summary":"Done."}),
        )
        .await;
        let refused = intent_details(&runner, "finish_refused_unfinished");
        std::env::remove_var("MOOSEDEV_HARNESS_PLAN_UNCHANGED");
        if off {
            assert!(unchanged.is_empty());
            assert_eq!(refused, ["missing: []; unedited: [notes.txt]"]);
        } else {
            assert_eq!(unchanged, ["notes.txt"]);
            assert_eq!(intent_details(&runner, "plan_unchanged"), ["notes.txt"]);
            assert!(refused.is_empty(), "{refused:?}");
            assert_ne!(runner.task.phase, Phase::Working);
        }
    }
}

/// Finish twice: sent back once naming notes.txt, then the question.
async fn finished_with_notes_missing(fixture: &Fixture) -> Runner {
    let mut runner = edited_with_notes_missing(fixture).await;
    act(
        fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.recovery.is_none(), "no repair is spent");
    assert_eq!(
        runner.task.last_response,
        "Not finished: planned file(s) notes.txt do not exist yet. Write what the plan still needs, then finish. A planned file that needs no change can stay as it is: finish again."
    );
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished"),
        vec!["missing: [notes.txt]; unedited: []"]
    );
    act(
        fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().kind,
        moosedev::harness::runner::ChoiceKind::MissingPlannedFile {
            files: vec!["notes.txt".into()]
        }
    );
    assert_eq!(choice_keys(&runner), ["write", "drop", "finish"]);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().default,
        "write"
    );
    assert_eq!(
        intent_details(&runner, "choice_asked"),
        vec!["missing_planned_file notes.txt: write, drop, finish (default write)"]
    );
    assert!(runner.task.check_results.is_empty(), "nothing verified");
    runner
}

#[tokio::test]
async fn a_finish_with_a_planned_file_missing_is_sent_back_then_asks_and_write_returns_to_the_model(
) {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_missing(&fixture).await;
    runner.choose("write").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(
        runner.task.last_response,
        "Write the missing planned file(s): notes.txt."
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["missing_planned_file:write"]
    );
    act(
        &fixture,
        &mut runner,
        json!({"action":"write","file":"notes.txt","content":"Why the repair.\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 2);
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished").len(),
        1
    );
}

#[tokio::test]
async fn dropping_the_missing_planned_file_verifies_the_amended_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_missing(&fixture).await;
    let calls = fixture.model_calls();
    runner.choose("drop").await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["code.txt"]);
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["code.txt"]
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["missing_planned_file:drop"]
    );
    assert!(intent_details(&runner, "finish_forced_missing").is_empty());
    // The approval stands for the amended plan: the checks run.
    runner.advance().await.unwrap();
    assert_ne!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.check_results.len(), 1);
    assert!(runner.task.check_results[0].success);
}

#[tokio::test]
async fn verifying_anyway_with_a_planned_file_missing_is_journaled() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_missing(&fixture).await;
    runner.choose("finish").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().files,
        ["code.txt", "notes.txt"]
    );
    assert_eq!(
        intent_details(&runner, "finish_forced_missing"),
        vec!["notes.txt"]
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["missing_planned_file:finish"]
    );
    runner.advance().await.unwrap();
    assert_ne!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.check_results.len(), 1);
}

/// Verifying anyway answers one finish. A new approval, or work sent back at
/// review, gates the next finish at the same source again.
#[tokio::test]
async fn verifying_anyway_does_not_outlive_a_new_approval_or_a_rework() {
    let _env_lock = ENVIRONMENT.lock().await;
    let accepted = |runner: &Runner| {
        runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .unfinished_accepted_at
    };

    // A new approval of the same plan.
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_missing(&fixture).await;
    runner.choose("finish").await.unwrap();
    assert_eq!(accepted(&runner), Some(1));
    runner.mode_plan().await.unwrap();
    act(&fixture, &mut runner, json!({"action":"plan","summary":"Repair code.txt and record the reasoning in notes.txt","files":["code.txt","notes.txt"],"checks":["true"]})).await;
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    runner.approve_plan().await.unwrap();
    assert_eq!(accepted(&runner), None);
    // The new approval's first finish is sent back, its second asks.
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working, "sent back first");
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice, "asked again");

    // Work sent back at the final review.
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_missing(&fixture).await;
    runner.choose("finish").await.unwrap();
    fixture.note("notes.txt is left for later.");
    fixture.typed_one("Lesson", "Notes come later");
    for _ in 0..16 {
        if runner.task.at_final_review() {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert!(runner.task.at_final_review());
    runner
        .rework("Write notes.txt as the plan says.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(accepted(&runner), None);
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice, "asked again");
}

/// A send-back under one plan does not make the first finish under the next
/// approval a repeat, even with no edit between: it is sent back again
/// before the human is asked.
#[tokio::test]
async fn a_new_approval_sends_its_first_unfinished_finish_back() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = edited_with_notes_missing(&fixture).await;
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().unfinished_refused_at,
        Some(1)
    );
    // A different plan, approved with no edit since the send-back.
    runner.mode_plan().await.unwrap();
    act(&fixture, &mut runner, json!({"action":"plan","summary":"Record the reasoning for the repair in notes.txt","files":["code.txt","notes.txt"],"checks":["true"]})).await;
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().unfinished_refused_at,
        None
    );
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working, "sent back, not asked");
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished").len(),
        2
    );
}

/// Finish twice with notes.txt on disk but never edited: sent back once,
/// then the human is asked whether it needs a change.
async fn finished_with_notes_unedited(fixture: &Fixture) -> Runner {
    std::fs::write(fixture.root.join("notes.txt"), "Earlier notes.\n").unwrap();
    let mut runner = edited_with_notes_missing(fixture).await;
    act(
        fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        runner.task.last_response,
        "Not finished: planned file(s) notes.txt have no edit since the plan was approved. Write what the plan still needs, then finish. A planned file that needs no change can stay as it is: finish again."
    );
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished"),
        vec!["missing: []; unedited: [notes.txt]"]
    );
    act(
        fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().kind,
        moosedev::harness::runner::ChoiceKind::UneditedPlannedFiles {
            files: vec!["notes.txt".into()]
        }
    );
    assert_eq!(choice_keys(&runner), ["work", "finish"]);
    assert_eq!(runner.task.pending_choice.as_ref().unwrap().default, "work");
    assert_eq!(
        intent_details(&runner, "choice_asked"),
        vec!["unedited_planned_files notes.txt: work, finish (default work)"]
    );
    assert!(runner.task.check_results.is_empty(), "nothing verified");
    runner
}

/// badciv P5 attempt 3: a4b finished again and again with planned files
/// untouched. The second finish at the same source asks the human; a plan
/// listing a file that needs no change still finishes, on their word.
#[tokio::test]
async fn a_planned_file_left_unedited_is_sent_back_once_then_asked() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_unedited(&fixture).await;
    runner.choose("finish").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        intent_details(&runner, "finish_forced_unedited"),
        vec!["notes.txt"]
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["unedited_planned_files:finish"]
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.check_results.len(), 1);

    // `work` returns to the model; a further finish at the same source, with
    // nothing edited since, parks with the files named instead of asking
    // again (badciv run 17 asked 40 times on the same answer).
    let fixture = Fixture::new().await;
    let mut runner = finished_with_notes_unedited(&fixture).await;
    runner.choose("work").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(
        runner.task.last_response,
        "The human says the plan still needs these files edited: notes.txt. Make those edits, then finish."
    );
    assert!(intent_details(&runner, "finish_forced_unedited").is_empty());
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(intent_details(&runner, "choice_asked").len(), 1);
    assert_eq!(
        intent_details(&runner, "unedited_work_parked"),
        ["notes.txt"]
    );
    assert!(
        runner.task.last_response.contains("notes.txt")
            && runner.task.last_response.contains("Guidance is needed"),
        "{}",
        runner.task.last_response
    );
    // After the human's answer the next finish asks again, where the human
    // can choose to verify as it stands.
    runner
        .submit_message("Continue with the approved plan.".into())
        .await
        .unwrap();
    for _ in 0..4 {
        if runner.task.phase == Phase::AwaitingChoice {
            break;
        }
        fixture.conversational(json!({"action":"finish","summary":"Done."}));
        runner.advance().await.unwrap();
    }
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingChoice,
        "{}",
        runner.task.last_response
    );
    assert_eq!(intent_details(&runner, "unedited_work_parked").len(), 1);
}

/// A plan cannot mark every planned file unchanged and finish having changed
/// nothing: `unchanged` exempts a file only once another planned file has an
/// edit this cycle.
#[tokio::test]
async fn a_plan_marking_every_file_unchanged_is_still_asked_for_edits() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("notes.txt"), "Earlier notes.\n").unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Repair code.txt and note it","files":["code.txt","notes.txt"],"checks":["true"],"unchanged":["code.txt","notes.txt"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        intent_details(&runner, "finish_refused_unfinished"),
        ["missing: []; unedited: [code.txt, notes.txt]"]
    );
}

#[tokio::test]
async fn a_journal_from_before_harness_questions_loads() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let runner = fixture.approved_interactive().await;
    let mut journal = journal_value(&runner);
    assert!(
        journal.get("pending_choice").is_none(),
        "absent is not written"
    );
    // Written by an older build: none of the new fields.
    journal.as_object_mut().unwrap().remove("pending_choice");
    let state = journal["symbolic"].as_object_mut().unwrap();
    state.remove("unfinished_refused_at");
    state.remove("unfinished_accepted_at");
    let legacy: moosedev::harness::runner::Task = serde_json::from_value(journal).unwrap();
    assert!(legacy.pending_choice.is_none());
    let state = legacy.symbolic.unwrap();
    assert_eq!(state.unfinished_refused_at, None);
    assert_eq!(state.unfinished_accepted_at, None);
}

#[tokio::test]
async fn a_choice_relying_on_a_withdrawn_approval_is_refused_and_the_question_discarded() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = escaped_to_other(&fixture).await;
    // The source changes outside the harness while the question waits.
    std::fs::write(
        fixture.root.join("labels.py"),
        "def render_name(name):\n    return name.lower()\n",
    )
    .unwrap();
    let error = runner.choose("add").await.unwrap_err().to_string();
    assert!(error.contains("plan evidence changed"), "{error}");
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert!(intent_details(&runner, "choice_made").is_empty());
}

fn preserve_rule() -> GoverningRule {
    GoverningRule {
        iri: PRESERVE.into(),
        label: "Preserve display label behavior".into(),
        kind: "Requirement".into(),
        claim: "hasDescription: Display labels render exactly as before.\n".into(),
        via: "via: linked to labels.py".into(),
        decided_by: Vec::new(),
    }
}

/// A plan kept although it neither addresses nor mentions a delivered rule
/// leaves that rule open: the plan stores it for the approval gate, and
/// approval records it as deferred.
#[tokio::test]
async fn a_plan_leaving_a_rule_open_stores_it_and_approval_defers_it() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule()];
    let mut runner = fixture.interactive().await;
    let plan = json!({"action":"plan","summary":"Add a helper","files":["labels.py"],"checks":["true"],"addresses":[]});
    fixture.conversational(plan.clone());
    runner.advance().await.unwrap();
    assert!(runner.task.plan.is_none(), "returned once for the rule");
    fixture.conversational(plan);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    let open = &runner.task.plan.as_ref().unwrap().open_rules;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].iri, PRESERVE);
    assert_eq!(open[0].label, "Preserve display label behavior");
    assert_eq!(open[0].kind, "Requirement");
    assert!(!open[0].mentioned);

    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.approved_plans[0].deferred, vec![PRESERVE]);
    assert_eq!(
        intent_details(&runner, "rules_deferred"),
        vec!["1 rule(s): Preserve display label behavior"]
    );
    // The builder is not shown what the plan left open.
    add_helper(&fixture);
    runner.advance().await.unwrap();
    assert!(!fixture
        .last_model_prompt("harness_action")
        .contains("open_rules"));
}

/// A summary that defers a rule satisfies plan coverage, but the plan does
/// not list the rule as implemented: it stays open, marked as mentioned, and
/// approval records its deferral. Only `addresses` closes a rule.
#[tokio::test]
async fn a_rule_the_summary_defers_is_still_open_and_approval_defers_it() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule()];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Add a helper. Preserve display label behavior is deferred outside this objective.","files":["labels.py"],"checks":["true"],"addresses":[]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan, "coverage is met");
    let open = &runner.task.plan.as_ref().unwrap().open_rules;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].iri, PRESERVE);
    assert!(open[0].mentioned);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.approved_plans[0].deferred, vec![PRESERVE]);
    assert_eq!(
        intent_details(&runner, "rules_deferred"),
        vec!["1 rule(s): Preserve display label behavior"]
    );

    // A plan that lists the rule in `addresses` leaves nothing open.
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule()];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Add a helper that preserves display label behavior.","files":["labels.py"],"checks":["true"],"addresses":[PRESERVE]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.plan.as_ref().unwrap().open_rules.is_empty());
    runner.approve_plan().await.unwrap();
    assert!(runner.task.approved_plans[0].deferred.is_empty());
    assert!(intent_details(&runner, "rules_deferred").is_empty());
}

/// A plan's open choices: an invalid one goes back for repair, the human
/// answers one at the gate, approval settles the other by its default, and
/// the builder is shown both decisions.
#[tokio::test]
async fn open_choices_are_answered_at_the_gate_and_defaulted_on_approval() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    let plan = |choices: Value| json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["true"],"addresses":[],"open_choices":choices});
    fixture.conversational(plan(json!([
        {"question":"Which separator joins words?","options":["space","dash"],"default":"tab"}
    ])));
    fixture.conversational(plan(json!([
        {"question":"Which separator joins words?","options":["space","dash"],"default":"space"},
        {"question":"Strip trailing whitespace?","options":["yes","no"],"default":"yes"}
    ])));
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(
        runner.task.events.iter().any(|event| event
            .message
            .contains("its default \"tab\" is not one of its options")),
        "the invalid choice went back for repair"
    );
    let choices = &runner.task.plan.as_ref().unwrap().open_choices;
    assert_eq!(choices.len(), 2);
    assert!(choices.iter().all(|choice| choice.answer.is_none()));

    assert!(runner.choose_plan_option("3 space").is_err());
    assert!(runner.choose_plan_option("1 tab").is_err());
    // An option by its number; a later answer replaces it.
    runner.choose_plan_option("1 1").unwrap();
    assert_eq!(
        runner.task.plan.as_ref().unwrap().open_choices[0]
            .answer
            .as_deref(),
        Some("space")
    );
    // The headless route: `choose ID "1 dash"`, by the option's text in any case.
    moosedev::harness::tui::execute(
        &mut runner,
        moosedev::harness::tui::Action::Choose("1 DASH".into()),
    )
    .await
    .unwrap();
    let choices = &runner.task.plan.as_ref().unwrap().open_choices;
    assert_eq!(choices[0].answer.as_deref(), Some("dash"));
    assert_eq!(choices[1].answer, None);

    runner.approve_plan().await.unwrap();
    assert_eq!(
        intent_details(&runner, "plan_choice"),
        vec!["1: space", "1: dash", "2: yes (default)"]
    );
    add_helper(&fixture);
    runner.advance().await.unwrap();
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(
        prompt.contains("Decided: Which separator joins words? → dash\\nDecided: Strip trailing whitespace? → yes"),
        "{prompt}"
    );
    assert!(!prompt.contains("\"open_choices\""));

    let id = runner.task.id.clone();
    drop(runner);
    let runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(
        runner.task.plan.as_ref().unwrap().open_choices[1]
            .answer
            .as_deref(),
        Some("yes"),
        "answers survive resume"
    );
}

/// `MOOSEDEV_HARNESS_PLAN_CHOICES=off` offers no `open_choices`, says
/// nothing of them, and drops any a model sends anyway.
#[tokio::test]
async fn the_plan_choices_switch_removes_open_choices() {
    let _env_lock = ENVIRONMENT.lock().await;
    let plan_parameters = |fixture: &Fixture| {
        let request = requests_of_kind(fixture, "model")
            .into_iter()
            .rfind(|request| request["schema"] == "harness_action")
            .unwrap();
        request["body"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "plan")
            .unwrap()["function"]["parameters"]
            .clone()
    };
    let plan = json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["true"],"addresses":[],"open_choices":[{"question":"Which separator?","options":["space","dash"],"default":"space"}]});

    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(plan.clone());
    runner.advance().await.unwrap();
    let on = plan_parameters(&fixture);
    assert!(on["required"]
        .as_array()
        .unwrap()
        .contains(&json!("open_choices")));
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("plan(summary,files,checks,addresses,satisfied,stubs,unchanged,open_choices)"));
    assert_eq!(runner.task.plan.as_ref().unwrap().open_choices.len(), 1);

    std::env::set_var("MOOSEDEV_HARNESS_PLAN_CHOICES", "off");
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(plan);
    let result = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_PLAN_CHOICES");
    result.unwrap();
    let off = plan_parameters(&fixture);
    assert!(off["properties"].get("open_choices").is_none());
    assert!(!off["required"]
        .as_array()
        .unwrap()
        .contains(&json!("open_choices")));
    assert!(!fixture
        .last_model_prompt("harness_action")
        .contains("open_choices"));
    // The satisfied field has its own switch and stays.
    assert!(off["properties"].get("satisfied").is_some());
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("plan(summary,files,checks,addresses,satisfied,stubs,unchanged)"));
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.plan.as_ref().unwrap().open_choices.is_empty());
}

/// A settled language-server error in labels.py, rustc's shape: `mod
/// helpers;` names a file that does not exist.
fn module_not_found(runner: &mut Runner) {
    let mut snapshot = diagnostics(1);
    snapshot.errors[0].file = "labels.py".into();
    snapshot.errors[0].message = "file not found for module `helpers`".into();
    snapshot.errors[0].detail = Some(
        "error[E0583]: file not found for module `helpers`\n  = help: to create the module `helpers`, create file \"helpers.rs\" or \"helpers/mod.rs\""
            .into(),
    );
    runner.task.diagnostics = Some(snapshot);
}

/// The approved labels.py plan, a settled error saying `helpers.rs` is
/// missing, and a model edit of labels.py that the result settles on.
async fn edited_with_module_missing(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    module_not_found(&mut runner);
    act(
        fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 1);
    runner
}

#[tokio::test]
async fn a_module_declared_without_its_file_asks_the_human_and_add_amends_the_plan() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = edited_with_module_missing(&fixture).await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    let pending = runner.task.pending_choice.clone().unwrap();
    assert_eq!(
        pending.kind,
        moosedev::harness::runner::ChoiceKind::MissingModule {
            file: "helpers.rs".into(),
            declared_in: "labels.py".into()
        }
    );
    assert_eq!(
        pending.prompt,
        "`labels.py` declares or imports `helpers.rs`, which does not exist and is outside the approved plan (labels.py)."
    );
    assert_eq!(choice_keys(&runner), ["add", "replan", "refuse"]);
    assert_eq!(pending.default, "add");
    assert_eq!(
        intent_details(&runner, "missing_module_asked"),
        vec!["helpers.rs in `labels.py`"]
    );
    assert_eq!(
        intent_details(&runner, "choice_asked"),
        vec!["missing_module helpers.rs: add, replan, refuse (default add)"]
    );
    let state = runner.task.symbolic.as_ref().unwrap();
    assert!(state.auto_verify_armed.is_none() && state.auto_fix_armed.is_none());

    let calls = fixture.model_calls();
    runner.choose("add").await.unwrap();
    assert_eq!(fixture.model_calls(), calls, "the model is asked nothing");
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().files,
        ["labels.py", "helpers.rs"]
    );
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["labels.py", "helpers.rs"]
    );
    assert_eq!(
        runner.task.last_response,
        "`helpers.rs` was added to the approved plan. Write it: `labels.py` declares or imports `helpers.rs`."
    );
    assert_eq!(intent_details(&runner, "scope_added"), vec!["helpers.rs"]);
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["missing_module:add"]
    );

    // The approval stands for the amended plan: the model's write of the
    // file is applied, and a planned file is not asked about again.
    act(
        &fixture,
        &mut runner,
        json!({"action":"write","file":"helpers.rs","content":"pub fn helper() {}\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 2);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_ne!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("helpers.rs")).unwrap(),
        "pub fn helper() {}\n"
    );
    assert_eq!(intent_details(&runner, "missing_module_asked").len(), 1);
}

#[tokio::test]
async fn refusing_a_missing_module_keeps_the_plan_and_it_is_asked_once() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = edited_with_module_missing(&fixture).await;
    runner.choose("refuse").await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert_eq!(
        runner.task.last_response,
        "`helpers.rs` stays outside the plan: remove its declaration or import from `labels.py`."
    );
    assert_eq!(
        intent_details(&runner, "choice_made"),
        vec!["missing_module:refuse"]
    );
    // The same error after another edit: answered already this approval.
    module_not_found(&mut runner);
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"name.strip()","new_text":"name.strip().title()"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 2);
    assert_ne!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(intent_details(&runner, "missing_module_asked").len(), 1);
}

#[tokio::test]
async fn choosing_replan_at_a_missing_module_replans_naming_it() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = edited_with_module_missing(&fixture).await;
    runner.choose("replan").await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.pending_choice.is_none());
    assert_eq!(runner.task.symbolic.as_ref().unwrap().scope_escapes, 1);
    assert_eq!(
        intent_details(&runner, "scope_escape_replan"),
        vec!["helpers.rs: escape 1, chosen by the human"]
    );
    assert_eq!(
        runner.task.last_response,
        "`labels.py` declares or imports `helpers.rs`, which does not exist and is outside the approved plan files [labels.py]; replan with every file the change needs, or without the module."
    );
}

#[tokio::test]
async fn a_planned_module_file_not_written_yet_or_the_switch_off_asks_nothing() {
    let _env_lock = ENVIRONMENT.lock().await;
    // Planned but not written: the unfinished-plan gate owns it.
    let fixture = symbolic_fixture().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper module","files":["labels.py","helpers.rs"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    module_not_found(&mut runner);
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 1);
    assert_ne!(runner.task.phase, Phase::AwaitingChoice);
    assert!(intent_details(&runner, "missing_module_asked").is_empty());

    // Switched off: the model is left to find out, as before.
    let fixture = symbolic_fixture().await;
    std::env::set_var("MOOSEDEV_HARNESS_STRUCTURAL_ASK", "off");
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    module_not_found(&mut runner);
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}));
    let result = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_STRUCTURAL_ASK");
    result.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert_ne!(runner.task.phase, Phase::AwaitingChoice);
    assert!(intent_details(&runner, "missing_module_asked").is_empty());
}

/// `mod inner;` in `src/foo.rs` wants `src/foo/inner.rs`, whose directory
/// the first module there creates: the declaration derives it, so the human
/// is asked although `src/foo/` does not exist.
#[tokio::test]
async fn a_rust_module_in_a_directory_not_created_yet_is_asked_about() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    assert!(!fixture.root.join("src/foo").exists());
    let mut snapshot = diagnostics(1);
    snapshot.errors[0].file = "src/foo.rs".into();
    snapshot.errors[0].message =
        "unresolved module, can't find module file: foo/inner.rs, or foo/inner/mod.rs".into();
    runner.task.diagnostics = Some(snapshot);
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().kind,
        moosedev::harness::runner::ChoiceKind::MissingModule {
            file: "src/foo/inner.rs".into(),
            declared_in: "src/foo.rs".into()
        }
    );
    assert_eq!(
        intent_details(&runner, "missing_module_asked"),
        vec!["src/foo/inner.rs in `src/foo.rs`"]
    );
}

#[tokio::test]
async fn a_failed_run_that_cannot_import_a_module_of_the_project_asks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::create_dir_all(fixture.root.join("pkg")).unwrap();
    std::fs::write(fixture.root.join("pkg/__init__.py"), "").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    // A package the project does not have is not asked about.
    act(
        &fixture,
        &mut runner,
        json!({"action":"command","command":"echo \"ModuleNotFoundError: No module named 'numpy.linalg'\"; exit 1"}),
    )
    .await;
    assert_ne!(runner.task.phase, Phase::AwaitingChoice);
    act(
        &fixture,
        &mut runner,
        json!({"action":"command","command":"echo \"ModuleNotFoundError: No module named 'pkg.helpers'\"; exit 1"}),
    )
    .await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().kind,
        moosedev::harness::runner::ChoiceKind::MissingModule {
            file: "pkg/helpers.py".into(),
            declared_in: String::new()
        }
    );
    assert_eq!(
        intent_details(&runner, "missing_module_asked"),
        vec!["pkg/helpers.py (named by a failed run)"]
    );
    runner.choose("refuse").await.unwrap();
    assert_eq!(
        runner.task.last_response,
        "`pkg/helpers.py` stays outside the plan: remove the import of it."
    );

    // A required check failing the same way asks too.
    let fixture = symbolic_fixture().await;
    std::fs::create_dir_all(fixture.root.join("pkg")).unwrap();
    std::fs::write(fixture.root.join("pkg/__init__.py"), "").unwrap();
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.task.plan.as_mut().unwrap().checks =
        vec!["echo \"ModuleNotFoundError: No module named 'pkg.helpers'\"; exit 1".into()];
    runner.approve_plan().await.unwrap();
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    act(
        &fixture,
        &mut runner,
        json!({"action":"finish","summary":"Done."}),
    )
    .await;
    for _ in 0..4 {
        if runner.task.phase != Phase::Verifying && runner.task.phase != Phase::Working {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.check_results.len(), 1);
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        intent_details(&runner, "missing_module_asked"),
        vec!["pkg/helpers.py (named by a failed run)"]
    );
}

const LIB_RS: &str = "pub enum Terrain {\n    Ocean,\n}\n\npub struct Tile {\n    pub terrain: Terrain,\n}\n\npub struct Map {\n    pub tiles: Vec<Tile>,\n}\n\npub mod codes;\npub mod parse;\n";
const TEST_ONLY_WRITE: &str = "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn empty_map() {\n        assert!(Map { tiles: vec![] }.tiles.is_empty());\n    }\n}\n";

/// lib.rs, read and planned, approved.
async fn approved_on_lib_rs(fixture: &Fixture) -> Runner {
    std::fs::write(fixture.root.join("lib.rs"), LIB_RS).unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"lib.rs"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Add a test of the map","files":["lib.rs"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    runner
}

/// badciv P5 attempt 3: a4b answered "add a test" with a `write` of lib.rs
/// holding only the test module. The write is a repair naming what it would
/// delete; a replace adds the test.
#[tokio::test]
async fn a_write_that_deletes_most_of_a_files_declarations_is_repaired() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = approved_on_lib_rs(&fixture).await;
    fixture.conversational(json!({"action":"write","file":"lib.rs","content":TEST_ONLY_WRITE}));
    fixture.conversational(json!({"action":"replace","file":"lib.rs","old_text":"pub mod parse;\n","new_text":format!("pub mod parse;\n\n{TEST_ONLY_WRITE}")}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert!(
        std::fs::read_to_string(fixture.root.join("lib.rs"))
            .unwrap()
            .starts_with(LIB_RS),
        "the declarations are kept"
    );
    assert!(runner.task.events.iter().any(|event| {
        event.message.starts_with("Correcting action, attempt 2 of 3")
            && event.message.contains("This write deletes `Terrain`, `Tile`, `Map`, `codes`, `parse` from lib.rs. To add to a file use replace on a span, or write the whole file including what it already declares.")
    }), "{:?}", runner.task.events.iter().map(|e| &e.message).collect::<Vec<_>>());
    assert_eq!(
        intent_details(&runner, "destructive_write_refused"),
        vec!["lib.rs: Terrain, Tile, Map, codes, parse"]
    );

    // Switched off, the write applies as before.
    let fixture = Fixture::new().await;
    let mut runner = approved_on_lib_rs(&fixture).await;
    std::env::set_var("MOOSEDEV_HARNESS_WRITE_GUARD", "off");
    fixture.conversational(json!({"action":"write","file":"lib.rs","content":TEST_ONLY_WRITE}));
    let advanced = runner.advance().await;
    std::env::remove_var("MOOSEDEV_HARNESS_WRITE_GUARD");
    advanced.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("lib.rs")).unwrap(),
        TEST_ONLY_WRITE
    );
    assert!(intent_details(&runner, "destructive_write_refused").is_empty());
}

/// badciv run 15: a plan splitting lib.rs into modules wrote the module
/// first, then lib.rs without what moved; the guard refused it three times
/// and parked. A declaration another file of the plan now defines is not
/// deleted, so the write applies; one defined nowhere else still counts.
#[tokio::test]
async fn a_write_moving_declarations_into_a_planned_module_applies() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("lib.rs"), LIB_RS).unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"lib.rs"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Move the map types into types.rs","files":["lib.rs","types.rs"],"checks":["true"]}));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();

    let types = "pub enum Terrain {\n    Ocean,\n}\n\npub struct Tile {\n    pub terrain: Terrain,\n}\n\npub struct Map {\n    pub tiles: Vec<Tile>,\n}\n";
    let lib = "pub mod codes;\npub mod parse;\npub mod types;\n\npub use types::*;\n";
    fixture.conversational(json!({"action":"write","file":"types.rs","content":types}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"write","file":"lib.rs","content":lib}));
    // The step after an edit may be the capture checkpoint's; advance until
    // the queued write has been taken.
    for _ in 0..3 {
        if runner.task.edits.len() == 2 {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("lib.rs")).unwrap(),
        lib,
        "{:?}",
        runner
            .task
            .events
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
    assert!(intent_details(&runner, "destructive_write_refused").is_empty());
}

/// badciv P5 attempts 2 and 3: old_text matched only after decoding JSON
/// escapes, and new_text mixed real line breaks with literal `\n`, which
/// was written into code. The replace is a repair naming the line.
#[tokio::test]
async fn a_decoded_replace_whose_new_text_holds_newline_escapes_in_code_is_repaired() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    std::fs::write(
        fixture.root.join("code.txt"),
        "let a = \"x\";\nlet b = 2;\n",
    )
    .unwrap();
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"let a = \\\"x\\\";\\nlet b = 2;\\n","new_text":"let a = \"y\";\nlet b = 2;\\n\\n    let c = 3;\\n    let d = 4;\n"}));
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"let b = 2;\n","new_text":"let b = 2;\n\nlet c = 3;\n"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "let a = \"x\";\nlet b = 2;\n\nlet c = 3;\n"
    );
    assert!(runner.task.events.iter().any(|event| {
        event.message.starts_with("Correcting action, attempt 2 of 3")
            && event.message.contains("new_text contains literal \\n escapes outside string literals on line 2; send the replacement with real line breaks")
    }), "{:?}", runner.task.events.iter().map(|e| &e.message).collect::<Vec<_>>());
    assert_eq!(
        intent_details(&runner, "replace_escapes_refused"),
        vec!["code.txt: new_text line 2"]
    );
}

/// badciv P5 attempt 3: a scaffolding step of `unimplemented!()` bodies drew
/// an `isMotivatedBy` edge to every rule its plan addressed. While a planned
/// file holds a stub, a rule is withheld unless every planned file it was
/// derived for is free of stubs; one derived for none is withheld too.
#[tokio::test]
async fn addressed_rules_are_withheld_while_planned_files_hold_stubs() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![
        GoverningRule {
            iri: PRESERVE.into(),
            label: "Preserve display label behavior".into(),
            kind: "Requirement".into(),
            claim: "hasDescription: Display labels render as before.\n".into(),
            via: "via: linked to labels.py".into(),
            decided_by: Vec::new(),
        },
        GoverningRule {
            iri: UNLINKED.into(),
            label: "Labels never exceed one line".into(),
            kind: "Constraint".into(),
            claim: "hasDescription: A label is a single line.\n".into(),
            via: "via: linked to labels.py".into(),
            decided_by: Vec::new(),
        },
    ];
    std::fs::write(
        fixture.root.join("other.py"),
        "def other():\n    return 1\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"read","file":"other.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display label behavior while adding a normalize helper; labels never exceed one line, in other.py","files":["labels.py","other.py"],"checks":["true"],"addresses":["Preserve display label behavior","[Constraint] Labels never exceed one line"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    let state = runner.task.symbolic.as_ref().unwrap();
    assert_eq!(state.obligations.get("labels.py").unwrap(), &[PRESERVE]);
    assert!(!state
        .obligations
        .values()
        .flatten()
        .any(|iri| iri == UNLINKED));
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    act(
        &fixture,
        &mut runner,
        json!({"action":"replace","file":"other.py","old_text":"    return 1\n","new_text":"    raise NotImplementedError\n"}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 2);

    // The stub gate sends the first finish back; the second goes on.
    for _ in 0..2 {
        fixture.conversational(json!({"action":"finish","summary":"Scaffolded."}));
    }
    for _ in 0..6 {
        match runner.task.phase {
            Phase::AwaitingReview => runner.review(true).await.unwrap(),
            Phase::Verifying => break,
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(intent_details(&runner, "finish_refused_stubs").len(), 1);
    runner.task.check_results = vec![passed_check()];
    fixture.note("Normalization lives in one helper.");
    runner.advance().await.unwrap();

    let request = fixture
        .shared
        .lock()
        .unwrap()
        .capture_type_requests
        .last()
        .cloned()
        .expect("the note was typed");
    assert_eq!(request.addressed_rules, vec![PRESERVE.to_string()]);
    assert_eq!(
        intent_details(&runner, "addressed_withheld"),
        vec!["1 of 2 addressed rules; stubs left in other.py"]
    );
    assert!(
        intent_details(&runner, "review_evidence").contains(
            &"Motivated-by edges withheld: stubs left in planned files (1 of 2 addressed rules)."
                .to_string()
        ),
        "{:?}",
        intent_details(&runner, "review_evidence")
    );
}

/// Approve the labels.py plan, make two edits to it (the second writes
/// `second`), then split the approvals: an earlier plan over `earlier` that
/// addressed UNLINKED owns the first edit, the current one addressed
/// PRESERVE. Finish (`finishes` times, so a stub send-back is answered),
/// verify, note, and return the capture request's addressed rules.
async fn addressed_rules_across_two_plans(
    fixture: &Fixture,
    earlier: &str,
    second: &str,
    finishes: usize,
) -> (Runner, Vec<String>) {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    act(
        fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name\n","new_text":"    return name.strip()\n"}),
    )
    .await;
    act(
        fixture,
        &mut runner,
        json!({"action":"replace","file":"labels.py","old_text":"    return name.strip()\n","new_text":second}),
    )
    .await;
    assert_eq!(runner.task.edits.len(), 2);
    let current = runner.task.approved_plans.last().unwrap().clone();
    runner.task.approved_plans = vec![
        moosedev::harness::runner::ApprovedPlan {
            summary: "Keep labels on one line".into(),
            files: vec![earlier.into()],
            addresses: vec![UNLINKED.into()],
            satisfied: vec![],
            edit_start: 0,
            ..current.clone()
        },
        moosedev::harness::runner::ApprovedPlan {
            addresses: vec![PRESERVE.into()],
            satisfied: vec![],
            edit_start: 1,
            ..current
        },
    ];
    for _ in 0..finishes {
        fixture.conversational(json!({"action":"finish","summary":"Done."}));
    }
    for _ in 0..6 {
        match runner.task.phase {
            Phase::AwaitingReview => runner.review(true).await.unwrap(),
            Phase::Verifying => break,
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.task.check_results = vec![passed_check()];
    fixture.note("Normalization lives in one helper.");
    runner.advance().await.unwrap();
    let request = fixture
        .shared
        .lock()
        .unwrap()
        .capture_type_requests
        .last()
        .cloned()
        .expect("the note was typed");
    (runner, request.addressed_rules)
}

/// Each approved plan is judged by its own files: an earlier plan whose
/// file still holds a stub withholds its rules even when the current plan's
/// files are clean.
#[tokio::test]
async fn an_earlier_plan_left_with_a_stub_withholds_its_rules() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(
        fixture.root.join("other.py"),
        "def other():\n    raise NotImplementedError\n",
    )
    .unwrap();
    let (runner, addressed) = addressed_rules_across_two_plans(
        &fixture,
        "other.py",
        "    return name.strip().lower()\n",
        1,
    )
    .await;
    assert_eq!(addressed, vec![PRESERVE.to_string()]);
    assert_eq!(
        intent_details(&runner, "addressed_withheld"),
        vec!["1 of 2 addressed rules; stubs left in other.py"]
    );
}

/// A stub the current plan left does not withhold the rules of an earlier
/// plan whose own files are free of stubs: that plan finished its work.
#[tokio::test]
async fn a_completed_plans_rules_are_kept_when_the_current_plan_leaves_a_stub() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    std::fs::write(
        fixture.root.join("other.py"),
        "def other():\n    return 1\n",
    )
    .unwrap();
    let (runner, addressed) = addressed_rules_across_two_plans(
        &fixture,
        "other.py",
        "    raise NotImplementedError\n",
        2,
    )
    .await;
    assert_eq!(intent_details(&runner, "finish_refused_stubs").len(), 1);
    assert_eq!(addressed, vec![UNLINKED.to_string()]);
    assert_eq!(
        intent_details(&runner, "addressed_withheld"),
        vec!["1 of 2 addressed rules; stubs left in labels.py"]
    );
}

/// Qwen3.5-9B on badciv sent `write` without `content` for a planned file
/// that did not exist yet; it read as deleting an absent file, a no-op, so a
/// finish, and the unfinished-plan gate asked about every planned file.
#[tokio::test]
async fn a_write_without_content_is_repaired_not_a_finish() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"write","file":"labels.py"}));
    fixture.conversational(json!({"action":"write","file":"labels.py","content":"def render_name(name):\n    return name.strip()\n"}));
    runner.advance().await.unwrap();
    assert!(intent_details(&runner, "noop_edit_continuation").is_empty());
    assert!(intent_details(&runner, "finish_refused_unfinished").is_empty());
    assert_eq!(runner.task.edits.len(), 1, "the repaired write applied");

    // Deleting a file that does not exist is not a no-op finish either.
    // codes.py is planned and not written yet.
    let fixture = symbolic_fixture().await;
    let mut runner = missing_module_runner(&fixture).await;
    fixture.conversational(json!({"action":"write","file":"codes.py","content":null}));
    fixture.conversational(json!({"action":"write","file":"codes.py","content":"CODES = {}\n"}));
    runner.advance().await.unwrap();
    assert!(intent_details(&runner, "noop_edit_continuation").is_empty());
    assert!(
        runner
            .task
            .events
            .iter()
            .any(|event| event.message.contains("nothing to delete")),
        "the repair names the absent file"
    );
    assert!(runner.task.edits.iter().any(|edit| edit.file == "codes.py"));
}

fn unlinked_rule() -> GoverningRule {
    GoverningRule {
        iri: UNLINKED.into(),
        label: "Labels never exceed one line".into(),
        kind: "Constraint".into(),
        claim: "hasDescription: A label is a single line.\n".into(),
        via: "via: linked to labels.py".into(),
        decided_by: Vec::new(),
    }
}

fn decided(rule: GoverningRule, decision: &str) -> GoverningRule {
    GoverningRule {
        decided_by: vec![decision.into()],
        ..rule
    }
}

/// The Project rules block of a step prompt, up to the action meanings.
fn rules_section(prompt: &str) -> String {
    let rules = prompt
        .split("\nProject rules (")
        .nth(1)
        .expect("a rules block");
    rules.split("\nAction meanings").next().unwrap().to_owned()
}

/// A rule an accepted decision is motivated by needs no answer: the plan is
/// not sent back for it and does not leave it open. A decided Requirement is
/// one line naming the decision; a decided Constraint is still shown whole.
#[tokio::test]
async fn rules_a_decision_settles_are_not_sent_back_or_left_open() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![
        decided(preserve_rule(), "urn:ad:preserve"),
        decided(unlinked_rule(), "urn:ad:unlinked"),
    ];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Add a helper","files":["labels.py"],"checks":["true"],"addresses":[],"satisfied":[]}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "{}",
        runner.task.last_response
    );
    assert!(runner.task.plan.as_ref().unwrap().open_rules.is_empty());
    assert!(intent_details(&runner, "constraint_coverage").is_empty());
    assert_eq!(
        intent_details(&runner, "rules_settled"),
        vec!["decided 2, addressed 0, satisfied 0 of 2 rule(s)"]
    );
    let prompt = fixture.last_model_prompt("harness_action");
    let rules = rules_section(&prompt);
    assert!(
        rules.contains(&format!("\n[Requirement] Preserve display label behavior ({PRESERVE}) — decided by urn:ad:preserve; via: linked to labels.py\n")),
        "{rules}"
    );
    assert!(!rules.contains("Display labels render exactly as before."));
    assert!(
        rules.contains(&format!("\n[Constraint] Labels never exceed one line ({UNLINKED})\nvia: linked to labels.py\nhasDescription: A label is a single line.\n")),
        "a decided Constraint stays whole: {rules}"
    );
    assert!(rules.contains("\n1 settled Requirement(s) are shown as one line without their claim and need no answer; search project knowledge for their claims\n"));
    assert!(
        prompt.contains(
            "\nEvery project rule is already settled (2 settled rule(s) need no answer)."
        ),
        "{prompt}"
    );
}

/// A rule an earlier approved plan of the task addressed, with an edit made
/// under it, is settled when planning again: one line, not sent back, not
/// left open. While that plan is being built its rules stay whole.
#[tokio::test]
async fn a_rule_an_earlier_approved_plan_addressed_is_settled_when_replanning() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule()];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display label behavior while adding a helper","files":["labels.py"],"checks":["true"],"addresses":[PRESERVE]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    // In Auto the builder keeps the claims its plan implements.
    let building = fixture.last_model_prompt("harness_action");
    assert!(
        rules_section(&building).contains(&format!("({PRESERVE})\nvia: linked to labels.py\nhasDescription: Display labels render exactly as before.\n")),
        "{building}"
    );

    fixture.conversational(
        json!({"action":"replan","reason":"Normalize must also collapse whitespace"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Collapse whitespace in normalize","files":["labels.py"],"checks":["true"],"addresses":[]}));
    for _ in 0..3 {
        if runner.task.phase == Phase::AwaitingPlan {
            break;
        }
        match runner.task.phase {
            Phase::AwaitingReview => runner.confirm_no_knowledge().await.unwrap(),
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::AwaitingPlan, "not sent back");
    assert!(runner.task.plan.as_ref().unwrap().open_rules.is_empty());
    assert_eq!(
        intent_details(&runner, "rules_settled"),
        vec!["decided 0, addressed 1, satisfied 0 of 1 rule(s)"]
    );
    let planning = fixture.last_model_prompt("harness_action");
    let rules = rules_section(&planning);
    assert!(
        rules.contains(&format!("\n[Requirement] Preserve display label behavior ({PRESERVE}) — addressed by approved plan 1; via: linked to labels.py\n")),
        "{rules}"
    );
    assert!(!rules.contains("Display labels render exactly as before."));
}

/// A plan's `satisfied` claims resolve like its addresses (which win on
/// overlap); an entry naming no rule is journaled and dropped. A claimed rule
/// is not sent back or left open, approval journals the claim and keeps it,
/// and it never becomes a knowledge edge.
#[tokio::test]
async fn a_plan_says_a_rule_already_holds_and_that_claim_mints_no_edge() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule(), unlinked_rule()];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    let planning = fixture.last_model_prompt("harness_action");
    assert!(planning
        .contains("plan(summary,files,checks,addresses,satisfied,stubs,unchanged,open_choices)"));
    assert!(planning.contains("that the existing code already satisfies it unchanged"));
    fixture.conversational(json!({"action":"plan","summary":"Add a normalize helper so labels never exceed one line","files":["labels.py"],"checks":["true"],"addresses":["[Constraint] Labels never exceed one line"],"satisfied":["Preserve display label behavior","No such rule",UNLINKED]}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "{}",
        runner.task.last_response
    );
    let plan = runner.task.plan.as_ref().unwrap();
    assert_eq!(plan.addresses, vec![UNLINKED.to_string()]);
    assert_eq!(plan.satisfied, vec![PRESERVE.to_string()]);
    assert!(plan.open_rules.is_empty());
    assert_eq!(
        intent_details(&runner, "plan_satisfied"),
        vec![format!("1 of 2 rule(s): {PRESERVE}")]
    );
    assert_eq!(
        intent_details(&runner, "plan_satisfied_unresolved"),
        vec!["No such rule"]
    );
    assert_eq!(
        intent_details(&runner, "rules_settled"),
        vec!["decided 0, addressed 0, satisfied 1 of 2 rule(s)"]
    );

    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.approved_plans[0].satisfied, vec![PRESERVE]);
    assert_eq!(
        intent_details(&runner, "rules_claimed_satisfied"),
        vec!["1 rule(s): Preserve display label behavior"]
    );
    assert!(intent_details(&runner, "rules_deferred").is_empty());
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    let building = rules_section(&fixture.last_model_prompt("harness_action"));
    assert!(
        building.contains(&format!("\n[Requirement] Preserve display label behavior ({PRESERVE}) — plan says already satisfied; via: linked to labels.py\n")),
        "{building}"
    );
    assert!(building.contains("hasDescription: A label is a single line.\n"));

    fixture.conversational(json!({"action":"finish","summary":"Normalized."}));
    for _ in 0..6 {
        match runner.task.phase {
            Phase::AwaitingReview => runner.review(true).await.unwrap(),
            Phase::Verifying => break,
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.task.check_results = vec![passed_check()];
    fixture.note("Normalization lives in one helper.");
    runner.advance().await.unwrap();
    let request = fixture
        .shared
        .lock()
        .unwrap()
        .capture_type_requests
        .last()
        .cloned()
        .expect("the note was typed");
    assert_eq!(request.addressed_rules, vec![UNLINKED.to_string()]);
}

/// `MOOSEDEV_HARNESS_PLAN_SATISFIED=off` while alive.
struct SatisfiedOff;

impl SatisfiedOff {
    fn new() -> Self {
        std::env::set_var("MOOSEDEV_HARNESS_PLAN_SATISFIED", "off");
        Self
    }
}

impl Drop for SatisfiedOff {
    fn drop(&mut self) {
        std::env::remove_var("MOOSEDEV_HARNESS_PLAN_SATISFIED");
    }
}

/// A task resumed with `MOOSEDEV_HARNESS_PLAN_SATISFIED=off` whose journal
/// already holds `satisfied` claims behaves as before the field existed: the
/// claims settle no rule, leave none closed at approval, are not journaled or
/// kept on the approved plan, and are neither shown nor sent to the model.
#[tokio::test]
async fn switched_off_satisfied_ignores_the_claims_a_resumed_journal_holds() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    fixture.shared.lock().unwrap().governing_rules = vec![preserve_rule(), unlinked_rule()];
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"Add a normalize helper so labels never exceed one line","files":["labels.py"],"checks":["true"],"addresses":["[Constraint] Labels never exceed one line"],"satisfied":["Preserve display label behavior"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().satisfied,
        vec![PRESERVE.to_string()]
    );

    let _off = SatisfiedOff::new();
    // The gate's "Says N rule(s) already hold" line reads these.
    assert!(runner
        .task
        .plan
        .as_ref()
        .unwrap()
        .satisfied_claims()
        .is_empty());
    runner.approve_plan().await.unwrap();
    let claimed = intent_details(&runner, "rules_claimed_satisfied");
    let kept = runner.task.approved_plans[0].satisfied.clone();
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    let building = fixture.last_model_prompt("harness_action");
    assert!(claimed.is_empty(), "{claimed:?}");
    assert!(kept.is_empty(), "{kept:?}");
    let rules = rules_section(&building);
    assert!(
        rules.contains(&format!("({PRESERVE})\nvia: linked to labels.py\nhasDescription: Display labels render exactly as before.\n")),
        "{rules}"
    );
    assert!(!rules.contains("plan says already satisfied"), "{rules}");
    assert!(!building.contains("\"satisfied\""), "{building}");
}

/// Both switches off, the rules settle by nothing and plans carry no
/// `satisfied`: the planning prompt is byte-identical whether or not the
/// daemon says a decision settles a rule, and it reads as before.
struct RulesByStateOff;

impl RulesByStateOff {
    fn new() -> Self {
        std::env::set_var("MOOSEDEV_HARNESS_RULES_BY_STATE", "off");
        std::env::set_var("MOOSEDEV_HARNESS_PLAN_SATISFIED", "off");
        Self
    }
}

impl Drop for RulesByStateOff {
    fn drop(&mut self) {
        std::env::remove_var("MOOSEDEV_HARNESS_RULES_BY_STATE");
        std::env::remove_var("MOOSEDEV_HARNESS_PLAN_SATISFIED");
    }
}

#[tokio::test]
async fn switched_off_rules_by_state_and_satisfied_leave_the_prompt_as_before() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _off = RulesByStateOff::new();
    let plan = json!({"action":"plan","summary":"Add a helper","files":["labels.py"],"checks":["true"],"addresses":[],"satisfied":["Preserve display label behavior"]});
    let mut prompts = Vec::new();
    for decision in [None, Some("urn:ad:preserve")] {
        let fixture = symbolic_fixture().await;
        let rules = vec![preserve_rule(), unlinked_rule()];
        fixture.shared.lock().unwrap().governing_rules = match decision {
            Some(decision) => rules
                .into_iter()
                .map(|rule| decided(rule, decision))
                .collect(),
            None => rules,
        };
        let mut runner = fixture.interactive().await;
        fixture.conversational(plan.clone());
        runner.advance().await.unwrap();
        // The satisfied claim is dropped, so coverage sends the plan back.
        assert!(runner.task.plan.is_none(), "returned for the rules");
        assert!(intent_details(&runner, "rules_settled").is_empty());
        let request = requests_of_kind(&fixture, "model")
            .into_iter()
            .rfind(|request| request["schema"] == "harness_action")
            .unwrap();
        let tools = request["body"]["tools"].as_array().unwrap();
        let plan_tool = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "plan")
            .unwrap();
        assert!(plan_tool["function"]["parameters"]["properties"]
            .get("satisfied")
            .is_none());
        assert_eq!(
            plan_tool["function"]["description"],
            "Propose the plan: a summary, the permitted files, the required checks, the project rules it implements (addresses), any planned files it deliberately leaves as stubs for a later task (stubs), and any planned files it lists only for reference, which need no edit (unchanged)."
        );
        prompts.push(fixture.last_model_prompt("harness_action"));
    }
    assert_eq!(prompts[0], prompts[1]);
    let prompt = &prompts[0];
    assert!(!prompt.contains("satisfied"), "{prompt}");
    assert!(!prompt.contains("settled"), "{prompt}");
    assert!(prompt.contains("plan(summary,files,checks,addresses,stubs,unchanged,open_choices)"));
    let rules = rules_section(prompt);
    let before = format!("hard requirements for any change that touches them; for each, your plan says it implements the rule, that the rule does not apply to this change, or that it is deferred because it lies outside this objective; list only the ones it implements in addresses):\n\n[Requirement] Preserve display label behavior ({PRESERVE})\nvia: linked to labels.py\nhasDescription: Display labels render exactly as before.\n\n[Constraint] Labels never exceed one line ({UNLINKED})\nvia: linked to labels.py\nhasDescription: A label is a single line.\n");
    let output_rule = rules
        .strip_prefix(&before)
        .unwrap_or_else(|| panic!("{rules}"));
    assert!(
        output_rule.starts_with("Call exactly one tool") || output_rule.starts_with("Return"),
        "nothing follows the rules but the output rule: {output_rule}"
    );
    assert!(prompt.contains("\nYour plan summary must say, for each project rule, whether this change implements it, it does not apply, or it is deferred as outside this objective: Preserve display label behavior; Labels never exceed one line. List only the ones it implements in addresses."));
}
