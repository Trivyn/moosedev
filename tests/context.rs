//! `get_relevant_context` retrieval: record a couple of decisions, then confirm
//! list-all returns them as structured items and a topic filters by label.
//! Symbolic — no LLM, fully hermetic.

use std::collections::HashSet;
use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};

fn record(state: &AppState, class_iri: &str, title: &str) {
    let _ = record_with_status(state, class_iri, title, "accepted");
}

fn record_with_status(state: &AppState, class_iri: &str, title: &str, status: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: class_iri.to_string(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.status.clone(), status.to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision")
}

fn insert_fixed_record(
    state: &AppState,
    iri: &str,
    class_iris: &[&str],
    title: &str,
    status: &str,
) {
    let graph = GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap());
    let subject = NamedNode::new(iri).unwrap();
    let mut txn = state.store.start_transaction().unwrap();
    for class_iri in class_iris {
        txn.insert(
            Quad::new(
                subject.clone(),
                NamedNode::new(moose::RDF_TYPE).unwrap(),
                NamedNode::new(*class_iri).unwrap(),
                graph.clone(),
            )
            .as_ref(),
        );
    }
    for (predicate, value) in [
        (moose::RDFS_LABEL, title),
        (state.capture.status.as_str(), status),
    ] {
        txn.insert(
            Quad::new(
                subject.clone(),
                NamedNode::new(predicate).unwrap(),
                Literal::new_simple_literal(value),
                graph.clone(),
            )
            .as_ref(),
        );
    }
    txn.commit().unwrap();
}

#[test]
fn relevant_context_lists_all_and_filters_by_topic() {
    let dir = std::env::temp_dir().join(format!("moosedev-context-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    record(&state, &class_iri, "Adopt rmcp for the MCP transport");
    record(
        &state,
        &class_iri,
        "Use the oxigraph on-disk store for the durable KG",
    );

    // No topic → both items, with their type, label, and status property.
    let all = graph::relevant_context(&state, None, 10, false).expect("list all");
    assert_eq!(all.len(), 2, "should list both recorded decisions");
    assert!(all.iter().all(|i| i.kind == "ArchitecturalDecision"));
    assert!(all.iter().any(|i| i
        .properties
        .iter()
        .any(|(k, v)| k == "hasLifecycleStatus" && v == "accepted")));

    // Topic → BM25 lexical relevance over each record's label + description
    // (moose `search_records`). "rmcp" appears in the rmcp decision's label.
    let hits = graph::relevant_context(&state, Some("rmcp"), 10, false).expect("topic search");
    assert!(
        hits.iter().any(|i| i.label.contains("rmcp")),
        "topic 'rmcp' should match the rmcp decision; got {:?}",
        hits.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    // Honest empty-state (invariant #6): a topic that shares no term with any
    // record returns nothing rather than padded noise — `search_records` excludes
    // zero-match records, so relevance is never asserted where none exists.
    let none = graph::relevant_context(&state, Some("kubernetes helm rollout"), 10, false)
        .expect("no-match topic");
    assert!(
        none.is_empty(),
        "topic sharing no term with any record should return nothing; got {:?}",
        none.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unratified_and_declined_records_never_reach_authoritative_recall() {
    let dir = std::env::temp_dir().join(format!("moosedev-context-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    record(&state, &class_iri, "Ratified quokka decision");
    let _ = record_with_status(&state, &class_iri, "Pending quokka capture", "proposed");
    let _ = record_with_status(&state, &class_iri, "Declined quokka idea", "rejected");
    let _ = record_with_status(&state, &class_iri, "Old quokka approach", "superseded");

    // Default recall = the ratified working set only: a proposed record lives
    // in the inbox and a rejected one was declined — neither may influence an
    // agent as recorded truth. Both list-all and topic search filter.
    for topic in [None, Some("quokka")] {
        let items = graph::relevant_context(&state, topic, 10, false).expect("recall");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Ratified quokka decision"],
            "topic {topic:?} must return only ratified knowledge"
        );
    }

    // include_history opts into everything outside the working set.
    let all = graph::relevant_context(&state, None, 10, true).expect("history");
    assert_eq!(
        all.len(),
        4,
        "history includes proposed/rejected/superseded"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn relevant_context_limit_is_applied_after_lifecycle_filtering() {
    let dir =
        std::env::temp_dir().join(format!("moosedev-context-budget-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();

    // The exact-title symbolic anchor is proposed, and the other stronger
    // lexical hits are rejected or retired. None may consume the visible page.
    let proposed = record_with_status(&state, &class_iri, "budgeted quokka retrieval", "proposed");
    for (status, suffix) in [
        ("rejected", "declined"),
        ("superseded", "replaced"),
        ("deprecated", "withdrawn"),
    ] {
        let _ = record_with_status(
            &state,
            &class_iri,
            &format!("budgeted quokka retrieval budgeted quokka retrieval {suffix}"),
            status,
        );
    }
    let accepted = record_with_status(
        &state,
        &class_iri,
        "budgeted quokka retrieval accepted fallback",
        "accepted",
    );

    let current = graph::relevant_context(&state, Some("budgeted quokka retrieval"), 1, false)
        .expect("current recall");
    assert_eq!(
        current.iter().map(|item| &item.iri).collect::<Vec<_>>(),
        vec![&accepted],
        "ineligible top-ranked hits must not hide accepted knowledge"
    );

    let history = graph::relevant_context(&state, Some("budgeted quokka retrieval"), 1, true)
        .expect("historical recall");
    assert_eq!(
        history.iter().map(|item| &item.iri).collect::<Vec<_>>(),
        vec![&proposed],
        "include_history preserves the exact-anchor ranking"
    );

    assert!(
        graph::relevant_context(&state, Some("budgeted quokka retrieval"), 0, false)
            .expect("zero-limit recall")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_all_exhausts_lifecycle_ineligible_candidates_before_applying_limit() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-context-list-budget-ws-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();

    for (status, title) in [
        ("proposed", "Pending list candidate"),
        ("rejected", "Rejected list candidate"),
        ("superseded", "Superseded list candidate"),
        ("deprecated", "Deprecated list candidate"),
    ] {
        let _ = record_with_status(&state, &class_iri, title, status);
    }
    let accepted = record_with_status(&state, &class_iri, "Accepted list fallback", "accepted");

    let current = graph::relevant_context(&state, None, 1, false).expect("list current");
    assert_eq!(
        current.iter().map(|item| &item.iri).collect::<Vec<_>>(),
        vec![&accepted],
        "list-all must fill its page from the complete eligible pool"
    );

    let history = graph::relevant_context(&state, None, 1, true).expect("list history");
    assert_eq!(history.len(), 1, "history still obeys the visible limit");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_all_deduplicates_multi_typed_subjects_before_spending_its_limit() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-context-list-multi-type-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let first_class = state.arch_vocab.classes[0].iri.as_str();
    let second_class = state.arch_vocab.classes[1].iri.as_str();
    let first_kind = first_class
        .rsplit(['#', '/'])
        .next()
        .expect("class local name");
    let multi_iri = "https://moosedev.dev/kg/test/a-multi-typed";
    let accepted_iri = "https://moosedev.dev/kg/test/z-accepted";

    // Fixed IRIs make the multi-typed subject sort before the distinct subject
    // in the second class. Without subject deduplication it consumes two slots.
    insert_fixed_record(
        &state,
        multi_iri,
        &[first_class, second_class],
        "Pending multi-typed record",
        "proposed",
    );
    insert_fixed_record(
        &state,
        accepted_iri,
        &[second_class],
        "Accepted distinct record",
        "accepted",
    );

    let history = graph::relevant_context(&state, None, 2, true).expect("history list");
    assert_eq!(
        history
            .iter()
            .map(|item| item.iri.as_str())
            .collect::<Vec<_>>(),
        vec![multi_iri, accepted_iri],
        "a multi-typed subject appears once and keeps the first encountered class"
    );
    assert_eq!(history[0].kind, first_kind);

    let current = graph::relevant_context(&state, None, 1, false).expect("current list");
    assert_eq!(
        current
            .iter()
            .map(|item| item.iri.as_str())
            .collect::<Vec<_>>(),
        vec![accepted_iri],
        "the proposed duplicate must not consume the visible result budget"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn link_candidate_limit_is_applied_after_lifecycle_filtering() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-link-candidates-ws-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();

    // These exact, term-heavy matches outrank the valid fallback, but none is
    // eligible for an editor link. They must not consume the result limit.
    for index in 0..8 {
        let _ = record_with_status(
            &state,
            &class_iri,
            &format!("build_server build_server build_server pending {index}"),
            "proposed",
        );
    }
    let valid = record_with_status(
        &state,
        &class_iri,
        "build_server accepted fallback decision",
        "accepted",
    );

    let candidates = graph::link_candidates(&state, "build_server", 1).expect("candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| &candidate.iri)
            .collect::<Vec<_>>(),
        vec![&valid],
        "bounded ineligible top hits must not hide a valid lower-ranked record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn link_candidate_limit_is_applied_after_caller_exclusions() {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-link-candidate-exclusions-ws-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();

    let mut excluded = HashSet::new();
    for index in 0..8 {
        excluded.insert(record_with_status(
            &state,
            &class_iri,
            &format!("parse_config parse_config parse_config linked {index}"),
            "accepted",
        ));
    }
    let valid = record_with_status(
        &state,
        &class_iri,
        "parse_config accepted fallback decision",
        "accepted",
    );

    let candidates =
        graph::link_candidates_excluding(&state, "parse_config", 1, &excluded).expect("candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| &candidate.iri)
            .collect::<Vec<_>>(),
        vec![&valid],
        "already-linked or pending top hits must not hide an eligible lower-ranked record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
