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
            timeouts: Default::default(),
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
        learned_from: None,
        reconciled: vec![],
    }
}

fn request(id: &str, proposals: Vec<KnowledgeProposal>) -> CaptureV2Request {
    CaptureV2Request {
        operation_id: id.into(),
        owner_id: "test-owner".into(),
        proposals,
        changed: vec![],
        restated: vec![],
    }
}

fn captured(response: CaptureV2Response) -> CaptureResponse {
    match response {
        CaptureV2Response::Captured { capture } => capture,
        CaptureV2Response::Collision { collisions } => {
            panic!("unexpected collision {collisions:?}")
        }
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

/// Normalized (versionless) symbols: the one definition the fixture index
/// contains, and one it does not.
const RENDER_NAME: &str = "scip-python python sample . labels/render_name().";
const NORMALIZE_NAME: &str = "scip-python python sample . labels/_normalize_name().";

/// A one-module rust-analyzer index for `src/harness.rs`; returns its symbol.
fn install_module_index(state: &AppState) -> &'static str {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation};
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
    symbol
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
    let mut meta = SubstrateMeta::single(
        producer,
        SubstrateMeta::current_head(&fixture.0),
        Utc::now(),
        1,
        1,
    );
    meta.indexed_started_at = Some(Utc::now());
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, &fixture.0).unwrap(),
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
            symbol: choices.entities[0].symbol.clone(),
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
            symbol: RENDER_NAME.into(),
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
fn intent_resolver_distinguishes_missing_index_and_unindexed_target() {
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
                symbol: NORMALIZE_NAME.into(),
                source_digest: None,
            }],
        },
    )
    .unwrap();
    assert!(response.links.is_empty());
    assert_eq!(response.unresolved.len(), 1);

    // The unresolved response is a durable assessment, not a permanent ban on
    // a target. A subsequent plan uses a new operation with an indexed symbol.
    let next = link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "intent-planned-followup".into(),
            revision: daemon::accepted_revision(&state).unwrap(),
            bindings: vec![IntentBinding {
                record_iri,
                file: "labels.py".into(),
                symbol: RENDER_NAME.into(),
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
                symbol: RENDER_NAME.into(),
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
            symbol: RENDER_NAME.into(),
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
            bindings: [RENDER_NAME, NORMALIZE_NAME]
                .into_iter()
                .map(|symbol| IntentBinding {
                    record_iri: record_iri.clone(),
                    file: "labels.py".into(),
                    symbol: symbol.into(),
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
async fn evidence_only_context_returns_topic_claims_without_inventory_or_dossiers() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record(&state, "Constraint", "Established coding constraint");
    let request = |topic: &str, evidence_only: bool, files: Vec<String>| ContextRequest {
        topic: topic.into(),
        files,
        evidence_only,
        max_bytes: None,
    };
    let full =
        daemon::context_snapshot(&state, &request("coding constraint", false, vec![])).unwrap();
    assert!(full.context.contains("Current knowledge inventory:"));
    assert!(full
        .context
        .contains("search with words from a record name returns its complete claims."));
    assert!(
        !full.context.contains("get_relevant_context"),
        "{}",
        full.context
    );
    assert!(full.governing_constraints.is_empty());
    assert!(!full
        .context
        .contains("retrieve more context when scope expands"));
    assert!(full.evidence_iris.is_empty());
    // Nothing is linked, so the full context falls back to topic evidence.
    assert!(full.context.contains("\nTopic evidence (fallback;"));
    assert!(!full.context.contains("Linked evidence"));
    let full_record = full
        .records
        .iter()
        .find(|record| record.iri == existing)
        .unwrap();
    assert_eq!(full_record.kind, "Constraint");
    assert!(full_record
        .claim
        .contains("Established Established coding constraint"));
    assert_eq!(
        full_record.provenance,
        vec!["current inventory", "topic fallback"]
    );

    let evidence =
        daemon::context_snapshot(&state, &request("coding constraint", true, vec![])).unwrap();
    assert_eq!(evidence.evidence_iris, vec![existing.clone()]);
    assert!(evidence.context.contains(&existing));
    assert!(evidence
        .context
        .contains("hasDescription: Established Established coding constraint"));
    // Exact bytes: topic evidence renders the header (label is the record's
    // rdfs:label, empty for this title-only fixture) and the claim body.
    assert_eq!(
        evidence.context,
        format!(
            "\n[Constraint]  ({existing})\nhasDescription: Established Established coding constraint\n"
        )
    );
    assert!(!evidence.context.contains("Current knowledge inventory"));
    assert!(!evidence.context.contains("Recall:"));
    assert!(evidence.files.is_empty());
    assert_eq!(evidence.revision, full.revision);
    assert_eq!(evidence.records.len(), 1);
    assert_eq!(evidence.records[0].iri, existing);
    assert_eq!(evidence.records[0].kind, "Constraint");
    assert_eq!(evidence.records[0].provenance, vec!["topic match"]);
    assert!(evidence.records[0]
        .claim
        .contains("Established Established coding constraint"));

    let unmatched = daemon::context_snapshot(
        &state,
        &request("zz_unmatched_inventory_probe", true, vec![]),
    )
    .unwrap();
    assert!(unmatched.evidence_iris.is_empty());
    assert!(unmatched.records.is_empty());
    assert!(unmatched.context.is_empty(), "{}", unmatched.context);
    assert!(daemon::context_snapshot(
        &state,
        &request("coding constraint", true, vec!["labels.py".into()])
    )
    .is_err());
}

#[test]
fn evidence_budget_degrades_whole_records_and_receipts_every_selected_record() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let rule = record_with(
        &state,
        "Constraint",
        "Budget probe governing rule",
        &format!(
            "The budget probe rule governs delivery. {}",
            "constraint-tail-λ".repeat(180)
        ),
        "accepted",
    );
    let first_lesson = record_with(
        &state,
        "Lesson",
        "Budget probe first lesson",
        &format!(
            "The budget probe first sentence is sufficient. {}",
            "first-lesson-tail-λ".repeat(180)
        ),
        "accepted",
    );
    let second_lesson = record_with(
        &state,
        "Lesson",
        "Budget probe second lesson",
        &format!(
            "The budget probe second sentence is sufficient. {}",
            "second-lesson-tail-λ".repeat(180)
        ),
        "accepted",
    );
    state.note_project_write();
    let request = |max_bytes| ContextRequest {
        topic: "budget probe".into(),
        files: vec![],
        evidence_only: true,
        max_bytes,
    };

    let unbounded = daemon::context_snapshot(&state, &request(None)).unwrap();
    assert!(
        unbounded.context.contains("constraint-tail-λ"),
        "{}",
        unbounded.context
    );
    assert!(
        unbounded.context.contains("first-lesson-tail-λ"),
        "{}",
        unbounded.context
    );
    assert!(
        unbounded.context.contains("second-lesson-tail-λ"),
        "{}",
        unbounded.context
    );
    let receipt = unbounded.delivery_receipt.as_ref().unwrap();
    assert_eq!(receipt.max_bytes, None);
    assert_eq!(receipt.context_bytes, unbounded.context.len());
    assert!(receipt
        .records
        .iter()
        .all(|record| record.tier == ContextRecordDeliveryTier::FullClaim));

    // One byte below the all-full rendering forces the lowest-ranked record
    // through the first rung as an atomic block, never through a byte slice.
    let first = daemon::context_snapshot(
        &state,
        &request(Some(unbounded.context.len().saturating_sub(1))),
    )
    .unwrap();
    let first_receipt = first.delivery_receipt.as_ref().unwrap();
    assert!(first.context.len() <= first_receipt.max_bytes.unwrap());
    assert!(first_receipt
        .records
        .iter()
        .any(|record| record.tier == ContextRecordDeliveryTier::FirstSentence));
    assert!(first
        .context
        .contains("claim shortened by the caller byte bound"));
    assert!(first.context.contains("Delivery under caller byte bound:"));

    // Tighten against the exact prior result: the same record becomes a title
    // pointer, then a counted omission. No tail fragment ever appears.
    let title = daemon::context_snapshot(
        &state,
        &request(Some(first.context.len().saturating_sub(1))),
    )
    .unwrap();
    assert!(title
        .delivery_receipt
        .as_ref()
        .unwrap()
        .records
        .iter()
        .any(|record| record.tier == ContextRecordDeliveryTier::TitleOnly));
    let omitted = daemon::context_snapshot(
        &state,
        &request(Some(title.context.len().saturating_sub(1))),
    )
    .unwrap();
    let omitted_receipt = omitted.delivery_receipt.as_ref().unwrap();
    assert!(omitted.context.contains("omitted record(s) (Lesson: 1)"));
    assert!(omitted_receipt
        .records
        .iter()
        .any(|record| record.tier == ContextRecordDeliveryTier::Omitted));
    assert_eq!(omitted_receipt.context_bytes, omitted.context.len());
    assert!(omitted.context.len() <= omitted_receipt.max_bytes.unwrap());
    for tail in [
        "constraint-tail-λ",
        "first-lesson-tail-λ",
        "second-lesson-tail-λ",
    ] {
        assert!(
            matches!(omitted.context.matches(tail).count(), 0 | 180),
            "a tail is either delivered whole in its record or absent: {tail}"
        );
    }
    let delivered = &omitted.evidence_iris;
    for record in &omitted_receipt.records {
        assert_eq!(
            delivered.contains(&record.iri),
            record.tier != ContextRecordDeliveryTier::Omitted,
            "evidence_iris must describe only what the model saw"
        );
    }
    let governing = omitted_receipt
        .records
        .iter()
        .find(|record| record.iri == rule)
        .unwrap();
    assert_ne!(governing.tier, ContextRecordDeliveryTier::Omitted);
    assert_eq!(omitted_receipt.records.len(), 3);
    assert!(omitted_receipt
        .records
        .iter()
        .any(|record| record.iri == first_lesson));
    assert!(omitted_receipt
        .records
        .iter()
        .any(|record| record.iri == second_lesson));
}

#[tokio::test]
async fn file_dossier_carries_the_topic_evidence_claim_body() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let existing = record(&state, "Constraint", "Established harness constraint");
    graph::link_code(
        &state,
        &existing,
        "constrains",
        &graph::CodeSelector::Symbol(symbol.into()),
        "test-human",
    )
    .unwrap();
    state.note_project_write();
    let request = |evidence_only: bool, files: Vec<String>| ContextRequest {
        topic: "harness constraint".into(),
        files,
        evidence_only,
        max_bytes: None,
    };

    let evidence = daemon::context_snapshot(&state, &request(true, vec![])).unwrap();
    assert_eq!(evidence.evidence_iris, vec![existing.clone()]);
    let body = evidence
        .context
        .strip_prefix(&format!("\n[Constraint]  ({existing})\n"))
        .expect("topic evidence header");
    assert!(
        body.starts_with("hasDescription: Established Established harness constraint\n"),
        "{body}"
    );

    let attached =
        daemon::context_snapshot(&state, &request(false, vec!["src/harness.rs".into()])).unwrap();
    // The file dossier and topic evidence render one claim body, byte for byte.
    assert!(
        attached.files[0].dossier.contains(body),
        "{}",
        attached.files[0].dossier
    );
}

fn record_with(
    state: &AppState,
    kind: &str,
    title: &str,
    description: &str,
    status: &str,
) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: state.resolve_class(kind).unwrap(),
            class_local: kind.into(),
            properties: vec![
                (state.capture.title.clone(), title.into()),
                (state.capture.description.clone(), description.into()),
                (state.capture.status.clone(), status.into()),
            ],
        },
        "test-human",
        Utc::now(),
    )
    .unwrap()
}

/// Full (not evidence-only) context for `files`.
fn linked_context(state: &AppState, topic: &str, files: &[&str]) -> ContextResponse {
    state.note_project_write();
    daemon::context_snapshot(
        state,
        &ContextRequest {
            topic: topic.into(),
            files: files.iter().map(|file| file.to_string()).collect(),
            evidence_only: false,
            max_bytes: None,
        },
    )
    .unwrap()
}

/// The context from its evidence section header on, so inventory names do not count.
fn evidence_section(context: &str) -> &str {
    let start = context
        .find("\nLinked evidence (")
        .or_else(|| context.find("\nTopic evidence ("))
        .expect("evidence section");
    &context[start..]
}

/// An accepted ArchitecturalDecision linked to the module entity of `src/harness.rs`.
fn direct_decision(state: &AppState, symbol: &str, title: &str) -> String {
    let decision = record_with(
        state,
        "ArchitecturalDecision",
        title,
        &format!("Decided {title}"),
        "accepted",
    );
    graph::link_code(
        state,
        &decision,
        "concerns",
        &graph::CodeSelector::Symbol(symbol.into()),
        "test-human",
    )
    .unwrap();
    decision
}

#[tokio::test]
async fn linked_evidence_delivers_unlinked_component_constraint() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let billing = record(&state, "SystemComponent", "Billing");
    let decision = direct_decision(&state, symbol, "Harness fee rules live in the policy");
    graph::relate(&state, &decision, "concerns", &billing).unwrap();
    // The deciding rule is linked to no code, only to the component.
    let np7 = record_with(
        &state,
        "Constraint",
        "Regulation NP-7 compliance for account billing",
        "No late fee may be charged to an account in a registered non-profit segment.",
        "accepted",
    );
    graph::relate(&state, &np7, "concerns", &billing).unwrap();
    for n in 0..3 {
        record_with(
            &state,
            "Constraint",
            &format!("Harness fee rule {n}"),
            &format!("Harness fee distractor claim {n}."),
            "accepted",
        );
    }

    let response = linked_context(&state, "harness fee rule", &["src/harness.rs"]);
    let evidence = evidence_section(&response.context);
    assert!(evidence.starts_with("\nLinked evidence ("), "{evidence}");
    assert!(!response.context.contains("\nTopic evidence ("));
    assert!(
        evidence.contains(&format!(
            "({np7})\nvia: component Billing\nclaim under Project rules\n"
        )),
        "{evidence}"
    );
    assert!(
        !evidence.contains("No late fee may be charged"),
        "{evidence}"
    );
    let rule = response
        .governing_constraints
        .iter()
        .find(|rule| rule.iri == np7)
        .expect("governing rule");
    assert_eq!(rule.via, "via: component Billing");
    // The shared claim renderer: literals, then relationship lines.
    assert!(
        rule.claim.starts_with(
            "hasDescription: No late fee may be charged to an account in a registered non-profit segment.\nconcerns: "
        ),
        "{}",
        rule.claim
    );
    for n in 0..3 {
        assert!(!response
            .context
            .contains(&format!("Harness fee distractor claim {n}.")));
    }
    // The direct record's claim is carried by the file dossier only.
    assert!(!evidence.contains(&decision));
    assert!(!evidence.contains("Decided Harness fee rules live in the policy"));
    assert!(
        response.files[0]
            .dossier
            .contains("hasDescription: Decided Harness fee rules live in the policy\n"),
        "{}",
        response.files[0].dossier
    );
}

#[tokio::test]
async fn linked_evidence_hops_follow_motivation_lessons_supersession_and_lifecycle() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let billing = record(&state, "SystemComponent", "Billing");
    let decision = direct_decision(&state, symbol, "Harness current decision");
    graph::relate(&state, &decision, "concerns", &billing).unwrap();
    let need = record_with(
        &state,
        "Requirement",
        "Harness need",
        "The harness needs fees.",
        "accepted",
    );
    graph::relate(&state, &decision, "isMotivatedBy", &need).unwrap();
    // Only the inverse edge is asserted for this driver.
    let driver = record_with(
        &state,
        "Constraint",
        "Harness driver",
        "A driving constraint.",
        "accepted",
    );
    graph::relate(&state, &driver, "motivates", &decision).unwrap();
    let lesson = record_with(
        &state,
        "Lesson",
        "Harness lesson",
        "Learned from the decision.",
        "accepted",
    );
    graph::relate(&state, &lesson, "learnedFrom", &decision).unwrap();

    let old = direct_decision(&state, symbol, "Harness old decision");
    let replace = |from: &str, title: &str| {
        graph::supersede_decision(
            &state,
            &graph::SupersedeInput {
                superseded_iri: from.into(),
                new: RecordInput {
                    class_iri: state.resolve_class("ArchitecturalDecision").unwrap(),
                    class_local: "ArchitecturalDecision".into(),
                    properties: vec![
                        (state.capture.title.clone(), title.into()),
                        (
                            state.capture.description.clone(),
                            format!("Decided {title}"),
                        ),
                    ],
                },
                rationale: format!("Replaced by {title}"),
            },
            "test-human",
            Utc::now(),
        )
        .unwrap()
        .new_iri
    };
    let middle = replace(&old, "Harness middle decision");
    let head = replace(&middle, "Harness head decision");

    let proposed = record_with(
        &state,
        "Constraint",
        "Harness proposed constraint",
        "Proposed only.",
        "proposed",
    );
    graph::relate(&state, &proposed, "concerns", &billing).unwrap();
    let rejected = record_with(
        &state,
        "Constraint",
        "Harness rejected constraint",
        "Rejected only.",
        "rejected",
    );
    graph::relate(&state, &rejected, "concerns", &billing).unwrap();
    // A first read materializes inferred inverse edges; pin the revision after
    // it, so the walk below is held to changing nothing.
    linked_context(&state, "harness", &[]);
    let revision = daemon::accepted_revision(&state).unwrap();

    let response = linked_context(&state, "harness", &["src/harness.rs"]);
    let evidence = evidence_section(&response.context);
    assert!(
        evidence.contains(&format!(
            "({need})\nvia: motivates Harness current decision\nhasDescription: The harness needs fees.\n"
        )),
        "{evidence}"
    );
    assert!(evidence.contains(&format!(
        "({driver})\nvia: motivates Harness current decision\nclaim under Project rules\n"
    )));
    let governing: Vec<_> = response
        .governing_constraints
        .iter()
        .map(|rule| (rule.iri.as_str(), rule.via.as_str()))
        .collect();
    assert_eq!(
        governing,
        vec![(driver.as_str(), "via: motivates Harness current decision")],
        "proposed and rejected Constraints are never governing"
    );
    assert!(response.governing_constraints[0]
        .claim
        .starts_with("hasDescription: A driving constraint.\nmotivates: "));
    assert!(evidence.contains(&format!(
        "({lesson})\nvia: learned from Harness current decision\nhasDescription: Learned from the decision.\n"
    )));
    assert!(evidence.contains(&format!(
        "({head})\nvia: supersedes Harness old decision\nhasDescription: Decided Harness head decision\n"
    )));
    // The head's claim links back to what it supersedes; the intermediate
    // record itself is never delivered.
    assert!(
        !evidence.contains(&format!("({middle})\nvia:")),
        "only the chain's head is delivered"
    );
    assert!(!evidence.contains(&proposed));
    assert!(!evidence.contains(&rejected));
    assert!(!evidence.contains(&format!("({decision})")));
    assert_eq!(response.revision, revision);
    assert_eq!(
        linked_context(&state, "harness", &["src/harness.rs"]).context,
        response.context
    );
}

#[tokio::test]
async fn linked_evidence_never_drops_accepted_constraints() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let billing = record(&state, "SystemComponent", "Billing");
    let decision = direct_decision(&state, symbol, "Harness cap decision");
    graph::relate(&state, &decision, "concerns", &billing).unwrap();
    let constraints: Vec<String> = (0..30)
        .map(|n| {
            let constraint = record_with(
                &state,
                "Constraint",
                &format!("Harness cap constraint {n:02}"),
                &format!("Cap constraint claim {n:02}."),
                "accepted",
            );
            graph::relate(&state, &constraint, "concerns", &billing).unwrap();
            constraint
        })
        .collect();
    for n in 0..10 {
        let lesson = record_with(
            &state,
            "Lesson",
            &format!("Harness cap lesson {n}"),
            &format!("Cap lesson claim {n}."),
            "accepted",
        );
        graph::relate(&state, &lesson, "learnedFrom", &decision).unwrap();
    }

    let response = linked_context(&state, "harness cap", &["src/harness.rs"]);
    let evidence = evidence_section(&response.context);
    for constraint in &constraints {
        assert!(
            evidence.contains(&format!("({constraint})\nvia: component Billing\n")),
            "{constraint} listed"
        );
    }
    assert_eq!(
        evidence
            .matches("hasDescription: Cap constraint claim")
            .count(),
        0
    );
    assert_eq!(evidence.matches("claim under Project rules\n").count(), 24);
    assert_eq!(response.governing_constraints.len(), 30);
    assert_eq!(
        response
            .governing_constraints
            .iter()
            .filter(|rule| rule
                .claim
                .starts_with("hasDescription: Cap constraint claim"))
            .count(),
        24
    );
    assert!(response.governing_constraints[24..]
        .iter()
        .all(|rule| rule.claim.is_empty()));
    assert_eq!(
        evidence.matches("hasDescription: Cap lesson claim").count(),
        6
    );
    assert!(
        evidence.ends_with(
            "\n10 further linked records not shown in full (Constraint: 6; Lesson: 4); search project knowledge for their claims\n"
        ),
        "{evidence}"
    );
    assert_eq!(
        linked_context(&state, "harness cap", &["src/harness.rs"]).context,
        response.context
    );
}

#[tokio::test]
async fn linked_evidence_walks_unindexed_file_by_component_path() {
    let fixture = Fixture::new();
    let state = fixture.state();
    std::fs::create_dir_all(fixture.0.join("src")).unwrap();
    std::fs::write(
        fixture.0.join("src/fees.py"),
        "def late_fee():\n    return 0\n",
    )
    .unwrap();
    let billing = record(&state, "SystemComponent", "Billing");
    graph::declare_component_paths(&state, &billing, &["src/fees.py".into()]).unwrap();
    let rule = record_with(
        &state,
        "Constraint",
        "Billing path constraint",
        "Fees round half up.",
        "accepted",
    );
    graph::relate(&state, &rule, "concerns", &billing).unwrap();

    let response = linked_context(&state, "unrelated topic words", &["src/fees.py"]);
    let evidence = evidence_section(&response.context);
    assert!(
        evidence.contains(&format!(
            "({rule})\nvia: component Billing\nclaim under Project rules\n"
        )),
        "{evidence}"
    );
    assert_eq!(response.governing_constraints.len(), 1);
    assert_eq!(
        response.governing_constraints[0].claim,
        format!("hasDescription: Fees round half up.\nconcerns: {billing}\n")
    );
}

#[tokio::test]
async fn fallback_topic_evidence_excludes_dossier_claims() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let direct = record(&state, "Constraint", "Harness fallback constraint");
    graph::link_code(
        &state,
        &direct,
        "constrains",
        &graph::CodeSelector::Symbol(symbol.into()),
        "test-human",
    )
    .unwrap();
    let other = record(&state, "Constraint", "Harness fallback guidance");

    let response = linked_context(&state, "harness fallback", &["src/harness.rs"]);
    let evidence = evidence_section(&response.context);
    assert!(
        evidence.starts_with("\nTopic evidence (fallback;"),
        "{evidence}"
    );
    assert!(
        evidence.contains(&format!(
            "({other})\nhasDescription: Established Harness fallback guidance\n"
        )),
        "{evidence}"
    );
    assert!(!evidence.contains(&direct), "{evidence}");
    assert!(response.files[0]
        .dossier
        .contains("hasDescription: Established Harness fallback constraint\n"));
    let governing: Vec<_> = response
        .governing_constraints
        .iter()
        .map(|rule| (rule.iri.as_str(), rule.via.as_str()))
        .collect();
    assert_eq!(
        governing,
        vec![(direct.as_str(), "via: linked to src/harness.rs")],
        "fallback topic hits are never governing"
    );

    // Without files nothing is excluded, so the fallback carries both.
    let bare = linked_context(&state, "harness fallback", &[]);
    let bare_evidence = evidence_section(&bare.context);
    assert!(bare_evidence.contains(&direct) && bare_evidence.contains(&other));
}

#[tokio::test]
async fn inventory_is_omitted_once_linked_evidence_is_supplied() {
    const INVENTORY: &str = "Current knowledge inventory:\n";
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let decision = direct_decision(&state, symbol, "Harness inventory decision");

    // The first request (no files) and a walk that adds nothing beyond the
    // file dossier keep the inventory and the preamble that names it.
    let first = linked_context(&state, "harness inventory", &[]);
    assert!(first.context.contains(INVENTORY), "{}", first.context);
    assert!(first
        .context
        .contains("the inventory lists current record names only"));
    let dossier_only = linked_context(&state, "harness inventory", &["src/harness.rs"]);
    assert!(dossier_only.governing_constraints.is_empty());
    assert!(
        dossier_only.context.contains(INVENTORY),
        "{}",
        dossier_only.context
    );

    // Linked evidence omits it, and the preamble no longer names it.
    let need = record_with(
        &state,
        "Requirement",
        "Harness inventory need",
        "The harness needs its motivating record.",
        "accepted",
    );
    graph::relate(&state, &decision, "isMotivatedBy", &need).unwrap();
    let linked = linked_context(&state, "harness inventory", &["src/harness.rs"]);
    assert!(linked.governing_constraints.is_empty());
    assert!(
        linked.context.contains("\nLinked evidence ("),
        "{}",
        linked.context
    );
    assert!(!linked.context.contains(INVENTORY), "{}", linked.context);
    let preamble = linked.context.lines().next().unwrap_or_default();
    assert!(preamble.starts_with("Recall: "), "{preamble}");
    assert!(!preamble.contains("inventory"), "{preamble}");
    // The first request still lists every record name.
    assert!(linked_context(&state, "harness inventory", &[])
        .context
        .contains(INVENTORY));
}

#[tokio::test]
async fn harness_claims_are_compact_while_push_keeps_full_claims() {
    use moosedev::policy::{self, PolicyDecision, PolicyEvent};
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    state.publish_http_addr("127.0.0.1:7474".parse().unwrap());
    let billing = record(&state, "SystemComponent", "Billing");
    let decision = direct_decision(&state, symbol, "Harness compact decision");
    graph::relate(&state, &decision, "concerns", &billing).unwrap();
    let need = record_with(
        &state,
        "Requirement",
        "Harness compact need",
        "The harness needs short claims.",
        "accepted",
    );
    graph::relate(&state, &decision, "isMotivatedBy", &need).unwrap();
    let rule = record_with(
        &state,
        "Constraint",
        "Harness compact rule",
        "Claims stay short in small prompts.",
        "accepted",
    );
    graph::relate(&state, &decision, "isConstrainedBy", &rule).unwrap();

    // Search (evidence-only topic evidence) renders the compact claim: link
    // lines name their targets' titles, at most three, then an omission line.
    state.note_project_write();
    let search = daemon::context_snapshot(
        &state,
        &ContextRequest {
            topic: "harness compact decision".into(),
            files: vec![],
            evidence_only: true,
            max_bytes: None,
        },
    )
    .unwrap();
    let header = format!("({decision})\n");
    let start = search.context.find(&header).expect("decision in search") + header.len();
    let rest = &search.context[start..];
    let body = &rest[..rest.find("\n[").unwrap_or(rest.len())];
    let links: Vec<&str> = body
        .lines()
        .filter(|line| {
            !line.starts_with("hasDescription: ") && !line.ends_with("retrieve them if relevant.")
        })
        .collect();
    assert_eq!(links.len(), 3, "{body}");
    assert!(body.contains("concerns: Billing\n"), "{body}");
    assert!(!body.contains("://"), "{body}");
    assert!(
        body.ends_with("1 further relationships omitted; retrieve them if relevant.\n"),
        "{body}"
    );

    // The harness file dossier carries the same bytes, without workbench links.
    let attached = linked_context(&state, "harness compact decision", &["src/harness.rs"]);
    let dossier = &attached.files[0].dossier;
    assert!(dossier.contains(body), "{dossier}");
    assert!(!dossier.contains("127.0.0.1:7474"), "{dossier}");

    // Policy push, which MCP and hover share, keeps the full claim and link.
    let push = policy::evaluate(
        &state,
        &state.project_root(),
        &PolicyEvent::EntityTouched {
            file: "src/harness.rs".into(),
            line: None,
            col: None,
            max_bytes: None,
        },
    )
    .unwrap();
    let PolicyDecision::Inject {
        dossier_markdown, ..
    } = push
    else {
        panic!("push injects the dossier");
    };
    assert!(
        dossier_markdown.contains(&format!("concerns: {billing}\n")),
        "{dossier_markdown}"
    );
    assert!(
        dossier_markdown.contains("127.0.0.1:7474"),
        "{dossier_markdown}"
    );
}

#[tokio::test]
async fn governing_rules_alone_omit_the_inventory() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
    let rule = record_with(
        &state,
        "Constraint",
        "Harness inventory rule",
        "Record names are omitted once rules arrive.",
        "accepted",
    );
    graph::link_code(
        &state,
        &rule,
        "constrains",
        &graph::CodeSelector::Symbol(symbol.into()),
        "test-human",
    )
    .unwrap();
    let response = linked_context(&state, "harness inventory", &["src/harness.rs"]);
    assert_eq!(response.governing_constraints.len(), 1);
    assert!(!response.context.contains("\nLinked evidence ("));
    assert!(
        !response.context.contains("Current knowledge inventory:"),
        "{}",
        response.context
    );
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
        evidence_only: false,
        max_bytes: None,
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
            evidence_only: false,
            max_bytes: None,
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
        .post("/api/v1/harness/capture/v2")
        .json(&request("all-kinds", proposals))
        .await;
    response.assert_status_ok();
    let captured = captured(response.json::<CaptureV2Response>());
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
        evidence_only: false,
        max_bytes: None,
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
    let rejected = server
        .post("/api/v1/harness/capture/v2")
        .json(&invalid)
        .await;
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
        .post("/api/v1/harness/capture/v2")
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
    let retry = server
        .post("/api/v1/harness/capture/v2")
        .json(&request)
        .await;
    retry.assert_status_ok();
    assert_eq!(
        captured(retry.json::<CaptureV2Response>()).proposals[0].iri,
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
    let fixture = Fixture::new();
    let state = fixture.state();
    let symbol = install_module_index(&state);
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
            evidence_only: false,
            max_bytes: None,
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

/// The accepted revision the runner holds when it asks for attestation.
fn review_base(state: &AppState) -> String {
    daemon::context_snapshot(
        state,
        &ContextRequest {
            topic: "review".into(),
            files: vec![],
            evidence_only: false,
            max_bytes: None,
        },
    )
    .unwrap()
    .revision
}

struct Reviewed {
    ok: bool,
    base: Option<String>,
    result: Option<String>,
    revision: Option<String>,
}

/// Accept `operation_id` over HTTP with the runner's expected-revision header.
async fn review_expecting(state: &Arc<AppState>, operation_id: &str, expected: &str) -> Reviewed {
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let response = server
        .post("/api/v1/harness/review")
        .add_header("x-moosedev-expected-revision", expected.to_string())
        .json(&ReviewRequest {
            operation_id: operation_id.into(),
            accept: true,
        })
        .await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    let ok = response.status_code().is_success();
    Reviewed {
        ok,
        base: header("x-moosedev-review-base-revision"),
        result: header("x-moosedev-review-result-revision"),
        revision: ok.then(|| response.json::<CheckpointResponse>().revision),
    }
}

#[tokio::test]
async fn unrelated_write_before_review_cannot_be_credited_to_acceptance() {
    for kind in ["Lesson", "Constraint"] {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.state());
        let base = review_base(&state);
        let captured = daemon::capture_operation(
            &state,
            request("stale-base", vec![proposal(kind, "Pending learning")]),
        )
        .unwrap();
        record(&state, "Constraint", "A concurrent governing change");
        state.note_project_write();
        let reviewed = review_expecting(&state, "stale-base", &base).await;
        assert!(!reviewed.ok, "{kind}");
        assert!(reviewed.result.is_none(), "{kind}");
        assert_eq!(
            status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
            Some("proposed"),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn governing_review_attests_its_own_revision_transition() {
    for kind in ["Constraint", "Requirement"] {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.state());
        let base = review_base(&state);
        daemon::capture_operation(
            &state,
            request("governing", vec![proposal(kind, &format!("A new {kind}"))]),
        )
        .unwrap();
        let reviewed = review_expecting(&state, "governing", &base).await;
        assert!(reviewed.ok, "{kind}");
        let revision = reviewed.revision.unwrap();
        assert_ne!(
            revision, base,
            "{kind}: acceptance changed accepted knowledge"
        );
        assert_eq!(reviewed.base.as_deref(), Some(base.as_str()), "{kind}");
        assert_eq!(
            reviewed.result.as_deref(),
            Some(revision.as_str()),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn linked_review_attests_its_own_link_materialization() {
    for (kind, preexisting) in [
        ("Lesson", false),
        ("Lesson", true),
        ("Constraint", false),
        ("Constraint", true),
    ] {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.state());
        let symbol = install_module_index(&state);
        let target = graph::DossierTarget::Symbol(symbol.into());
        if preexisting {
            let earlier = record(&state, "Lesson", "An earlier harness lesson");
            graph::link_code(
                &state,
                &earlier,
                "concerns",
                &graph::CodeSelector::Symbol(symbol.into()),
                "test-human",
            )
            .unwrap();
            state.note_project_write();
        }
        let base = review_base(&state);
        let mut proposed = proposal(kind, "Harness entity knowledge");
        proposed.files.push("src/harness.rs".into());
        let captured =
            daemon::capture_operation(&state, request("linked-attested", vec![proposed])).unwrap();
        assert_eq!(captured.proposals[0].links.len(), 1);
        assert_eq!(
            graph::get_entity_dossier(&state, &target)
                .unwrap()
                .is_some(),
            preexisting,
            "the entity exists before review only in the pre-existing variant"
        );
        let reviewed = review_expecting(&state, "linked-attested", &base).await;
        assert!(reviewed.ok, "{kind} preexisting {preexisting}");
        let revision = reviewed.revision.unwrap();
        assert_ne!(revision, base);
        assert_eq!(
            reviewed.base.as_deref(),
            Some(base.as_str()),
            "{kind} preexisting {preexisting}"
        );
        assert_eq!(
            reviewed.result.as_deref(),
            Some(revision.as_str()),
            "{kind} preexisting {preexisting}: the link materialization is this acceptance's own write"
        );
        let dossier = graph::get_entity_dossier(&state, &target).unwrap().unwrap();
        assert!(dossier
            .direct_records
            .iter()
            .any(|record| record.iri == captured.proposals[0].iri));
    }
}

#[tokio::test]
async fn lifecycle_review_is_never_attested() {
    for retracts in [false, true] {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.state());
        let existing = record(&state, "Lesson", "Replaceable learning");
        let base = review_base(&state);
        let mut proposed = proposal("Lesson", "Replacement learning");
        if retracts {
            proposed.retracts = Some(existing.clone());
        } else {
            proposed.supersedes = Some(existing.clone());
        }
        daemon::capture_operation(&state, request("lifecycle", vec![proposed])).unwrap();
        let reviewed = review_expecting(&state, "lifecycle", &base).await;
        assert!(reviewed.ok, "retracts {retracts}");
        assert!(reviewed.base.is_none(), "retracts {retracts}");
        assert!(reviewed.result.is_none(), "retracts {retracts}");
    }
}

#[tokio::test]
async fn foreign_entity_mint_before_review_is_not_credited() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let symbol = install_module_index(&state);
    let other = record(&state, "Lesson", "A concurrent harness lesson");
    let base = review_base(&state);
    let mut proposed = proposal("Constraint", "Harness entity constraint");
    proposed.files.push("src/harness.rs".into());
    let captured =
        daemon::capture_operation(&state, request("foreign-mint", vec![proposed])).unwrap();
    // Another client mints the entity the capture links to after the runner
    // read its revision.
    graph::link_code(
        &state,
        &other,
        "concerns",
        &graph::CodeSelector::Symbol(symbol.into()),
        "test-human",
    )
    .unwrap();
    state.note_project_write();
    let reviewed = review_expecting(&state, "foreign-mint", &base).await;
    assert!(!reviewed.ok);
    assert!(reviewed.result.is_none());
    assert_eq!(
        status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
        Some("proposed")
    );
}

#[test]
fn context_response_without_contract_fields_deserializes_with_empty_vectors() {
    let legacy = json!({
        "project_root": "/tmp/project",
        "revision": "accepted-v1",
        "context": "",
        "files": []
    });
    let context: ContextResponse = serde_json::from_value(legacy).unwrap();
    assert!(context.capture_contracts.is_empty());
    assert!(context.intent_contracts.is_empty());
    assert!(context.governing_constraints.is_empty());
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
            changed: vec![],
            restated: vec![],
        })
        .await;
    response.assert_status_ok();
    match response.json::<CaptureV2Response>() {
        CaptureV2Response::Collision { collisions } => {
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
    use daemon::candidates::candidate_page;

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
fn capture_v2_replays_a_completed_operation_from_its_journal() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let request = request(
        "replayed-capture",
        vec![proposal("Lesson", "Specific capture recovery boundary")],
    );
    let first = captured(daemon::capture_v2_operation(&state, request.clone()).unwrap());
    // The first capture makes the title pending knowledge. A lost-response
    // retry must replay the durable operation journal rather than re-run
    // collision typing against the record it created.
    let retry = captured(daemon::capture_v2_operation(&state, request).unwrap());
    assert_eq!(first.proposals[0].iri, retry.proposals[0].iri);
    assert_eq!(
        graph::list_proposals(&state, Some("proposed"))
            .unwrap()
            .len(),
        1
    );
}

fn sha256_text(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[test]
fn associate_reports_deleted_files_as_unresolved_without_bindings() {
    use daemon::associate::associate_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let requirement = record(&state, "Requirement", "Rendering remains stable");
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    std::fs::remove_file(fixture.0.join("labels.py")).unwrap();
    let page = associate_page(
        &state,
        &AssociateRequest {
            files: vec![ChangedFile {
                file: "labels.py".into(),
                before_digest: Some(sha256_text(&source)),
                after_digest: None,
                changed_ranges: Vec::new(),
                before_ranges: vec![],
                ranges_coalesced: false,
            }],
            governing: [("labels.py".to_string(), vec![requirement])]
                .into_iter()
                .collect(),
            refresh_policy: IntentRefreshPolicy::None,
            knowledge_revision: daemon::accepted_revision(&state).unwrap(),
        },
    )
    .unwrap();
    assert!(page.bindings.is_empty());
    assert!(page.skipped.is_empty());
    assert!(page.ungoverned.is_empty());
    assert_eq!(page.unresolved.len(), 1);
    assert_eq!(page.unresolved[0].file, "labels.py");
    assert!(page.unresolved[0].reason.contains("deleted"));
    assert_eq!(page.index.status, IntentIndexStatus::Stale);
}

#[test]
fn unindexed_changed_file_yields_no_binding_and_a_stale_index() {
    use daemon::associate::associate_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let requirement = record(&state, "Requirement", "Helpers stay pure");
    let source = "def helper():\n    return 1\n";
    std::fs::write(fixture.0.join("helper.py"), source).unwrap();
    let page = associate_page(
        &state,
        &AssociateRequest {
            files: vec![ChangedFile {
                file: "helper.py".into(),
                before_digest: None,
                after_digest: Some(sha256_text(source)),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 0 },
                    end: HarnessSourcePosition { line: 1, col: 12 },
                }],
                before_ranges: vec![],
                ranges_coalesced: false,
            }],
            governing: [("helper.py".to_string(), vec![requirement])]
                .into_iter()
                .collect(),
            refresh_policy: IntentRefreshPolicy::None,
            knowledge_revision: daemon::accepted_revision(&state).unwrap(),
        },
    )
    .unwrap();
    assert!(page.bindings.is_empty(), "{:#?}", page.bindings);
    assert!(page.skipped.is_empty());
    assert!(page.unresolved.is_empty(), "{:?}", page.unresolved);
    assert!(page.ungoverned.is_empty());
    assert_eq!(page.index.status, IntentIndexStatus::Stale);
}

#[test]
fn unsupported_refresh_is_explicit_and_does_not_hide_current_index() {
    use daemon::associate::associate_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index_as(&fixture, &state, "rust-analyzer");
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let digest = sha256_text(&source);
    let page = associate_page(
        &state,
        &AssociateRequest {
            files: vec![changed("labels.py", &digest, &[(0, 4, 0, 15)])],
            governing: Default::default(),
            refresh_policy: IntentRefreshPolicy::SupportedFrozen,
            knowledge_revision: daemon::accepted_revision(&state).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(page.index.refresh_action, IntentRefreshAction::Unsupported);
    assert_eq!(page.index.status, IntentIndexStatus::Current);
    assert_eq!(page.index.producer.as_deref(), Some("rust-analyzer"));
}

#[test]
fn candidate_digest_changes_with_index_snapshot_even_when_scope_is_identical() {
    use daemon::associate::associate_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let requirement = record(&state, "Requirement", "Rendering remains stable");
    let source = std::fs::read_to_string(fixture.0.join("labels.py")).unwrap();
    let digest = sha256_text(&source);
    let request = AssociateRequest {
        files: vec![changed("labels.py", &digest, &[(0, 4, 0, 15)])],
        governing: [("labels.py".to_string(), vec![requirement.clone()])]
            .into_iter()
            .collect(),
        refresh_policy: IntentRefreshPolicy::None,
        knowledge_revision: daemon::accepted_revision(&state).unwrap(),
    };
    let first = associate_page(&state, &request).unwrap();
    assert_eq!(first.bindings.len(), 1, "{:#?}", first);
    assert_eq!(first.bindings[0].name.as_deref(), Some("render_name"));
    assert_eq!(first.bindings[0].record_iri, requirement);
    assert_eq!(first.bindings[0].predicate, "concerns");
    assert_eq!(first.bindings[0].source_digest, digest);
    std::thread::sleep(std::time::Duration::from_millis(2));
    install_intent_index(&fixture, &state);
    let second = associate_page(&state, &request).unwrap();
    assert_eq!(second.bindings.len(), 1);
    assert_ne!(first.index.revision, second.index.revision);
    assert_ne!(first.scope_digest, second.scope_digest);
    assert_ne!(
        first.bindings[0].candidate_digest,
        second.bindings[0].candidate_digest
    );
    assert_eq!(
        first.bindings[0].assertion_digest, second.bindings[0].assertion_digest,
        "the record itself did not change; only the index snapshot did"
    );
}

#[test]
fn associate_rejects_non_utf8_boundary_columns() {
    use daemon::associate::associate_page;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_intent_index(&fixture, &state);
    let source = "éx\n";
    std::fs::write(fixture.0.join("unicode.py"), source).unwrap();
    let error = associate_page(
        &state,
        &AssociateRequest {
            files: vec![ChangedFile {
                file: "unicode.py".into(),
                before_digest: None,
                after_digest: Some(sha256_text(source)),
                changed_ranges: vec![HarnessSourceRange {
                    start: HarnessSourcePosition { line: 0, col: 1 },
                    end: HarnessSourcePosition { line: 0, col: 2 },
                }],
                before_ranges: vec![],
                ranges_coalesced: false,
            }],
            governing: Default::default(),
            refresh_policy: IntentRefreshPolicy::None,
            knowledge_revision: daemon::accepted_revision(&state).unwrap(),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("UTF-8 byte boundary"), "{error}");
}

/// Two Python definitions plus a parameter in `labels.py`, and one function in
/// a test module: the symbolic association filter has something to drop.
fn install_symbolic_index(fixture: &Fixture, state: &AppState) {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation};
    let source = "def render_name(name):\n    return name.strip()\n\ndef normalize(value):\n    return value\n\nLIMIT = 80\n";
    let path = fixture.0.join("labels.py");
    std::fs::write(&path, source).unwrap();
    let tests_dir = fixture.0.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let test_source = "def test_render():\n    assert True\n";
    std::fs::write(tests_dir.join("test_labels.py"), test_source).unwrap();
    for file in [&path, &tests_dir.join("test_labels.py")] {
        std::fs::File::open(file)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2)),
            )
            .unwrap();
    }
    fn definition(
        symbol: &str,
        name: &str,
        kind: symbol_information::Kind,
        range: Vec<i32>,
        enclosing: Vec<i32>,
    ) -> (SymbolInformation, Occurrence) {
        let mut info = SymbolInformation::new();
        info.symbol = symbol.into();
        info.display_name = name.into();
        info.kind = EnumOrUnknown::new(kind);
        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol.into();
        occurrence.symbol_roles = 1;
        occurrence.range = range;
        occurrence.enclosing_range = enclosing;
        (info, occurrence)
    }
    let mut labels = Document::new();
    labels.relative_path = "labels.py".into();
    for (info, occurrence) in [
        definition(
            "scip-python python sample 1 labels/render_name().",
            "render_name",
            symbol_information::Kind::Function,
            vec![0, 4, 15],
            vec![0, 0, 1, 23],
        ),
        definition(
            "scip-python python sample 1 labels/render_name().(name)",
            "name",
            symbol_information::Kind::Parameter,
            vec![0, 16, 20],
            vec![],
        ),
        definition(
            "scip-python python sample 1 labels/normalize().",
            "normalize",
            symbol_information::Kind::Function,
            vec![3, 4, 13],
            vec![3, 0, 4, 16],
        ),
        // scip-python emits no kind for parameters; the grammar still says so.
        definition(
            "scip-python python sample 1 labels/normalize().(value)",
            "value",
            symbol_information::Kind::UnspecifiedKind,
            vec![3, 14, 19],
            vec![],
        ),
        definition(
            "scip-python python sample 1 labels/LIMIT.",
            "LIMIT",
            symbol_information::Kind::Constant,
            vec![6, 0, 5],
            vec![],
        ),
    ] {
        labels.symbols.push(info);
        labels.occurrences.push(occurrence);
    }
    let mut tests = Document::new();
    tests.relative_path = "tests/test_labels.py".into();
    let (info, occurrence) = definition(
        "scip-python python sample 1 tests/test_labels/test_render().",
        "test_render",
        symbol_information::Kind::Function,
        vec![0, 4, 15],
        vec![0, 0, 1, 15],
    );
    tests.symbols.push(info);
    tests.occurrences.push(occurrence);
    let mut index = Index::new();
    index.documents.push(labels);
    index.documents.push(tests);
    // The rooted substrate re-checks the repository head two seconds after
    // construction; index the head the fixture actually has so a slow suite
    // run cannot turn the synthetic index stale mid-test.
    let mut meta = SubstrateMeta::single(
        "scip-python",
        SubstrateMeta::current_head(&fixture.0),
        Utc::now(),
        2,
        4,
    );
    meta.indexed_started_at = Some(Utc::now());
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, &fixture.0).unwrap(),
    ));
}

fn changed(file: &str, digest: &str, ranges: &[(u32, u32, u32, u32)]) -> ChangedFile {
    ChangedFile {
        file: file.into(),
        before_digest: Some(digest.into()),
        after_digest: Some(digest.into()),
        changed_ranges: ranges
            .iter()
            .map(|(l1, c1, l2, c2)| HarnessSourceRange {
                start: HarnessSourcePosition {
                    line: *l1,
                    col: *c1,
                },
                end: HarnessSourcePosition {
                    line: *l2,
                    col: *c2,
                },
            })
            .collect(),
        before_ranges: vec![],
        ranges_coalesced: false,
    }
}

#[test]
fn symbolic_association_filters_kinds_and_binds_by_legal_predicate() {
    use daemon::associate::associate_page;
    use daemon::intent::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_symbolic_index(&fixture, &state);
    let requirement = record(&state, "Requirement", "Display names stay stable");
    let constraint = record(&state, "Constraint", "Labels never exceed one line");
    // The requirement is already linked to render_name through the ordinary
    // reviewed link path, so it is an existing direct dossier record there.
    let resolved = resolve_entities(
        &state,
        &IntentResolveRequest {
            files: vec!["labels.py".into()],
            refresh_index: false,
        },
    )
    .unwrap();
    let render = resolved
        .entities
        .iter()
        .find(|entity| entity.name == "render_name")
        .unwrap();
    link_operation(
        &state,
        IntentLinkRequest {
            operation_id: "seed-render-link".into(),
            revision: resolved.revision.clone(),
            bindings: vec![IntentBinding {
                record_iri: requirement.clone(),
                file: "labels.py".into(),
                symbol: render.symbol.clone(),
                source_digest: Some(render.source_digest.clone()),
            }],
        },
    )
    .unwrap();
    review_links(
        &state,
        &ReviewRequest {
            operation_id: "seed-render-link".into(),
            accept: true,
        },
    )
    .unwrap();
    let revision = daemon::accepted_revision(&state).unwrap();
    let labels_digest = sha256_text(&std::fs::read_to_string(fixture.0.join("labels.py")).unwrap());
    let tests_digest =
        sha256_text(&std::fs::read_to_string(fixture.0.join("tests/test_labels.py")).unwrap());
    let request = AssociateRequest {
        files: vec![
            // A parameter token, the new helper's signature line (its name and
            // its unspecified-kind parameter) and a module constant changed.
            changed(
                "labels.py",
                &labels_digest,
                &[(0, 16, 0, 20), (3, 4, 3, 20), (6, 0, 7, 0)],
            ),
            changed("tests/test_labels.py", &tests_digest, &[(0, 4, 0, 15)]),
        ],
        governing: [
            (
                "labels.py".to_string(),
                vec![requirement.clone(), constraint.clone()],
            ),
            (
                "tests/test_labels.py".to_string(),
                vec![requirement.clone()],
            ),
        ]
        .into_iter()
        .collect(),
        refresh_policy: IntentRefreshPolicy::None,
        knowledge_revision: revision.clone(),
    };
    let page = associate_page(&state, &request).unwrap();
    assert_eq!(
        page.index.status,
        IntentIndexStatus::Current,
        "index {:#?}; unresolved {:?}; stale={}; head={:?}; indexed labels digest={:?}",
        page.index,
        page.unresolved,
        state.substrate().unwrap().is_stale(),
        moosedev::code::substrate::SubstrateMeta::current_head(&fixture.0),
        state
            .substrate()
            .unwrap()
            .indexed_source_digest("labels.py"),
    );
    assert!(page.unresolved.is_empty(), "{:?}", page.unresolved);
    assert!(page.ungoverned.is_empty());
    let bound: Vec<(String, String, String, String)> = page
        .bindings
        .iter()
        .map(|b| {
            (
                b.name.clone().unwrap(),
                b.record_kind.clone(),
                b.predicate.clone(),
                format!("{:?}", b.basis),
            )
        })
        .collect();
    assert_eq!(
        bound,
        vec![
            // Sorted by file, definition range, symbol, then record IRI.
            (
                "render_name".to_string(),
                "Constraint".to_string(),
                "constrains".to_string(),
                "Obligation".to_string()
            ),
            (
                "normalize".to_string(),
                "Constraint".to_string(),
                "constrains".to_string(),
                "Obligation".to_string()
            ),
            (
                "normalize".to_string(),
                "Requirement".to_string(),
                "concerns".to_string(),
                "Obligation".to_string()
            ),
            // A module-level constant is a knowledge anchor of its own.
            (
                "LIMIT".to_string(),
                "Constraint".to_string(),
                "constrains".to_string(),
                "Obligation".to_string()
            ),
            (
                "LIMIT".to_string(),
                "Requirement".to_string(),
                "concerns".to_string(),
                "Obligation".to_string()
            ),
        ],
        "{:#?}",
        page.bindings
    );
    assert!(page
        .bindings
        .iter()
        .all(|b| b.source_digest == labels_digest && !b.assertion_digest.is_empty()));
    assert_eq!(
        page.bindings[0].scope_basis,
        IntentScopeBasis::EnclosingDefinition,
        "the parameter change resolves to its enclosing kept function"
    );
    assert_eq!(
        page.bindings[1].scope_basis,
        IntentScopeBasis::ChangedDefinition
    );
    assert_eq!(
        page.bindings[2].scope_basis,
        IntentScopeBasis::ChangedDefinition
    );
    let skipped: Vec<(String, String)> = page
        .skipped
        .iter()
        .map(|s| {
            (
                s.symbol.rsplit('/').next().unwrap().to_string(),
                format!("{:?}", s.reason),
            )
        })
        .collect();
    assert!(
        skipped.contains(&("render_name().(name)".to_string(), "Parameter".to_string())),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("test_render().".to_string(), "TestPath".to_string())),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("normalize().(value)".to_string(), "Parameter".to_string())),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("render_name().".to_string(), "AlreadyLinked".to_string())),
        "{skipped:?}"
    );
    assert!(page
        .skipped
        .iter()
        .find(|s| s.reason == SkipReason::AlreadyLinked)
        .is_some_and(|s| s.record_iri.as_deref() == Some(requirement.as_str())));
    // Deterministic and idempotent for the same snapshot.
    let again = associate_page(&state, &request).unwrap();
    assert_eq!(again, page);
    // Bound to the knowledge revision the runner holds.
    let stale = AssociateRequest {
        knowledge_revision: "stale".into(),
        ..request.clone()
    };
    let error = associate_page(&state, &stale).unwrap_err().to_string();
    assert!(
        error.contains("knowledge changed before association"),
        "{error}"
    );
    // A file with no governing records and no sibling dossier records is
    // reported, not guessed.
    let ungoverned = AssociateRequest {
        files: vec![changed("labels.py", &labels_digest, &[(3, 4, 3, 13)])],
        governing: Default::default(),
        refresh_policy: IntentRefreshPolicy::None,
        knowledge_revision: revision.clone(),
    };
    let page = associate_page(&state, &ungoverned).unwrap();
    assert!(page.bindings.is_empty());
    assert_eq!(page.ungoverned, vec!["labels.py".to_string()]);
    // A sibling's direct record reaches the new helper through the file dossier.
    let sibling = AssociateRequest {
        files: vec![changed(
            "labels.py",
            &labels_digest,
            &[(0, 16, 0, 20), (3, 4, 3, 13)],
        )],
        governing: Default::default(),
        refresh_policy: IntentRefreshPolicy::None,
        knowledge_revision: revision,
    };
    let page = associate_page(&state, &sibling).unwrap();
    assert_eq!(page.bindings.len(), 1, "{:#?}", page.bindings);
    assert_eq!(page.bindings[0].name.as_deref(), Some("normalize"));
    assert_eq!(page.bindings[0].record_iri, requirement);
    assert_eq!(page.bindings[0].basis, DerivedBasis::FileDossier);
    assert!(page.ungoverned.is_empty());
}

fn record_described(state: &AppState, kind: &str, title: &str, description: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: state.resolve_class(kind).unwrap(),
            class_local: kind.into(),
            properties: vec![
                (
                    "http://www.w3.org/2000/01/rdf-schema#label".into(),
                    title.into(),
                ),
                (state.capture.title.clone(), title.into()),
                (state.capture.description.clone(), description.into()),
            ],
        },
        "test-human",
        Utc::now(),
    )
    .unwrap()
}

#[test]
fn relate_with_confidence_annotates_edges_and_conforms() {
    use oxigraph::model::{GraphNameRef, NamedNodeRef};
    let fixture = Fixture::new();
    let state = fixture.state();
    let broad = record(&state, "Lesson", "Retry ledger keys are stable");
    let narrow = record(
        &state,
        "Lesson",
        "Retry ledger keys are stable for every tenant",
    );
    let outcome = graph::relate_with_confidence(&state, &narrow, "refines", &broad, 0.77).unwrap();
    assert!(
        outcome.predicate_iri.ends_with("#refines"),
        "{}",
        outcome.predicate_iri
    );
    assert_eq!(
        graph::relation_confidence(&state, &narrow, "refines", &broad).unwrap(),
        Some(0.77)
    );
    // A second annotation replaces the first; the edge itself is one quad.
    graph::relate_with_confidence(&state, &narrow, "refines", &broad, 0.9).unwrap();
    assert_eq!(
        graph::relation_confidence(&state, &narrow, "refines", &broad).unwrap(),
        Some(0.9)
    );
    let project = GraphNameRef::NamedNode(NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap());
    let edges = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(&narrow).unwrap().into()),
            Some(NamedNodeRef::new(&outcome.predicate_iri).unwrap()),
            Some(NamedNodeRef::new(&broad).unwrap().into()),
            Some(project),
        )
        .count();
    assert_eq!(edges, 1);
    // Symmetric restatement between two existing records.
    let a = record(&state, "Lesson", "Configuration is loaded once");
    let b = record(&state, "Lesson", "Configuration is read a single time");
    graph::relate_with_confidence(&state, &a, "restates", &b, 0.85).unwrap();
    assert_eq!(
        graph::relation_confidence(&state, &a, "restates", &b).unwrap(),
        Some(0.85)
    );
    assert_eq!(
        graph::relation_confidence(&state, &b, "restates", &a).unwrap(),
        None,
        "the annotation sits on the asserted direction; the inverse is inferred, not annotated"
    );
    // The annotations are ordinary SPARQL-visible quads on the reifiers.
    let query = "SELECT (COUNT(?r) AS ?n) WHERE { GRAPH <https://moosedev.dev/kg/project> { ?r <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> ?edge ; <http://trivyn.io/ontology#confidence> ?c } }";
    let count = moosedev::sparql::run_query(&state.store, query).unwrap();
    assert!(count.contains("\"value\":\"2\""), "{count}");
    // SHACL still conforms and the canonical export carries the reifiers.
    let report = moosedev::validation::validate_project(&state).unwrap();
    assert!(report.conforms(), "{:#?}", report.violations);
    let dump = moosedev::export::export_canonical_project(&state.store).unwrap();
    assert!(
        dump.text.contains("reifies"),
        "export dropped the reifier quads"
    );
    assert!(dump.text.contains("<<("), "export dropped the triple term");
    let candidate = daemon::candidates::candidate_page(
        &state,
        &CaptureCandidateRequest {
            owner_id: "task-a".into(),
            proposal: proposal("Lesson", "Retry ledger keys are stable"),
            topic: None,
            cursor: None,
            limit: None,
        },
    )
    .unwrap();
    let broad_candidate = candidate
        .candidates
        .iter()
        .find(|candidate| candidate.iri == broad)
        .unwrap();
    assert!(broad_candidate
        .relations
        .iter()
        .any(|relation| relation.predicate == "refines" && relation.incoming));
    assert!(
        graph::relate_with_confidence(&state, &a, "restates", &b, 1.5).is_err(),
        "confidence outside 0..=1 is refused"
    );
}

#[test]
fn symbolic_reconciliation_scores_restates_refines_and_distinct() {
    use daemon::reconcile_score::*;
    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record_described(
        &state,
        "Lesson",
        "Retry ledger keys are stable across restarts",
        "The ledger key is derived from the request id and never regenerated after a restart.",
    );
    let thresholds = ReconcileThresholds::from_env().unwrap();
    assert_eq!(thresholds, ReconcileThresholds::default());
    assert_eq!(
        (
            thresholds.restates,
            thresholds.refines,
            thresholds.refines_containment,
            thresholds.tiebreak_band
        ),
        (0.80, 0.55, 0.60, 0.08)
    );
    let mut same = proposal("Lesson", "Retry ledger keys are stable across restarts");
    same.description = "Ledger keys derive from the request id and survive a restart.".into();
    let scored = score_proposal(&state, "task-a", &same, thresholds).unwrap();
    assert!(
        matches!(&scored.disposition, ScoredDisposition::Restates { candidate_iri, .. } if candidate_iri == &existing),
        "{:#?}",
        scored
    );
    assert_eq!(scored.candidates[0].title_score, 1.0);
    assert_eq!(scored.thresholds, thresholds);
    assert!(!scored.candidate_revision.is_empty() && !scored.proposal_digest.is_empty());

    let mut narrower = proposal("Lesson", "Tenant-scoped retry ledger keys stay stable");
    narrower.description = "The ledger key is derived from the request id and never regenerated after a restart. Each tenant owns a separate key space, so keys never collide across tenants.".into();
    let scored = score_proposal(&state, "task-a", &narrower, thresholds).unwrap();
    assert!(
        matches!(&scored.disposition, ScoredDisposition::Refines { candidate_iri, containment, .. } if candidate_iri == &existing && *containment >= 0.6),
        "{:#?}",
        scored
    );
    assert!(scored.candidates[0].longer);

    let mut unrelated = proposal("Lesson", "Display labels are normalized before rendering");
    unrelated.description = "Labels are trimmed and lower-cased in the renderer.".into();
    let scored = score_proposal(&state, "task-a", &unrelated, thresholds).unwrap();
    assert!(
        matches!(scored.disposition, ScoredDisposition::Distinct { .. }),
        "{:#?}",
        scored
    );

    // Same title, different kind: never reconciled across kinds.
    let other_kind = proposal(
        "ArchitecturalDecision",
        "Retry ledger keys are stable across restarts",
    );
    let scored = score_proposal(&state, "task-a", &other_kind, thresholds).unwrap();
    assert!(
        matches!(
            scored.disposition,
            ScoredDisposition::Distinct { nearest: None }
        ),
        "{:#?}",
        scored
    );

    // Receipts are durable, idempotent, and carry the thresholds.
    let receipt = ScoreReceipt {
        operation_id: "score-1".into(),
        owner_id: "task-a".into(),
        proposal_digest: "p".into(),
        candidate_revision: "r".into(),
        thresholds,
        disposition: "restates".into(),
        candidate_iri: Some(existing.clone()),
        candidate_digest: Some("d".into()),
        score: 0.91,
        confidence: 0.91,
        resolved_by: "symbolic".into(),
    };
    assert_eq!(record_receipt(&state, receipt.clone()).unwrap(), receipt);
    assert_eq!(record_receipt(&state, receipt.clone()).unwrap(), receipt);
    assert_eq!(
        load_receipt(&state, "score-1").unwrap(),
        Some(receipt.clone())
    );
    let mut changed = receipt.clone();
    changed.score = 0.5;
    assert!(record_receipt(&state, changed).is_err());
    assert_eq!(load_receipt(&state, "score-none").unwrap(), None);
}

fn typing_request(
    operation_id: &str,
    note: &str,
    plan_summary: &str,
    changed: &[&str],
    checks: &[(&str, bool, bool)],
    revision: &str,
) -> CaptureTypeRequest {
    CaptureTypeRequest {
        owner_id: "task-a".into(),
        operation_id: operation_id.into(),
        note: note.into(),
        note_evidence: vec!["event 12: capture note".into()],
        plan_summary: plan_summary.into(),
        changed_files: changed.iter().map(|f| f.to_string()).collect(),
        check_history: checks
            .iter()
            .map(|(command, success, after_edit)| CheckOutcome {
                command: command.to_string(),
                success: *success,
                after_edit: *after_edit,
            })
            .collect(),
        knowledge_revision: revision.into(),
        obligation_iris: vec![],
        obligations_digest: String::new(),
    }
}

#[tokio::test]
async fn symbolic_capture_typing_reconciles_without_a_sensor() {
    use daemon::capture_type::capture_type_operation;
    use daemon::reconcile_score::load_receipt;
    let fixture = Fixture::new();
    let state = fixture.state();
    let existing = record_described(
        &state,
        "ArchitecturalDecision",
        "Preserve display behavior",
        "Display labels keep their rendered form when the helper changes.",
    );
    let revision = daemon::accepted_revision(&state).unwrap();
    // The plan restates an accepted decision: receipt only. A check that
    // failed then passed after the edit is a distinct lesson.
    let request = typing_request(
        "type-1",
        "",
        "Preserve display behavior",
        &["labels.py"],
        &[("pytest -q", false, false), ("pytest -q", true, true)],
        &revision,
    );
    let response = capture_type_operation(&state, request.clone())
        .await
        .unwrap();
    assert_eq!(response.typing_mode, TypingMode::SymbolicOnly);
    assert!(response
        .typing_note
        .as_deref()
        .unwrap()
        .contains("symbolic typing only"));
    assert_eq!(response.thresholds, ReconcileThresholds::default());
    assert_eq!(response.proposals.len(), 2, "{:#?}", response.proposals);
    let decision = &response.proposals[0];
    assert_eq!(decision.origin, ProposalOrigin::SymbolicDecision);
    assert_eq!(decision.proposal.kind, "ArchitecturalDecision");
    assert_eq!(decision.proposal.files, vec!["labels.py".to_string()]);
    assert!(decision
        .proposal
        .evidence
        .contains(&"plan approved: Preserve display behavior".to_string()));
    let TypedDisposition::Restates {
        candidate_iri,
        receipt_operation_id,
        confidence,
        ..
    } = &decision.disposition
    else {
        panic!("{:#?}", decision.disposition);
    };
    assert_eq!(candidate_iri, &existing);
    assert!(*confidence >= 0.8);
    let receipt = load_receipt(&state, receipt_operation_id).unwrap().unwrap();
    assert_eq!(receipt.disposition, "restates");
    assert_eq!(receipt.thresholds, ReconcileThresholds::default());
    assert_eq!(receipt.candidate_iri.as_deref(), Some(existing.as_str()));
    assert_eq!(receipt.resolved_by, "symbolic");
    let lesson = &response.proposals[1];
    assert_eq!(lesson.origin, ProposalOrigin::SymbolicLesson);
    assert_eq!(lesson.proposal.kind, "Lesson");
    assert!(lesson.proposal.title.contains("pytest -q"));
    assert!(lesson
        .proposal
        .evidence
        .contains(&"check passed after edit: pytest -q".to_string()));
    assert!(matches!(
        lesson.disposition,
        TypedDisposition::Distinct { .. }
    ));
    assert!(lesson.proposal.reconciled.is_empty());
    // Idempotent by operation id; a different request under the same id is refused.
    assert_eq!(
        capture_type_operation(&state, request.clone())
            .await
            .unwrap(),
        response
    );
    let mut changed = request.clone();
    changed.note = "different".into();
    assert!(capture_type_operation(&state, changed).await.is_err());
    // The distinct lesson captures through the ordinary path.
    let captured = daemon::capture_operation(
        &state,
        CaptureV2Request {
            operation_id: "cap-1".into(),
            owner_id: "task-a".into(),
            proposals: vec![lesson.proposal.clone()],
            changed: vec![],
            restated: vec![],
        },
    )
    .unwrap();
    assert_eq!(captured.proposals.len(), 1);
    assert_eq!(
        status_literal(&state, &captured.proposals[0].iri, &state.capture.status).as_deref(),
        Some("proposed")
    );

    // A narrower plan refines the accepted decision: proposal plus a
    // receipt-backed edge, annotated with confidence at capture.
    let revision = daemon::accepted_revision(&state).unwrap();
    let request = typing_request(
        "type-2",
        "Display labels keep their rendered form when the helper changes. Tenant-scoped labels additionally keep the tenant prefix so two tenants never render the same label.",
        "Preserve display behavior for tenant-scoped labels",
        &["labels.py"],
        &[],
        &revision,
    );
    let response = capture_type_operation(&state, request).await.unwrap();
    assert_eq!(response.proposals.len(), 1, "{:#?}", response.proposals);
    let refined = &response.proposals[0];
    let TypedDisposition::Refines {
        candidate_iri,
        confidence,
        receipt_operation_id,
        ..
    } = &refined.disposition
    else {
        panic!("{:#?}", refined.disposition);
    };
    assert_eq!(candidate_iri, &existing);
    assert_eq!(refined.proposal.reconciled.len(), 1);
    assert_eq!(refined.proposal.reconciled[0].predicate, "refines");
    assert_eq!(refined.proposal.reconciled[0].target_iri, existing);
    assert_eq!(refined.proposal.reconciled[0].confidence, *confidence);
    assert_eq!(
        &refined.proposal.reconciled[0].receipt_operation_id,
        receipt_operation_id
    );
    let mut tampered = refined.proposal.clone();
    tampered.reconciled[0].confidence = 0.99;
    assert!(daemon::capture_operation(
        &state,
        CaptureV2Request {
            operation_id: "cap-tampered".into(),
            owner_id: "task-a".into(),
            proposals: vec![tampered],
            changed: vec![],
            restated: vec![],
        },
    )
    .is_err());
    let captured = daemon::capture_operation(
        &state,
        CaptureV2Request {
            operation_id: "cap-2".into(),
            owner_id: "task-a".into(),
            proposals: vec![refined.proposal.clone()],
            changed: vec![],
            restated: vec![],
        },
    )
    .unwrap();
    let new_iri = captured.proposals[0].iri.clone();
    assert_eq!(
        graph::relation_confidence(&state, &new_iri, "refines", &existing).unwrap(),
        Some(*confidence)
    );
    assert!(moosedev::validation::validate_project(&state)
        .unwrap()
        .conforms());

    // A distinct claim whose title an accepted record of another kind already
    // uses is qualified so capture accepts it.
    record_described(
        &state,
        "Requirement",
        "Check pytest -q failed before the edit and passed after it",
        "A requirement that happens to share the lesson's title.",
    );
    let revision = daemon::accepted_revision(&state).unwrap();
    let request = typing_request(
        "type-3",
        "",
        "Preserve display behavior",
        &["labels.py"],
        &[("pytest -q", false, false), ("pytest -q", true, true)],
        &revision,
    );
    let response = capture_type_operation(&state, request).await.unwrap();
    let lesson = response
        .proposals
        .iter()
        .find(|p| p.origin == ProposalOrigin::SymbolicLesson)
        .unwrap();
    assert!(
        matches!(
            lesson.disposition,
            TypedDisposition::Distinct {
                nearest_iri: None,
                ..
            }
        ),
        "{:#?}",
        lesson.disposition
    );
    assert!(
        lesson.proposal.title.ends_with("(labels.py)"),
        "{}",
        lesson.proposal.title
    );

    // A symbolic decision whose title is already at the cap keeps its
    // qualifier: the base is shortened, never the qualifier. Symbolic
    // campaign v2 cell 7 retyped an identical capped title until the retype
    // budget parked a solved task.
    let long_summary = "Refactor the duplicated name normalization logic in render_name and render_names into a single private helper that trims and substitutes";
    let capped = format!(
        "{}…",
        long_summary.chars().take(97).collect::<String>().trim_end()
    );
    record_described(
        &state,
        "Requirement",
        &capped,
        "A requirement that shares the capped decision title.",
    );
    let revision = daemon::accepted_revision(&state).unwrap();
    let response = capture_type_operation(
        &state,
        typing_request(
            "type-long",
            "",
            long_summary,
            &["labels.py"],
            &[("pytest -q", true, true)],
            &revision,
        ),
    )
    .await
    .unwrap();
    let decision = response
        .proposals
        .iter()
        .find(|p| p.origin == ProposalOrigin::SymbolicDecision)
        .unwrap();
    assert_ne!(decision.proposal.title, capped);
    assert!(
        decision.proposal.title.ends_with(" (labels.py)"),
        "{}",
        decision.proposal.title
    );
    assert!(decision.proposal.title.chars().count() <= 100);

    // When the file-qualified title is taken as well, the operation prefix,
    // which every retype renews, qualifies it instead.
    record_described(
        &state,
        "Requirement",
        &decision.proposal.title,
        "Another requirement sharing the qualified title.",
    );
    let revision = daemon::accepted_revision(&state).unwrap();
    let response = capture_type_operation(
        &state,
        typing_request(
            "type-long-2",
            "",
            long_summary,
            &["labels.py"],
            &[("pytest -q", true, true)],
            &revision,
        ),
    )
    .await
    .unwrap();
    let decision = response
        .proposals
        .iter()
        .find(|p| p.origin == ProposalOrigin::SymbolicDecision)
        .unwrap();
    assert!(
        decision.proposal.title.ends_with(" (type-lon)"),
        "{}",
        decision.proposal.title
    );
    assert!(decision.proposal.title.chars().count() <= 100);
}

#[tokio::test]
async fn sensor_capture_typing_uses_the_daemon_model_and_degrades_on_failure() {
    use axum::extract::State as AxumState;
    use axum::routing::post;
    use axum::{Json as AxumJson, Router};
    use daemon::capture_type::capture_type_operation;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    #[derive(Clone)]
    struct Script {
        fail: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
    }
    async fn completions(
        AxumState(script): AxumState<Script>,
        AxumJson(body): AxumJson<serde_json::Value>,
    ) -> (axum::http::StatusCode, AxumJson<serde_json::Value>) {
        script.requests.lock().unwrap().push(body);
        if script.fail.load(Ordering::Acquire) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                AxumJson(json!({"error":"scripted outage"})),
            );
        }
        let content = json!({
            "proposals": [
                {"kind":"Lesson","title":"Renderer trims labels before display","description":"Trimming happens in the renderer, not the model layer."},
                {"kind":"ArchitecturalDecision","title":"Preserve display behavior","description":"Duplicate of the symbolic decision; dropped."},
                {"kind":"Bogus","title":"Not a kind","description":"Dropped."}
            ],
            "reason": "The note states one gotcha."
        });
        (
            axum::http::StatusCode::OK,
            AxumJson(
                json!({"choices":[{"message":{"role":"assistant","content":content.to_string()},"finish_reason":"stop"}]}),
            ),
        )
    }
    let script = Script {
        fail: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(script.clone());
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });

    let fixture = Fixture::new();
    let state = AppState::bootstrap_with_llm_config(
        &fixture.0.join(".moosedev"),
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
        LlmConfig {
            base_url: format!("{url}/v1"),
            api_key: "fixture".into(),
            model: "scripted-daemon-model".into(),
            configured: true,
            context_window_tokens: moosedev::llm::DEFAULT_LLM_CONTEXT_WINDOW_TOKENS,
            structured_output: moosedev::llm::StructuredOutputMode::Required,
            timeouts: Default::default(),
        },
    )
    .unwrap();
    let revision = daemon::accepted_revision(&state).unwrap();
    let request = typing_request(
        "type-sensor",
        "The renderer trims labels; the model layer must not.",
        "Preserve display behavior",
        &["labels.py"],
        &[],
        &revision,
    );
    let response = capture_type_operation(&state, request).await.unwrap();
    assert_eq!(response.typing_mode, TypingMode::Sensor);
    assert!(response.typing_note.is_none(), "{:?}", response.typing_note);
    let origins: Vec<_> = response.proposals.iter().map(|p| p.origin).collect();
    assert_eq!(
        origins,
        vec![ProposalOrigin::SymbolicDecision, ProposalOrigin::LlmSensor],
        "{:#?}",
        response.proposals
    );
    let sensed = &response.proposals[1];
    assert_eq!(sensed.proposal.kind, "Lesson");
    assert_eq!(
        sensed.proposal.title,
        "Renderer trims labels before display"
    );
    assert_eq!(
        sensed.proposal.evidence,
        vec!["event 12: capture note".to_string()]
    );
    assert_eq!(sensed.proposal.files, vec!["labels.py".to_string()]);
    assert!(matches!(
        sensed.disposition,
        TypedDisposition::Distinct { .. }
    ));
    let recorded = script.requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["response_format"]["json_schema"]["name"],
        "harness_capture_typing"
    );
    assert_eq!(recorded[0]["model"], "scripted-daemon-model");

    script.fail.store(true, Ordering::Release);
    let revision = daemon::accepted_revision(&state).unwrap();
    let request = typing_request(
        "type-sensor-outage",
        "The renderer trims labels; the model layer must not.",
        "Preserve display behavior",
        &["labels.py"],
        &[],
        &revision,
    );
    let response = capture_type_operation(&state, request).await.unwrap();
    assert_eq!(response.typing_mode, TypingMode::Sensor);
    assert!(response
        .typing_note
        .as_deref()
        .unwrap()
        .starts_with("sensor typing failed"));
    assert_eq!(response.proposals.len(), 1);
    assert_eq!(
        response.proposals[0].origin,
        ProposalOrigin::SymbolicDecision
    );
    server.abort();
}

const SERVER_MODULE: &str = "rust-analyzer cargo sample 0.1.0 server/";
const SERVER_LIMIT: &str = "rust-analyzer cargo sample 0.1.0 server/LIMIT.";
const BUILD_SERVER: &str = "rust-analyzer cargo sample 0.1.0 server/build_server().";
const SERVER_HELPER: &str = "rust-analyzer cargo sample 0.1.0 server/helper().";

/// The capture-anchoring Rust fixture before the task's change: a constant, a
/// struct with a field, a public function and a private helper.
const SERVER_SOURCE: &str = "const LIMIT: usize = 8;\n\npub struct Server {\n    port: u16,\n}\n\npub fn build_server() -> Server {\n    Server { port: 80 }\n}\n\nfn helper() -> u16 {\n    LIMIT as u16\n}\n";

type RustDefinition = (
    String,
    scip::types::symbol_information::Kind,
    Vec<i32>,
    Vec<i32>,
);

fn server_definitions() -> Vec<RustDefinition> {
    use scip::types::symbol_information::Kind;
    vec![
        (
            SERVER_MODULE.into(),
            Kind::Module,
            vec![0, 0, 13, 0],
            vec![],
        ),
        (
            SERVER_LIMIT.into(),
            Kind::Constant,
            vec![0, 6, 11],
            vec![0, 0, 23],
        ),
        (
            "rust-analyzer cargo sample 0.1.0 server/Server#".into(),
            Kind::Struct,
            vec![2, 11, 17],
            vec![2, 0, 4, 1],
        ),
        (
            "rust-analyzer cargo sample 0.1.0 server/Server#port.".into(),
            Kind::Field,
            vec![3, 4, 8],
            vec![3, 4, 13],
        ),
        (
            BUILD_SERVER.into(),
            Kind::Function,
            vec![6, 7, 19],
            vec![6, 0, 8, 1],
        ),
        (
            SERVER_HELPER.into(),
            Kind::Function,
            vec![10, 3, 9],
            vec![10, 0, 12, 1],
        ),
    ]
}

/// A rust-analyzer-shaped index over `(file, indexed source, disk source,
/// definitions)`. The published digest proves the indexed source. When the
/// disk differs, the file was edited after the producer ran (the usual Rust
/// case), so the filesystem no longer proves it either.
fn install_rust_index(
    fixture: &Fixture,
    state: &AppState,
    files: Vec<(&str, &str, &str, Vec<RustDefinition>)>,
) {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{Document, Index, Occurrence, SymbolInformation};
    let mut index = Index::new();
    let mut meta = SubstrateMeta::single(
        "rust-analyzer",
        SubstrateMeta::current_head(&fixture.0),
        Utc::now(),
        files.len(),
        0,
    );
    let mut refreshed = true;
    for (file, indexed, disk, definitions) in files {
        let path = fixture.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, disk).unwrap();
        if indexed == disk {
            std::fs::File::open(&path)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(
                        std::time::SystemTime::now() - std::time::Duration::from_secs(2),
                    ),
                )
                .unwrap();
        } else {
            refreshed = false;
        }
        meta.source_digests
            .insert(file.into(), sha256_text(indexed));
        let mut document = Document::new();
        document.relative_path = file.into();
        for (symbol, kind, range, enclosing) in definitions {
            let mut info = SymbolInformation::new();
            info.symbol = symbol.clone();
            info.kind = EnumOrUnknown::new(kind);
            document.symbols.push(info);
            let mut occurrence = Occurrence::new();
            occurrence.symbol = symbol;
            occurrence.symbol_roles = 1;
            occurrence.range = range;
            occurrence.enclosing_range = enclosing;
            document.occurrences.push(occurrence);
        }
        index.documents.push(document);
    }
    meta.indexed_started_at = Some(if refreshed {
        Utc::now()
    } else {
        Utc::now() - chrono::Duration::seconds(60)
    });
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, &fixture.0).unwrap(),
    ));
}

/// Line hunks `(before start, before end, after start, after end)`.
fn line_hunks(
    file: &str,
    before: &str,
    after: &str,
    hunks: &[(u32, u32, u32, u32)],
) -> ChangedFile {
    let line = |line: u32| HarnessSourcePosition { line, col: 0 };
    let range = |start: u32, end: u32| HarnessSourceRange {
        start: line(start),
        end: line(end),
    };
    ChangedFile {
        file: file.into(),
        before_digest: Some(sha256_text(before)),
        after_digest: Some(sha256_text(after)),
        changed_ranges: hunks.iter().map(|(_, _, a1, a2)| range(*a1, *a2)).collect(),
        before_ranges: hunks.iter().map(|(b1, b2, _, _)| range(*b1, *b2)).collect(),
        ranges_coalesced: false,
    }
}

fn anchored_request(
    id: &str,
    kind: &str,
    title: &str,
    changed: Vec<ChangedFile>,
) -> CaptureV2Request {
    let mut proposed = proposal(kind, title);
    proposed.files = changed.iter().map(|change| change.file.clone()).collect();
    CaptureV2Request {
        changed,
        ..request(id, vec![proposed])
    }
}

fn normalized(raw: &str) -> String {
    raw.replacen(" 0.1.0 ", " . ", 1)
}

fn anchor_summary(captured: &CapturedProposal) -> Vec<(String, AnchorBasis)> {
    captured
        .anchors
        .iter()
        .map(|anchor| (anchor.symbol.clone(), anchor.basis))
        .collect()
}

fn accept(state: &AppState, operation_id: &str) {
    daemon::review_operation(
        state,
        &ReviewRequest {
            operation_id: operation_id.into(),
            accept: true,
        },
    )
    .unwrap();
}

fn direct_record_iris(state: &AppState, symbol: &str) -> Vec<String> {
    graph::get_entity_dossier(state, &graph::DossierTarget::Symbol(symbol.into()))
        .unwrap()
        .map(|dossier| dossier.direct_records.into_iter().map(|r| r.iri).collect())
        .unwrap_or_default()
}

#[test]
fn capture_anchors_the_changed_function_and_its_dossier_lists_the_capture() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let after = SERVER_SOURCE.replace("port: 80", "port: 443");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let changed = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(7, 8, 7, 8)]);
    let captured = daemon::capture_operation(
        &state,
        anchored_request("anchored", "Lesson", "Server port choice", vec![changed]),
    )
    .unwrap();
    let proposal = &captured.proposals[0];
    assert_eq!(
        anchor_summary(proposal),
        vec![(normalized(BUILD_SERVER), AnchorBasis::Definition)]
    );
    assert!(proposal.anchor_notes.is_empty() && proposal.unanchored.is_empty());
    assert_eq!(proposal.links.len(), 1);
    accept(&state, "anchored");
    assert_eq!(
        direct_record_iris(&state, BUILD_SERVER),
        vec![proposal.iri.clone()]
    );
    assert!(direct_record_iris(&state, SERVER_HELPER).is_empty());
    assert!(direct_record_iris(&state, SERVER_MODULE).is_empty());
}

#[test]
fn a_change_outside_every_definition_anchors_the_module_and_reaches_file_pushes() {
    use moosedev::policy::{evaluate, PolicyDecision, PolicyEvent};
    let fixture = Fixture::new();
    let state = fixture.state();
    let after = SERVER_SOURCE.replacen("\n\npub struct", "\n// Sized for tests.\npub struct", 1);
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let changed = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(1, 2, 1, 2)]);
    let captured = daemon::capture_operation(
        &state,
        anchored_request("commented", "Lesson", "Server sizing note", vec![changed]),
    )
    .unwrap();
    assert_eq!(
        anchor_summary(&captured.proposals[0]),
        vec![(normalized(SERVER_MODULE), AnchorBasis::Module)]
    );
    accept(&state, "commented");
    let event = PolicyEvent::EntityTouched {
        file: "src/server.rs".into(),
        line: None,
        col: None,
        max_bytes: None,
    };
    let PolicyDecision::Inject {
        dossier_markdown, ..
    } = evaluate(&state, &fixture.0, &event).unwrap()
    else {
        panic!("a file push carries the module-anchored capture");
    };
    assert!(
        dossier_markdown.contains("Server sizing note"),
        "{dossier_markdown}"
    );
}

#[test]
fn a_refreshed_index_proves_after_ranges_and_keeps_a_changed_module_constant() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let after = SERVER_SOURCE.replace("usize = 8", "usize = 16");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", &after, &after, server_definitions())],
    );
    let changed = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(0, 1, 0, 1)]);
    let captured = daemon::capture_operation(
        &state,
        anchored_request(
            "constant",
            "Constraint",
            "Server limit bound",
            vec![changed],
        ),
    )
    .unwrap();
    assert_eq!(
        anchor_summary(&captured.proposals[0]),
        vec![(normalized(SERVER_LIMIT), AnchorBasis::Definition)]
    );
    assert!(captured.proposals[0].anchor_notes.is_empty());
}

#[test]
fn before_proof_skips_rewritten_names_and_an_unproven_index_falls_back_to_the_module() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let after = SERVER_SOURCE.replace("pub fn build_server()", "pub fn build_server_v2()");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let changed = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(6, 7, 6, 7)]);
    let renamed = daemon::capture_operation(
        &state,
        anchored_request(
            "renamed",
            "Lesson",
            "Server builder rename",
            vec![changed.clone()],
        ),
    )
    .unwrap();
    // The original index names build_server inside the rewritten hunk: the
    // symbol may no longer exist, so the file's module anchors instead.
    assert_eq!(
        anchor_summary(&renamed.proposals[0]),
        vec![(normalized(SERVER_MODULE), AnchorBasis::Module)]
    );
    assert!(renamed.proposals[0].anchor_notes.is_empty());
    install_rust_index(
        &fixture,
        &state,
        vec![(
            "src/server.rs",
            "stale source\n",
            &after,
            server_definitions(),
        )],
    );
    let unproven = daemon::capture_operation(
        &state,
        anchored_request("unproven", "Lesson", "Server builder note", vec![changed]),
    )
    .unwrap();
    assert_eq!(
        anchor_summary(&unproven.proposals[0]),
        vec![(normalized(SERVER_MODULE), AnchorBasis::Module)]
    );
    assert_eq!(
        unproven.proposals[0].anchor_notes,
        vec![AnchorNote {
            file: "src/server.rs".into(),
            note: AnchorNoteKind::IndexUnproven,
        }]
    );
}

#[test]
fn definition_anchors_are_capped_and_ambiguous_spans_fall_back_to_the_module() {
    use scip::types::symbol_information::Kind;
    let fixture = Fixture::new();
    let state = fixture.state();
    let many: String = (0..10).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let many_file = |n: usize| {
        let mut definitions: Vec<RustDefinition> = vec![(
            format!("rust-analyzer cargo sample 0.1.0 many_{n}/"),
            Kind::Module,
            vec![0, 0, 10, 0],
            vec![],
        )];
        for i in 0..10 {
            definitions.push((
                format!("rust-analyzer cargo sample 0.1.0 many_{n}/f{i}()."),
                Kind::Function,
                vec![i, 3, 5],
                vec![i, 0, 10],
            ));
        }
        definitions
    };
    let twin = "fn twin() {}\n";
    let twin_definitions: Vec<RustDefinition> = vec![
        (
            "rust-analyzer cargo sample 0.1.0 twin/".into(),
            Kind::Module,
            vec![0, 0, 1, 0],
            vec![],
        ),
        (
            "rust-analyzer cargo sample 0.1.0 twin/twin().".into(),
            Kind::Function,
            vec![0, 3, 7],
            vec![0, 0, 12],
        ),
        (
            "rust-analyzer cargo sample 0.1.0 twin/twin_alias().".into(),
            Kind::Function,
            vec![0, 3, 7],
            vec![0, 0, 12],
        ),
    ];
    install_rust_index(
        &fixture,
        &state,
        vec![
            ("src/many_0.rs", &many, &many, many_file(0)),
            ("src/many_1.rs", &many, &many, many_file(1)),
            ("src/many_2.rs", &many, &many, many_file(2)),
            ("src/twin.rs", twin, twin, twin_definitions),
        ],
    );
    let changed = |file: &str, source: &str, lines: u32| {
        let mut change = line_hunks(file, "", source, &[(0, 0, 0, lines)]);
        change.before_ranges.clear();
        change
    };
    let captured = daemon::capture_operation(
        &state,
        anchored_request(
            "capped",
            "Lesson",
            "Generated functions",
            vec![
                changed("src/twin.rs", twin, 1),
                changed("src/many_2.rs", &many, 10),
                changed("src/many_0.rs", &many, 10),
                changed("src/many_1.rs", &many, 10),
            ],
        ),
    )
    .unwrap();
    let proposal = &captured.proposals[0];
    let count = |basis: AnchorBasis| proposal.anchors.iter().filter(|a| a.basis == basis).count();
    assert_eq!(count(AnchorBasis::Definition), 16);
    assert_eq!(count(AnchorBasis::Module), 4);
    assert_eq!(proposal.links.len(), 20);
    // Files anchor in path order; the first eight definitions of each file.
    assert_eq!(
        proposal.anchors[..9]
            .iter()
            .map(|a| a.symbol.rsplit('/').next().unwrap().to_string())
            .collect::<Vec<_>>(),
        ["f0().", "f1().", "f2().", "f3().", "f4().", "f5().", "f6().", "f7().", ""]
    );
    let note = |file: &str, note: AnchorNoteKind| AnchorNote {
        file: file.into(),
        note,
    };
    assert_eq!(
        proposal.anchor_notes,
        vec![
            note("src/many_0.rs", AnchorNoteKind::AnchorOverflow),
            note("src/many_1.rs", AnchorNoteKind::AnchorOverflow),
            note("src/many_2.rs", AnchorNoteKind::AnchorOverflow),
            note("src/twin.rs", AnchorNoteKind::AnchorAmbiguous),
        ]
    );
}

#[test]
fn replaying_an_operation_with_different_hunks_is_refused() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let after = SERVER_SOURCE
        .replace("port: 80", "port: 443")
        .replace("LIMIT as u16", "(LIMIT * 2) as u16");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let first = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(7, 8, 7, 8)]);
    daemon::capture_operation(
        &state,
        anchored_request("replayed", "Lesson", "Server replay", vec![first]),
    )
    .unwrap();
    let other = line_hunks("src/server.rs", SERVER_SOURCE, &after, &[(11, 12, 11, 12)]);
    let error = daemon::capture_operation(
        &state,
        anchored_request("replayed", "Lesson", "Server replay", vec![other]),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("already used for a different capture"),
        "{error}"
    );
}

fn literal_quads(state: &AppState, iri: &str) -> Vec<String> {
    use oxigraph::model::{NamedNodeRef, Term};
    let mut quads: Vec<String> = state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(iri).unwrap().into()),
            None,
            None,
            None,
        )
        .filter_map(Result::ok)
        .filter(|quad| matches!(quad.object, Term::Literal(_)))
        .map(|quad| quad.to_string())
        .collect();
    quads.sort();
    quads
}

#[tokio::test]
async fn multi_anchor_review_attests_with_one_existing_and_one_minted_entity() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let after = SERVER_SOURCE
        .replace("port: 80", "port: 443")
        .replace("LIMIT as u16", "(LIMIT * 2) as u16");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let earlier = record(&state, "Lesson", "An earlier helper lesson");
    let helper = graph::link_code(
        &state,
        &earlier,
        "concerns",
        &graph::CodeSelector::Symbol(SERVER_HELPER.into()),
        "test-human",
    )
    .unwrap()
    .entity_iri;
    state.note_project_write();
    let helper_literals = literal_quads(&state, &helper);
    let base = review_base(&state);
    let changed = line_hunks(
        "src/server.rs",
        SERVER_SOURCE,
        &after,
        &[(7, 8, 7, 8), (11, 12, 11, 12)],
    );
    let captured = daemon::capture_operation(
        &state,
        anchored_request("multi", "Constraint", "Server limits", vec![changed]),
    )
    .unwrap();
    assert_eq!(
        anchor_summary(&captured.proposals[0]),
        vec![
            (normalized(BUILD_SERVER), AnchorBasis::Definition),
            (normalized(SERVER_HELPER), AnchorBasis::Definition),
        ]
    );
    let reviewed = review_expecting(&state, "multi", &base).await;
    assert!(reviewed.ok);
    assert!(reviewed.result.is_some());
    assert_eq!(
        reviewed.result, reviewed.revision,
        "both link materializations are this acceptance's own writes"
    );
    assert_eq!(
        literal_quads(&state, &helper),
        helper_literals,
        "linking an existing entity rewrites none of its literals"
    );
    let iri = captured.proposals[0].iri.clone();
    assert!(direct_record_iris(&state, BUILD_SERVER).contains(&iri));
    assert!(direct_record_iris(&state, SERVER_HELPER).contains(&iri));
}

fn restates_receipt(state: &AppState, id: &str, owner: &str, candidate: &str) {
    daemon::reconcile_score::record_receipt(
        state,
        daemon::reconcile_score::ScoreReceipt {
            operation_id: id.into(),
            owner_id: owner.into(),
            proposal_digest: "fixture".into(),
            candidate_revision: "fixture".into(),
            thresholds: ReconcileThresholds::default(),
            disposition: "restates".into(),
            candidate_iri: Some(candidate.into()),
            candidate_digest: None,
            score: 0.93,
            confidence: 0.93,
            resolved_by: "symbolic".into(),
        },
    )
    .unwrap();
}

#[tokio::test]
async fn restated_links_attest_and_skip_definitions_the_record_already_reaches() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let after = SERVER_SOURCE
        .replace("port: 80", "port: 443")
        .replace("LIMIT as u16", "(LIMIT * 2) as u16");
    install_rust_index(
        &fixture,
        &state,
        vec![("src/server.rs", SERVER_SOURCE, &after, server_definitions())],
    );
    let existing = record(&state, "Constraint", "Server ports stay configurable");
    // The record already constrains helper; another record already minted
    // build_server's entity, so the new edge lands on an existing entity.
    graph::link_code(
        &state,
        &existing,
        "constrains",
        &graph::CodeSelector::Symbol(SERVER_HELPER.into()),
        "test-human",
    )
    .unwrap();
    let other = record(&state, "Lesson", "An earlier builder lesson");
    let builder = graph::link_code(
        &state,
        &other,
        "concerns",
        &graph::CodeSelector::Symbol(BUILD_SERVER.into()),
        "test-human",
    )
    .unwrap()
    .entity_iri;
    state.note_project_write();
    let builder_literals = literal_quads(&state, &builder);
    restates_receipt(&state, "restated-r0", "test-owner", &existing);
    let base = review_base(&state);
    let changed = line_hunks(
        "src/server.rs",
        SERVER_SOURCE,
        &after,
        &[(7, 8, 7, 8), (11, 12, 11, 12)],
    );
    let restated = RestatedCandidate {
        candidate_iri: existing.clone(),
        receipt_operation_id: "restated-r0".into(),
        files: vec!["src/server.rs".into()],
    };
    let captured = daemon::capture_operation(
        &state,
        CaptureV2Request {
            changed: vec![changed.clone()],
            restated: vec![restated.clone()],
            ..request("restated", vec![])
        },
    )
    .unwrap();
    assert!(captured.proposals.is_empty());
    assert_eq!(captured.restated.len(), 1);
    let links = &captured.restated[0];
    assert_eq!(links.candidate_iri, existing);
    assert_eq!(
        links
            .anchors
            .iter()
            .map(|anchor| (anchor.symbol.clone(), anchor.basis))
            .collect::<Vec<_>>(),
        vec![(normalized(BUILD_SERVER), AnchorBasis::Definition)]
    );
    assert_eq!(links.links.len(), 1);
    assert!(
        !direct_record_iris(&state, BUILD_SERVER).contains(&existing),
        "the link waits for review"
    );
    let reviewed = review_expecting(&state, "restated", &base).await;
    assert!(reviewed.ok);
    assert!(reviewed.result.is_some());
    assert_eq!(
        reviewed.result, reviewed.revision,
        "the restated record's new edge is this acceptance's own write"
    );
    assert!(direct_record_iris(&state, BUILD_SERVER).contains(&existing));
    assert_eq!(literal_quads(&state, &builder), builder_literals);
    // A receipt owned by another task proves nothing for this one.
    restates_receipt(&state, "foreign-r0", "other-owner", &existing);
    let error = daemon::capture_operation(
        &state,
        CaptureV2Request {
            changed: vec![changed],
            restated: vec![RestatedCandidate {
                receipt_operation_id: "foreign-r0".into(),
                ..restated
            }],
            ..request("foreign-restated", vec![])
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("receipt"), "{error}");
}

/// A rooted Python index for grounding: a module constant in `channels.py`, a
/// same-named function in the edited `billing.py`, a parameter in `routing.py`
/// and a test function, all named like the compared attribute.
fn install_grounding_index(fixture: &Fixture, state: &AppState) {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information::Kind, Document, Index, Occurrence, SymbolInformation};
    let files = [
        ("channels.py", "CHANNELS = {\"wire\", \"card\"}\n"),
        (
            "billing.py",
            "def fee(order):\n    return 0\n\ndef channel():\n    return None\n",
        ),
        ("routing.py", "def route(channel):\n    return channel\n"),
        (
            "tests/test_channels.py",
            "def channel():\n    return None\n",
        ),
    ];
    for (file, source) in files {
        let path = fixture.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2)),
            )
            .unwrap();
    }
    let mut index = Index::new();
    for (file, symbol, name, kind, range) in [
        (
            "channels.py",
            "channels/CHANNELS.",
            "CHANNELS",
            Kind::Constant,
            vec![0, 0, 8],
        ),
        (
            "billing.py",
            "billing/channel().",
            "channel",
            Kind::Function,
            vec![3, 4, 11],
        ),
        (
            "routing.py",
            "routing/route().(channel)",
            "channel",
            Kind::Parameter,
            vec![0, 10, 17],
        ),
        (
            "tests/test_channels.py",
            "tests/test_channels/channel().",
            "channel",
            Kind::Function,
            vec![0, 4, 11],
        ),
    ] {
        let symbol = format!("scip-python python sample 1 {symbol}");
        let mut info = SymbolInformation::new();
        info.symbol = symbol.clone();
        info.display_name = name.into();
        info.kind = EnumOrUnknown::new(kind);
        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol;
        occurrence.symbol_roles = 1;
        occurrence.range = range;
        let mut document = Document::new();
        document.relative_path = file.into();
        document.symbols.push(info);
        document.occurrences.push(occurrence);
        index.documents.push(document);
    }
    let mut meta = SubstrateMeta::single(
        "scip-python",
        SubstrateMeta::current_head(&fixture.0),
        Utc::now(),
        4,
        4,
    );
    meta.indexed_started_at = Some(Utc::now());
    state.set_substrate(Arc::new(
        Substrate::from_index_rooted(index, meta, false, &fixture.0).unwrap(),
    ));
}

fn ground_lines(first: u32, end: u32) -> Vec<HarnessSourceRange> {
    vec![HarnessSourceRange {
        start: HarnessSourcePosition {
            line: first,
            col: 0,
        },
        end: HarnessSourcePosition { line: end, col: 0 },
    }]
}

#[test]
fn grounding_names_proven_definitions_and_literal_mismatches() {
    use daemon::ground::ground_edit;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_grounding_index(&fixture, &state);
    let after = "def fee(order):\n    if order.channel == \"cash\":\n        return 1\n    return 0\n\ndef channel():\n    return None\n";
    let request =
        |after: &str, ranges: Vec<HarnessSourceRange>, ranges_coalesced: bool| GroundRequest {
            file: "billing.py".into(),
            after: after.into(),
            ranges,
            ranges_coalesced,
        };

    let response = ground_edit(&state, &request(after, ground_lines(1, 3), false)).unwrap();
    let keys: Vec<(&str, Vec<&str>)> = response
        .keys
        .iter()
        .map(|key| {
            (
                key.attribute.as_str(),
                key.literals.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(keys, vec![("channel", vec!["cash"])]);
    let definitions: Vec<(&str, &str, &str)> = response
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.file.as_str(),
                definition.name.as_str(),
                definition.role.as_str(),
            )
        })
        .collect();
    assert_eq!(
        definitions,
        vec![("channels.py", "CHANNELS", "declaration")],
        "the edited file, a parameter and test code never ground an edit"
    );
    let preview = &response.definitions[0].preview;
    assert!(
        preview.starts_with("CHANNELS = {\"wire\", \"card\"}"),
        "{preview}"
    );
    assert!(preview.len() <= 256);
    let mismatches: Vec<(&str, &str, &str, &str)> = response
        .mismatches
        .iter()
        .map(|mismatch| {
            (
                mismatch.key.as_str(),
                mismatch.literal.as_str(),
                mismatch.file.as_str(),
                mismatch.definition.as_str(),
            )
        })
        .collect();
    assert_eq!(
        mismatches,
        vec![("channel", "cash", "channels.py", "CHANNELS")]
    );

    // A defined literal is no mismatch.
    let defined = after.replace("cash", "wire");
    let response = ground_edit(&state, &request(&defined, ground_lines(1, 3), false)).unwrap();
    assert_eq!(response.definitions.len(), 1);
    assert!(response.mismatches.is_empty());

    // Ranges are end-exclusive; outside them, coalesced, or unparsable, there are no keys.
    let outside = ground_edit(&state, &request(after, ground_lines(3, 4), false)).unwrap();
    assert!(outside.keys.is_empty());
    let before_comparison =
        ground_edit(&state, &request(after, ground_lines(0, 1), false)).unwrap();
    assert!(before_comparison.keys.is_empty());
    let coalesced = ground_edit(&state, &request(after, ground_lines(1, 3), true)).unwrap();
    assert!(coalesced.keys.is_empty() && coalesced.definitions.is_empty());
    let broken = ground_edit(
        &state,
        &request(
            "def fee(order):\n    if order.channel == \"cash\"\n",
            ground_lines(1, 2),
            false,
        ),
    )
    .unwrap();
    assert!(broken.keys.is_empty());

    // Source the index cannot prove current is never previewed or compared.
    std::fs::write(fixture.0.join("channels.py"), "CHANNELS = {\"cash\"}\n").unwrap();
    let unproven = ground_edit(&state, &request(after, ground_lines(1, 3), false)).unwrap();
    assert_eq!(unproven.definitions.len(), 1);
    assert_eq!(unproven.definitions[0].preview, "");
    assert!(unproven.mismatches.is_empty());
    let _ = std::fs::remove_dir_all(&fixture.0);
}

#[test]
fn grounding_reads_getattr_on_a_parameter_like_attribute_access() {
    use daemon::ground::ground_edit;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_grounding_index(&fixture, &state);
    let ground = |after: &str| {
        ground_edit(
            &state,
            &GroundRequest {
                file: "billing.py".into(),
                after: after.into(),
                ranges: ground_lines(1, 3),
                ranges_coalesced: false,
            },
        )
        .unwrap()
    };
    type Summary = (
        Vec<(String, Vec<String>)>,
        Vec<(String, String, String)>,
        Vec<(String, String, String, String)>,
    );
    let summary = |response: &moosedev::harness::protocol::GroundResponse| -> Summary {
        (
            response
                .keys
                .iter()
                .map(|key| (key.attribute.clone(), key.literals.clone()))
                .collect(),
            response
                .definitions
                .iter()
                .map(|d| (d.file.clone(), d.name.clone(), d.role.clone()))
                .collect(),
            response
                .mismatches
                .iter()
                .map(|m| {
                    (
                        m.key.clone(),
                        m.literal.clone(),
                        m.file.clone(),
                        m.definition.clone(),
                    )
                })
                .collect(),
        )
    };
    let attribute = ground(
        "def fee(order):\n    if order.channel == \"cash\":\n        return 1\n    return 0\n",
    );
    assert!(!attribute.keys.is_empty() && !attribute.mismatches.is_empty());
    for form in [
        "getattr(order, 'channel', None)",
        "getattr(order, \"channel\")",
    ] {
        let response = ground(&format!(
            "def fee(order):\n    if {form} == \"cash\":\n        return 1\n    return 0\n"
        ));
        assert_eq!(summary(&response), summary(&attribute), "{form}");
    }
    // Only a parameter and a literal attribute name make a key.
    for form in [
        "getattr(order, field)",
        "getattr(order, f\"chan{suffix}\")",
        "getattr(note, 'channel')",
    ] {
        let response = ground(&format!(
            "def fee(order, field, suffix):\n    note = order\n    if {form} == \"cash\":\n        return 1\n    return 0\n"
        ));
        assert!(response.keys.is_empty(), "{form}: {:?}", response.keys);
    }
    let _ = std::fs::remove_dir_all(&fixture.0);
}

#[test]
fn plan_grounding_reads_accesses_and_compared_literals_from_plan_text() {
    use daemon::ground::{ground_plan_text, plan_keys};
    use moosedev::harness::protocol::PlanGroundRequest;
    let fixture = Fixture::new();
    let state = fixture.state();
    install_grounding_index(&fixture, &state);
    let files = vec!["billing.py".to_string()];
    let keys = |text: &str| -> Vec<(String, Vec<String>)> {
        plan_keys(text, &files)
            .into_iter()
            .map(|key| (key.attribute, key.literals))
            .collect()
    };
    let text = "Update billing.py so fee() returns 1 when `order.channel` is 'cash'; the model's note says 'NP-7' matters. Then check getattr(order, 'region') == \"north\", keep self.total and read order.amount, e.g. as before.";
    assert_eq!(
        keys(text),
        vec![
            ("channel".to_string(), vec!["cash".to_string()]),
            ("region".to_string(), vec!["north".to_string()]),
            ("amount".to_string(), vec![]),
        ],
        "file names, self, abbreviations, apostrophes and a literal in another clause are neither keys nor values"
    );

    let request = |text: &str, files: &[String]| PlanGroundRequest {
        text: text.into(),
        files: files.to_vec(),
    };
    let response = ground_plan_text(&state, &request(text, &files)).unwrap();
    let definitions: Vec<(&str, &str)> = response
        .definitions
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str()))
        .collect();
    assert_eq!(
        definitions,
        vec![("channels.py", "CHANNELS")],
        "the plan's own files, parameters and test code never ground a plan"
    );
    assert!(response.definitions[0]
        .preview
        .starts_with("CHANNELS = {\"wire\", \"card\"}"));
    let mismatches: Vec<(&str, &str, &str)> = response
        .mismatches
        .iter()
        .map(|m| (m.key.as_str(), m.literal.as_str(), m.definition.as_str()))
        .collect();
    assert_eq!(mismatches, vec![("channel", "cash", "CHANNELS")]);

    // A defined value is no mismatch.
    let defined = ground_plan_text(
        &state,
        &request("Return 1 when `order.channel` == 'wire'", &files),
    )
    .unwrap();
    assert_eq!(defined.definitions.len(), 1);
    assert!(defined.mismatches.is_empty());

    // At most eight keys; nothing to read yields nothing.
    let many: String = (1..=10).map(|n| format!("order.field{n} ")).collect();
    assert_eq!(plan_keys(&many, &files).len(), 8);
    assert!(plan_keys("Keep the approved plan as it is.", &files).is_empty());

    // Source the index cannot prove current is never previewed or compared.
    std::fs::write(fixture.0.join("channels.py"), "CHANNELS = {\"cash\"}\n").unwrap();
    let unproven = ground_plan_text(&state, &request(text, &files)).unwrap();
    assert_eq!(unproven.definitions.len(), 1);
    assert_eq!(unproven.definitions[0].preview, "");
    assert!(unproven.mismatches.is_empty());
    let _ = std::fs::remove_dir_all(&fixture.0);
}

#[tokio::test]
async fn http_plan_ground_route_answers_and_rejects_escaping_paths_and_unknown_fields() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let answered = server
        .post("/api/v1/harness/ground/plan")
        .json(&json!({"text": "Return 1 when `order.kind` is 'x'", "files": ["billing.py"]}))
        .await;
    answered.assert_status_ok();
    let response: GroundResponse = answered.json();
    assert_eq!(response.keys.len(), 1);
    assert_eq!(response.keys[0].attribute, "kind");
    assert!(response.definitions.is_empty(), "no index, no definitions");
    for body in [
        json!({"text": "x", "files": ["../outside.py"]}),
        json!({"text": "x", "files": [], "extra": true}),
    ] {
        let rejected = server
            .post("/api/v1/harness/ground/plan")
            .json(&body)
            .expect_failure()
            .await;
        assert!(!rejected.status_code().is_success(), "{body}");
    }
    let _ = std::fs::remove_dir_all(&fixture.0);
}

#[tokio::test]
async fn http_ground_route_answers_keys_and_rejects_escaping_paths_and_unknown_fields() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.state());
    let server = TestServer::new(build_routes(state.clone())).unwrap();
    let body = GroundRequest {
        file: "billing.py".into(),
        after: "def fee(order):\n    return order.kind == \"x\"\n".into(),
        ranges: ground_lines(1, 2),
        ranges_coalesced: false,
    };
    let response = server.post("/api/v1/harness/ground").json(&body).await;
    response.assert_status_ok();
    let response: GroundResponse = response.json();
    assert_eq!(response.keys.len(), 1);
    assert_eq!(response.keys[0].attribute, "kind");
    assert!(
        response.definitions.is_empty() && response.mismatches.is_empty(),
        "no index, no definitions"
    );

    let escaping = GroundRequest {
        file: "../outside.py".into(),
        ..body
    };
    let rejected = server
        .post("/api/v1/harness/ground")
        .json(&escaping)
        .expect_failure()
        .await;
    assert!(!rejected.status_code().is_success());
    let unknown = server
        .post("/api/v1/harness/ground")
        .json(&json!({"file": "billing.py", "after": "", "ranges": [], "extra": true}))
        .expect_failure()
        .await;
    assert!(!unknown.status_code().is_success());
    let _ = std::fs::remove_dir_all(&fixture.0);
}

/// A two-module rust-analyzer index, so one rule can govern code in two files
/// and the cross-file behaviour of a single prompt becomes observable.
fn install_two_module_index(state: &AppState) -> (&'static str, &'static str) {
    use moosedev::code::substrate::{Substrate, SubstrateMeta};
    use protobuf::EnumOrUnknown;
    use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation};
    let modules = [
        (
            "rust-analyzer cargo sample 0.1.0 harness/",
            "harness",
            "src/harness.rs",
        ),
        (
            "rust-analyzer cargo sample 0.1.0 gateway/",
            "gateway",
            "src/gateway.rs",
        ),
    ];
    let mut index = Index::new();
    for (symbol, display_name, path) in modules {
        let mut info = SymbolInformation::new();
        info.symbol = symbol.into();
        info.display_name = display_name.into();
        info.kind = EnumOrUnknown::new(symbol_information::Kind::Module);
        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol.into();
        occurrence.symbol_roles = 1;
        occurrence.range = vec![0, 0, 10];
        occurrence.enclosing_range = vec![0, 0, 10];
        let mut document = Document::new();
        document.relative_path = path.into();
        document.symbols.push(info);
        document.occurrences.push(occurrence);
        index.documents.push(document);
    }
    state.set_substrate(Arc::new(
        Substrate::from_index(
            index,
            SubstrateMeta::single("rust-analyzer", "test", Utc::now(), 1, 2),
            false,
        )
        .unwrap(),
    ));
    (modules[0].0, modules[1].0)
}

/// One prompt renders a dossier per file but the model reads them together, so a
/// rule governing code in two files must arrive with its claim body once. In the
/// floor study this repetition was roughly 30-40% of late-episode dossier bytes,
/// and the prompt is what eventually exceeded the context budget.
#[tokio::test]
async fn one_prompt_carries_a_shared_claim_once_across_its_files() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let (harness_symbol, gateway_symbol) = install_two_module_index(&state);
    let rule = record_with(
        &state,
        "Constraint",
        "Listener binds before serving",
        "Serve only after the listener binds.",
        "accepted",
    );
    for symbol in [harness_symbol, gateway_symbol] {
        graph::link_code(
            &state,
            &rule,
            "constrains",
            &graph::CodeSelector::Symbol(symbol.into()),
            "test-human",
        )
        .unwrap();
    }

    let response = linked_context(&state, "listener", &["src/harness.rs", "src/gateway.rs"]);
    assert_eq!(response.files.len(), 2);
    let first = &response.files[0].dossier;
    let second = &response.files[1].dossier;
    let claim = "hasDescription: Serve only after the listener binds.";
    assert_eq!(first.matches(claim).count(), 1, "{first}");
    assert_eq!(
        second.matches(claim).count(),
        0,
        "the second file repeated a claim body this prompt already carries: {second}"
    );
    assert!(
        second.contains("claim shown above for `harness`"),
        "{second}"
    );
    assert!(
        second.contains("- [Constraint] Listener binds before serving"),
        "the header still names the rule under every file it governs: {second}"
    );
}
