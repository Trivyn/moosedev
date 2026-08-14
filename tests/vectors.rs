//! Builds the ontology vector store from the shipped ontologies and verifies it
//! holds the right elements (by IRI), the right dimension, and a stamp matching
//! the active embedding model. `build_and_open` succeeding already proves the
//! stamp + per-row dim are valid (`VecStore::open` validates both); we then decode
//! the rows with MOOSE's own reader to assert coverage. Loads the embedding
//! backbone, so this is slower than the symbolic tests.

use std::path::Path;

use moosedev::ontology;
use moosedev::vectors;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use oxigraph::store::Store;

const GRAPHS: &[&str] = &[
    ontology::SE_DOMAIN_GRAPH_IRI,
    ontology::ARCH_DOMAIN_GRAPH_IRI,
];

/// Load the shipped ontologies into a fresh in-memory store (mirrors bootstrap).
fn store_with_ontologies() -> Store {
    let store = Store::new().unwrap();
    let onto_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    ontology::load_ontologies(&store, &onto_dir).expect("load ontologies");
    store
}

/// A unique temp dir per test (PID + name), removed and recreated fresh.
fn fresh_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("moosedev-vectors-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn pool(db_path: &Path) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(&format!("sqlite://{}", db_path.display()))
        .await
        .expect("open store for inspection")
}

/// The layout generation a persisted store records, or `None` if unstamped.
async fn schema_version(db_path: &Path) -> Option<String> {
    let pool = pool(db_path).await;
    let found: Option<(String,)> =
        sqlx::query_as("SELECT value FROM store_meta WHERE key = 'moosedev_vector_schema'")
            .fetch_optional(&pool)
            .await
            .expect("read store_meta");
    pool.close().await;
    found.map(|(v,)| v)
}

/// The `ontology_vectors` column names, in declaration order.
async fn columns(db_path: &Path) -> Vec<String> {
    let pool = pool(db_path).await;
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('ontology_vectors')")
            .fetch_all(&pool)
            .await
            .expect("read table info");
    pool.close().await;
    rows.into_iter().map(|(n,)| n).collect()
}

/// Rewrite a freshly built store into the pre-namespace shape MOOSE 0.6 wrote:
/// four columns, no provenance, no layout stamp. The fingerprint sidecar is left
/// alone, so the reuse path is entered exactly as it would be on a real upgrade.
async fn downgrade_to_pre_rekey_shape(db_path: &Path) {
    let pool = pool(db_path).await;
    // One connection for the whole sequence: these are schema edits, and pooled
    // connections give no ordering guarantee between them.
    let mut conn = pool.acquire().await.expect("acquire connection");
    for stmt in [
        "ALTER TABLE ontology_vectors DROP COLUMN namespace",
        "ALTER TABLE ontology_vectors DROP COLUMN owning_graph",
        "DELETE FROM store_meta WHERE key = 'moosedev_vector_schema'",
    ] {
        sqlx::query(stmt)
            .execute(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    drop(conn);
    pool.close().await;
}

/// Overwrite the layout stamp, leaving the rows and fingerprint valid.
async fn set_schema_version(db_path: &Path, version: &str) {
    let pool = pool(db_path).await;
    sqlx::query(
        "INSERT INTO store_meta(key, value) VALUES('moosedev_vector_schema', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(version)
    .execute(&pool)
    .await
    .expect("set schema version");
    pool.close().await;
}

#[tokio::test]
async fn builds_vector_store_for_shipped_ontologies() {
    let dir = std::env::temp_dir().join(format!("moosedev-vectors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Load both domains into a fresh store (mirrors bootstrap).
    let store = Store::new().unwrap();
    let onto_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    ontology::load_ontologies(&store, &onto_dir).expect("load ontologies");

    let db_path = dir.join("vectors.db");
    let vs = vectors::build_and_open(
        &store,
        &[
            ontology::SE_DOMAIN_GRAPH_IRI,
            ontology::ARCH_DOMAIN_GRAPH_IRI,
        ],
        &db_path,
    )
    .await
    .expect("build + open vector store (stamp + dims validated by open)");
    assert!(vs.is_enabled(), "store should be enabled (has vectors)");

    // Decode rows with MOOSE's own reader and assert coverage.
    let rows = moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
        .await
        .expect("read back vectors");
    assert!(!rows.is_empty(), "expected ontology vectors");
    assert!(
        rows.iter().all(|r| r.embedding.len() == 384),
        "every vector should be 384-dim (arctic-embed-s)"
    );
    for expected in ["ArchitecturalDecision", "Constraint", "Lesson"] {
        assert!(
            rows.iter().any(|r| r.iri.ends_with(expected)),
            "expected a vector for architecture class {expected}; got {} rows",
            rows.len()
        );
    }
    assert!(
        rows.iter().any(|r| r.iri.ends_with("Component")),
        "expected an SE Component vector (both domains are embedded)"
    );

    // The provenance contract MOOSEDev owns as the writer of this table.
    assert!(
        rows.iter()
            .all(|r| r.namespace == vectors::ONTOLOGY_VECTOR_NAMESPACE),
        "every row must carry this producer's namespace, or a scoped read finds nothing"
    );
    for graph in GRAPHS {
        assert!(
            rows.iter().any(|r| r.owning_graph == *graph),
            "expected rows owned by {graph}; owning_graph is likely not threaded per-graph"
        );
    }
    assert!(
        rows.iter()
            .all(|r| GRAPHS.contains(&r.owning_graph.as_str())),
        "owning_graph must be one of the embedded domain graphs"
    );

    // MOOSE rejects the whole store at open if two rows collide here, so assert it
    // directly — the failure names the offender, which the engine's error will not.
    let mut keys: Vec<_> = rows
        .iter()
        .map(|r| (r.element_type.as_db_value(), r.iri.as_str()))
        .collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        total,
        "producer emitted a duplicate (element_type, iri)"
    );

    assert_eq!(
        schema_version(&db_path).await.as_deref(),
        Some("2"),
        "a freshly built store must record the layout generation that wrote it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Unchanged ontology ⇒ the second build reuses the persisted store: the DB file
/// is never rewritten (a cache hit opens it read-only). Proves we don't reload the
/// backbone or re-embed when nothing changed.
#[tokio::test]
async fn reuses_cached_store_when_ontology_unchanged() {
    let dir = fresh_dir("reuse");
    let store = store_with_ontologies();
    let db_path = dir.join("vectors.db");

    vectors::build_and_open(&store, GRAPHS, &db_path)
        .await
        .expect("first build");
    let mtime_after_build = std::fs::metadata(&db_path).unwrap().modified().unwrap();
    let count_after_build =
        moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
            .await
            .unwrap()
            .len();

    // Same ontology, same model → cache hit. Read-only open must not rewrite the DB.
    vectors::build_and_open(&store, GRAPHS, &db_path)
        .await
        .expect("second build (expected cache hit)");
    let mtime_after_reuse = std::fs::metadata(&db_path).unwrap().modified().unwrap();
    let count_after_reuse =
        moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
            .await
            .unwrap()
            .len();

    assert_eq!(
        mtime_after_build, mtime_after_reuse,
        "cache hit must not rewrite the vector DB (it was rebuilt)"
    );
    assert_eq!(
        count_after_build, count_after_reuse,
        "row count must be stable across a cache hit"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A store written by any other generation of this producer is regenerated, not
/// migrated. Covers the three ways a store can be found stale — the real 0.6 shape
/// every existing data dir is in, an older stamp, and an *unknown future* stamp —
/// and asserts all three converge on the current shape. The future-version case is
/// the one that pins the branch as a mismatch test rather than a `<` test, so a
/// downgrade-then-upgrade regenerates instead of reading a shape it can't parse.
#[tokio::test]
async fn regenerates_store_from_any_other_schema() {
    for (name, staleness) in [
        ("prerekey", Staleness::PreRekeyShape),
        ("older", Staleness::Stamp("1")),
        ("future", Staleness::Stamp("99")),
    ] {
        let dir = fresh_dir(&format!("regen-{name}"));
        let store = store_with_ontologies();
        let db_path = dir.join("vectors.db");

        vectors::build_and_open(&store, GRAPHS, &db_path)
            .await
            .expect("first build");
        let fingerprint_path = dir.join("vectors.db.fingerprint");
        let fingerprint_before = std::fs::read_to_string(&fingerprint_path).expect("fingerprint");

        match staleness {
            Staleness::PreRekeyShape => downgrade_to_pre_rekey_shape(&db_path).await,
            Staleness::Stamp(v) => set_schema_version(&db_path, v).await,
        }
        // The content fingerprint is deliberately untouched: the layout stamp, not
        // the ontology text, has to be what forces the rebuild.
        assert_eq!(
            std::fs::read_to_string(&fingerprint_path).unwrap(),
            fingerprint_before,
            "{name}: fingerprint must be unchanged so the reuse path is entered"
        );

        let vs = vectors::build_and_open(&store, GRAPHS, &db_path)
            .await
            .unwrap_or_else(|e| panic!("{name}: stale store should regenerate, got {e}"));
        assert!(
            vs.is_enabled(),
            "{name}: regenerated store should have vectors"
        );

        let cols = columns(&db_path).await;
        for required in ["namespace", "owning_graph"] {
            assert!(
                cols.iter().any(|c| c == required),
                "{name}: regenerated store missing {required}; got {cols:?}"
            );
        }
        assert_eq!(
            schema_version(&db_path).await.as_deref(),
            Some("2"),
            "{name}: regenerated store must be restamped with the current generation"
        );
        assert!(
            !moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
                .await
                .unwrap_or_else(|e| panic!("{name}: regenerated store unreadable: {e}"))
                .is_empty(),
            "{name}: regenerated store should be readable at the production scope"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

enum Staleness {
    /// The four-column, unstamped table MOOSE 0.6 read.
    PreRekeyShape,
    /// Current shape, but stamped with another generation.
    Stamp(&'static str),
}

/// Changed ontology ⇒ rebuild: adding an altLabel to an existing class changes its
/// embed text, flips the fingerprint, and the class is re-embedded (its vector
/// differs). Proves the cache invalidates on real ontology content changes.
#[tokio::test]
async fn rebuilds_when_ontology_changes() {
    let dir = fresh_dir("rebuild");
    let store = store_with_ontologies();
    let db_path = dir.join("vectors.db");

    vectors::build_and_open(&store, GRAPHS, &db_path)
        .await
        .expect("first build");
    let rows1 = moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
        .await
        .unwrap();
    let lesson = rows1
        .iter()
        .find(|r| r.iri.ends_with("Lesson"))
        .expect("Lesson class vector present");
    let lesson_iri = lesson.iri.clone();
    let embedding_before = lesson.embedding.clone();

    // Mutate the ontology: add a *novel* altLabel to the Lesson class in the ARCH
    // graph (where it was extracted from, so `embed_text` picks it up). Must not
    // already exist on the class, or RDF set semantics make the insert a no-op.
    let graph = GraphName::NamedNode(NamedNode::new(ontology::ARCH_DOMAIN_GRAPH_IRI).unwrap());
    store
        .insert(&Quad::new(
            NamedNode::new(&lesson_iri).unwrap(),
            NamedNode::new(moose::SKOS_ALT_LABEL).unwrap(),
            Literal::new_simple_literal("Retrospective finding (test-only)"),
            graph,
        ))
        .unwrap();

    vectors::build_and_open(&store, GRAPHS, &db_path)
        .await
        .expect("second build (expected rebuild)");
    let rows2 = moose::embeddings::read_ontology_vectors(&db_path, vectors::ONTOLOGY_VECTOR_SCOPE)
        .await
        .unwrap();
    let embedding_after = rows2
        .iter()
        .find(|r| r.iri == lesson_iri)
        .expect("Lesson class still present after rebuild")
        .embedding
        .clone();

    assert_eq!(
        rows1.len(),
        rows2.len(),
        "same class set ⇒ row count unchanged by an altLabel edit"
    );
    assert_ne!(
        embedding_before, embedding_after,
        "altLabel change should re-embed the class (cache should have invalidated)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
