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
    let rows = moose::embeddings::read_ontology_vectors(&db_path)
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
    let count_after_build = moose::embeddings::read_ontology_vectors(&db_path)
        .await
        .unwrap()
        .len();

    // Same ontology, same model → cache hit. Read-only open must not rewrite the DB.
    vectors::build_and_open(&store, GRAPHS, &db_path)
        .await
        .expect("second build (expected cache hit)");
    let mtime_after_reuse = std::fs::metadata(&db_path).unwrap().modified().unwrap();
    let count_after_reuse = moose::embeddings::read_ontology_vectors(&db_path)
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
    let rows1 = moose::embeddings::read_ontology_vectors(&db_path)
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
    let rows2 = moose::embeddings::read_ontology_vectors(&db_path)
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
