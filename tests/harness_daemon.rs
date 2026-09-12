//! Real daemon boundary tests: no model is involved in memory or ratification calls.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum_test::TestServer;
use chrono::Utc;
use moosedev::api::routes::build_routes;
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::harness::daemon;
use moosedev::harness::protocol::*;
use moosedev::llm::LlmConfig;
use serde_json::json;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("moosedev-harness-daemon-{}", uuid::Uuid::new_v4())))
    }
    fn state(&self) -> AppState {
        let cfg = LlmConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "test".into(),
            model: "unused".into(),
            configured: false,
            context_window_tokens: moosedev::llm::DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
            structured_output: moosedev::llm::StructuredOutputMode::Auto,
        };
        AppState::bootstrap_with_llm_config(
            &self.0.join(".moosedev"),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
            cfg,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn status_literal(state: &AppState, iri: &str, predicate: &str) -> Option<String> {
    use oxigraph::model::{NamedNodeRef, Term};
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(iri).ok()?.into()),
            Some(NamedNodeRef::new(predicate).ok()?),
            None,
            None,
        )
        .filter_map(Result::ok)
        .find_map(|q| match q.object {
            Term::Literal(v) => Some(v.value().into()),
            _ => None,
        })
}

fn proposal(kind: &str, title: &str) -> KnowledgeProposal {
    KnowledgeProposal {
        kind: kind.into(),
        title: title.into(),
        description: format!("Claim: {title}"),
        evidence: vec!["user message 1: explicit design choice".into()],
        files: vec![],
        components: vec![],
        requirement: None,
        supersedes: None,
        retracts: None,
    }
}

fn request(id: &str, proposals: Vec<KnowledgeProposal>) -> CaptureRequest {
    CaptureRequest {
        operation_id: id.into(),
        proposals,
    }
}

fn record(state: &AppState, kind: &str, title: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: state.resolve_class(kind).unwrap(),
            class_local: kind.into(),
            properties: vec![
                (state.capture.title.clone(), title.into()),
                (
                    state.capture.description.clone(),
                    format!("Established {title}"),
                ),
            ],
        },
        "test-human",
        Utc::now(),
    )
    .unwrap()
}

fn install_intent_index(fixture: &Fixture, state: &AppState) {
    install_intent_index_as(fixture, state, "scip-python")
}

fn install_intent_index_as(fixture: &Fixture, state: &AppState, producer: &str) {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation};
    let path = fixture.0.join("labels.py");
    std::fs::write(&path, "def render_name(name):\n    return name.strip()\n").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2)),
        )
        .unwrap();
    let symbol = "scip-python python sample 1 labels/render_name().";
    let mut info = SymbolInformation::new();
    info.symbol = symbol.into();
    info.display_name = "render_name".into();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.into();
    occurrence.symbol_roles = 1;
    occurrence.range = vec![0, 4, 15];
    let mut document = Document::new();
    document.relative_path = "labels.py".into();
    document.symbols.push(info);
    document.occurrences.push(occurrence);
    let mut index = Index::new();
    index.documents.push(document);
    let mut meta = SubstrateMeta::single(producer, "test", Utc::now(), 1, 1);
    meta.indexed_started_at = Some(Utc::now());
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, &fixture.0).unwrap(),
    ));
}

fn install_deleted_intent_index_with_digest(state: &AppState, root: &Path, digest: String) {
    use moosedev::code::substrate::{
        HistoricalDefinitionProof, HistoricalFileProof, Substrate, SubstrateMeta,
    };
    use scip::types::Index;
    let symbol = "scip-python python sample 1 labels/render_name().";
    let index = Index::new();
    let mut meta = SubstrateMeta::single("scip-python", "test", Utc::now(), 0, 0);
    meta.indexed_started_at = Some(Utc::now());
    meta.historical_files.insert(
        "labels.py".into(),
        HistoricalFileProof {
            source_digest: digest,
            index_generation: Some("prior-generation".into()),
            definitions: vec![HistoricalDefinitionProof {
                symbol: symbol.into(),
                name: Some("render_name".into()),
                definition_range: [0, 4, 0, 15],
                enclosing_range: None,
            }],
        },
    );
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, root).unwrap(),
    ));
}

#[test]
fn intent_bindings_are_proven_reviewed_and_idempotent() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Requirement", "Preserve display names");
    let resolve = || {
        resolve_entities(
            &state,
            &IntentResolveRequest {
                files: vec!["labels.py".into()],
                refresh_index: false,
            },
        )
        .unwrap()
    };
    let choices = resolve();
    assert!(choices.unresolved.is_empty());
    assert_eq!(choices.entities.len(), 1);
    assert!(choices.entities[0].dossier_records.is_empty());
    let request = IntentLinkRequest {
        operation_id: "intent-reuse".into(),
        revision: choices.revision,
        bindings: vec![IntentBinding {
            record_iri: record_iri.clone(),
            file: "labels.py".into(),
            symbol: Some(choices.entities[0].symbol.clone()),
            planned_name: None,
            source_digest: Some(choices.entities[0].source_digest.clone()),
        }],
    };
    let linked = link_operation(&state, request.clone()).unwrap();
    assert_eq!(linked.links.len(), 1);
    assert!(resolve().entities[0].dossier_records.is_empty());
    assert_eq!(linked.links, link_operation(&state, request).unwrap().links);
    let review = ReviewRequest {
        operation_id: "intent-reuse".into(),
        accept: true,
    };
    assert!(review_links(&state, &review).unwrap().pending.is_empty());
    assert!(review_links(&state, &review).unwrap().pending.is_empty());
    assert_eq!(resolve().entities[0].dossier_records, vec![record_iri]);
}

#[test]
fn intent_review_rejects_changed_source_and_allows_human_rejection() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Constraint", "Preserve names");
    let choices = resolve_entities(
        &state,
        &IntentResolveRequest {
            files: vec!["labels.py".into()],
            refresh_index: false,
        },
    )
    .unwrap();
    let request = IntentLinkRequest {
        operation_id: "intent-stale".into(),
        revision: choices.revision,
        bindings: vec![IntentBinding {
            record_iri,
            file: "labels.py".into(),
            symbol: None,
            planned_name: Some("render_name".into()),
            source_digest: None,
        }],
    };
    assert_eq!(link_operation(&state, request).unwrap().resolved.len(), 1);
    std::fs::write(fixture.0.join("labels.py"), "def replacement(): pass\n").unwrap();
    let mut review = ReviewRequest {
        operation_id: "intent-stale".into(),
        accept: true,
    };
    assert!(review_links(&state, &review)
        .unwrap_err()
        .to_string()
        .contains("proven indexed"));
    review.accept = false;
    assert!(review_links(&state, &review).unwrap().pending.is_empty());
}

#[test]
fn intent_resolver_distinguishes_missing_index_and_unresolved_planned_target() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    let missing = resolve_entities(
        &state,
        &IntentResolveRequest {
            files: vec!["labels.py".into()],
            refresh_index: false,
        },
    )
    .unwrap();
    assert!(missing.unresolved[0].contains("missing index"));
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Requirement", "Shared label intent");
    let response = link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "intent-planned".into(),
            revision: daemon::accepted_revision(&state).unwrap(),
            bindings: vec![IntentBinding {
                record_iri: record_iri.clone(),
                file: "labels.py".into(),
                symbol: None,
                planned_name: Some("_normalize_name".into()),
                source_digest: None,
            }],
        },
    )
    .unwrap();
    assert!(response.links.is_empty());
    assert_eq!(response.unresolved.len(), 1);

    // The unresolved response is a durable assessment, not a permanent ban on
    // a target. A subsequent plan uses a new operation with the current name.
    let next = link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "intent-planned-followup".into(),
            revision: daemon::accepted_revision(&state).unwrap(),
            bindings: vec![IntentBinding {
                record_iri,
                file: "labels.py".into(),
                symbol: None,
                planned_name: Some("render_name".into()),
                source_digest: None,
            }],
        },
    )
    .unwrap();
    assert_eq!(next.resolved.len(), 1);
    assert!(next.unresolved.is_empty());
}

#[test]
fn intent_review_rejects_truncated_journal_associations() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Requirement", "Frozen association");
    link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "intent-tamper".into(),
            revision: daemon::accepted_revision(&state).unwrap(),
            bindings: vec![IntentBinding {
                record_iri,
                file: "labels.py".into(),
                symbol: None,
                planned_name: Some("render_name".into()),
                source_digest: None,
            }],
        },
    )
    .unwrap();
    let path = state
        .data_dir
        .join("harness/operations/intent-tamper.intent.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["response"]["resolved"] = json!([]);
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = review_links(
        &state,
        &ReviewRequest {
            operation_id: "intent-tamper".into(),
            accept: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("lengths differ"));
    assert_eq!(
        graph::list_proposals(&state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn abandoning_uncertain_intent_rejects_unacknowledged_links_and_blocks_late_requests() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Requirement", "Cancel uncertain binding");
    let make_request = |id: &str| IntentLinkRequest {
        operation_id: id.into(),
        revision: daemon::accepted_revision(&state).unwrap(),
        bindings: vec![IntentBinding {
            record_iri: record_iri.clone(),
            file: "labels.py".into(),
            symbol: None,
            planned_name: Some("render_name".into()),
            source_digest: None,
        }],
    };
    let early = make_request("abandoned-before-arrival");
    abandon_links(
        &state,
        &ReviewRequest {
            operation_id: early.operation_id.clone(),
            accept: false,
        },
    )
    .unwrap();
    assert!(
        link_operation(&state, early).is_err(),
        "a delayed request must not resurrect abandoned work"
    );
    let uncertain = make_request("abandoned-after-write");
    let response = link_operation(&state, uncertain.clone()).unwrap();
    let path = state
        .data_dir
        .join("harness/operations/abandoned-after-write.intent.json");
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal["prepared"] = json!(false);
    journal["response"]["links"] = json!([]);
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(abandon_links(
        &state,
        &ReviewRequest {
            operation_id: uncertain.operation_id.clone(),
            accept: false
        }
    )
    .unwrap()
    .pending
    .is_empty());
    assert_eq!(
        status_literal(&state, &response.links[0], &state.capture.status).as_deref(),
        Some("rejected")
    );
    assert!(link_operation(&state, uncertain).is_err());
}

#[test]
fn intent_batch_with_one_unresolved_target_has_no_partial_graph_effects() {
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let record_iri = record(&state, "Requirement", "Atomic bindings");
    let response = link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "atomic-unresolved".into(),
            revision: daemon::accepted_revision(&state).unwrap(),
            bindings: ["render_name", "not_created_yet"]
                .into_iter()
                .map(|name| IntentBinding {
                    record_iri: record_iri.clone(),
                    file: "labels.py".into(),
                    symbol: None,
                    planned_name: Some(name.into()),
                    source_digest: None,
                })
                .collect(),
        },
    )
    .unwrap();
    assert_eq!(response.unresolved.len(), 1);
    assert!(response.resolved.is_empty());
    assert!(response.links.is_empty());
    assert!(graph::list_proposals(&state, Some("proposed"))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn http_capture_all_kinds_is_proposed_and_review_is_explicit() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let existing = record(&state, "Constraint", "Established coding constraint");
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let body = ContextRequest {
        topic: "coding constraint".into(),
        files: vec![],
    };
    let before = server.post("/api/v1/harness/context").json(&body).await;
    before.assert_status_ok();
    let before: ContextResponse = before.json();
    assert!(before.context.contains(&existing));
    assert!(before
        .context
        .contains("hasDescription: Established Established coding constraint"));
    let inventory = daemon::context_snapshot(
        &state,
        &ContextRequest {
            topic: "zz_unmatched_inventory_probe".into(),
            files: vec![],
        },
    )
    .unwrap();
    assert!(inventory.context.contains(&existing));
    assert!(
        !inventory.context.contains("hasDescription:"),
        "broad inventory must not expand every record's properties"
    );
    assert_eq!(
        Path::new(&before.project_root),
        fixture.0.canonicalize().unwrap()
    );
    let proposals = [
        "ArchitecturalDecision",
        "Requirement",
        "Constraint",
        "Lesson",
        "Pattern",
        "AntiPattern",
    ]
    .into_iter()
    .map(|kind| proposal(kind, &format!("Harness candidate {kind}")))
    .collect();
    let response = server
        .post("/api/v1/harness/capture")
        .json(&request("all-kinds", proposals))
        .await;
    response.assert_status_ok();
    let captured: CaptureResponse = response.json();
    assert_eq!(captured.proposals.len(), 6);
    let queue = graph::list_proposals(&state, Some("proposed")).unwrap();
    for proposal in &captured.proposals {
        assert!(queue.iter().any(|p| p.iri == proposal.iri));
        assert!(!daemon::context_snapshot(&state, &body)
            .unwrap()
            .context
            .contains(&proposal.iri));
    }
    assert_eq!(
        before.revision,
        daemon::context_snapshot(&state, &body).unwrap().revision
    );
    let checkpoint = server
        .get("/api/v1/harness/checkpoint?operation_id=all-kinds")
        .await;
    checkpoint.assert_status_ok();
    assert!(!checkpoint.json::<CheckpointResponse>().pending.is_empty());
    let review = server
        .post("/api/v1/harness/review")
        .json(&ReviewRequest {
            operation_id: "all-kinds".into(),
            accept: true,
        })
        .await;
    review.assert_status_ok();
    let reviewed: CheckpointResponse = review.json();
    assert!(reviewed.conforms && reviewed.durable && reviewed.pending.is_empty());
    assert_ne!(before.revision, reviewed.revision);
    assert!(std::fs::read_to_string(state.data_dir.join("kg.nq"))
        .unwrap()
        .contains(&captured.proposals[0].iri));
}

#[test]
fn lost_capture_response_and_restart_reuse_record_identity() {
    let fixture = Fixture::new();
    let capture = request(
        "lost-response",
        vec![proposal("Lesson", "Retry evidence survives")],
    );
    let original = {
        let state = fixture.state();
        daemon::capture_operation(&state, capture.clone()).unwrap()
    };
    // Simulate interruption after graph commit but before the completion bit
    // was durably journaled; this is stricter than replaying a completed call.
    let journal = fixture
        .0
        .join(".moosedev/harness/operations/lost-response.json");
    let mut operation: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    operation["captured"] = json!(false);
    std::fs::write(&journal, serde_json::to_vec(&operation).unwrap()).unwrap();
    let state = fixture.state();
    let retry = daemon::capture_operation(&state, capture.clone()).unwrap();
    assert_eq!(original.proposals[0].iri, retry.proposals[0].iri);
    assert_eq!(
        graph::list_proposals(&state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );
    let review = ReviewRequest {
        operation_id: capture.operation_id.clone(),
        accept: false,
    };
    assert!(daemon::review_operation(&state, &review)
        .unwrap()
        .pending
        .is_empty());
    assert!(daemon::review_operation(&state, &review)
        .unwrap()
        .pending
        .is_empty());
    assert!(daemon::review_operation(
        &state,
        &ReviewRequest {
            accept: true,
            ..review
        }
    )
    .is_err());
    let mut different = capture;
    different.proposals[0].description = "different claim".into();
    assert!(daemon::capture_operation(&state, different).is_err());
    let node = oxigraph::model::NamedNode::new(&original.proposals[0].iri).unwrap();
    let quads = state
        .store
        .quads_for_pattern(Some(node.as_ref().into()), None, None, None)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for quad in quads {
        state.store.remove(&quad).unwrap();
    }
    assert!(daemon::checkpoint_snapshot(&state, Some("lost-response"))
        .unwrap()
        .pending
        .contains(&original.proposals[0].iri));
}

#[test]
fn supersession_and_retraction_leave_predecessor_current_until_review() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let original = record(&state, "Requirement", "Prior requirement");
    let mut replacement = proposal("Requirement", "Replacement requirement");
    replacement.supersedes = Some(original.clone());
    let supersession =
        daemon::capture_operation(&state, request("replace", vec![replacement])).unwrap();
    let context = ContextRequest {
        topic: "requirement".into(),
        files: vec![],
    };
    assert!(daemon::context_snapshot(&state, &context)
        .unwrap()
        .context
        .contains(&original));
    daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "replace".into(),
            accept: true,
        },
    )
    .unwrap();
    let current = graph::relevant_context(&state, None, 100, false).unwrap();
    assert!(!current.iter().any(|p| p.iri == original));
    let replacement = supersession.proposals[0].iri.clone();
    assert!(current.iter().any(|p| p.iri == replacement));
    let mut retract = proposal("Lesson", "Requirement was abandoned");
    retract.retracts = Some(replacement.clone());
    daemon::capture_operation(&state, request("retract", vec![retract])).unwrap();
    assert!(graph::relevant_context(&state, None, 100, false)
        .unwrap()
        .iter()
        .any(|p| p.iri == replacement));
    let review = ReviewRequest {
        operation_id: "retract".into(),
        accept: true,
    };
    daemon::review_operation(&state, &review).unwrap();
    daemon::review_operation(&state, &review).unwrap();
    assert!(!graph::relevant_context(&state, None, 100, false)
        .unwrap()
        .iter()
        .any(|p| p.iri == replacement));
}

#[tokio::test]
async fn invalid_batch_writes_nothing_and_unindexed_files_are_reported() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let mut invalid = proposal("Constraint", "Invalid evidence");
    invalid.evidence.clear();
    let invalid = request("invalid", vec![proposal("Lesson", "Valid first"), invalid]);
    assert!(daemon::capture_operation(&state, invalid.clone()).is_err());
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let rejected = server.post("/api/v1/harness/capture").json(&invalid).await;
    rejected.assert_status_bad_request();
    assert!(!state.data_dir.join("harness/operations/invalid.json").exists(),
        "HTTP 400 must mean no operation was persisted and the sensor can safely revise the proposal");
    assert!(graph::list_proposals(&state, None).unwrap().is_empty());
    let mut valid = proposal("Pattern", "File scoped pattern");
    valid.files = vec!["src/new.rs".into()];
    let response = daemon::capture_operation(&state, request("unindexed", vec![valid])).unwrap();
    assert_eq!(response.proposals[0].unanchored, ["src/new.rs"]);
    assert!(daemon::capture_operation(&state, request("../escape", vec![])).is_err());
}

#[tokio::test]
async fn checkpoint_never_claims_durability_when_canonical_publication_fails() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    std::fs::create_dir(state.data_dir.join("kg.nq")).unwrap();
    assert!(daemon::checkpoint_snapshot(&state, None).is_err());
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let request = request(
        "uncertain",
        vec![proposal("Lesson", "Persisted but not acknowledged")],
    );
    server
        .post("/api/v1/harness/capture")
        .json(&request)
        .await
        .assert_status_internal_server_error();
    assert!(state
        .data_dir
        .join("harness/operations/uncertain.json")
        .exists());
    let pending = graph::list_proposals(&state, Some("proposed")).unwrap();
    assert_eq!(pending.len(), 1);
    std::fs::remove_dir(state.data_dir.join("kg.nq")).unwrap();
    let retry = server.post("/api/v1/harness/capture").json(&request).await;
    retry.assert_status_ok();
    assert_eq!(
        retry.json::<CaptureResponse>().proposals[0].iri,
        pending[0].iri
    );
}

#[test]
fn concurrent_capture_retries_share_one_operation() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let request = request(
        "concurrent",
        vec![proposal("Pattern", "Concurrent capture")],
    );
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let state = state.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                daemon::capture_operation(&state, request)
                    .unwrap()
                    .proposals[0]
                    .iri
                    .clone()
            })
        })
        .collect();
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(ids.iter().all(|iri| iri == &ids[0]));
    assert_eq!(
        graph::list_proposals(&state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn file_links_are_queued_and_materialized_only_after_record_ratification() {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation};
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = "rust-analyzer cargo sample 0.1.0 harness/";
    let mut info = SymbolInformation::new();
    info.symbol = symbol.into();
    info.display_name = "harness".into();
    info.kind = EnumOrUnknown::new(symbol_information::Kind::Module);
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.into();
    occurrence.symbol_roles = 1;
    occurrence.range = vec![0, 0, 10];
    occurrence.enclosing_range = vec![0, 0, 10];
    let mut document = Document::new();
    document.relative_path = "src/harness.rs".into();
    document.symbols.push(info);
    document.occurrences.push(occurrence);
    let mut index = Index::new();
    index.documents.push(document);
    state.set_substrate(Arc::new(
        Substrate::from_index(
            index,
            SubstrateMeta::single("rust-analyzer", "test", Utc::now(), 1, 1),
            false,
        )
        .unwrap(),
    ));
    let mut proposed = proposal("Constraint", "Harness entity constraint");
    proposed.files.push("src/harness.rs".into());
    let captured = daemon::capture_operation(&state, request("linked", vec![proposed])).unwrap();
    assert_eq!(captured.proposals[0].links.len(), 1);
    assert!(captured.proposals[0].unanchored.is_empty());
    let target = graph::DossierTarget::Symbol(symbol.into());
    assert!(graph::get_entity_dossier(&state, &target)
        .unwrap()
        .is_none());
    let review = ReviewRequest {
        operation_id: "linked".into(),
        accept: true,
    };
    let result = daemon::review_operation(&state, &review).unwrap();
    assert!(result.pending.is_empty() && result.conforms && result.durable);
    let dossier = graph::get_entity_dossier(&state, &target).unwrap().unwrap();
    assert!(dossier
        .direct_records
        .iter()
        .any(|p| p.iri == captured.proposals[0].iri));
    // Simulate a lost journal acknowledgement after graph acceptance: replay
    // must recognize its own materialized constrains edge, without re-linking.
    let journal = fixture.0.join(".moosedev/harness/operations/linked.json");
    let mut operation: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    operation["reviewed"] = json!(false);
    std::fs::write(&journal, serde_json::to_vec(&operation).unwrap()).unwrap();
    assert!(daemon::review_operation(&state, &review)
        .unwrap()
        .pending
        .is_empty());
}

#[test]
fn empty_capture_is_an_explicit_human_review_obligation() {
    let fixture = Fixture::new();
    let state = fixture.state();
    daemon::capture_operation(&state, request("no-change", vec![])).unwrap();
    let checkpoint = daemon::checkpoint_snapshot(&state, Some("no-change")).unwrap();
    assert_eq!(checkpoint.pending, ["operation:no-change"]);
    let review = ReviewRequest {
        operation_id: "no-change".into(),
        accept: true,
    };
    assert!(daemon::review_operation(&state, &review)
        .unwrap()
        .pending
        .is_empty());
}

#[test]
fn changed_pending_claim_requires_fresh_review() {
    use oxigraph::model::{Literal, NamedNode, Quad};
    let fixture = Fixture::new();
    let state = fixture.state();
    let captured = daemon::capture_operation(
        &state,
        request(
            "changed",
            vec![proposal("Constraint", "Original human reviewed claim")],
        ),
    )
    .unwrap();
    // Add an unseen second claim through another graph writer.
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(&captured.proposals[0].iri).unwrap(),
            NamedNode::new(&state.capture.description).unwrap(),
            Literal::new_simple_literal("Unexpected external revision"),
            NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
        ))
        .unwrap();
    assert!(daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "changed".into(),
            accept: true
        }
    )
    .is_err());
    assert_eq!(
        graph::list_proposals(&state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn browser_requests_cannot_reach_review_import_or_checkpoint() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    for path in [
        "/api/v1/harness/review",
        "/api/v1/harness/checkpoint",
        "/api/v1/graph/import",
        "/api/v1/proposals/fake/accept",
        "/api/v1/capture",
    ] {
        for origin in ["https://evil.example", "null"] {
            let response = server
                .post(path)
                .add_header("host", "127.0.0.1:7474")
                .add_header("origin", origin)
                .json(&json!({}))
                .await;
            response.assert_status_forbidden();
            assert!(response
                .headers()
                .get("access-control-allow-origin")
                .is_none());
        }
    }
    for (host, site) in [
        ("rebind.example:7474", "same-origin"),
        ("127.0.0.1:7474", "cross-site"),
    ] {
        server
            .get("/api/v1/harness/checkpoint")
            .add_header("host", host)
            .add_header("sec-fetch-site", site)
            .await
            .assert_status_forbidden();
    }
    server
        .method(axum::http::Method::OPTIONS, "/api/v1/harness/review")
        .add_header("host", "127.0.0.1:7474")
        .add_header("origin", "https://evil.example")
        .add_header("access-control-request-method", "POST")
        .await
        .assert_status_forbidden();
    server
        .get("/api/v1/health")
        .add_header("host", "127.0.0.1:7474")
        .add_header("origin", "http://127.0.0.1:7474")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn checkpoint_get_is_read_only_without_browser_fetch_metadata() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let canonical = state.data_dir.join("kg.nq");
    assert!(!canonical.exists());
    let generation = state.project_write_generation();
    let server = TestServer::new(build_routes(state.clone())).unwrap();

    // Old browsers can omit both Origin and Sec-Fetch-Site on cross-site GETs.
    let response = server
        .get("/api/v1/harness/checkpoint")
        .add_header("host", "127.0.0.1:7474")
        .await;
    response.assert_status_ok();
    assert!(!response.json::<CheckpointResponse>().durable);
    assert!(!canonical.exists());
    assert_eq!(state.project_write_generation(), generation);

    let response = server
        .post("/api/v1/harness/checkpoint")
        .add_header("host", "127.0.0.1:7474")
        .await;
    response.assert_status_ok();
    assert!(response.json::<CheckpointResponse>().durable);
    assert!(canonical.is_file());
}

#[test]
fn unseen_supersedes_relation_cannot_retire_a_record() {
    use oxigraph::model::{NamedNode, Quad};
    let fixture = Fixture::new();
    let state = fixture.state();
    let original = record(&state, "Lesson", "Still authoritative");
    let captured = daemon::capture_operation(
        &state,
        request("tampered-edge", vec![proposal("Lesson", "Ordinary lesson")]),
    )
    .unwrap();
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(&captured.proposals[0].iri).unwrap(),
            NamedNode::new(state.resolve_object_property("supersedes").unwrap()).unwrap(),
            NamedNode::new(&original).unwrap(),
            NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
        ))
        .unwrap();
    let error = daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "tampered-edge".into(),
            accept: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("relation changed"), "{error}");
    assert_eq!(
        status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
        Some("proposed")
    );
    assert_eq!(
        status_literal(&state, &original, &state.capture.status).as_deref(),
        Some("accepted")
    );
    daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "tampered-edge".into(),
            accept: false,
        },
    )
    .unwrap();
    assert_eq!(
        status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
        Some("rejected")
    );
    assert_eq!(
        status_literal(&state, &original, &state.capture.status).as_deref(),
        Some("accepted")
    );
}

#[test]
fn unseen_semantic_edges_cannot_be_accepted_with_an_unchanged_title() {
    use oxigraph::model::{NamedNode, Quad};
    let fixture = Fixture::new();
    let state = fixture.state();
    for (predicate, kind, target_kind) in [
        ("constrains", "Constraint", "ArchitecturalDecision"),
        ("violates", "AntiPattern", "Constraint"),
        ("learnedFrom", "Lesson", "ArchitecturalDecision"),
    ] {
        let target = record(&state, target_kind, &format!("Unseen {predicate} target"));
        let captured = daemon::capture_operation(
            &state,
            request(
                predicate,
                vec![proposal(kind, &format!("Unchanged {predicate} claim"))],
            ),
        )
        .unwrap();
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(&captured.proposals[0].iri).unwrap(),
                NamedNode::new(state.resolve_object_property(predicate).unwrap()).unwrap(),
                NamedNode::new(target).unwrap(),
                NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
            ))
            .unwrap();
        let error = daemon::review_operation(
            &state,
            &ReviewRequest {
                operation_id: predicate.into(),
                accept: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("relation changed"),
            "{predicate}: {error}"
        );
        assert_eq!(
            status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
            Some("proposed")
        );
    }
}

#[test]
fn missing_retraction_target_leaves_entire_batch_pending_and_rejectable() {
    use oxigraph::model::{GraphNameRef, NamedNodeRef};
    let fixture = Fixture::new();
    let state = fixture.state();
    let target = record(&state, "Requirement", "Removed after capture");
    let mut retract = proposal("Lesson", "Retire prior requirement");
    retract.retracts = Some(target.clone());
    let captured = daemon::capture_operation(
        &state,
        request(
            "missing-target",
            vec![proposal("Lesson", "Other batch member"), retract],
        ),
    )
    .unwrap();
    let quads: Vec<_> = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(&target).unwrap().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(
                NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap(),
            )),
        )
        .collect::<Result<_, _>>()
        .unwrap();
    for quad in quads {
        state.store.remove(&quad).unwrap();
    }
    let error = daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "missing-target".into(),
            accept: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("target disappeared"), "{error}");
    for entry in &captured.proposals {
        assert_eq!(
            status_literal(&state, &entry.iri, &state.capture.status).as_deref(),
            Some("proposed")
        );
    }
    daemon::review_operation(
        &state,
        &ReviewRequest {
            operation_id: "missing-target".into(),
            accept: false,
        },
    )
    .unwrap();
    for entry in &captured.proposals {
        assert_eq!(
            status_literal(&state, &entry.iri, &state.capture.status).as_deref(),
            Some("rejected")
        );
    }
}

#[tokio::test]
async fn simple_review_attests_its_revision_transition_and_retries_keep_that_pair() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    // Establish an enriched baseline, as the runner does before approving work.
    let base = daemon::context_snapshot(
        &state,
        &ContextRequest {
            topic: "review".into(),
            files: vec![],
        },
    )
    .unwrap()
    .revision;
    daemon::capture_operation(
        &state,
        request("attested", vec![proposal("Lesson", "A new learning")]),
    )
    .unwrap();
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let request = ReviewRequest {
        operation_id: "attested".into(),
        accept: true,
    };
    let response = server
        .post("/api/v1/harness/review")
        .add_header("x-moosedev-expected-revision", base.clone())
        .json(&request)
        .await;
    response.assert_status_ok();
    let checkpoint: CheckpointResponse = response.json();
    assert_eq!(
        response
            .headers()
            .get("x-moosedev-review-base-revision")
            .unwrap(),
        base.as_str()
    );
    assert_eq!(
        response
            .headers()
            .get("x-moosedev-review-result-revision")
            .unwrap(),
        checkpoint.revision.as_str()
    );
    let frozen_result = checkpoint.revision;
    record(&state, "Lesson", "Unrelated later write");
    state.note_project_write();
    let retry = server
        .post("/api/v1/harness/review")
        .add_header("x-moosedev-expected-revision", base.clone())
        .json(&request)
        .await;
    retry.assert_status_ok();
    assert_eq!(
        retry
            .headers()
            .get("x-moosedev-review-base-revision")
            .unwrap(),
        base.as_str()
    );
    assert_eq!(
        retry
            .headers()
            .get("x-moosedev-review-result-revision")
            .unwrap(),
        frozen_result.as_str()
    );
    assert_ne!(retry.json::<CheckpointResponse>().revision, frozen_result);
}

#[tokio::test]
async fn unrelated_write_before_review_cannot_be_credited_to_acceptance() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let base = daemon::context_snapshot(
        &state,
        &ContextRequest {
            topic: "review".into(),
            files: vec![],
        },
    )
    .unwrap()
    .revision;
    let captured = daemon::capture_operation(
        &state,
        request("stale-base", vec![proposal("Lesson", "Pending learning")]),
    )
    .unwrap();
    record(&state, "Constraint", "A concurrent governing change");
    state.note_project_write();
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let response = server
        .post("/api/v1/harness/review")
        .add_header("x-moosedev-expected-revision", base)
        .json(&ReviewRequest {
            operation_id: "stale-base".into(),
            accept: true,
        })
        .await;
    assert!(!response.status_code().is_success());
    assert!(response
        .headers()
        .get("x-moosedev-review-result-revision")
        .is_none());
    assert_eq!(
        status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
        Some("proposed")
    );
}

#[test]
fn capture_targets_are_typed_current_choices_from_the_context_snapshot() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let component = record(&state, "SystemComponent", "Ledger");
    let requirement = record(&state, "Requirement", "Preserve retry identity");
    let constraint = record(&state, "Constraint", "Preserve public API");
    let pending = daemon::capture_operation(
        &state,
        request(
            "pending-target",
            vec![proposal("Lesson", "Unreviewed proposal")],
        ),
    )
    .unwrap();
    let context = daemon::context_snapshot(
        &state,
        &ContextRequest {
            topic: "retry identity".into(),
            files: vec![],
        },
    )
    .unwrap();
    let choices = context.capture_targets.as_ref().unwrap();
    assert!(choices
        .components
        .iter()
        .any(|target| target.iri == component && target.kind == "SystemComponent"));
    assert!(choices
        .records
        .iter()
        .any(|target| target.iri == requirement && target.kind == "Requirement"));
    assert!(choices
        .records
        .iter()
        .any(|target| target.iri == constraint && target.kind == "Constraint"));
    assert!(!choices
        .records
        .iter()
        .any(|target| target.iri == component || target.iri == pending.proposals[0].iri));
    let mut legacy = serde_json::to_value(&context).unwrap();
    legacy.as_object_mut().unwrap().remove("capture_targets");
    assert!(serde_json::from_value::<ContextResponse>(legacy)
        .unwrap()
        .capture_targets
        .is_none());
}

#[tokio::test]
async fn capture_v2_returns_typed_collision_without_creating_an_operation() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let existing = record(&state, "Lesson", "Reuse the existing capture");
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let response = server
        .post("/api/v1/harness/capture/v2")
        .json(&CaptureV2Request {
            operation_id: "typed-collision".into(),
            owner_id: "task-a".into(),
            proposals: vec![proposal("Lesson", "Reuse the existing capture")],
            reconciliation_operation_ids: vec![],
        })
        .await;
    response.assert_status_ok();
    match response.json::<CaptureV2Response>() {
        CaptureV2Response::ReconciliationRequired { collisions } => {
            assert_eq!(collisions.len(), 1);
            assert_eq!(collisions[0].candidate_iris, [existing]);
        }
        CaptureV2Response::Captured { .. } => panic!("collision was silently captured"),
    }
    assert!(!fixture
        .0
        .join(".moosedev/harness/operations/typed-collision.json")
        .exists());
}

#[test]
fn candidate_lookup_is_complete_bounded_and_shacl_derived() {
    use daemon::reconciliation::candidate_page;

    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Requirement", "Preserve retry identity");
    let request = CaptureCandidateRequest {
        owner_id: "task-a".into(),
        proposal: proposal("ArchitecturalDecision", "Preserve retry identity"),
        topic: None,
        cursor: None,
        limit: Some(1),
    };
    let page = candidate_page(&state, &request).unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == existing)
        .unwrap();
    assert!(candidate.exact_title);
    assert_eq!(candidate.status, "accepted");
    assert!(candidate
        .literals
        .iter()
        .any(|claim| claim.predicate == "hasDescription"));
    assert!(!candidate.assertion_digest.is_empty());
    assert!(candidate
        .legal_relations
        .iter()
        .any(|relation| relation.predicate == "isMotivatedBy"));
    assert!(candidate.origin.is_none());
    assert!(!candidate.owned_by_requester);
}

#[test]
fn accepted_reuse_is_a_durable_human_receipt_without_graph_mutation() {
    use daemon::reconciliation::{candidate_page, reconcile_operation, review_operation};

    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Lesson", "A durable existing lesson");
    let proposed = proposal("Lesson", "A durable existing lesson");
    let page = candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposed.clone(),
            topic: None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == existing)
        .unwrap();
    let response = reconcile_operation(
        &state,
        ReconcileCaptureRequest {
            operation_id: "reuse-accepted".into(),
            owner_id: "task-a".into(),
            proposal: proposed,
            candidate_iri: candidate.iri.clone(),
            candidate_digest: candidate.assertion_digest.clone(),
            candidate_revision: page.revision.clone(),
            disposition: CaptureDisposition::ReuseUnchanged,
            replacement_proposal: None,
            rationale: "The existing lesson expresses the same observation.".into(),
        },
    )
    .unwrap();
    assert!(response.requires_human_review);
    assert!(response.pending_capture_operation.is_none());
    assert_eq!(
        candidate_page(
            &state,
            &CaptureCandidateRequest {
                owner_id: "task-a".into(),
                proposal: proposal("Lesson", "A durable existing lesson"),
                topic: None,
                cursor: None,
                limit: None,
            }
        )
        .unwrap()
        .revision,
        page.revision
    );
    let reviewed = review_operation(
        &state,
        &ReconcileReviewRequest {
            operation_id: "reuse-accepted".into(),
            accept: true,
        },
    )
    .unwrap();
    assert_eq!(reviewed.review, Some(true));
    record(&state, "Constraint", "An unrelated later assertion");
    assert_eq!(
        review_operation(
            &state,
            &ReconcileReviewRequest {
                operation_id: "reuse-accepted".into(),
                accept: true,
            }
        )
        .unwrap()
        .review,
        Some(true),
        "a lost review response must remain replayable after unrelated writes"
    );
    assert_eq!(
        status_literal(&state, &existing, &state.capture.status).as_deref(),
        Some("accepted")
    );
}

#[test]
fn pending_reuse_requires_persisted_owner_and_cannot_import_review_authority() {
    use daemon::reconciliation::{candidate_page, reconcile_operation, review_operation};

    let fixture = Fixture::new();
    let state = fixture.state();
    let proposal = proposal("Lesson", "One owned pending lesson");
    let captured = daemon::capture_operation_owned(
        &state,
        CaptureV2Request {
            operation_id: "owned-capture".into(),
            owner_id: "task-a".into(),
            proposals: vec![proposal.clone()],
            reconciliation_operation_ids: vec![],
        },
    )
    .unwrap();
    let candidate_iri = captured.proposals[0].iri.clone();
    let page = candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposal.clone(),
            topic: None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == candidate_iri)
        .unwrap();
    assert!(candidate.owned_by_requester);
    assert_eq!(
        candidate.origin.as_ref().unwrap().operation_id,
        "owned-capture"
    );
    let base = ReconcileCaptureRequest {
        operation_id: "reuse-owned".into(),
        owner_id: "task-a".into(),
        proposal: proposal.clone(),
        candidate_iri: candidate_iri.clone(),
        candidate_digest: candidate.assertion_digest.clone(),
        candidate_revision: page.revision.clone(),
        disposition: CaptureDisposition::ReuseUnchanged,
        replacement_proposal: None,
        rationale: "This is the already queued proposal.".into(),
    };
    let reused = reconcile_operation(&state, base.clone()).unwrap();
    assert_eq!(
        reused.pending_capture_operation.as_deref(),
        Some("owned-capture")
    );
    review_operation(
        &state,
        &ReconcileReviewRequest {
            operation_id: "reuse-owned".into(),
            accept: true,
        },
    )
    .unwrap();
    assert_eq!(
        status_literal(&state, &candidate_iri, &state.capture.status).as_deref(),
        Some("proposed"),
        "reuse review must not ratify the originating capture"
    );
    let mut external = base;
    external.operation_id = "reuse-external".into();
    external.owner_id = "task-b".into();
    assert!(reconcile_operation(&state, external).is_err());
}

#[test]
fn reuse_review_rejects_a_candidate_changed_after_the_model_disposition() {
    use daemon::reconciliation::{candidate_page, reconcile_operation, review_operation};

    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Lesson", "Fresh at recommendation time");
    let proposed = proposal("Lesson", "Fresh at recommendation time");
    let page = candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposed.clone(),
            topic: None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == existing)
        .unwrap();
    reconcile_operation(
        &state,
        ReconcileCaptureRequest {
            operation_id: "stale-reuse".into(),
            owner_id: "task-a".into(),
            proposal: proposed,
            candidate_iri: existing.clone(),
            candidate_digest: candidate.assertion_digest.clone(),
            candidate_revision: page.revision,
            disposition: CaptureDisposition::ReuseUnchanged,
            replacement_proposal: None,
            rationale: "The claims currently match.".into(),
        },
    )
    .unwrap();
    graph::retract_decision(
        &state,
        &existing,
        "Candidate changed before reuse review",
        "test-human",
        Utc::now(),
    )
    .unwrap();
    assert!(review_operation(
        &state,
        &ReconcileReviewRequest {
            operation_id: "stale-reuse".into(),
            accept: true,
        }
    )
    .is_err());
}

#[test]
fn reconciliation_retry_is_exact_and_revision_bound() {
    use daemon::reconciliation::{candidate_page, reconcile_operation};

    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Lesson", "Exact durable retry");
    let proposed = proposal("Lesson", "Exact durable retry");
    let page = candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposed.clone(),
            topic: None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == existing)
        .unwrap();
    let request = ReconcileCaptureRequest {
        operation_id: "exact-reconciliation".into(),
        owner_id: "task-a".into(),
        proposal: proposed,
        candidate_iri: existing,
        candidate_digest: candidate.assertion_digest.clone(),
        candidate_revision: page.revision,
        disposition: CaptureDisposition::ReuseUnchanged,
        replacement_proposal: None,
        rationale: "Same semantic observation.".into(),
    };
    let first = reconcile_operation(&state, request.clone()).unwrap();
    assert_eq!(first, reconcile_operation(&state, request.clone()).unwrap());
    let mut changed = request;
    changed.rationale = "Different retry payload.".into();
    assert!(reconcile_operation(&state, changed).is_err());
}

#[test]
fn completed_v2_capture_replays_without_revalidating_spent_candidate_snapshot() {
    use daemon::reconciliation::{candidate_page, reconcile_operation};

    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Lesson", "General capture recovery");
    let proposed = proposal("Lesson", "Specific capture recovery boundary");
    let page = candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposed.clone(),
            topic: Some("capture recovery".into()),
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let candidate = page
        .candidates
        .iter()
        .find(|candidate| candidate.iri == existing)
        .unwrap();
    reconcile_operation(
        &state,
        ReconcileCaptureRequest {
            operation_id: "distinct-receipt".into(),
            owner_id: "task-a".into(),
            proposal: proposed.clone(),
            candidate_iri: existing,
            candidate_digest: candidate.assertion_digest.clone(),
            candidate_revision: page.revision,
            disposition: CaptureDisposition::DistinctKnowledge,
            replacement_proposal: Some(proposed.clone()),
            rationale: "The narrower boundary is a different lesson.".into(),
        },
    )
    .unwrap();
    let request = CaptureV2Request {
        operation_id: "distinct-capture".into(),
        owner_id: "task-a".into(),
        proposals: vec![proposed],
        reconciliation_operation_ids: vec!["distinct-receipt".into()],
    };
    let first = match daemon::capture_v2_operation(&state, request.clone()).unwrap() {
        CaptureV2Response::Captured { capture } => capture,
        CaptureV2Response::ReconciliationRequired { .. } => panic!("title was noncolliding"),
    };
    // The capture itself changes the full assertion revision. A lost response
    // retry must use its durable operation journal rather than spend the receipt again.
    let retry = match daemon::capture_v2_operation(&state, request).unwrap() {
        CaptureV2Response::Captured { capture } => capture,
        CaptureV2Response::ReconciliationRequired { .. } => panic!("retry did not replay"),
    };
    assert_eq!(first.proposals[0].iri, retry.proposals[0].iri);
}

fn sha256_text(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[test]
fn purpose_candidates_are_accepted_complete_paged_and_snapshot_bound() {
    use daemon::intent_candidates::purpose_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    record(&state, "Requirement", "Purpose paging alpha");
    record(&state, "Constraint", "Purpose paging beta");
    let request = PurposeCandidateRequest {
        objective: "Purpose paging".into(),
        files: Vec::new(),
        cursor: None,
        limit: Some(1),
    };
    let graph_path = fixture.0.join(".moosedev/kg.nq");
    let graph_before = std::fs::read(&graph_path).ok();
    let generation_before = state.project_write_generation();
    let first = purpose_page(&state, &request).unwrap();
    assert_eq!(std::fs::read(&graph_path).ok(), graph_before);
    assert_eq!(state.project_write_generation(), generation_before);
    assert!(!fixture.0.join(".moosedev/harness/operations").exists());
    assert_eq!(first.candidates.len(), 1);
    assert_eq!(first.candidates[0].lifecycle, "accepted");
    assert!(!first.candidates[0].claim.literals.is_empty());
    assert!(!first.candidates[0].assertion_digest.is_empty());
    let mut continuation = request.clone();
    continuation.cursor = first.next_cursor.clone();
    let second = purpose_page(&state, &continuation).unwrap();
    assert_eq!(second.candidates.len(), 1);
    assert_ne!(first.candidates[0].iri, second.candidates[0].iri);
    record(&state, "Lesson", "Purpose paging changed snapshot");
    assert!(purpose_page(&state, &continuation)
        .unwrap_err()
        .to_string()
        .contains("stale snapshot"));
}

#[test]
fn postedit_candidates_prove_current_source_and_definition_scope() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    record(&state, "Requirement", "Rendering remains stable");
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let digest = sha256_text(&source);
    let page = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "labels.py".into(),
                before_digest: Some(digest.clone()),
                after_digest: Some(digest.clone()),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 4 },
                    end: HarnessSourcePosition { line: 0, col: 15 },
                }],
            }],
            refresh_policy: IntentRefreshPolicy::None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(page.index.status, IntentIndexStatus::Current);
    assert!(page.unresolved.is_empty());
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(
        page.candidates[0].scope_basis,
        IntentScopeBasis::ChangedDefinition
    );
    assert!(page.candidates[0].symbol.is_some());
    assert_eq!(page.candidates[0].source_digest, digest);
    assert!(!page.candidates[0].record_choices.is_empty());
    assert!(page.candidates[0].record_choices.iter().any(|record| {
        record.kind == "Requirement" && record.legal_predicates.iter().any(|p| p == "concerns")
    }));
}

#[test]
fn deletion_keeps_historical_scope_without_offering_a_link_candidate() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    assert_eq!(
        state
            .substrate()
            .unwrap()
            .indexed_source_digest("labels.py"),
        Some(sha256_text(&source))
    );
    std::fs::remove_file(fixture.0.join("labels.py")).unwrap();
    let page = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "labels.py".into(),
                before_digest: Some(sha256_text(&source)),
                after_digest: None,
                changed_ranges: Vec::new(),
            }],
            refresh_policy: IntentRefreshPolicy::None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    assert!(page.unresolved.is_empty());
    assert!(page.candidates.is_empty());
    assert_eq!(page.deleted.len(), 1);
    assert!(page.deleted[0].symbol.contains("render_name"));
}

#[test]
fn deletion_without_matching_daemon_source_proof_abstains() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    // Deliberately do not ask the substrate to prove/cache the source before deletion.
    std::fs::remove_file(fixture.0.join("labels.py")).unwrap();
    let page = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "labels.py".into(),
                before_digest: Some(sha256_text(&source)),
                after_digest: None,
                changed_ranges: Vec::new(),
            }],
            refresh_policy: IntentRefreshPolicy::None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    assert!(page.deleted.is_empty());
    assert!(page.candidates.is_empty());
    assert_eq!(page.index.status, IntentIndexStatus::Stale);
    assert!(page
        .unresolved
        .iter()
        .any(|item| item.reason.contains("proven")));
}

#[test]
fn deletion_scope_recovers_from_durable_index_proof_after_daemon_restart() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let first = fixture.state();
    install_intent_index(&fixture, &first);
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let before_digest = sha256_text(&source);
    std::fs::remove_file(fixture.0.join("labels.py")).unwrap();
    drop(first);
    let restarted = fixture.state();
    install_deleted_intent_index_with_digest(&restarted, &fixture.0, before_digest.clone());
    let request = IntentCandidateRequest {
        files: vec![ChangedFile {
            file: "labels.py".into(),
            before_digest: Some(before_digest.clone()),
            after_digest: None,
            changed_ranges: Vec::new(),
        }],
        refresh_policy: IntentRefreshPolicy::None,
        cursor: None,
        limit: None,
    };
    let page = intent_page(&restarted, &request).unwrap();
    assert!(page.unresolved.is_empty());
    assert!(page.candidates.is_empty());
    assert_eq!(page.deleted.len(), 1);
    assert!(page.deleted[0].symbol.contains("render_name"));
    assert_eq!(
        intent_page(&restarted, &request).unwrap(),
        page,
        "lost response retry must replay the same historical projection"
    );
    let mut forged = request;
    forged.files[0].before_digest = Some("00".repeat(32));
    let rejected = intent_page(&restarted, &forged).unwrap();
    assert!(rejected.deleted.is_empty());
    assert_eq!(rejected.index.status, IntentIndexStatus::Stale);
    assert!(!rejected.unresolved.is_empty());
}

#[test]
fn purpose_candidates_never_nominate_pending_records() {
    use daemon::intent_candidates::purpose_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    let accepted = record(&state, "Requirement", "Current purpose choice");
    let pending = daemon::capture_operation(
        &state,
        request(
            "pending-purpose",
            vec![proposal("Requirement", "Pending purpose choice")],
        ),
    )
    .unwrap()
    .proposals[0]
        .iri
        .clone();
    let page = purpose_page(
        &state,
        &PurposeCandidateRequest {
            objective: "purpose choice".into(),
            files: Vec::new(),
            cursor: None,
            limit: Some(16),
        },
    )
    .unwrap();
    assert!(page
        .candidates
        .iter()
        .any(|candidate| candidate.iri == accepted));
    assert!(page
        .candidates
        .iter()
        .all(|candidate| candidate.iri != pending && candidate.lifecycle == "accepted"));
}

#[test]
fn new_unindexed_file_is_conservative_and_never_guesses_a_symbol() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = "def helper():\n    return 1\n";
    std::fs::write(fixture.0.join("helper.py"), source).unwrap();
    let page = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "helper.py".into(),
                before_digest: None,
                after_digest: Some(sha256_text(source)),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 0 },
                    end: HarnessSourcePosition { line: 1, col: 12 },
                }],
            }],
            refresh_policy: IntentRefreshPolicy::None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(
        page.candidates[0].scope_basis,
        IntentScopeBasis::ConservativeFile
    );
    assert!(page.candidates[0].symbol.is_none());
    assert!(page.candidates[0].definition_range.is_none());
    assert_eq!(page.index.status, IntentIndexStatus::Stale);
}

#[test]
fn unsupported_refresh_is_explicit_and_does_not_hide_current_index() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index_as(&fixture, &state, "rust-analyzer");
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let digest = sha256_text(&source);
    let page = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "labels.py".into(),
                before_digest: Some(digest.clone()),
                after_digest: Some(digest),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 4 },
                    end: HarnessSourcePosition { line: 0, col: 15 },
                }],
            }],
            refresh_policy: IntentRefreshPolicy::SupportedFrozen,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(page.index.refresh_action, IntentRefreshAction::Unsupported);
    assert_eq!(page.index.status, IntentIndexStatus::Current);
}

#[test]
fn candidate_digest_changes_with_index_snapshot_even_when_scope_is_identical() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let digest = sha256_text(&source);
    let request = IntentCandidateRequest {
        files: vec![ChangedFile {
            file: "labels.py".into(),
            before_digest: Some(digest.clone()),
            after_digest: Some(digest),
            changed_ranges: vec![HarnessSourceRange {
                start: HarnessSourcePosition { line: 0, col: 4 },
                end: HarnessSourcePosition { line: 0, col: 15 },
            }],
        }],
        refresh_policy: IntentRefreshPolicy::None,
        cursor: None,
        limit: None,
    };
    let first = intent_page(&state, &request).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    install_intent_index(&fixture, &state);
    let second = intent_page(&state, &request).unwrap();
    assert_ne!(first.index.revision, second.index.revision);
    assert_ne!(
        first.candidates[0].candidate_digest,
        second.candidates[0].candidate_digest
    );
}

#[test]
fn postedit_rejects_non_utf8_boundary_columns_in_current_source() {
    use daemon::intent_candidates::intent_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = "éx\n";
    std::fs::write(fixture.0.join("unicode.py"), source).unwrap();
    let error = intent_page(
        &state,
        &IntentCandidateRequest {
            files: vec![ChangedFile {
                file: "unicode.py".into(),
                before_digest: None,
                after_digest: Some(sha256_text(source)),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 1 },
                    end: HarnessSourcePosition { line: 0, col: 2 },
                }],
            }],
            refresh_policy: IntentRefreshPolicy::None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("UTF-8 byte boundary"));
}
