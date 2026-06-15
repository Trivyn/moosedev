//! End-to-end write path: bootstrap state, record a typed decision via the
//! `graph` layer (which calls MOOSE's `kg::assert_instance`), and verify both
//! that the typed quad lands in the durable project graph and that the new
//! instance is immediately findable through the cache-coherent entity index.

use std::path::Path;

use moosedev::graph::{
    self, AppState, RecordInput, ARCH_DESCRIPTION, ARCH_STATUS, PROJECT_KG_GRAPH_IRI,
};
use oxigraph::model::{GraphNameRef, NamedNodeRef, QuadRef};

#[test]
fn records_decision_into_durable_kg_and_is_findable() {
    let dir = std::env::temp_dir().join(format!("moosedev-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ttl = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies/architecture.ttl");

    let state = AppState::bootstrap(&dir, &ttl).expect("bootstrap app state");

    let class_iri = state
        .resolve_class("ArchitecturalDecision")
        .expect("ArchitecturalDecision is a known class");
    let title = "Adopt rmcp for the MCP transport";
    let input = RecordInput {
        class_iri: class_iri.clone(),
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), title.to_string()),
            (
                ARCH_DESCRIPTION.to_string(),
                "Chose the official Rust SDK over a hand-rolled JSON-RPC loop.".to_string(),
            ),
            (ARCH_STATUS.to_string(), "accepted".to_string()),
        ],
    };

    let subject = graph::record_instance(&state, &input).expect("record decision");

    // 1) The rdf:type quad is in the durable project KG named graph.
    let type_quad = QuadRef::new(
        NamedNodeRef::new(&subject).unwrap(),
        NamedNodeRef::new(moose::RDF_TYPE).unwrap(),
        NamedNodeRef::new(&class_iri).unwrap(),
        GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    assert!(
        state.store.contains(type_quad).unwrap(),
        "the typed instance should be asserted into the project KG graph"
    );

    // 2) Read-after-write: the new instance is findable via the (coherent) index.
    let hits = state.entity_index.search_classes(
        title,
        &[class_iri],
        &state.store,
        &[PROJECT_KG_GRAPH_IRI.to_string()],
        moose::LABEL_PREDICATES,
        10,
    );
    assert!(
        hits.iter().any(|h| h.iri == subject),
        "the recorded decision should be findable immediately after the write; got {:?}",
        hits.iter().map(|h| &h.iri).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
