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
