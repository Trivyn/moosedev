//! The reviewed code-link path: a derived association batch is frozen as an
//! `intent/link` operation, reviewed by a human, and its disposition journaled.
//! The daemon's proof checks are tested in harness_daemon; these verify the
//! orchestration around the link receipt, its review, and its abandonment.
use super::*;

#[tokio::test]
async fn malformed_link_response_never_becomes_a_review_card() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    // Both records govern `render_name`, so the new helper derives two bindings.
    fixture.shared.lock().unwrap().intent_accepted.push(
        moosedev::harness::daemon::intent::IntentBinding {
            record_iri: UNLINKED.into(),
            file: "labels.py".into(),
            symbol: RENDER_NAME.into(),
            source_digest: None,
        },
    );
    let mut runner = edited_symbolic_runner(&fixture).await;
    fixture.shared.lock().unwrap().malformed_intent_response = true;
    finish(&fixture);
    let error = runner.advance().await.unwrap_err();
    assert!(error.to_string().contains("one unique link"), "{error:#}");
    assert!(runner.task.reviews.is_empty());
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("other"));
    let persisted = journal_value(&runner);
    assert!(persisted["pending_intent_links"].is_object());
    let association = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .association
        .clone()
        .unwrap();
    assert_eq!(association.status, "awaiting_review");
    assert_eq!(association.page.bindings.len(), 2);
    let operation = association.link_operation_id.clone().unwrap();
    assert_eq!(link_operation_ids(&fixture), vec![operation.clone()]);

    // Once the daemon answers correctly the frozen request is retried
    // byte-identically and becomes exactly one review card.
    fixture.shared.lock().unwrap().malformed_intent_response = false;
    let calls = fixture.model_calls();
    runner.advance().await.unwrap();
    assert_eq!(
        fixture.model_calls(),
        calls,
        "the retry asks the model nothing"
    );
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    let links = runner.task.reviews[0].intent_links.clone().unwrap();
    assert_eq!(links.operation_id, operation);
    assert_eq!(links.bindings.len(), 2);
    assert_eq!(runner.task.reviews[0].response.proposals[0].links.len(), 2);
    assert_eq!(
        link_operation_ids(&fixture),
        vec![operation.clone(), operation]
    );
    let requests = fixture.shared.lock().unwrap().intent_link_requests.clone();
    assert_eq!(requests[0], requests[1]);
    assert!(journal_value(&runner)["pending_intent_links"].is_null());
}

#[tokio::test]
async fn steering_during_link_review_rederives_associations() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = edited_symbolic_runner(&fixture).await;
    fixture.shared.lock().unwrap().intent_link_status = Some(503);
    finish(&fixture);
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("daemon HTTP 503") && error.contains("intent/link"),
        "{error}"
    );
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
    assert_eq!(runner.task.phase, Phase::Working);
    let abandoned = runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .association
        .as_ref()
        .unwrap()
        .link_operation_id
        .clone()
        .unwrap();
    assert!(journal_value(&runner)["pending_intent_links"].is_object());

    // Human steering while the link operation is in flight abandons it at the
    // daemon and discards the derived batch with it.
    runner
        .submit_message("Also trim trailing dots in the helper.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(journal_value(&runner)["pending_intent_links"].is_null());
    assert!(
        runner.task.symbolic.as_ref().unwrap().association.is_none(),
        "an abandoned batch must not survive as awaiting_review"
    );
    let abandonments = intent_details(&runner, "intent_abandoned");
    assert_eq!(abandonments.len(), 1);
    assert!(abandonments[0].starts_with(&abandoned), "{abandonments:?}");
    assert!(abandonments[0].contains("new human guidance"));
    assert!(fixture.shared.lock().unwrap().reviewed.contains(&abandoned));
    assert_eq!(runner.task.edits.len(), 1, "the applied edit stays applied");

    // The next approved plan and finish derive the batch afresh under a new
    // link operation; the abandoned one is never retried.
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a trimming helper","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    finish(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.reviews.len(), 1);
    let fresh = runner.task.reviews[0]
        .intent_links
        .clone()
        .unwrap()
        .operation_id;
    assert_ne!(fresh, abandoned);
    assert_eq!(
        link_operation_ids(&fixture),
        vec![abandoned.clone(), fresh.clone()]
    );
    assert_eq!(intent_details(&runner, "association_derived").len(), 2);
    assert_eq!(
        fixture
            .shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request["kind"] == "intent_associate")
            .count(),
        2,
        "the batch is derived again, not replayed"
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
            .link_operation_id
            .as_deref(),
        Some(fresh.as_str())
    );

    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(runner.task.mode, Mode::Auto);
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(link_operation_ids(&fixture).len(), 2);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn steering_during_link_review_keeps_the_guidance_after_the_review() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = edited_symbolic_runner(&fixture).await;
    finish(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let operation = runner.task.reviews[0]
        .intent_links
        .clone()
        .unwrap()
        .operation_id;

    // Steering while the derived associations await review holds the review
    // and returns the task to Plan; the review must not undo that.
    let guidance = "Also trim trailing dots in the helper.";
    runner.submit_message(guidance.into()).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.reviews.len(), 1);
    assert!(journal_value(&runner)["approved_revision"].is_null());

    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert!(runner.task.reviews.is_empty());
    assert_eq!(
        runner.task.phase,
        Phase::Planning,
        "the accepted review resumes planning with the guidance, not verification"
    );
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(
        journal_value(&runner)["approved_revision"].is_null(),
        "a link disposition never re-approves a plan the human steered away from"
    );
    assert_eq!(journal_value(&runner)["guidance"], guidance);
    let reviews = intent_details(&runner, "link_review");
    assert_eq!(reviews.len(), 1);
    assert!(reviews[0].starts_with("accepted") && reviews[0].contains(&operation));
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

    // The next model step plans against the guidance.
    fixture.conversational(json!({"action":"plan","summary":"Trim trailing dots in the helper","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(fixture
        .last_model_prompt("harness_action")
        .contains(&format!("Current human guidance: {guidance}")));
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn last_error_kind_classifies_model_daemon_and_service_failures() {
    let _lock = ENVIRONMENT.lock().await;
    // (a) Three invalid replacements exhaust the repair budget: model output.
    let fixture = symbolic_fixture().await;
    let mut runner = planned_symbolic_runner(&fixture).await;
    runner.approve_plan().await.unwrap();
    assert!(runner.task.last_error_kind.is_none());
    for _ in 0..3 {
        fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"not in the file","new_text":"changed"}));
    }
    assert!(runner.advance().await.is_err());
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(runner.task.edits.is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let persisted: Value =
        serde_json::from_slice(&std::fs::read(journal_path(&fixture, &id)).unwrap()).unwrap();
    assert_eq!(persisted["last_error_kind"], "model_output");
    let runner = reload(&fixture, &id);
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("model_output"));

    // (b) A daemon 4xx on capture typing is a daemon rejection that spends no
    // repair budget and keeps the note.
    let fixture = symbolic_fixture().await;
    let mut runner = symbolic_task_ready_for_final_capture(&fixture).await;
    fixture.shared.lock().unwrap().capture_type_status = Some(400);
    fixture.note("Keep normalization in one helper.");
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("daemon HTTP 400") && error.contains("capture/type"),
        "{error}"
    );
    assert_eq!(
        runner.task.last_error_kind.as_deref(),
        Some("daemon_rejection")
    );
    assert_eq!(runner.task.last_error.as_deref(), Some(error.as_str()));
    assert!(runner.task.recovery.is_none());
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
        "asked"
    );
    assert!(runner.task.capture_request.is_none());

    // (c) A daemon 5xx on association derivation is a service failure; the
    // batch is derived on the next finish, not replayed.
    let fixture = symbolic_fixture().await;
    let mut runner = edited_symbolic_runner(&fixture).await;
    fixture.shared.lock().unwrap().associate_status = Some(500);
    finish(&fixture);
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("daemon HTTP 500") && error.contains("intent/associate"),
        "{error}"
    );
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.recovery.is_none());
    assert!(runner.task.symbolic.as_ref().unwrap().association.is_none());
    fixture.shared.lock().unwrap().associate_status = None;
    finish(&fixture);
    runner.advance().await.unwrap();
    assert!(runner.task.last_error_kind.is_none());
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(link_operation_ids(&fixture).len(), 1);
    assert_eq!(
        intent_details(&runner, "association_derived").len(),
        1,
        "only the successful derivation is journaled"
    );
}

/// Every persisted snapshot of a full symbolic task, including a restart and
/// human steering during the link review, is checked by the background
/// poller and at each step boundary: a plan is never awaiting approval while
/// a capture checkpoint is still due.
#[tokio::test]
async fn no_persisted_state_is_awaiting_plan_with_capture_due() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let watch = JournalWatch::start(&fixture);
    let mut runner = planned_symbolic_runner(&fixture).await;
    assert_journal_invariant(&fixture, "plan checkpoint");
    runner.approve_plan().await.unwrap();
    assert_journal_invariant(&fixture, "approval");
    add_helper(&fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_journal_invariant(&fixture, "edit checkpoint");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_journal_invariant(&fixture, "resume");
    finish(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_journal_invariant(&fixture, "link review pending");
    runner
        .submit_message("Looks right; keep the helper name.".into())
        .await
        .unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_journal_invariant(&fixture, "steering during link review");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_journal_invariant(&fixture, "resume with link review");
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    // Steering is never implicit approval: the accepted review resumes the
    // planning the steering started, and the human approves the plan again.
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_journal_invariant(&fixture, "link review accepted");
    fixture.conversational(json!({"action":"plan","summary":"Keep the helper name as implemented","files":["labels.py"],"checks":["true"]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_journal_invariant(&fixture, "replan after steering");
    runner.approve_plan().await.unwrap();
    assert_journal_invariant(&fixture, "second approval");
    finish(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Verifying,
        "the resolved association batch is not derived again for unchanged edits"
    );
    runner.task.check_results = vec![passed_check()];
    fixture.note("nothing beyond the diff");
    fixture.typed(vec![]);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_journal_invariant(&fixture, "final checkpoint");
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_journal_invariant(&fixture, "completion");
    assert_eq!(watch.finish(), Vec::<String>::new());
}
