//! Cross-boundary intent tests use the same scripted sensor and filesystem as
//! the existing runner tests. The actual daemon's proof checks are tested in
//! harness_daemon; these verify orchestration never edits past a failed gate.
use super::*;
use moosedev::harness::daemon::intent::{
    IntentEntity, IntentLinkRequest, IntentLinkResponse, IntentResolveRequest,
    IntentResolveResponse,
};
use moosedev::harness::runner::IntentPolicy;
use sha2::{Digest, Sha256};

pub(super) async fn purpose_candidates(
    State(state): State<Shared>,
    Json(request): Json<PurposeCandidateRequest>,
) -> (StatusCode, Json<Value>) {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"purpose_candidates","request":request}));
    if let Some(status) = script.purpose_candidates_status {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":"fixture purpose lookup failure"})),
        );
    }
    let candidates = script
        .intent_records
        .iter()
        .enumerate()
        .map(|(index, record)| PurposeCandidate {
            handle: format!("r{index}"),
            iri: record.iri.clone(),
            kind: record.kind.clone(),
            title: record.label.clone(),
            claim: CompleteClaim {
                literals: vec![CandidateLiteral {
                    predicate: "https://moosedev.dev/kg/description".into(),
                    value: record.label.clone(),
                    datatype: None,
                    language: None,
                }],
            },
            lifecycle: "accepted".into(),
            assertion_digest: format!("digest-{index}"),
            relations: vec![],
            legal_predicates: vec![if record.kind == "Constraint" {
                "https://moosedev.dev/kg/constrains".into()
            } else {
                "https://moosedev.dev/kg/concerns".into()
            }],
        })
        .collect::<Vec<_>>();
    let retrieval = script.purpose_retrieval_override.clone().unwrap_or({
        if candidates.is_empty() {
            PurposeRetrieval::ExhaustedEmpty
        } else {
            PurposeRetrieval::Page
        }
    });
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(PurposeCandidatePage {
                revision: script.revision.clone(),
                retrieval,
                candidates,
                next_cursor: None,
            })
            .unwrap(),
        ),
    )
}

pub(super) async fn postedit_candidates(
    State(state): State<Shared>,
    Json(request): Json<IntentCandidateRequest>,
) -> Json<IntentCandidatePage> {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"postedit_candidates","request":request}));
    let mut candidates = Vec::new();
    for changed in &request.files {
        let content = std::fs::read_to_string(script.root.join(&changed.file)).unwrap_or_default();
        let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
        let name = script
            .postedit_symbol_name
            .as_deref()
            .unwrap_or("render_name");
        let symbol = format!("scip-python python fixture . {}/{}().", changed.file, name);
        let existing_record_iris = script
            .intent_accepted
            .iter()
            .filter(|binding| {
                binding.file == changed.file && binding.symbol.as_deref() == Some(&symbol)
            })
            .map(|binding| binding.record_iri.clone())
            .collect();
        let record_choices = script
            .intent_records
            .iter()
            .enumerate()
            .map(|(index, record)| PurposeCandidate {
                handle: format!("r{index}"),
                iri: record.iri.clone(),
                kind: record.kind.clone(),
                title: record.label.clone(),
                claim: CompleteClaim {
                    literals: vec![CandidateLiteral {
                        predicate: "https://moosedev.dev/kg/description".into(),
                        value: record.label.clone(),
                        datatype: None,
                        language: None,
                    }],
                },
                lifecycle: "accepted".into(),
                assertion_digest: format!("digest-{index}"),
                relations: vec![],
                legal_predicates: vec!["https://moosedev.dev/kg/concerns".into()],
            })
            .collect();
        let conservative = script.postedit_conservative;
        candidates.push(PostEditCandidate {
            id: format!("e{}", candidates.len()),
            file: changed.file.clone(),
            symbol: (!conservative).then_some(symbol),
            name: (!conservative).then_some(name.into()),
            definition_range: (!conservative)
                .then(|| changed.changed_ranges.first().copied())
                .flatten(),
            enclosing_range: None,
            source_digest: digest,
            scope_basis: if conservative {
                IntentScopeBasis::ConservativeFile
            } else {
                IntentScopeBasis::ChangedDefinition
            },
            candidate_digest: format!("candidate-{}", changed.file),
            existing_record_iris,
            record_choices,
        });
    }
    Json(IntentCandidatePage {
        knowledge_revision: script.revision.clone(),
        index: IntentIndexSnapshot {
            revision: Some("index-v1".into()),
            producer: Some("fixture".into()),
            status: IntentIndexStatus::Current,
            refresh_action: IntentRefreshAction::Refreshed,
        },
        scope_digest: "scope-v1".into(),
        candidates,
        deleted: vec![],
        unresolved: vec![],
        next_cursor: None,
    })
}

async fn intent_fixture() -> Fixture {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join("labels.py"), "original\n").unwrap();
    fixture
}

async fn current_runner_ready_for_postedit(fixture: &Fixture) -> Runner {
    seed(fixture);
    let mut runner = fixture.interactive().await;
    runner.task.postedit_association_contract = 1;
    runner.set_intent_policy(IntentPolicy::Current).unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"original\n","new_text":"def render_name(name):\n    return name.strip()\n"}));
    runner.advance().await.unwrap();
    fixture.no_capture();
    runner.advance().await.unwrap();
    runner
}

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
        // The earlier minimal gate fixtures use a non-code text body while
        // providing one explicit synthetic indexed function.
        if names.is_empty() {
            names.push("render_name".into());
        }
        for name in names {
            let symbol = format!("scip-python python fixture . {file}/{name}().");
            let purpose_scoped = script.purpose_scoped_entities
                && script
                    .purpose_scoped_name
                    .as_deref()
                    .is_none_or(|expected| expected == name);
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
                    .chain(purpose_scoped.then_some(()).into_iter().flat_map(|_| {
                        script
                            .intent_records
                            .iter()
                            .map(|record| record.iri.clone())
                    }))
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

pub(super) async fn link(
    State(state): State<Shared>,
    Json(request): Json<IntentLinkRequest>,
) -> Json<IntentLinkResponse> {
    let mut script = state.lock().unwrap();
    script
        .requests
        .push(json!({"kind":"intent_link","operation_id":request.operation_id}));
    script.intent_link_requests.push(request.clone());
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
    Json(IntentLinkResponse {
        links,
        resolved: request.bindings,
        unresolved: Vec::new(),
    })
}

fn seed(fixture: &Fixture) {
    fixture.shared.lock().unwrap().intent_records = vec![CaptureTarget {
        iri: "https://moosedev.dev/kg/Requirement/preserve-labels".into(),
        label: "Preserve display label behavior".into(),
        kind: "Requirement".into(),
    }];
}

fn mapping(missing: bool, planned: bool) -> Value {
    json!({"purpose":if missing { vec![] } else { vec!["r0"] }, "obligations":[],
        "targets":[{"file":"labels.py","entity":if planned {"_normalize"} else {"entity_0"},"planned":planned,
            "records":if missing { vec![] } else { vec!["r0"] }}],
        "missing":if missing { Some("The public task introduces behavior with no recorded purpose.") } else { None }})
}

fn plan(fixture: &Fixture, mapping: Value) {
    fixture.conversational(
        json!({"action":"plan", "summary":"Preserve behavior with one shared helper",
        "files":["labels.py"],"checks":["fixture-required-check"],"change_intent":mapping}),
    );
    fixture.no_capture();
}

async fn v2_missing_checkpoint(fixture: &Fixture, seed_records: bool) -> Runner {
    if seed_records {
        seed(fixture);
    }
    let mut runner = fixture
        .interactive_objective("Preserve display behavior while repairing labels.py")
        .await;
    runner.task.postedit_association_contract = 1;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"missing","grounded_reason":"No supplied accepted record establishes the requested behavior."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        true
    );
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    runner
}

fn capture_unrelated_label_lesson(fixture: &Fixture) {
    fixture.reply("harness_capture", json!({"reason":"Retain an unrelated implementation observation for normal review.","proposals":[{
        "kind":"Lesson","title":"Unrelated label implementation observation","description":"The task exposed a local implementation detail.",
        "evidence":["Capture assessment:"],"files":["labels.py"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
}

#[tokio::test]
async fn v2_missing_checkpoint_empty_inventory_enters_durable_guidance_after_restart() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let runner = v2_missing_checkpoint(&fixture, false).await;
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner
        .task
        .last_response
        .contains("accepted-purpose inventory is empty"));
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    assert_eq!(
        intent_events(&runner, "purpose_missing_unresolved").len(),
        1
    );
    assert!(
        intent_events(&runner, "purpose_missing_rounds_exhausted").is_empty(),
        "an empty inventory is unresolved, not a consecutive-cycle exhaustion"
    );
}

#[tokio::test]
async fn v2_missing_checkpoint_keeps_unrelated_review_then_reselects_fresh_purpose() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    capture_unrelated_label_lesson(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    let operation = runner.task.reviews[0].request.operation_id.clone();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted display requirement governs the plan."}));
    fixture.reply("harness_purpose_selection", json!({"decision":"done","rationale":"The governing purpose is selected from the refreshed inventory."}));
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "ready"
    );
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
}

#[tokio::test]
async fn v2_missing_checkpoint_keeps_reuse_review_through_restart_before_refresh() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    fixture.shared.lock().unwrap().capture_candidates = vec![CaptureCandidate {
        iri: "urn:existing-lesson".into(),
        title: "Observed label behavior".into(),
        kind: "Lesson".into(),
        status: "accepted".into(),
        assertion_digest: "lesson-digest".into(),
        literals: vec![CandidateLiteral {
            predicate: "hasDescription".into(),
            value: "The task exposed a local implementation detail.".into(),
            datatype: None,
            language: None,
        }],
        relations: vec![],
        origin: None,
        owned_by_requester: false,
        exact_title: true,
        legal_relations: vec![],
    }];
    fixture.reply("harness_capture", json!({"reason":"The observation may already be represented.","proposals":[{
        "kind":"Lesson","title":"Observed label behavior","description":"The task exposed a local implementation detail.",
        "evidence":["Capture assessment:"],"files":["labels.py"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    fixture.reply("harness_capture_resolution", json!({"disposition":"reuse_unchanged","candidate_id":"c0","rationale":"The complete claim and relationships are unchanged.","revised_title":null,"revised_description":null}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert!(runner.task.reviews[0].capture_resolution.is_some());
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    let operation = runner.task.reviews[0].request.operation_id.clone();
    let id = runner.task.id.clone();
    drop(runner);
    let journal = fixture
        .root
        .join(".moosedev/harness/tasks")
        .join(format!("{id}.json"));
    let mut persisted: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    persisted["events"]
        .as_array_mut()
        .unwrap()
        .push(json!({"message":"Human response: inspect the later checkpoint page."}));
    let checkpoint_end = persisted["events"].as_array().unwrap().len();
    persisted["capture_due"] = json!(true);
    persisted["capture_checkpoint_end"] = json!(checkpoint_end);
    persisted["capture_files"] = json!(["labels.py"]);
    std::fs::write(&journal, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    let capture_calls = fixture
        .shared
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| request["schema"] == "harness_capture")
        .count();
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    fixture.no_capture();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted display requirement governs the plan."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"The refreshed governing purpose is selected."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "ready"
    );
    assert!(fixture.shared.lock().unwrap().capture_requests.is_empty());
    assert_eq!(
        fixture
            .shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request["schema"] == "harness_capture")
            .count(),
        capture_calls + 1,
        "the retained later checkpoint page is assessed exactly once"
    );
}

#[tokio::test]
async fn v2_missing_checkpoint_refresh_error_preserves_marker_for_resume() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    capture_unrelated_label_lesson(&fixture);
    runner.advance().await.unwrap();
    let operation = runner.task.reviews[0].request.operation_id.clone();
    let missing_rounds =
        serde_json::to_value(&runner.task).unwrap()["intent_missing_rounds"].clone();
    fixture.shared.lock().unwrap().fail_context = true;
    assert!(runner.review_operation(&operation, true).await.is_err());
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_missing_rounds"],
        missing_rounds
    );
    let id = runner.task.id.clone();
    drop(runner);
    fixture.shared.lock().unwrap().fail_context = false;
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted display requirement governs the plan."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"The purpose is selected after recovery."}),
    );
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "ready"
    );
}

#[tokio::test]
async fn v2_repeated_semantic_missing_is_bounded_by_consecutive_cycles() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    for round in 2..=3 {
        fixture.no_capture();
        fixture.reply(
            "harness_purpose_selection",
            json!({"decision":"missing","grounded_reason":format!("No applicable governing record in semantic round {round}.")}),
        );
        runner.advance().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["capture_due"],
        false
    );
    assert!(runner
        .task
        .last_response
        .contains("Three semantic purpose-selection cycles"));
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    assert_eq!(
        intent_events(&runner, "purpose_missing_rounds_exhausted").len(),
        1
    );
    assert_eq!(intent_events(&runner, "intent_missing").len(), 3);
}

#[tokio::test]
async fn v2_missing_checkpoint_rejects_empty_page_without_exhaustion_proof() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = v2_missing_checkpoint(&fixture, false).await;
    fixture.no_capture();
    fixture.shared.lock().unwrap().purpose_retrieval_override = Some(PurposeRetrieval::Page);
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(error.contains("empty without an exhausted-empty proof"));
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
}

async fn treatment(fixture: &Fixture) -> Runner {
    let mut runner = fixture.interactive().await;
    runner.set_intent_policy(IntentPolicy::ChangeLevel).unwrap();
    runner
}

#[tokio::test]
async fn missing_purpose_returns_to_planning_without_edit_and_survives_resume() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = treatment(&fixture).await;
    plan(&fixture, mapping(true, true));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_missing_rounds"],
        1
    );
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.intent_policy, IntentPolicy::ChangeLevel);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_missing_rounds"],
        1
    );
    // Repeated missing intent has a bounded, human-usable exit, not an
    // approval loop. No source mutation is attempted by any of these plans.
    for _ in 0..2 {
        plan(&fixture, mapping(true, true));
        runner.advance().await.unwrap();
        runner.approve_plan().await.unwrap();
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner.task.last_response.contains("Human guidance"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn withdrawn_purpose_invalidates_plan_before_source_mutation() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = treatment(&fixture).await;
    plan(&fixture, mapping(false, true));
    runner.advance().await.unwrap();
    {
        let mut script = fixture.shared.lock().unwrap();
        script.intent_records.clear();
        script.revision = "accepted-v2".into();
    }
    assert!(runner
        .approve_plan()
        .await
        .unwrap_err()
        .to_string()
        .contains("renewed approval"));
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
    assert!(runner
        .task
        .intent_events
        .iter()
        .any(|event| event.kind == "intent_invalidated"));
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn rejected_entity_associations_require_replan_and_allow_fresh_human_guidance() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = treatment(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    plan(&fixture, mapping(false, false));
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(runner.task.reviews.len(), 1);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    runner.review(false).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_bound"],
        json!([])
    );
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
    runner
        .submit_message(
            "Use the existing public function's intent; abandon the helper extraction.".into(),
        )
        .await
        .unwrap();
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_ne!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn current_policy_can_request_reviewed_links_without_a_mandatory_mapping() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = fixture.interactive().await;
    runner.task.entity_links = true;
    assert_eq!(runner.task.intent_policy, IntentPolicy::Current);
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.plan.as_ref().unwrap().change_intent.is_none());
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    fixture.conversational(json!({"action":"associate","targets":[{"file":"labels.py","entity":"entity_0","planned":false,"records":["r0"]}]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_bound"],
        json!([])
    );
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-with-association".into());
    fixture.shared.lock().unwrap().fail_context = true;
    assert!(runner.review(true).await.is_err());
    assert!(
        runner.task.reviews.is_empty(),
        "daemon review receipt was committed locally before refresh"
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "link_review")
            .count(),
        1
    );
    let task_id = runner.task.id.clone();
    drop(runner);
    fixture.shared.lock().unwrap().fail_context = false;
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(
        fixture.shared.lock().unwrap().reviewed.len(),
        1,
        "refresh retry must not replay durable review"
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "link_review")
            .count(),
        1
    );
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["intent_bound"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(runner.task.plan.as_ref().unwrap().change_intent.is_none());
    assert_eq!(
        serde_json::to_value(&runner.task).unwrap()["approved_revision"].as_str(),
        Some("accepted-with-association")
    );
}

#[tokio::test]
async fn stale_index_proof_cannot_approve_an_existing_target() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = treatment(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    plan(&fixture, mapping(false, false));
    runner.advance().await.unwrap();
    fixture.shared.lock().unwrap().intent_stale_source = true;
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Planning);
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(serde_json::to_value(&runner.task).unwrap()["approved_revision"].is_null());
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn planned_helper_links_complete_after_one_reapproval_and_journal_resume() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let original = "def render_name(name):\n    return name.strip() or '(unnamed)'\n\ndef render_names(names):\n    return [name.strip() or '(unnamed)' for name in names]\n";
    // Put the helper first: entity choice indices shift, but approved symbols
    // must remain stable across authorized edits and current-source indexing.
    let changed = "def _normalize(name):\n    return name.strip() or '(unnamed)'\n\ndef render_name(name):\n    return _normalize(name)\n\ndef render_names(names):\n    return [_normalize(name) for name in names]\n";
    std::fs::write(fixture.root.join("labels.py"), original).unwrap();
    seed(&fixture);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.intent_records.push(CaptureTarget {
            iri: "https://moosedev.dev/kg/Constraint/preserve-order".into(),
            label: "Preserve order and duplicates".into(),
            kind: "Constraint".into(),
        });
        let records = script.intent_records.clone();
        for name in ["render_name", "render_names"] {
            for record in &records {
                script
                    .intent_accepted
                    .push(moosedev::harness::daemon::intent::IntentBinding {
                        record_iri: record.iri.clone(),
                        file: "labels.py".into(),
                        symbol: Some(format!("scip-python python fixture . labels.py/{name}().")),
                        planned_name: None,
                        source_digest: Some(format!("{:x}", Sha256::digest(original.as_bytes()))),
                    });
            }
        }
    }
    let mut runner = treatment(&fixture).await;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    plan(
        &fixture,
        json!({"purpose":["r0"],"obligations":["r1"],"missing":null,"targets":[
            {"file":"labels.py","entity":"entity_0","planned":false,"records":["r0","r1"]},
            {"file":"labels.py","entity":"entity_1","planned":false,"records":["r0","r1"]},
            {"file":"labels.py","entity":"_normalize","planned":true,"records":["r0","r1"]}
        ]}),
    );
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(
        fixture
            .shared
            .lock()
            .unwrap()
            .intent_link_requests
            .is_empty(),
        "seed associations must be reused"
    );

    fixture.conversational(json!({"action":"write","file":"labels.py","content":changed}));
    runner.advance().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        changed
    );
    fixture.no_capture();
    runner.advance().await.unwrap();
    fixture.conversational(
        json!({"action":"finish","summary":"Shared helper extracted; ready to verify."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    {
        let mut script = fixture.shared.lock().unwrap();
        assert_eq!(script.intent_link_requests.len(), 1);
        let bindings = &script.intent_link_requests[0].bindings;
        assert_eq!(bindings.len(), 2);
        assert!(bindings
            .iter()
            .all(|b| b.symbol.as_deref()
                == Some("scip-python python fixture . labels.py/_normalize().")));
        script.revision_on_accept = Some("accepted-helper-links".into());
    }
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    let persisted_events = serde_json::to_value(&runner.task.intent_events).unwrap();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &id).unwrap();
    runner.configure(fixture.config(), None);
    assert_eq!(
        serde_json::to_value(&runner.task.intent_events).unwrap(),
        persisted_events
    );
    runner.resume().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(
        runner.task.phase,
        Phase::Working,
        "own edit's historical digest must not require a new plan"
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|e| e.kind == "plan_approval_attempt")
            .count(),
        2
    );
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|e| e.kind == "link_review")
            .count(),
        2
    );
    assert!(runner
        .task
        .intent_events
        .iter()
        .any(|e| e.kind == "knowledge_revision_changed"));
    fixture.conversational(json!({"action":"finish","summary":"Accepted associations are current; run the required check."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(
        fixture.shared.lock().unwrap().intent_link_requests.len(),
        1,
        "accepted helper links must prevent repeat proposals"
    );
    // This is an orchestration test; executor behavior is covered separately.
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "scripted completed verification".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

async fn run_stage2_arm(policy: IntentPolicy) {
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = fixture.interactive().await;
    runner.task.postedit_association_contract = 1;
    runner.set_intent_policy(policy).unwrap();
    if policy == IntentPolicy::ChangeLevelV2 {
        fixture.shared.lock().unwrap().purpose_scoped_entities = true;
    }
    assert_eq!(runner.task.intent_policy, policy);

    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    if policy == IntentPolicy::ChangeLevelV2 {
        fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted requirement governs display behavior."}));
        fixture.reply(
            "harness_purpose_selection",
            json!({"decision":"done","rationale":"The governing purpose is selected."}),
        );
    }
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    runner.approve_plan().await.unwrap();
    if policy == IntentPolicy::ChangeLevelV2 {
        assert!(
            runner.task.purpose_selection.is_some(),
            "{}",
            serde_json::to_string_pretty(&runner.task).unwrap()
        );
    }
    assert_eq!(runner.task.phase, Phase::Working);

    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"original\n","new_text":"def render_name(name):\n    return name\n"}));
    runner.advance().await.unwrap();
    fixture.no_capture();
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name.strip()"}));
    runner.advance().await.unwrap();
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(
        runner.task.mode,
        Mode::Auto,
        "owned source chain retained plan approval"
    );

    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    fixture.reply("harness_association_selection", json!({"decision":"associate","record_handles":["r0"],"rationale":"The helper implements the selected display requirement."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    let task_id = runner.task.id.clone();
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    drop(runner);

    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner.configure(fixture.config(), None);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: true,
        output: "scripted verification".into(),
    }];
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    runner.confirm_no_knowledge().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Complete);
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 1);
    assert_eq!(
        runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "link_review")
            .count(),
        1
    );
    assert_eq!(
        intent_events(&runner, "edit_applied"),
        vec!["labels.py", "labels.py"],
        "each of the arm's two applied edits is journaled exactly once"
    );
    assert_eq!(runner.task.edits.len(), 2);
}

#[tokio::test]
async fn current_stage2_arm_uses_shared_postedit_selection_through_completion() {
    let _lock = ENVIRONMENT.lock().await;
    run_stage2_arm(IntentPolicy::Current).await;
}

#[tokio::test]
async fn invalid_purpose_transition_exhausts_with_guidance_and_survives_restart() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    for _ in 0..3 {
        fixture.reply(
            "harness_purpose_selection",
            json!({"decision":"done","rationale":"Done before selecting a purpose."}),
        );
    }
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(error.contains("model output failed validation"), "{error}");
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message.contains("Provide human guidance")));
    assert_eq!(
        intent_events(&runner, "repair_exhausted"),
        vec!["harness_purpose_selection"]
    );
    assert!(
        runner.task.events.iter().any(|event| event
            .message
            .contains("purpose selection failed validation")),
        "the exhaustion message names the purpose-selection stage"
    );
    let task_id = runner.task.id.clone();
    drop(runner);
    let resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    assert_eq!(resumed.task.phase, Phase::AwaitingInput);
    assert_eq!(resumed.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(resumed
        .task
        .purpose_selection
        .as_ref()
        .unwrap()
        .selected
        .is_empty());
}

#[tokio::test]
async fn unavailable_next_page_uses_bounded_fallback_recovery() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    for _ in 0..3 {
        fixture.reply(
            "harness_purpose_selection",
            json!({"decision":"next_page","rationale":"Request a nonexistent continuation."}),
        );
    }
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(error.contains("model output failed validation"), "{error}");
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(runner
        .task
        .purpose_selection
        .as_ref()
        .unwrap()
        .selected
        .is_empty());
    let task_id = runner.task.id.clone();
    drop(runner);
    let resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    assert_eq!(resumed.task.phase, Phase::AwaitingInput);
    assert_eq!(resumed.task.recovery.as_ref().unwrap().attempts, 3);
    assert!(resumed.task.edits.is_empty());
    assert!(resumed.task.reviews.is_empty());
}

#[tokio::test]
async fn postedit_empty_eligible_choices_are_deterministic_and_restart_safe() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = current_runner_ready_for_postedit(&fixture).await;
    fixture.shared.lock().unwrap().intent_records.clear();
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    let association = runner.task.postedit_association.as_ref().unwrap();
    assert_eq!(association.decisions.len(), 1);
    assert_eq!(association.decisions[0].disposition, "no_eligible_choices");
    assert!(association.proposed_bindings.is_empty());
    assert!(!runner
        .task
        .model_requests
        .iter()
        .any(|request| { request["purpose"] == "harness_association_selection" }));
    assert_eq!(fixture.shared.lock().unwrap().intent_link_requests.len(), 0);
    let task_id = runner.task.id.clone();
    drop(runner);

    let mut resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    resumed.configure(fixture.config(), None);
    resumed.resume().await.unwrap();
    assert_eq!(resumed.task.phase, Phase::Verifying);
    assert_eq!(
        resumed
            .task
            .postedit_association
            .as_ref()
            .unwrap()
            .decisions
            .len(),
        1
    );
    assert_eq!(
        fixture
            .shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request["kind"] == "postedit_candidates")
            .count(),
        1
    );
}

#[tokio::test]
async fn duplicate_association_handles_fail_before_link_side_effects() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = current_runner_ready_for_postedit(&fixture).await;
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    for _ in 0..3 {
        fixture.reply(
            "harness_association_selection",
            json!({"decision":"associate","record_handles":["r0","r0"],"rationale":"duplicate probe"}),
        );
    }
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(error.contains("duplicate record handle"), "{error}");
    assert_eq!(runner.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(runner
        .task
        .postedit_association
        .as_ref()
        .unwrap()
        .decisions
        .is_empty());
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    let task_id = runner.task.id.clone();
    drop(runner);
    let mut resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    resumed.configure(fixture.config(), None);
    assert_eq!(resumed.task.recovery.as_ref().unwrap().attempts, 3);
    assert_eq!(resumed.task.phase, Phase::AwaitingInput);
    assert!(resumed
        .task
        .postedit_association
        .as_ref()
        .unwrap()
        .decisions
        .is_empty());
}

#[tokio::test]
async fn conservative_postedit_scope_advances_once_without_model_judgment() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    let mut runner = current_runner_ready_for_postedit(&fixture).await;
    fixture.shared.lock().unwrap().postedit_conservative = true;
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    let association = runner.task.postedit_association.as_ref().unwrap();
    assert_eq!(association.status, "resolved");
    assert_eq!(association.decisions.len(), 1);
    assert_eq!(association.decisions[0].disposition, "no_linkable_entity");
    assert!(association.proposed_bindings.is_empty());
    assert!(!runner
        .task
        .model_requests
        .iter()
        .any(|request| { request["purpose"] == "harness_association_selection" }));
    assert!(fixture
        .shared
        .lock()
        .unwrap()
        .intent_link_requests
        .is_empty());
    let task_id = runner.task.id.clone();
    drop(runner);
    let mut resumed = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    resumed.configure(fixture.config(), None);
    resumed.resume().await.unwrap();
    assert_eq!(resumed.task.phase, Phase::Verifying);
    assert_eq!(
        resumed.task.postedit_association.unwrap().decisions.len(),
        1
    );
}

#[tokio::test]
async fn change_level_v2_selects_purpose_and_uses_shared_postedit_through_completion() {
    let _lock = ENVIRONMENT.lock().await;
    run_stage2_arm(IntentPolicy::ChangeLevelV2).await;
}

#[tokio::test]
async fn change_level_v2_reviews_unlinked_scope_once_then_retains_owned_scope() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    std::fs::write(
        fixture.root.join("labels.py"),
        "def render_name(name):\n    return name\n\ndef unrelated(value):\n    return value\n",
    )
    .unwrap();
    seed(&fixture);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.purpose_scoped_entities = false;
    }
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior in render_name","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"This requirement governs render_name."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"Purpose selected."}),
    );
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    {
        let mut script = fixture.shared.lock().unwrap();
        script.postedit_symbol_name = Some("unrelated".into());
        script.intent_records.push(CaptureTarget {
            iri: "https://moosedev.dev/kg/Constraint/unrelated-scope".into(),
            label: "Keep unrelated transformations stable".into(),
            kind: "Constraint".into(),
        });
    }
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return value","new_text":"return value + 1"}));
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r1","role":"obligation","rationale":"This constraint is implicated by the exact unrelated definition scope."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"New governing knowledge resolved."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value\n"));
    let task_id = runner.task.id.clone();
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner.configure(fixture.config(), None);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value\n"));
    let task = serde_json::to_value(&runner.task).unwrap();
    let assessment = task["scope_assessments"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(assessment["disposition"], "awaiting_scope_approval");
    assert!(!assessment["pages"].as_array().unwrap().is_empty());
    assert!(runner
        .task
        .intent_events
        .iter()
        .any(|event| event.kind == "scope_approval_required"));
    drop(runner);
    let mut runner = Runner::load(fixture.root.clone(), fixture.url.clone(), &task_id).unwrap();
    runner.configure(fixture.config(), None);
    runner.approve_policy().await.unwrap();
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value + 1"));
    fixture.no_capture();
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return value + 1","new_text":"return value + 2"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working, "owned second edit in the human-approved definition scope must not request another approval");
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value + 2"));
}

#[tokio::test]
async fn malformed_same_count_intent_response_never_becomes_a_review_card() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    fixture
        .shared
        .lock()
        .unwrap()
        .intent_records
        .push(CaptureTarget {
            iri: "https://moosedev.dev/kg/Constraint/second".into(),
            label: "Second governing record".into(),
            kind: "Constraint".into(),
        });
    let mut runner = fixture.interactive().await;
    runner.task.entity_links = true;
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    fixture.shared.lock().unwrap().malformed_intent_response = true;
    fixture.conversational(json!({"action":"associate","targets":[{"file":"labels.py","entity":"entity_0","planned":false,"records":["r0","r1"]}]}));
    let error = runner.advance().await.unwrap_err();
    assert!(error.to_string().contains("one unique link"));
    assert!(runner.task.reviews.is_empty());
    assert!(serde_json::to_value(&runner.task).unwrap()["pending_intent_links"].is_object());
}

fn intent_events(runner: &Runner, kind: &str) -> Vec<String> {
    runner
        .task
        .intent_events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.detail.clone())
        .collect()
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

fn scope_dispositions(runner: &Runner) -> Vec<String> {
    serde_json::to_value(&runner.task).unwrap()["scope_assessments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|assessment| assessment["disposition"].as_str().unwrap().to_owned())
        .collect()
}

/// A change-level-v2 plan approved for `render_name`, with an `unrelated`
/// definition that a later edit will touch and a Constraint governing it that
/// the approved purpose inventory never saw. The next edit proposal therefore
/// parks in `awaiting_governing_selection` and opens the governing detour.
async fn v2_governing_detour_ready(fixture: &Fixture) -> Runner {
    std::fs::write(
        fixture.root.join("labels.py"),
        "def render_name(name):\n    return name\n\ndef unrelated(value):\n    return value\n",
    )
    .unwrap();
    seed(fixture);
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior in render_name","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"This requirement governs render_name."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"Purpose selected."}),
    );
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    {
        let mut script = fixture.shared.lock().unwrap();
        script.postedit_symbol_name = Some("unrelated".into());
        script.intent_records.push(CaptureTarget {
            iri: "https://moosedev.dev/kg/Constraint/unrelated-scope".into(),
            label: "Keep unrelated transformations stable".into(),
            kind: "Constraint".into(),
        });
    }
    runner
}

fn propose_unrelated_edit(fixture: &Fixture) {
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return value","new_text":"return value + 1"}));
}

fn select_fresh_purpose(fixture: &Fixture) {
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The display requirement still governs the plan."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"Purpose reselected from the refreshed inventory."}),
    );
}

/// After the detour moved the knowledge revision, re-approval must not surface
/// the parked edit as an unapprovable policy card: it is discarded and the
/// task returns to Working so the model can re-propose it.
fn assert_parked_edit_invalidated(fixture: &Fixture, runner: &Runner) {
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert!(runner.task.pending_edit.is_none());
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    let dispositions = scope_dispositions(runner);
    assert!(
        dispositions.contains(&"invalidated".to_owned()),
        "{dispositions:?}"
    );
    assert!(
        !dispositions
            .iter()
            .any(|d| d == "awaiting_governing_selection" || d == "awaiting_scope_approval"),
        "{dispositions:?}"
    );
    assert_eq!(intent_events(runner, "scope_edit_invalidated").len(), 1);
    assert!(intent_events(runner, "scope_edit_invalidated")[0].contains("labels.py"));
    assert!(intent_events(runner, "edit_applied").is_empty());
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value\n"));
}

async fn reproposed_edit_is_applied(fixture: &Fixture, runner: &mut Runner) {
    propose_unrelated_edit(fixture);
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    // The unlinked `unrelated` definition scope still needs the human, but the
    // card is now approvable because the edit was proposed at the current revision.
    assert_eq!(runner.task.phase, Phase::AwaitingPolicy);
    runner.approve_policy().await.unwrap();
    assert_eq!(intent_events(runner, "edit_applied"), vec!["labels.py"]);
    assert_eq!(runner.task.edits.len(), 1);
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return value + 1"));
}

#[tokio::test]
async fn v2_governing_detour_with_moved_revision_discards_parked_edit_and_reaches_first_edit() {
    let _lock = ENVIRONMENT.lock().await;
    // Detour A: the implicated Constraint is selected, but the project revision
    // moves before the human re-approves; a journal reload sits mid-detour.
    let fixture = intent_fixture().await;
    let mut runner = v2_governing_detour_ready(&fixture).await;
    propose_unrelated_edit(&fixture);
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r1","role":"obligation","rationale":"This constraint is implicated by the unrelated definition scope."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"New governing knowledge resolved."}),
    );
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(
        runner.task.pending_edit.as_ref().unwrap().revision,
        "accepted-v1"
    );
    assert_eq!(
        scope_dispositions(&runner).last().unwrap(),
        "awaiting_governing_selection"
    );
    fixture.shared.lock().unwrap().revision = "accepted-v2".into();
    let error = runner.approve_plan().await.unwrap_err().to_string();
    assert!(error.contains("renewed approval"), "{error}");
    assert!(runner.task.pending_edit.is_some());
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    assert!(runner.task.pending_edit.is_some());
    select_fresh_purpose(&fixture);
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(intent_events(&runner, "purpose_review_required").len(), 1);
    runner.approve_plan().await.unwrap();
    assert_parked_edit_invalidated(&fixture, &runner);
    reproposed_edit_is_applied(&fixture, &mut runner).await;
    assert!(fixture.shared.lock().unwrap().replies.is_empty());

    // Detour B: the model declares the implicated page missing, the resulting
    // capture is accepted (moving the revision), and the purpose is reselected
    // from the refreshed inventory; the reload sits at the pending review.
    let fixture = intent_fixture().await;
    let mut runner = v2_governing_detour_ready(&fixture).await;
    propose_unrelated_edit(&fixture);
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"missing","grounded_reason":"The implicated page carries no applicable governing record."}),
    );
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(runner.task.phase, Phase::Planning);
    assert!(runner.task.pending_edit.is_some());
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "awaiting_missing_capture"
    );
    assert_eq!(
        scope_dispositions(&runner).last().unwrap(),
        "awaiting_governing_selection"
    );
    fixture.reply("harness_capture", json!({"reason":"The proposed change exposed an unrelated transformation detail.","proposals":[{
        "kind":"Lesson","title":"Unrelated transformation observation","description":"The unrelated definition applies a numeric adjustment.",
        "evidence":["Model action:"],"files":["labels.py"],"components":[],"requirement":null,"supersedes":null,"retracts":null
    }]}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    let operation = runner.task.reviews[0].request.operation_id.clone();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert!(runner.task.pending_edit.is_some());
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-v2".into());
    select_fresh_purpose(&fixture);
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_eq!(runner.task.knowledge_revision, "accepted-v2");
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().status,
        "ready"
    );
    assert!(runner.task.pending_edit.is_some());
    runner.approve_plan().await.unwrap();
    assert_parked_edit_invalidated(&fixture, &runner);
    reproposed_edit_is_applied(&fixture, &mut runner).await;
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

#[tokio::test]
async fn v2_second_edit_after_postedit_link_review_and_failing_check_is_accepted() {
    let _lock = ENVIRONMENT.lock().await;
    let fixture = intent_fixture().await;
    seed(&fixture);
    fixture.shared.lock().unwrap().purpose_scoped_entities = true;
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior while adding a helper","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted requirement governs display behavior."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"The governing purpose is selected."}),
    );
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"original\n","new_text":"def render_name(name):\n    return name\n"}));
    runner.advance().await.unwrap();
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(intent_events(&runner, "edit_applied").len(), 1);
    fixture.conversational(json!({"action":"finish","summary":"The helper is implemented."}));
    fixture.reply("harness_association_selection", json!({"decision":"associate","record_handles":["r0"],"rationale":"The helper implements the selected display requirement."}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    fixture.shared.lock().unwrap().revision_on_accept = Some("accepted-links".into());
    runner.review(true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::Verifying);
    assert_eq!(runner.task.knowledge_revision, "accepted-links");
    assert_eq!(
        runner.task.purpose_selection.as_ref().unwrap().revision,
        "accepted-links",
        "the ready purpose selection must follow the task's own association revision"
    );

    // The required check fails. As in the other completion-gate fixtures the
    // executor verdict is supplied directly; the runner returns to Working.
    runner.task.check_results = vec![CheckResult {
        command: "fixture-required-check".into(),
        success: false,
        output: "fixture: required check failed".into(),
    }];
    runner.task.phase = Phase::Working;
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"return name","new_text":"return name.strip()"}));
    runner.advance().await.unwrap();
    assert!(
        runner.task.last_error.is_none(),
        "{:?}",
        runner.task.last_error
    );
    assert_eq!(
        intent_events(&runner, "edit_applied"),
        vec!["labels.py", "labels.py"]
    );
    assert_eq!(runner.task.edits.len(), 2);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(runner.task.mode, Mode::Auto);
    assert!(std::fs::read_to_string(fixture.root.join("labels.py"))
        .unwrap()
        .contains("return name.strip()"));
    assert!(fixture.shared.lock().unwrap().replies.is_empty());
}

/// Polls every journal under the task directory and records any persisted
/// snapshot that is `AwaitingPlan` with a capture still due. Journal writes are
/// atomic renames, so each read observes one complete persisted state; the
/// poll interval bounds how many short-lived snapshots can slip past.
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
                "{}: phase={} capture_due={} purpose={}",
                path.display(),
                journal["phase"],
                journal["capture_due"],
                journal["purpose_selection"]["status"]
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

#[tokio::test]
async fn no_persisted_state_is_awaiting_plan_with_capture_due() {
    let _lock = ENVIRONMENT.lock().await;
    // Every persisted snapshot of the v2 missing-checkpoint scripts is checked
    // both by the background poller and at each step boundary.

    // Empty inventory: durable guidance after restart.
    let fixture = intent_fixture().await;
    let watch = JournalWatch::start(&fixture);
    let runner = v2_missing_checkpoint(&fixture, false).await;
    assert_journal_invariant(&fixture, "missing checkpoint");
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_journal_invariant(&fixture, "resume");
    fixture.no_capture();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_journal_invariant(&fixture, "drained checkpoint");
    assert_eq!(watch.finish(), Vec::<String>::new());

    // Unrelated review kept, then the purpose is reselected from a fresh page.
    let fixture = intent_fixture().await;
    let watch = JournalWatch::start(&fixture);
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    capture_unrelated_label_lesson(&fixture);
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingReview);
    assert_journal_invariant(&fixture, "unrelated review pending");
    let operation = runner.task.reviews[0].request.operation_id.clone();
    let id = runner.task.id.clone();
    drop(runner);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_journal_invariant(&fixture, "resume with review");
    select_fresh_purpose(&fixture);
    runner.review_operation(&operation, true).await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_journal_invariant(&fixture, "purpose reselected");
    assert_eq!(watch.finish(), Vec::<String>::new());

    // A context outage during the checkpoint refresh preserves the marker.
    let fixture = intent_fixture().await;
    let watch = JournalWatch::start(&fixture);
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    capture_unrelated_label_lesson(&fixture);
    runner.advance().await.unwrap();
    let operation = runner.task.reviews[0].request.operation_id.clone();
    fixture.shared.lock().unwrap().fail_context = true;
    assert!(runner.review_operation(&operation, true).await.is_err());
    assert_journal_invariant(&fixture, "refresh outage");
    let id = runner.task.id.clone();
    drop(runner);
    fixture.shared.lock().unwrap().fail_context = false;
    select_fresh_purpose(&fixture);
    let mut runner = reload(&fixture, &id);
    runner.resume().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingPlan);
    assert_journal_invariant(&fixture, "recovered refresh");
    assert_eq!(watch.finish(), Vec::<String>::new());

    // Three consecutive semantic missing rounds.
    let fixture = intent_fixture().await;
    let watch = JournalWatch::start(&fixture);
    let mut runner = v2_missing_checkpoint(&fixture, true).await;
    for round in 2..=3 {
        fixture.no_capture();
        fixture.reply(
            "harness_purpose_selection",
            json!({"decision":"missing","grounded_reason":format!("No applicable governing record in semantic round {round}.")}),
        );
        runner.advance().await.unwrap();
        assert_journal_invariant(&fixture, &format!("missing round {round}"));
    }
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert_eq!(watch.finish(), Vec::<String>::new());
}

/// A change-level-v2 purpose lookup that the daemon answers with `status`.
async fn v2_purpose_lookup_failure(status: u16) -> Runner {
    let fixture = intent_fixture().await;
    seed(&fixture);
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.shared.lock().unwrap().purpose_candidates_status = Some(status);
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains(&format!("daemon HTTP {status}"))
            && error.contains("intent/purpose/candidates"),
        "{error}"
    );
    assert_eq!(runner.task.last_error.as_deref(), Some(error.as_str()));
    assert!(runner.task.purpose_selection.is_none());
    runner
}

#[tokio::test]
async fn last_error_kind_classifies_controller_daemon_and_service_failures() {
    let _lock = ENVIRONMENT.lock().await;
    // (a) A journal whose ready purpose selection no longer matches the
    // approved scope is a controller-state failure, not model output.
    let fixture = intent_fixture().await;
    seed(&fixture);
    fixture.shared.lock().unwrap().purpose_scoped_entities = true;
    let mut runner = fixture.interactive().await;
    runner
        .set_intent_policy(IntentPolicy::ChangeLevelV2)
        .unwrap();
    fixture.conversational(json!({"action":"read","file":"labels.py"}));
    runner.advance().await.unwrap();
    fixture.conversational(json!({"action":"plan","summary":"Preserve display behavior","files":["labels.py"],"checks":["fixture-required-check"]}));
    fixture.no_capture();
    fixture.reply("harness_purpose_selection", json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"The accepted requirement governs display behavior."}));
    fixture.reply(
        "harness_purpose_selection",
        json!({"decision":"done","rationale":"The governing purpose is selected."}),
    );
    runner.advance().await.unwrap();
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(runner.task.last_error_kind.is_none());
    let id = runner.task.id.clone();
    drop(runner);
    let journal = journal_path(&fixture, &id);
    let mut persisted: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    assert_eq!(persisted["purpose_selection"]["revision"], "accepted-v1");
    persisted["purpose_selection"]["revision"] = json!("corrupted-revision");
    std::fs::write(&journal, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
    let mut runner = reload(&fixture, &id);
    fixture.conversational(json!({"action":"replace","file":"labels.py","old_text":"original\n","new_text":"def render_name(name):\n    return name\n"}));
    let error = format!("{:#}", runner.advance().await.unwrap_err());
    assert!(
        error.contains("approved purpose assertions or roles changed"),
        "{error}"
    );
    assert_eq!(
        runner.task.last_error_kind.as_deref(),
        Some("controller_invariant")
    );
    assert!(runner
        .task
        .last_error
        .as_deref()
        .unwrap()
        .contains("approved purpose assertions or roles changed"));
    assert!(runner.task.edits.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("labels.py")).unwrap(),
        "original\n"
    );

    // (b) A daemon 4xx on the purpose lookup is a daemon rejection.
    let runner = v2_purpose_lookup_failure(409).await;
    assert_eq!(
        runner.task.last_error_kind.as_deref(),
        Some("daemon_rejection")
    );

    // (c) A daemon 5xx on the same lookup is a service failure.
    let runner = v2_purpose_lookup_failure(500).await;
    assert_eq!(runner.task.last_error_kind.as_deref(), Some("service"));
}
