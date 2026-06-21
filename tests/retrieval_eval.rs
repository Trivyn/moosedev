//! Hybrid (BM25F ⊕ dense) seeding for `get_relevant_context` — the calibration and
//! regression harness for the dense channel and its confidence floor.
//!
//! Two properties, both gated by the floor (`DEFAULT_DENSE_FLOOR`, overridable via
//! `MOOSEDEV_DENSE_FLOOR`):
//!   1. Recall (the win): a paraphrased topic that shares little surface vocabulary
//!      with a record still seeds it via the dense channel — the comprehension-debt
//!      fix (the same concept named differently as a project ages).
//!   2. Honest empty state (invariant #6): an unrelated topic still returns nothing,
//!      because the floor stops dense from manufacturing a spurious nearest neighbor.
//!
//! Loads the embedding backbone (candle-cpu + arctic-s, always compiled in), like
//! `tests/alignment.rs`, so it is one of the slower tests. Run with `--nocapture` to
//! print the per-channel cosine table used to calibrate the floor. The complementary
//! soft-fail path (no dense index → pure BM25) is covered by `tests/context.rs`.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};

fn record(state: &AppState, class_iri: &str, title: &str, description: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: class_iri.to_string(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.description.clone(), description.to_string()),
                (state.capture.status.clone(), "accepted".to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision")
}

#[tokio::test]
async fn hybrid_dense_seed_recovers_paraphrases_and_preserves_empty_state() {
    let dir = std::env::temp_dir().join(format!("moosedev-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let onto = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let mut state = AppState::bootstrap(&dir, &onto).expect("bootstrap");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    // Records whose vocabulary is deliberately distinct from the paraphrase queries
    // below, so a hit is the dense channel's doing rather than lexical overlap.
    let flaky = record(
        &state,
        &class_iri,
        "Flaky tests from shared global state",
        "Suites that pass or break nondeterministically because cases mutate shared global fixtures and ordering leaks between them.",
    );
    let rocksdb = record(
        &state,
        &class_iri,
        "Adopt RocksDB for the durable store",
        "Use an embedded log-structured key-value engine as the on-disk persistence backend for the knowledge graph.",
    );
    let bluegreen = record(
        &state,
        &class_iri,
        "Blue-green deployment for releases",
        "Cut traffic between two identical production environments to roll out changes with no downtime and instant rollback.",
    );

    // Build the ABox dense index over the records just written.
    state
        .build_instance_index()
        .await
        .expect("build instance index");
    assert!(
        state.instance_store.is_enabled(),
        "embedding backbone must be available for this eval (candle-cpu + arctic-s are compiled in)"
    );

    // (paraphrase query, target record IRI, short label) — the gold set.
    let cases: [(&str, String, &str); 3] = [
        (
            "test suite that intermittently breaks without code changes",
            flaky,
            "flaky tests",
        ),
        (
            "which on-disk database engine should we use for persistence",
            rocksdb,
            "rocksdb store",
        ),
        (
            "how do we ship updates without taking the service offline",
            bluegreen,
            "blue-green deploy",
        ),
    ];

    // Calibration view: print each gold pair's per-channel diagnostics (raw cosine
    // is shown with NO floor) so the floor can be chosen against real separations.
    let class_iris: Vec<String> = state
        .arch_vocab
        .classes
        .iter()
        .map(|c| c.iri.clone())
        .collect();
    let data_graphs = [graph::PROJECT_KG_GRAPH_IRI.to_string()];
    let text_fields = [
        (moose::RDFS_LABEL, 2.0_f32),
        (state.capture.description.as_str(), 1.0_f32),
    ];
    eprintln!(
        "\ndense-floor calibration ({:<28}) {:>7} {:>7} {:>5}",
        "target", "bm25f", "cosine", "rank"
    );
    for (query, target, name) in &cases {
        let hybrid = state.entity_index.search_records_hybrid(
            query,
            &class_iris,
            &state.store,
            &data_graphs,
            &text_fields,
            10,
            &state.instance_store,
            None, // raw cosine, unfiltered, for calibration
        );
        match hybrid.iter().find(|h| h.iri == *target) {
            Some(h) => eprintln!(
                "  {name:<48} {:>7} {:>7.3} {:>5}",
                h.bm25f
                    .map(|v| format!("{v:.2}"))
                    .unwrap_or_else(|| "—".into()),
                h.dense_cosine.unwrap_or(f32::NAN),
                h.fused_rank,
            ),
            None => eprintln!("  {name:<48} (not found)"),
        }
    }

    // 1. Recall (the win): each paraphrase surfaces its target through the production
    //    path (`relevant_context`, which applies the real confidence floor).
    for (query, target, name) in &cases {
        let hits = graph::relevant_context(&state, Some(query), 10, false).expect("hybrid seed");
        assert!(
            hits.iter().any(|i| i.iri == *target),
            "paraphrase {name:?} ({query:?}) should dense-seed its record; got {:?}",
            hits.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    // 2. Honest empty state (invariant #6): a topic from an unrelated domain shares no
    //    term (BM25 empty) and sits far below the cosine floor (dense empty), so the
    //    result is empty — the floor prevents a manufactured nearest neighbor. This is
    //    the regression guard dense-without-a-floor would silently break.
    let none = graph::relevant_context(
        &state,
        Some("sourdough bread fermentation schedule"),
        10,
        false,
    )
    .expect("irrelevant topic");
    assert!(
        none.is_empty(),
        "an unrelated topic must return nothing (floor preserves invariant #6); got {:?}",
        none.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Symbolic-first anchoring: when the topic is the exact title of a record, that
/// record is seeded first (invariant #1) — ahead of whatever the lexical+dense
/// ranking would surface. No instance index is built, so the hybrid seed soft-falls
/// to BM25, isolating the symbolic anchor as the property under test.
#[test]
fn exact_title_anchors_the_named_record_first() {
    let dir = std::env::temp_dir().join(format!("moosedev-anchor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let onto = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &onto).expect("bootstrap");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let anchored = record(
        &state,
        &class_iri,
        "Adopt RocksDB for the durable store",
        "Embedded key-value persistence backend.",
    );
    // A decoy that also shares the word "store" so plain lexical ranking could
    // outrank the exact-title record without anchoring.
    record(
        &state,
        &class_iri,
        "Store request logs in object storage",
        "Persist raw request logs to an object store bucket.",
    );

    // Exact title (case-insensitive) → the named record is seed #0.
    let hits = graph::relevant_context(
        &state,
        Some("adopt rocksdb for the durable store"),
        10,
        false,
    )
    .expect("anchored topic");
    assert_eq!(
        hits.first().map(|i| i.iri.as_str()),
        Some(anchored.as_str()),
        "exact-title topic must anchor its record first; got {:?}",
        hits.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Persist + incremental: the instance index is durable, so a warm restart on the
/// same data dir loads the vectors from disk and re-embeds NOTHING — startup cost is
/// proportional to churn, not graph size. This is the property that makes a large
/// A-box index affordable on every boot.
#[tokio::test]
async fn instance_index_persists_across_restart_and_is_incremental() {
    let dir = std::env::temp_dir().join(format!("moosedev-eval-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let onto = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    // First boot: record two records and build the index from cold.
    let cold = {
        let mut state = AppState::bootstrap(&dir, &onto).expect("bootstrap 1");
        let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
        record(
            &state,
            &class_iri,
            "Adopt RocksDB for the durable store",
            "Embedded key-value persistence backend for the knowledge graph.",
        );
        record(
            &state,
            &class_iri,
            "Blue-green deployment for releases",
            "Two identical production environments cut over with no downtime.",
        );
        let embedded = state.build_instance_index().await.expect("cold build");
        assert!(
            state.instance_store.is_enabled(),
            "index should be populated after the cold build"
        );
        embedded
    }; // state dropped → RocksDB + the durable sqlite store are released
    assert_eq!(cold, 2, "cold build should embed both records");

    // Second boot on the SAME data dir: the durable store already holds both
    // vectors, so the incremental reconcile embeds nothing.
    let mut state = AppState::bootstrap(&dir, &onto).expect("bootstrap 2");
    let warm = state.build_instance_index().await.expect("warm build");
    assert_eq!(
        warm, 0,
        "warm restart must reuse the persisted index, not re-embed"
    );
    assert!(
        state.instance_store.is_enabled(),
        "reused index should still be populated"
    );

    // And dense seeding still works against the reused index (paraphrase, low overlap).
    let hits = graph::relevant_context(
        &state,
        Some("which embedded database engine for on-disk persistence"),
        10,
        false,
    )
    .expect("seed against reused index");
    assert!(
        hits.iter().any(|i| i.label.contains("RocksDB")),
        "reused dense index should still surface the paraphrase; got {:?}",
        hits.iter().map(|i| &i.label).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
