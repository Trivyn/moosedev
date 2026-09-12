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
    assert_eq!(scope.checks, vec!["fixture-required-check".to_string()]);
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
async fn symbolic_replan_rederives_obligations_and_keeps_task_counters() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().scope_escapes = 2;
    fixture
        .conversational(json!({"action":"replan","reason":"Narrow the change to the helper only"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
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
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior in the helper","files":["labels.py"],"checks":["fixture-required-check"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.approved_change_scope.is_none());
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    let state = runner.task.symbolic.clone().unwrap();
    assert_eq!(state.scope_escapes, 2, "task-wide bounds survive a replan");
    assert_eq!(
        state.obligations.get("labels.py").unwrap(),
        &vec![UNLINKED.to_string(), PRESERVE.to_string()]
    );
    assert_eq!(intent_details(&runner, "obligations_derived").len(), 2);
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
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior and update the constant","files":["labels.py","other.py"],"checks":["fixture-required-check"]}));
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
async fn symbolic_first_noop_edit_runs_checks_and_the_second_repairs() {
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

    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    runner.task.symbolic.as_mut().unwrap().noop_continuations = 1;
    for _ in 0..3 {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name"}));
    }
    let error = runner.advance().await.unwrap_err();
    assert!(
        format!("{error:#}").contains("edit makes no change"),
        "{error:#}"
    );
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));
    assert!(intent_details(&runner, "noop_edit_continuation").is_empty());
    assert_eq!(intent_details(&runner, "repair_exhausted").len(), 1);
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
        "typed"
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
    fixture.typed(vec![TypedProposal {
        proposal: KnowledgeProposal {
            kind: "Requirement".into(),
            title: "Preserve display label behavior".into(),
            description: "Labels keep their display form.".into(),
            evidence: vec!["Event 1: capture note".into()],
            files: vec!["labels.py".into()],
            components: vec![],
            requirement: None,
            supersedes: None,
            retracts: None,
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
