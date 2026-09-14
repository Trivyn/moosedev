#![cfg(feature = "harness")]
//! Orchestration tests with a scripted sensor: the coding model answers only
//! `harness_action` and one final `harness_capture_note`; every other decision
//! comes from the scripted daemon. Executor confinement is tested separately;
//! completed-check fixtures below isolate the human-review and daemon-durability
//! completion gates.
use std::sync::Arc;

use moosedev::harness::protocol::*;
use moosedev::harness::runner::{CheckResult, Mode, Phase, Runner};
use serde_json::{json, Value};

#[path = "harness_runner/links.rs"]
mod links;
#[path = "harness_runner/mock.rs"]
mod mock;
#[path = "harness_runner/symbolic.rs"]
mod symbolic;

use mock::*;

#[tokio::test]
async fn first_edit_guard_and_deny_gate_precede_any_write() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Repair code.txt while preserving behavior".into(),
    )
    .await
    .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(fixture.model_calls(), 0);
    assert_eq!(
        fixture.shared.lock().unwrap().requests[0]["kind"],
        "context"
    );

    fixture.edit();
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("Plan mode"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );

    fixture.reply("harness_action", json!({"action":"plan","summary":"Make a localized behavior-preserving repair","files":["code.txt"],"checks":["false"]}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingReview,
        "a headless checkpoint asks for one no-change confirmation"
    );
    assert!(runner.task.plan.is_some());
    assert!(runner.task.capture_request.is_none());
    assert!(
        runner.approve_plan().await.is_err(),
        "the checkpoint confirmation precedes plan approval"
    );
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();

    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("First-edit guard")));
    assert!(runner.task.read_files.contains(&"code.txt".into()));

    fixture.shared.lock().unwrap().deny_edit = true;
    fixture.edit();
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("denied"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    fixture.shared.lock().unwrap().deny_edit = false;
    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    let prompt = fixture.last_model_prompt("harness_action");
    assert!(prompt.contains("COMPLETE_DOSSIER_FOR_code.txt") && prompt.contains("original"));
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn standing_guidance_is_snapshotted_capped_and_replayed() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let create = |objective: &str| {
        Runner::create(fixture.root.clone(), fixture.url.clone(), objective.into())
    };
    let loaded = |runner: &Runner| {
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "guidance_loaded")
            .map(|event| event.detail.clone())
            .collect::<Vec<_>>()
    };

    let absent = create("Repair code.txt").await.unwrap();
    let standing = absent.task.standing_guidance.clone().unwrap();
    assert_eq!(standing.source, "default");
    assert_eq!(
        standing.text,
        moosedev::harness::runner::DEFAULT_GUIDANCE.trim()
    );
    assert!(
        loaded(&absent)[0].starts_with("default, "),
        "{:?}",
        loaded(&absent)
    );
    // One harness owns a workspace at a time.
    drop(absent);

    let guidance = fixture.root.join(".moosedev/GUIDANCE.md");
    std::fs::write(&guidance, "Prefer small functions.\n").unwrap();
    let present = create("Repair code.txt").await.unwrap();
    let standing = present.task.standing_guidance.clone().unwrap();
    assert_eq!(
        (standing.source.as_str(), standing.text.as_str()),
        ("file", "Prefer small functions.")
    );
    assert!(loaded(&present)[0].contains(&standing.sha256));

    // Resume replays the snapshot even when the file changes afterwards.
    std::fs::write(&guidance, "Something else entirely.\n").unwrap();
    let id = present.task.id.clone();
    drop(present);
    let resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(
        resumed.task.standing_guidance.as_ref().unwrap().text,
        "Prefer small functions."
    );
    drop(resumed);

    std::fs::write(&guidance, "   \n").unwrap();
    let empty = create("Repair code.txt").await.unwrap();
    assert_eq!(
        empty.task.standing_guidance.as_ref().unwrap().source,
        "empty"
    );
    assert!(empty
        .task
        .standing_guidance
        .as_ref()
        .unwrap()
        .text
        .is_empty());

    drop(empty);
    std::fs::write(&guidance, "x".repeat(4097)).unwrap();
    let Err(error) = create("Repair code.txt").await else {
        panic!("an over-cap guidance file fails task creation");
    };
    let error = error.to_string();
    assert!(error.contains("4096 bytes"), "{error}");

    // A journal written before the guidance file resumes with the default.
    std::fs::remove_file(&guidance).unwrap();
    let old = create("Repair code.txt").await.unwrap();
    let id = old.task.id.clone();
    drop(old);
    let journal = fixture
        .root
        .join(format!(".moosedev/harness/tasks/{id}.json"));
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("standing_guidance");
    value["intent_events"] = json!([]);
    std::fs::write(&journal, serde_json::to_string(&value).unwrap()).unwrap();
    let legacy = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(
        legacy.task.standing_guidance.as_ref().unwrap().source,
        "default"
    );
    assert_eq!(loaded(&legacy).len(), 1);
}

#[tokio::test]
async fn frozen_capture_request_survives_restart_and_cancel() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().capture_links_per_proposal = 2;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The repair keeps the observed contract.");
    fixture.typed_one("Lesson", "Local repair evidence");
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(runner.advance().await.is_err());
    let frozen = runner.task.capture_request.clone().unwrap();
    assert_eq!(fixture.note_calls(), 1);
    assert!(runner.task.pending_capture.is_none());
    runner.cancel().await.unwrap();
    let id = runner.task.id.clone();
    drop(runner);

    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Verifying,
        "capture recovery precedes approval invalidation"
    );
    assert_eq!(
        runner.task.capture_request.as_ref().unwrap().operation_id,
        frozen.operation_id
    );
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "the retry reuses the frozen note and typing"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let captures = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].operation_id, frozen.operation_id);
    assert_eq!(
        serde_json::to_value(&captures[0]).unwrap(),
        serde_json::to_value(&captures[1]).unwrap()
    );
    assert_eq!(fixture.typing_ids().len(), 1);
    assert_eq!(runner.task.reviews.len(), 1);

    runner.review(false).await.unwrap();
    assert_eq!(intent_details(&runner, "review_interaction").len(), 1);
    assert_eq!(intent_details(&runner, "record_review").len(), 1);
    let link_events = intent_details(&runner, "link_review");
    assert_eq!(link_events.len(), 2);
    assert!(link_events
        .iter()
        .all(|detail| detail.contains("rejected https://moosedev.dev/kg/ProposedLink/")));
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn interrupted_command_intent_asks_a_human_never_replays() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let runner = fixture.approved_interactive().await;
    let id = runner.task.id.clone();
    // Simulate a crash after journaling a command intent but before observing
    // its outcome. Resumption must ask a human, never replay the command.
    let mut journal = journal_value(&runner);
    journal["intent"] = json!({"Command":"touch duplicated-marker"});
    journal["phase"] = json!("Working");
    let path = journal_path(&fixture, &id);
    drop(runner);
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut runner = reload(&fixture, &id);
    let calls = fixture.model_calls();
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("unknown"));
    assert!(!fixture.root.join("duplicated-marker").exists());
    assert_eq!(fixture.model_calls(), calls);
    assert!(runner.advance().await.is_err(), "parked until a human acts");
    assert!(!fixture.root.join("duplicated-marker").exists());
}

#[tokio::test]
async fn completed_verification_requires_human_confirmation_and_durable_checkpoint() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Inspect the existing code".into(),
    )
    .await
    .unwrap();
    fixture.reply("harness_action", json!({"action":"plan","summary":"Inspect code without changes","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"Inspection complete; require verification."}),
    );
    runner.advance().await.unwrap();
    // The executor's correctness is independent; provide its completed result
    // as the starting state for this completion-gate scenario.
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.pending_capture.is_none());
    assert_ne!(
        runner.task.phase,
        Phase::Complete,
        "the daemon's typing cannot replace human confirmation"
    );
    fixture.shared.lock().unwrap().checkpoint_durable = false;
    assert!(runner.confirm_no_knowledge().await.is_err());
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.confirm_no_knowledge().await.is_err());
    fixture.shared.lock().unwrap().checkpoint_durable = true;
    fixture.shared.lock().unwrap().revision = "accepted-v2".into();
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "changed knowledge must invalidate completion approval"
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(
        runner.task.check_results.is_empty(),
        "changed governing evidence invalidates the old verification"
    );
    runner.approve_plan().await.unwrap();
    fixture.reply("harness_action", json!({"action":"finish","summary":"Reviewed the updated governing knowledge; verify again."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![passed_check()];
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(
        fixture.note_calls(),
        1,
        "the stored note serves the repeated final checkpoint without a model call"
    );
    let typing = fixture.typing_ids();
    assert_eq!(
        typing.len(),
        2,
        "typing stored before the knowledge change is stale; the note is typed again at the new revision"
    );
    assert_ne!(typing[0], typing[1]);
    assert_eq!(
        intent_details(&runner, "capture_note_invalidated"),
        vec!["source or accepted knowledge changed".to_string()]
    );
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn rejected_typed_capture_is_retyped_with_fresh_ids_and_bounded() {
    let _env_lock = ENVIRONMENT.lock().await;
    // A title collision is retyped once under fresh identities and then lands.
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The repair keeps the observed contract.");
    fixture.typed_one("Lesson", "Local repair evidence");
    fixture.shared.lock().unwrap().collide_capture_once = true;
    runner.advance().await.unwrap();
    assert!(runner.task.last_error.is_none());
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert!(runner.task.capture_request.is_none());
    assert_eq!(
        intent_details(&runner, "capture_retyped").len(),
        1,
        "{:?}",
        runner.task.intent_events
    );
    assert!(intent_details(&runner, "capture_retyped")[0].starts_with("1 of 3: title collisions"));
    let note = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .clone()
        .unwrap();
    assert_eq!(note.status, "asked");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(fixture.note_calls(), 1, "the note is never asked again");
    let typing = fixture.typing_ids();
    let captures = fixture.capture_ids();
    assert_eq!(typing.len(), 2);
    assert_eq!(captures.len(), 2);
    assert_ne!(typing[0], typing[1]);
    assert_ne!(captures[0], captures[1]);
    assert_eq!(runner.task.reviews[0].request.operation_id, captures[1]);
    assert_eq!(runner.task.symbolic.as_ref().unwrap().retypes, 1);

    // Persistent pre-persistence rejections are bounded per task.
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The repair keeps the observed contract.");
    fixture.typed_one("Lesson", "Local repair evidence");
    fixture.shared.lock().unwrap().reject_capture = true;
    for retype in 1..=3 {
        runner.advance().await.unwrap();
        assert!(runner.task.last_error.is_none());
        assert_eq!(runner.task.phase, Phase::Verifying);
        assert!(runner.task.recovery.is_none(), "no repair budget is spent");
        assert_eq!(runner.task.symbolic.as_ref().unwrap().retypes, retype);
    }
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("Provide guidance"));
    assert_eq!(intent_details(&runner, "capture_retyped").len(), 3);
    assert_eq!(
        intent_details(&runner, "capture_retype_exhausted"),
        vec!["retype 4, bound 3"]
    );
    assert_eq!(fixture.note_calls(), 1);
    let typing = fixture.typing_ids();
    let captures = fixture.capture_ids();
    assert_eq!(typing.len(), 4);
    assert_eq!(captures.len(), 4);
    assert_eq!(
        typing
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4,
        "every retype types under a fresh operation id"
    );
    assert_eq!(
        captures
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    let note = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .clone()
        .unwrap();
    assert_eq!(note.note, "The repair keeps the observed contract.");
    assert!(runner.task.capture_request.is_none());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.symbolic.as_ref().unwrap().retypes, 4);
    assert!(runner.advance().await.is_err(), "parked until a human acts");
    assert_eq!(fixture.capture_ids().len(), 4);
}

#[tokio::test]
async fn final_typed_capture_is_reviewed_once_and_requires_a_durable_checkpoint() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":"changed\n"}),
    );
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.reviews.is_empty());
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"changed\n","after":"complete\n"}),
    );
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.reviews.is_empty());
    assert_eq!(
        intent_details(&runner, "capture_deferred").len(),
        3,
        "the plan and both edits each journaled one deferred checkpoint"
    );
    fixture.conversational(json!({"action":"finish","summary":"Ready for required checks."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![passed_check()];
    fixture.note("Both edits keep the public behavior.");
    fixture.typed_one("Lesson", "Observed change");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(runner.confirm_no_knowledge().await.is_err());
    let operation = runner.task.reviews[0].request.operation_id.clone();
    fixture.shared.lock().unwrap().checkpoint_durable = false;
    assert!(runner.review_operation(&operation, true).await.is_err());
    assert_eq!(runner.task.reviews.len(), 1);
    assert_ne!(runner.task.phase, Phase::Complete);
    fixture.shared.lock().unwrap().checkpoint_durable = true;
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "complete\n"
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn governing_typed_proposal_blocks_completion_until_reviewed() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Every caller must preserve the observed contract.");
    fixture.typed_one("Constraint", "Preserve the observed contract");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.reviews[0].request.has_governing());
    let calls = fixture.model_calls();
    assert!(runner.advance().await.is_err());
    assert_eq!(fixture.model_calls(), calls);
    assert!(runner.approve_plan().await.is_err());
    assert!(runner.confirm_no_knowledge().await.is_err());
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, false).await.unwrap();
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn steering_keeps_the_frozen_capture_request_and_retries_it_byte_identically() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The change is a local repair.");
    fixture.typed_one("Pattern", "Local repair");
    fixture.shared.lock().unwrap().fail_capture_once = true;
    assert!(runner.advance().await.is_err());
    let frozen = runner.task.capture_request.clone().unwrap();
    runner
        .submit_message("Stop changing code and explain the change.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        runner.task.capture_request.as_ref().unwrap().operation_id,
        frozen.operation_id
    );
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "the uncertain capture retry reuses its frozen request"
    );
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(runner.task.phase, Phase::Planning);
    let requests = fixture.shared.lock().unwrap().capture_requests.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        serde_json::to_value(&requests[0]).unwrap(),
        serde_json::to_value(&requests[1]).unwrap()
    );
    // The steering itself is a new checkpoint, journaled without a model call.
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(journal_value(&runner)["capture_due"], false);
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"changed\n","after":"forbidden\n"}),
    );
    assert!(runner
        .advance()
        .await
        .unwrap_err()
        .to_string()
        .contains("Plan mode"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains("Stop changing code and explain the change."));
    assert_eq!(runner.task.reviews.len(), 1);
    runner.request_review().unwrap();
    runner.review(false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
}

#[tokio::test]
async fn steering_after_no_change_confirmation_is_journaled_for_the_next_checkpoint() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(json!({"action":"finish","summary":"Ready for required verification."}));
    runner.advance().await.unwrap();
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let steering = "Preserve the public interface as a hard constraint for all future changes.";
    runner.submit_message(steering.into()).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        journal_value(&runner)["capture_due"],
        true,
        "the steering opens a new checkpoint"
    );
    let deferred = intent_details(&runner, "capture_deferred").len();
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(
        intent_details(&runner, "capture_deferred").len(),
        deferred + 1
    );
    assert_eq!(journal_value(&runner)["capture_due"], false);
    fixture.conversational(
        json!({"action":"reply","message":"Understood; the interface stays fixed."}),
    );
    runner.advance().await.unwrap();
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains(steering));
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_ne!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn large_observations_are_consumed_by_one_checkpoint_and_survive_restart() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let knowledge = format!(
        "Constraint: {} END_REQUIRED",
        "governing claim ".repeat(3000)
    );
    fixture.shared.lock().unwrap().context = Some(knowledge.clone());
    let mut runner = fixture.interactive().await;
    let evidence = format!(
        "Read code.txt: BEGIN_EVIDENCE\n{}\nEND_EVIDENCE",
        "λ😀\"\\\n".repeat(18000)
    );
    runner.task.events.push(moosedev::harness::runner::Event {
        message: evidence.clone(),
    });
    runner.task.last_response = "failed command observation\n".repeat(5000);
    runner.task.check_results = (0..20)
        .map(|index| CheckResult {
            command: format!("test-{index}"),
            success: false,
            output: "long failing test output\n".repeat(5000),
        })
        .collect();
    fixture.conversational(json!({"action":"plan","summary":"Inspect and repair within scope","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "one checkpoint consumes every observation without paging"
    );
    let saved = journal_value(&runner);
    assert_eq!(saved["capture_due"], false);
    let cursor = saved["capture_cursor"].as_u64().unwrap() as usize;
    assert_eq!(
        cursor + 1,
        runner.task.events.len(),
        "everything before the deferral notice is consumed"
    );
    assert!(runner.task.events[cursor]
        .message
        .starts_with("Capture checkpoint deferred"));
    let deferred = intent_details(&runner, "capture_deferred");
    assert_eq!(deferred.len(), 1);
    assert!(deferred[0].ends_with(" events"), "{deferred:?}");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message == evidence));
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    let script = fixture.shared.lock().unwrap();
    let requests: Vec<_> = script
        .requests
        .iter()
        .filter(|r| r["kind"] == "model")
        .collect();
    assert_eq!(requests.len(), 1);
    for request in &requests {
        let prompt = request["body"]["messages"][0]["content"].as_str().unwrap();
        assert!(
            prompt.len() <= 86016,
            "request overflowed: {}",
            prompt.len()
        );
        assert!(
            prompt.contains(&knowledge),
            "governing evidence must remain complete"
        );
    }
}

#[tokio::test]
async fn headless_pending_review_imports_into_interactive_with_checkpoint_bookkeeping() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Review a repair plan for code.txt".into(),
    )
    .await
    .unwrap();
    runner.configure(fixture.config(), None);
    fixture.reply("harness_action", json!({"action":"plan","summary":"Review the implementation","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.edit();
    runner.advance().await.unwrap();
    fixture.edit();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"The repair is ready for verification."}),
    );
    runner.advance().await.unwrap();
    runner.task.check_results = vec![passed_check()];
    fixture.note("The observed implementation detail warrants review.");
    fixture.typed_one("Lesson", "Review the observed implementation");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.pending_capture.is_some());
    let before = journal_value(&runner);
    let next_event = before["capture_end"].as_u64().unwrap();
    let operation = runner
        .task
        .capture_request
        .as_ref()
        .unwrap()
        .operation_id
        .clone();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.enable_interactive().unwrap();
    let imported = journal_value(&runner);
    assert_eq!(imported["capture_cursor"], next_event);
    assert_eq!(imported["capture_due"], false);
    assert!(imported["capture_end"].is_null());
    assert!(runner.task.capture_request.is_none());
    assert!(runner.task.pending_capture.is_none());
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(runner.task.reviews[0].request.operation_id, operation);
    let calls = fixture.model_calls();
    runner.review_operation(&operation, false).await.unwrap();
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn malformed_capture_note_stops_after_bounded_attempts_across_reload_and_resume() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    // Parsing and semantic validation share exactly three candidates for the
    // note, including across advance/resume calls.
    for _ in 0..40 {
        fixture.reply("harness_capture_note", json!("not a note object"));
    }
    for _ in 0..12 {
        let _ = runner.advance().await;
        if runner.task.phase == Phase::AwaitingInput {
            break;
        }
        runner.resume().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));
    let state = journal_value(&runner);
    assert_eq!(state["capture_due"], true);
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .capture_note
        .is_none());
    assert_eq!(
        fixture.note_calls(),
        3,
        "note attempts escaped their checkpoint budget"
    );
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(fixture.typing_ids().is_empty());
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    for _ in 0..3 {
        runner.resume().await.unwrap();
        assert!(runner.advance().await.is_err());
    }
    assert_eq!(
        fixture.model_calls(),
        calls,
        "resumption alone must not reset failed note attempts"
    );
    fixture.shared.lock().unwrap().replies.clear();
    runner
        .submit_message("Answer with one plain note about the repair.".into())
        .await
        .unwrap();
    assert!(runner.task.recovery.is_none());
    assert_eq!(runner.task.mode, Mode::Plan);
    // The interrupted checkpoint and then the steering itself are journaled
    // without any model call.
    for _ in 0..4 {
        if journal_value(&runner)["capture_due"] == false {
            break;
        }
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(journal_value(&runner)["capture_due"], false);
    assert_eq!(fixture.model_calls(), calls);
}

#[tokio::test]
async fn headless_final_no_change_review_requests_one_confirmation() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Review code.txt".into(),
    )
    .await
    .unwrap();
    runner.configure(fixture.config(), None);
    runner.task.events.push(moosedev::harness::runner::Event {
        message: "Observed implementation detail\n".repeat(10_000),
    });
    fixture.reply("harness_action", json!({"action":"plan","summary":"Review the observed file","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(journal_value(&runner)["capture_due"], false);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.confirm_no_knowledge().await.is_err());
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"Reviewed; run the required check."}),
    );
    runner.advance().await.unwrap();
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.pending_capture.is_none());
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        runner
            .task
            .events
            .iter()
            .filter(|event| event
                .message
                .contains("Human confirmed that no durable knowledge changed"))
            .count(),
        2
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn capture_typing_outages_preserve_the_note_and_spend_no_repair_budget() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Keep the repair local.");
    fixture.shared.lock().unwrap().fail_capture_type = true;
    let calls = fixture.model_calls();
    for _ in 0..4 {
        let error = runner.advance().await.unwrap_err();
        assert!(format!("{error:#}").contains("503"));
        assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
        assert_eq!(runner.task.phase, Phase::Verifying);
        assert!(runner.task.recovery.is_none());
        let note = runner
            .task
            .symbolic
            .as_ref()
            .unwrap()
            .capture_note
            .clone()
            .unwrap();
        assert_eq!(note.status, "asked");
        assert_eq!(note.note, "Keep the repair local.");
        assert!(runner.task.capture_request.is_none());
    }
    assert_eq!(fixture.model_calls(), calls + 1, "the note is asked once");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    fixture.shared.lock().unwrap().fail_capture_type = false;
    fixture.typed_one("Lesson", "Local repair");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(fixture.model_calls(), calls + 1);
    let typing = fixture.typing_ids();
    assert_eq!(typing.len(), 5);
    assert!(typing.iter().all(|id| id == &typing[0]));
    assert!(
        !runner
            .task
            .events
            .iter()
            .any(|event| event.message.starts_with("Human response:")),
        "service recovery must not require invented human guidance"
    );
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn capture_ack_outages_retry_the_frozen_operation_without_model_repairs() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Keep the observed repair.");
    fixture.typed_one("Lesson", "Keep the observed repair");
    fixture.shared.lock().unwrap().fail_capture = true;
    assert!(runner.advance().await.is_err());
    let request = runner.task.capture_request.clone().unwrap();
    let before = journal_value(&runner);
    let calls = fixture.model_calls();
    for _ in 0..3 {
        assert!(runner.advance().await.is_err());
        assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
    }
    assert!(runner.task.recovery.is_none());
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(fixture.typing_ids().len(), 1);
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    let restored = journal_value(&runner);
    for field in ["capture_cursor", "capture_end", "capture_checkpoint_end"] {
        assert_eq!(restored[field], before[field]);
    }
    fixture.shared.lock().unwrap().fail_capture = false;
    runner.advance().await.unwrap();
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(
        runner.task.reviews[0].request.operation_id,
        request.operation_id
    );
    assert_eq!(fixture.model_calls(), calls);
    let script = fixture.shared.lock().unwrap();
    assert_eq!(script.capture_requests.len(), 5);
    assert!(script.capture_requests.iter().all(|sent| {
        sent.operation_id == request.operation_id
            && sent.owner_id == id
            && sent.proposals == request.proposals
    }));
}

#[tokio::test]
#[ignore = "requires the explicitly configured local Gemma server at 127.0.0.1:1234"]
async fn live_local_model_reaches_enforced_plan_capture_without_editing() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    std::env::set_var("MOOSEDEV_LLM_BASE_URL", "http://127.0.0.1:1234/v1");
    std::env::set_var("MOOSEDEV_LLM_MODEL", "google/gemma-4-26b-a4b-qat");
    std::env::set_var("MOOSEDEV_LLM_STRUCTURED_OUTPUT", "auto");
    let mut runner = Runner::create(fixture.root.clone(), fixture.url.clone(),
        "Prepare a plan to replace the word original with changed in code.txt. Read the file first. The only permitted file is code.txt. Use a shell check that verifies its eventual contents. Stay in Plan mode; do not execute the change.".into()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        for _ in 0..8 {
            if matches!(
                runner.task.phase,
                Phase::AwaitingReview | Phase::AwaitingPlan
            ) {
                break;
            }
            runner.advance().await.unwrap_or_else(|error| {
                panic!("live-model step failed: {error:#}; task={:?}", runner.task)
            });
        }
    })
    .await
    .expect("live local model did not reach an enforced plan checkpoint within 120 seconds");
    assert!(
        matches!(
            runner.task.phase,
            Phase::AwaitingReview | Phase::AwaitingPlan
        ),
        "live model did not reach plan review: {:?}",
        runner.task
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert!(
        runner.task.plan.is_some(),
        "the plan must reach its checkpoint even with a real local model"
    );
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|request| request["kind"] == "context"
            && request["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file == "code.txt"))));
}

#[tokio::test]
#[ignore = "requires functional OS confinement; run explicitly outside a nested sandbox"]
async fn confined_successful_check_reaches_human_review_before_completion() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let _env = Env::configure(&fixture.url);
    let mut runner = Runner::create(
        fixture.root.clone(),
        fixture.url.clone(),
        "Verify that code.txt exists".into(),
    )
    .await
    .unwrap();
    fixture.reply("harness_action", json!({"action":"plan","summary":"Verify the existing file without editing it","files":["code.txt"],"checks":["test -f code.txt"]}));
    runner.advance().await.unwrap();
    runner.confirm_no_knowledge().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.reply(
        "harness_action",
        json!({"action":"finish","summary":"Run the required filesystem check."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.check_results.len(), 1);
    assert!(
        runner.task.check_results[0].success,
        "real confined check failed: {:?}",
        runner.task.check_results[0]
    );
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_ne!(runner.task.phase, Phase::Complete);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn interactive_greeting_and_model_question_fit_a_populated_project() {
    let fixture = Fixture::new().await;
    let knowledge = format!(
        "GOVERNING_EVIDENCE_BEGIN\n{}GOVERNING_EVIDENCE_END\n",
        "Constraint: Preserve the complete public contract and its recorded rationale.\n"
            .repeat(640)
    );
    assert!(knowledge.len() > 48_000);
    fixture.shared.lock().unwrap().context = Some(knowledge.clone());
    let mut path_bytes = 0;
    for index in 0..1000 {
        let path = format!(
            "components/long_named_subsystem/module_{:03}/implementation_details/behavior_contract_{index:04}.rs",
            index / 50
        );
        path_bytes += path.len() + 1;
        let path = fixture.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "// fixture source\n").unwrap();
    }
    assert!(path_bytes > 80_000);

    let mut runner = fixture.interactive_objective("Hello").await;
    fixture.conversational(json!({"action":"reply","message":"Hello! How can I help?"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert!(runner.task.plan.is_none());

    runner
        .submit_message("What model are you using".into())
        .await
        .unwrap();
    fixture.conversational(
        json!({"action":"reply","message":"The selected model is scripted-local-model."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.turn_finished);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.check_results.is_empty());

    let script = fixture.shared.lock().unwrap();
    let calls: Vec<_> = script
        .requests
        .iter()
        .filter(|r| r["kind"] == "model")
        .collect();
    assert_eq!(
        calls.len(),
        2,
        "replies are journaled without a capture assessment"
    );
    for call in calls {
        let prompt = call["body"]["messages"][0]["content"].as_str().unwrap();
        assert!(prompt.len() <= 86_016, "sent {} bytes", prompt.len());
        assert!(
            prompt.contains(&knowledge),
            "governing evidence must remain complete"
        );
        assert_eq!(call["schema"], "harness_action");
        assert!(prompt.contains("scripted-local-model"));
        assert!(
            prompt.contains("omitted"),
            "discovery omission must be explicit"
        );
        assert!(
            prompt.contains("code.txt"),
            "some navigation should remain available"
        );
    }
    assert!(script.replies.is_empty());
}

#[tokio::test]
async fn interactive_oversized_governing_evidence_blocks_before_model_call() {
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().context = Some(
        "Constraint: This complete governing evidence cannot be silently discarded.\n".repeat(1500),
    );
    let mut runner = fixture.interactive_objective("Hello").await;
    let error = runner.advance().await.unwrap_err().to_string();
    assert!(
        error.contains("context") && error.contains("budget"),
        "{error}"
    );
    assert_eq!(fixture.model_calls(), 0);
    assert!(runner.task.model_requests.is_empty());
    assert!(runner.task.plan.is_none());
    assert_ne!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn interactive_reply_needs_no_modification_plan_or_checks() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"reply","message":"The current file contains the original implementation."}));
    runner.advance().await.unwrap();
    assert!(runner.task.batch_capture);
    assert!(runner.task.turn_finished);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.check_results.is_empty());
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    assert_eq!(
        fixture.shared.lock().unwrap().requests[0]["kind"],
        "context"
    );
    runner
        .submit_message("Explain the behavior in more detail.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(!runner.task.turn_finished);
    fixture
        .conversational(json!({"action":"reply","message":"Here is the requested explanation."}));
    runner.advance().await.unwrap();
    assert!(runner.task.turn_finished);
    assert_eq!(fixture.note_calls(), 0);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn interactive_message_delivery_is_idempotent_after_restart() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let message_id = uuid::Uuid::new_v4().to_string();
    let message = "Explain the existing file before proposing changes.";
    runner
        .submit_message_once(&message_id, message.into())
        .await
        .unwrap();
    let task_id = runner.task.id.clone();
    let events = runner.task.events.len();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner
        .submit_message_once(&message_id, message.into())
        .await
        .unwrap();
    assert_eq!(runner.task.events.len(), events);
    assert_eq!(
        runner
            .task
            .events
            .iter()
            .filter(|event| event.message == format!("Human response: {message}"))
            .count(),
        1
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(runner.task.batch_capture);
    assert_eq!(fixture.model_calls(), 0);
}

#[tokio::test]
async fn search_reaches_paths_and_contents_beyond_navigation_preview() {
    let fixture = Fixture::new().await;
    for i in 0..2001 {
        std::fs::write(fixture.root.join(format!("file-{i:04}.txt")), "ordinary\n").unwrap();
    }
    std::fs::write(
        fixture.root.join("zz-hidden-target.txt"),
        "distinctive-content\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    for query in ["zz-hidden-target", "distinctive-content"] {
        fixture.conversational(json!({"action":"search","query":query}));
        runner.advance().await.unwrap();
        assert!(runner.task.last_response.contains("zz-hidden-target.txt"));
    }
    assert!(runner.task.last_response.contains("distinctive-content"));
}

#[tokio::test]
async fn inspect_pages_complete_journal_observations_without_replaying_actions() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let index = runner.task.events.len();
    runner.task.events.push(moosedev::harness::runner::Event {
        message: format!("{}hidden detail", "x".repeat(60000)),
    });
    fixture.conversational(json!({"action":"inspect","event":index,"offset":60000}));
    runner.advance().await.unwrap();
    assert!(runner.task.last_response.contains("hidden detail"));
    assert!(runner.task.plan.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn final_review_preserves_checks_only_for_attested_own_knowledge_changes() {
    let _env_lock = ENVIRONMENT.lock().await;
    for kind in ["Lesson", "Constraint", "Requirement"] {
        for attest in [true, false] {
            let fixture = Fixture::new().await;
            let mut runner = fixture.ready_for_final().await;
            fixture.note("The repair was verified.");
            fixture.typed_one(kind, "Verified repair observation");
            runner.advance().await.unwrap();
            assert_eq!(runner.task.phase, Phase::AwaitingReview);
            {
                let mut script = fixture.shared.lock().unwrap();
                script.attest_review = attest;
                script.revision_on_accept = Some("accepted-v2".into());
            }
            let id = runner.task.reviews[0].request.operation_id.clone();
            runner.review_operation(&id, true).await.unwrap();
            assert_eq!(
                fixture.shared.lock().unwrap().review_headers,
                vec![Some("accepted-v1".to_string())],
                "{kind}: the task's own final capture always asks for attestation"
            );
            if attest {
                assert_eq!(runner.task.phase, Phase::Complete, "{kind}");
                assert_eq!(runner.task.check_results.len(), 1, "{kind}");
                assert_eq!(runner.task.knowledge_revision, "accepted-v2");
                assert_eq!(intent_details(&runner, "final_review_attested").len(), 1);
            } else {
                assert_eq!(runner.task.phase, Phase::AwaitingPlan, "{kind}");
                assert!(
                    runner.task.check_results.is_empty(),
                    "unproven graph changes must invalidate verification"
                );
                assert!(intent_details(&runner, "final_review_attested").is_empty());
            }
        }
    }
}

#[tokio::test]
async fn accepted_governing_final_capture_completes_when_attested() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Display names must never exceed one line.");
    fixture.typed_one("Requirement", "Display names stay on one line");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.reviews[0].request.has_governing());
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
    }
    let approvals = intent_details(&runner, "plan_approved").len();
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(
        intent_details(&runner, "plan_approved").len(),
        approvals,
        "no second plan approval"
    );
    assert_eq!(runner.task.check_results.len(), 1, "no re-verification");
    assert_eq!(intent_details(&runner, "final_review_attested").len(), 1);
    assert_eq!(journal_value(&runner)["approved_revision"], "accepted-v2");
    if let Some(scope) = runner.task.approved_change_scope.as_ref() {
        assert_eq!(scope.knowledge_revision, "accepted-v2");
    }
    assert_eq!(fixture.note_calls(), 1);
    assert_eq!(fixture.typing_ids().len(), 1);
}

#[tokio::test]
async fn external_revision_bump_before_final_accept_is_not_credited() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The repair was verified.");
    fixture.typed_one("Lesson", "Verified repair observation");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.reject_stale_review = true;
        // Another client changed accepted knowledge while the card waited.
        script.revision = "outside-v2".into();
        script.revision_on_accept = Some("accepted-v3".into());
    }
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(
        fixture.shared.lock().unwrap().review_headers,
        vec![None],
        "a stale expected revision is never sent"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.check_results.is_empty());
    assert!(intent_details(&runner, "final_review_attested").is_empty());
}

#[tokio::test]
async fn lifecycle_final_capture_never_requests_attestation() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("This replaces the earlier repair lesson.");
    let mut typed = distinct_proposal("Lesson", "Replacement repair lesson");
    typed.proposal.supersedes = Some("urn:fixture:earlier-lesson".into());
    fixture.typed(vec![typed]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
    }
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(fixture.shared.lock().unwrap().review_headers, vec![None]);
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(intent_details(&runner, "final_review_attested").is_empty());
}

#[tokio::test]
async fn steered_governing_final_review_still_regates() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Every caller must preserve the observed contract.");
    fixture.typed_one("Constraint", "Preserve the observed contract");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
    }
    runner
        .submit_message("Also handle empty names before finishing.".into())
        .await
        .unwrap();
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(fixture.shared.lock().unwrap().review_headers, vec![None]);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(journal_value(&runner)["approved_revision"].is_null());
    assert!(intent_details(&runner, "final_review_attested").is_empty());
}

#[tokio::test]
async fn final_attestation_requires_passed_plan_checks() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("The repair was verified.");
    fixture.typed_one("Lesson", "Verified repair observation");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.task.check_results[0].success = false;
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
    }
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(fixture.shared.lock().unwrap().review_headers, vec![None]);
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(intent_details(&runner, "final_review_attested").is_empty());
}

#[tokio::test]
async fn failed_final_checkpoint_retries_without_claiming_a_no_knowledge_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    let scratch = fixture
        .root
        .join(".moosedev/harness/scratch")
        .join(&runner.task.id);
    std::fs::create_dir_all(scratch.join("build")).unwrap();
    std::fs::write(scratch.join("build/artifact"), "cached build").unwrap();
    fixture.note("The repair result was reviewed.");
    fixture.typed_one("Lesson", "Reviewed repair result");
    runner.advance().await.unwrap();
    let operation = runner.task.reviews[0].request.operation_id.clone();
    {
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision_on_accept = Some("accepted-v2".into());
        script.fail_global_checkpoint_once = true;
    }
    assert!(runner.review_operation(&operation, true).await.is_err());
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(journal_value(&runner)["completion_pending"], true);
    assert!(runner.confirm_no_knowledge().await.is_err());
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert!(
        !scratch.exists(),
        "completed tasks must release their source and build scratch"
    );
    assert_eq!(
        fixture.model_calls(),
        calls,
        "retry only completion checks, not capture or generation"
    );
    assert!(!runner.task.events.iter().any(|event| event
        .message
        .contains("Human confirmed that no durable knowledge changed")));
    assert_eq!(fixture.shared.lock().unwrap().reviewed, [operation]);
}

#[tokio::test]
async fn cancellation_cleans_scratch_and_keeps_the_task_resumable() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let scratch = fixture
        .root
        .join(".moosedev/harness/scratch")
        .join(&runner.task.id);
    std::fs::create_dir_all(scratch.join("build")).unwrap();
    std::fs::write(scratch.join("build/artifact"), "cached build").unwrap();
    runner.cancel().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Cancelled);
    assert!(!scratch.exists());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.plan.is_some());
}

#[tokio::test]
async fn oversized_plan_summary_is_rejected_before_becoming_required_prompt_state() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"plan","summary":"large summary ".repeat(10_000),"files":["code.txt"],"checks":["true"]}));
    assert!(runner.advance().await.is_err());
    assert!(runner.task.plan.is_none());
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(intent_details(&runner, "capture_deferred").is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn new_guidance_journals_the_discarded_policy_edit_without_applying_it() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original\n","after":null}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    assert!(runner.task.pending_edit.is_some());
    let prior_events = runner.task.events.len();
    runner
        .submit_message("Keep the file and explain it instead.".into())
        .await
        .unwrap();
    assert!(runner.task.pending_edit.is_none());
    assert_eq!(runner.task.mode, Mode::Plan);
    let id = runner.task.id.clone();
    drop(runner);
    let runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert!(
        runner.task.events[prior_events..].iter().any(|event| {
            let message = event.message.to_lowercase();
            (message.contains("discard") || message.contains("invalidat"))
                && message.contains("code.txt")
                && message.contains("original")
        }),
        "the durable journal must retain the discarded exact edit and why it was invalidated"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_cleanup_failure_survives_reload_and_explicit_cancel_or_resume_retry() {
    use fs2::FileExt;
    for resume_directly in [false, true] {
        let fixture = Fixture::new().await;
        let mut runner = fixture.interactive().await;
        let scratch = fixture
            .root
            .join(".moosedev/harness/scratch")
            .join(&runner.task.id);
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("cache-data"), "owned task cache").unwrap();
        let owner = std::fs::File::open(&scratch).unwrap();
        owner.try_lock_exclusive().unwrap();
        let error = runner.cancel().await.unwrap_err();
        assert!(format!("{error:#}").contains("Cancellation took effect"));
        assert_eq!(runner.task.phase, Phase::Cancelled);
        assert!(runner.task.cleanup_pending);
        assert!(runner
            .task
            .last_error
            .as_ref()
            .unwrap()
            .contains("scratch cleanup is pending"));
        assert!(runner.mode_plan().await.is_err());
        assert!(runner.request_review().is_err());
        assert!(runner
            .submit_message("Continue the work".into())
            .await
            .is_err());
        assert_eq!(runner.task.phase, Phase::Cancelled);
        let id = runner.task.id.clone();
        drop(runner);
        let mut runner = reload(&fixture, &id);
        assert!(runner.task.cleanup_pending);
        assert!(runner.resume().await.is_err());
        assert_eq!(runner.task.phase, Phase::Cancelled);
        assert!(scratch.join("cache-data").exists());
        FileExt::unlock(&owner).unwrap();
        drop(owner);
        if resume_directly {
            runner.resume().await.unwrap();
            assert_eq!(runner.task.phase, Phase::Planning);
        } else {
            runner.cancel().await.unwrap();
            assert_eq!(runner.task.phase, Phase::Cancelled);
        }
        assert!(!runner.task.cleanup_pending);
        assert!(!scratch.exists());
        assert!(runner.task.last_error.is_none());
        assert!(!runner
            .task
            .events
            .iter()
            .any(|event| event.message.starts_with("Human response:")));
    }
}

#[tokio::test]
async fn malformed_and_fragment_edit_share_one_budget_and_apply_once() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.shared.lock().unwrap().usage =
        Some(json!({"prompt_tokens":19,"completion_tokens":5,"total_tokens":24}));
    let start = runner.task.model_requests.len();
    fixture.reply("harness_action", json!("not an action object"));
    fixture.conversational(
        json!({"action":"edit","file":"code.txt","before":"original","after":"changed"}),
    );
    fixture.conversational(
        json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"changed"}),
    );
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(runner.task.edits.len(), 1);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.last_error.is_none());
    let requests = &runner.task.model_requests[start..];
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["attempt"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(requests
        .iter()
        .all(|r| r["decision_id"] == requests[0]["decision_id"]));
    assert!(requests[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("last candidate was rejected"));
    assert!(requests[2]["prompt"]
        .as_str()
        .unwrap()
        .contains("entire supplied file"));
    let metered = metered_decision(&runner, requests[0]["decision_id"].as_str().unwrap());
    assert_eq!(metered.len(), 3, "each correction is a physical request");
    assert_eq!(
        metered
            .iter()
            .map(|r| r["context"]["candidate"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        metered
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert!(metered.iter().all(|r| r["status"] == "completed"
        && r["context"]["purpose"] == "harness_action"
        && r["tokens"]["prompt_tokens"] == 19
        && r["tokens"]["completion_tokens"] == 5));
}

#[tokio::test]
async fn invalid_replacements_exhaust_without_write_and_restart_cannot_refill() {
    use moosedev::harness::runner::RecoveryStatus;
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    for old_text in ["", "missing", "not there"] {
        fixture.conversational(
            json!({"action":"replace","file":"code.txt","old_text":old_text,"new_text":"changed"}),
        );
    }
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(
        runner.task.recovery.as_ref().unwrap().status,
        RecoveryStatus::AwaitingGuidance
    );
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
    let calls = fixture.model_calls();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert!(runner.advance().await.is_err());
    assert_eq!(fixture.model_calls(), calls);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
}

#[tokio::test]
async fn full_write_uses_snapshot_and_deletion_still_requires_review() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.conversational(json!({"action":"write","file":"code.txt","content":"replacement\n"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits[0].before.as_deref(), Some("original\n"));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"write","file":"code.txt","content":null}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    assert_eq!(runner.task.edits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "replacement\n"
    );
    let pending = runner.task.pending_edit.as_ref().unwrap();
    assert_eq!(pending.before.as_deref(), Some("replacement\n"));
    assert_eq!(pending.after, None);
}

#[tokio::test]
async fn source_change_during_generation_requires_fresh_approval_without_edit() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    fixture.shared.lock().unwrap().mutation_during_model = Some("human change\n".into());
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"model change"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(runner.task.edits.is_empty());
    assert!(runner.task.pending_edit.is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "human change\n"
    );
}

#[tokio::test]
async fn cancellation_during_generation_preserves_charged_candidate_on_resume() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.approved_interactive().await;
    let received = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    fixture.shared.lock().unwrap().held_response = Some((received.clone(), release.clone()));
    fixture.conversational(json!({"action":"replace","file":"code.txt","old_text":"original","new_text":"must not apply"}));
    {
        let advance = runner.advance();
        tokio::pin!(advance);
        tokio::select! {
            result = &mut advance => panic!("held generation completed before cancellation: {result:?}"),
            waited = tokio::time::timeout(std::time::Duration::from_secs(10), received.notified()) => waited.expect("fixture did not receive model request"),
        }
        // Dropping the future now interrupts an actually received request, not
        // a filesystem write or scheduling delay before the HTTP call.
    }
    release.notify_one();
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 1);
    let decision = runner.task.recovery.as_ref().unwrap().id.clone();
    runner.cancel().await.unwrap();
    let interrupted = metered_decision(&runner, &decision);
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0]["status"], "cancelled");
    assert!(interrupted[0]["tokens"]["prompt_tokens"].is_null());
    assert!(interrupted[0]["tokens"]["completion_tokens"].is_null());
    assert!(runner.task.edits.is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    assert_eq!(metered_decision(&runner, &decision), interrupted);
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    for _ in 0..2 {
        fixture.reply("harness_action", json!("invalid action"));
    }
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    let metered = metered_decision(&runner, &decision);
    assert_eq!(metered.len(), 3, "reloading must not duplicate accounting");
    assert_eq!(
        metered
            .iter()
            .filter(|r| r["status"] == "cancelled")
            .count(),
        1
    );
    assert_eq!(
        metered
            .iter()
            .map(|r| r["context"]["candidate"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("code.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn journal_without_last_error_kind_deserializes() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let fresh = journal_value(&runner);
    assert!(
        fresh.get("last_error_kind").is_none(),
        "an absent kind is not serialized"
    );
    runner.task.last_error = Some("fixture failure".into());
    runner.task.last_error_kind = Some("service".into());
    let mut journal = journal_value(&runner);
    assert_eq!(journal["last_error_kind"], "service");
    journal.as_object_mut().unwrap().remove("last_error_kind");
    let legacy: moosedev::harness::runner::Task = serde_json::from_value(journal).unwrap();
    assert_eq!(legacy.last_error.as_deref(), Some("fixture failure"));
    assert!(legacy.last_error_kind.is_none());
}

#[tokio::test]
async fn prose_plan_checks_are_repaired_before_plan_approval() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"read","file":"code.txt"}));
    runner.advance().await.unwrap();
    // The smoke and campaign runs: a small model wrote what the checks should
    // verify instead of commands, and the shell answered exit 127.
    fixture.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["The implementation must preserve the original behavior."]}));
    fixture.conversational(json!({"action":"plan","summary":"Make a localized repair","files":["code.txt"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().checks,
        vec!["true".to_string()]
    );
    let rejected = intent_details(&runner, "plan_check_rejected");
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(rejected[0].contains("`The`"), "{rejected:?}");
    assert!(
        runner.task.events.iter().any(|event| event
            .message
            .starts_with("Correcting action, attempt 2 of 3")
            && event.message.contains("not a runnable shell command")),
        "the repair feedback names the invalid check"
    );
    assert!(runner.task.recovery.is_none());
}

#[tokio::test]
async fn accepted_governing_capture_is_not_retyped_after_approval_invalidation() {
    // Symbolic campaign v2, cell 7: accepting the final note's governing
    // proposal changed accepted knowledge, the approval was invalidated, and
    // the already-captured note was typed and submitted again until its own
    // accepted title collided and the retype budget parked a solved task.
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.ready_for_final().await;
    fixture.note("Every caller must preserve the observed contract.");
    fixture.typed_one("Constraint", "Preserve the observed contract");
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.reviews[0].request.has_governing());
    {
        // An outside write while the card waited: the acceptance cannot be
        // attested, so the approval is invalidated.
        let mut script = fixture.shared.lock().unwrap();
        script.attest_review = true;
        script.revision = "outside-v2".into();
        script.revision_on_accept = Some("accepted-v2".into());
    }
    let id = runner.task.reviews[0].request.operation_id.clone();
    runner.review_operation(&id, true).await.unwrap();
    assert_eq!(fixture.shared.lock().unwrap().review_headers, vec![None]);
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "unattested knowledge change"
    );
    runner.approve_plan().await.unwrap();
    fixture.conversational(
        json!({"action":"finish","summary":"Verify again under the accepted constraint."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    runner.task.check_results = vec![passed_check()];
    runner.advance().await.unwrap();
    assert_eq!(fixture.note_calls(), 1, "the note is asked once per task");
    assert_eq!(
        fixture.typing_ids().len(),
        1,
        "a captured note is never typed again"
    );
    assert_eq!(
        fixture.shared.lock().unwrap().capture_requests.len(),
        1,
        "a captured note is never submitted again"
    );
    assert!(intent_details(&runner, "capture_note_invalidated").is_empty());
    assert!(intent_details(&runner, "capture_retyped").is_empty());
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
}

#[tokio::test]
async fn search_returns_accepted_knowledge_before_repository_matches() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    fixture.shared.lock().unwrap().search_knowledge = Some(
        "[Constraint] Originals stay original (urn:fixture:search-knowledge)\nhasDescription: Never rename the original marker.\n"
            .into(),
    );
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"search","query":"original"}));
    runner.advance().await.unwrap();
    let response = runner.task.last_response.clone();
    assert!(
        response.starts_with("Accepted project knowledge for 'original' (authoritative):\n"),
        "{response}"
    );
    let knowledge = response
        .find("Never rename the original marker.")
        .unwrap_or_else(|| panic!("{response}"));
    let repository = response
        .find("Repository matches:\ncode.txt:1: original")
        .unwrap_or_else(|| panic!("{response}"));
    assert!(knowledge < repository, "{response}");
    assert_eq!(
        requests_of_kind(&fixture, "knowledge_search"),
        vec![json!({"kind":"knowledge_search","topic":"original"})]
    );
    assert_eq!(
        intent_details(&runner, "knowledge_search"),
        vec!["1 records, 1 repository matches: original"]
    );
}

#[tokio::test]
async fn search_with_no_knowledge_match_returns_repository_matches_only() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"search","query":"original"}));
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.last_response,
        "Repository matches:\ncode.txt:1: original\n"
    );
    fixture.conversational(json!({"action":"search","query":"zz-absent"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.last_response, "No matches.");
    assert_eq!(requests_of_kind(&fixture, "knowledge_search").len(), 2);
    assert_eq!(
        intent_details(&runner, "knowledge_search"),
        vec![
            "0 records, 1 repository matches: original",
            "0 records, 0 repository matches: zz-absent"
        ]
    );
}
