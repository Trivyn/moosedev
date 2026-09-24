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
    runner.advance().await.unwrap();
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
    runner.advance().await.unwrap();
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
async fn symbolic_noop_edit_runs_checks_unless_this_source_already_failed() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.last_error.is_none());
    assert!(runner.task.edits.is_empty());
    assert_eq!(intent_details(&runner, "noop_edit_continuation").len(), 1);
    assert_eq!(runner.task.symbolic.as_ref().unwrap().noop_continuations, 1);

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
    for _ in 0..3 {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"}));
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
    for _ in 0..3 {
        fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
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
    for _ in 0..3 {
        fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
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
    fixture.conversational(json!({"action":"finish","summary":"Nothing to change."}));
    runner.advance().await.unwrap();
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
        },
        GoverningRule {
            iri: UNLINKED.into(),
            label: "Labels never exceed one line".into(),
            kind: "Constraint".into(),
            claim: "hasDescription: A label is a single line.\n".into(),
            via: "via: linked to labels.py".into(),
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
