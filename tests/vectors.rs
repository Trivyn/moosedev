//! Builds the ontology vector store from the shipped ontologies and verifies it
//! holds the right elements (by IRI), the right dimension, and a stamp matching
//! the active embedding model. `build_and_open` succeeding already proves the
//! stamp + per-row dim are valid (`VecStore::open` validates both); we then decode
//! the rows with MOOSE's own reader to assert coverage. Loads the embedding
//! backbone, so this is slower than the symbolic tests.

use std::path::Path;

use moosedev::ontology;
use moosedev::vectors;
use oxigraph::store::Store;

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
