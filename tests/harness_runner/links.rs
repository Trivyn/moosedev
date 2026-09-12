//! The reviewed code-link path: a derived association batch is frozen as an
//! `intent/link` operation, reviewed by a human, and its disposition journaled.
//! The daemon's proof checks are tested in harness_daemon; these verify the
//! orchestration around the link receipt, its review, and its abandonment.
use super::symbolic::{
    add_helper, planned_symbolic_runner, symbolic_fixture, symbolic_task_ready_for_final_capture,
    RENDER_NAME, UNLINKED,
};
use super::*;
use moosedev::harness::daemon::intent::{
    IntentEntity, IntentLinkRequest, IntentLinkResponse, IntentResolveRequest,
    IntentResolveResponse,
};
use sha2::{Digest, Sha256};

/// Mock of `intent/resolve`: every `def` in a plan file is an indexed Function
/// whose dossier records are the bindings the human has accepted for it.
pub(super) async fn resolve(
    State(state): State<Shared>,
    Json(request): Json<IntentResolveRequest>,
) -> Json<IntentResolveResponse> {
    let mut script = state.lock().unwrap();
    script.requests.push(json!({"kind":"intent_resolve","refresh_index":request.refresh_index,"files":request.files}));
    let mut entities = Vec::new();
    for file in &request.files {
        let Ok(content) = std::fs::read_to_string(script.root.join(file)) else {
            continue;
        };
        let mut names: Vec<_> = content
            .lines()
            .filter_map(|line| {
                line.strip_prefix("def ")?
                    .split_once('(')
                    .map(|(name, _)| name.to_owned())
            })
            .collect();
        // Non-code fixture bodies still expose one synthetic indexed function.
        if names.is_empty() {
            names.push("render_name".into());
        }
        for name in names {
            let symbol = format!("scip-python python fixture . {file}/{name}().");
            entities.push(IntentEntity {
                handle: format!("entity_{}", entities.len()),
                symbol: symbol.clone(),
                file: file.clone(),
                name,
                source_digest: if script.intent_stale_source {
                    "stale-digest".into()
                } else {
                    format!("{:x}", Sha256::digest(content.as_bytes()))
                },
                dossier_records: script
                    .intent_accepted
                    .iter()
                    .filter(|binding| {
                        &binding.file == file && binding.symbol.as_ref() == Some(&symbol)
                    })
                    .map(|binding| binding.record_iri.clone())
                    .collect(),
            });
        }
    }
    Json(IntentResolveResponse {
        revision: script.revision.clone(),
        records: script.intent_records.clone(),
        entities,
        unresolved: Vec::new(),
    })
}

/// Mock of `intent/link`: one proposed link per binding, recorded before any
/// scripted failure so a retry can be compared with the interrupted attempt.
pub(super) async fn link(
    State(state): State<Shared>,
    Json(request): Json<IntentLinkRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"intent_link","operation_id":request.operation_id}));
    script.intent_link_requests.push(request.clone());
    if let Some(status) = script.intent_link_status.take() {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":"scripted link failure"})),
        );
    }
    let mut links: Vec<_> = request
        .bindings
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                "https://moosedev.dev/kg/ProposedLink/{}-{index}",
                request.operation_id
            )
        })
        .collect();
    if script.malformed_intent_response && links.len() > 1 {
        links[1] = links[0].clone();
    }
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(IntentLinkResponse {
                links,
                resolved: request.bindings,
                unresolved: Vec::new(),
            })
            .unwrap(),
        ),
    )
}

fn journal_path(fixture: &Fixture, id: &str) -> PathBuf {
    fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"))
}

fn reload(fixture: &Fixture, id: &str) -> Runner {
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), id).unwrap();
    runner.configure(fixture.config(), None);
    runner
}

/// Approved symbolic plan with the helper edit applied and its intermediate
/// checkpoint journaled: the next `finish` derives the association batch.
async fn edited_symbolic_runner(fixture: &Fixture) -> Runner {
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    add_helper(fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    runner
}

fn finish(fixture: &Fixture) {
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
}

fn link_operation_ids(fixture: &Fixture) -> Vec<String> {
    fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .iter()
        .map(|request| request.operation_id.clone())
        .collect()
}

#[tokio::test]
async fn malformed_link_response_never_becomes_a_review_card() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    // Both records govern `render_name`, so the new helper derives two bindings.
    fixture.shared.lock().unwrap().intent_accepted.push(
        moosedev::harness::daemon::intent::IntentBinding {
            record_iri: UNLINKED.into(),
            file: "labels.py".into(),
            symbol: Some(RENDER_NAME.into()),
            planned_name: None,
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
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a trimming helper","files":["labels.py"],"checks":["fixture-required-check"]}));
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

fn journal_snapshots(dir: &std::path::Path) -> Vec<(PathBuf, Value)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let journal = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some((path, journal))
        })
        .collect()
}

fn awaiting_plan_with_capture_due(snapshots: &[(PathBuf, Value)]) -> Vec<String> {
    snapshots
        .iter()
        .filter(|(_, journal)| journal["phase"] == "AwaitingPlan" && journal["capture_due"] == true)
        .map(|(path, journal)| {
            format!(
                "{}: phase={} capture_due={} mode={}",
                path.display(),
                journal["phase"],
                journal["capture_due"],
                journal["mode"]
            )
        })
        .collect()
}

fn assert_journal_invariant(fixture: &Fixture, step: &str) {
    let snapshots = journal_snapshots(&fixture.root.join(".moosedev/harness/tasks"));
    assert!(!snapshots.is_empty(), "no journal persisted after {step}");
    let violations = awaiting_plan_with_capture_due(&snapshots);
    assert!(violations.is_empty(), "after {step}: {violations:?}");
}

struct JournalWatch {
    stop: Arc<std::sync::atomic::AtomicBool>,
    observed: Arc<std::sync::atomic::AtomicUsize>,
    violations: Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl JournalWatch {
    fn start(fixture: &Fixture) -> Self {
        let dir = fixture.root.join(".moosedev/harness/tasks");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let violations = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let stop = stop.clone();
            let observed = observed.clone();
            let violations = violations.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let snapshots = journal_snapshots(&dir);
                    observed.fetch_add(snapshots.len(), std::sync::atomic::Ordering::SeqCst);
                    violations
                        .lock()
                        .unwrap()
                        .extend(awaiting_plan_with_capture_due(&snapshots));
                    std::thread::sleep(std::time::Duration::from_micros(250));
                }
            })
        };
        Self {
            stop,
            observed,
            violations,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
        assert!(
            self.observed.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "the poller never observed a persisted journal"
        );
        let mut violations = self.violations.lock().unwrap().clone();
        violations.dedup();
        violations
    }
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
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_journal_invariant(&fixture, "link review accepted");
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
