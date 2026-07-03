//! End-to-end flows for the committed canonical `kg.nq` (Requirement d459cac2):
//! write-through on capture, adoption of an existing store, fresh-clone
//! hydration, git-pull replacement, and both-sides-moved divergence merge.
//! One sequential test — each phase's state is the next phase's fixture, and
//! bootstrap (moose init + ontology load) is paid once per reopen only.

use std::path::{Path, PathBuf};

use chrono::Utc;
use moosedev::canonical;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphNameRef, NamedNodeRef};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("moosedev-canonical-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

/// Record a decision through the graph layer and run the same post-write hook
/// the MCP/HTTP write surfaces call.
fn record(state: &AppState, title: &str) -> String {
    let class_iri = state
        .resolve_class("ArchitecturalDecision")
        .expect("ArchitecturalDecision is a known class");
    let input = RecordInput {
        class_iri,
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), title.to_string()),
            (
                state.capture.description.clone(),
                format!("{title} — canonical-text integration fixture"),
            ),
            (state.capture.status.clone(), "accepted".to_string()),
        ],
    };
    let iri = graph::record_instance(state, &input, "test-agent", Utc::now()).expect("record");
    state.note_project_write();
    iri
}

fn contains_instance(state: &AppState, iri: &str) -> bool {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap());
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(iri).unwrap().into()),
            None,
            None,
            Some(graph),
        )
        .next()
        .is_some()
}

#[test]
fn canonical_text_lifecycle_end_to_end() {
    let onto = ontology_dir();
    let dir_a = temp_dir("a");

    // 1) Fresh project: no kg.nq until the first write.
    let state_a = AppState::bootstrap(&dir_a, &onto).expect("bootstrap A");
    assert!(
        !canonical::canonical_path(&dir_a).exists(),
        "an empty project should not grow an empty kg.nq at boot"
    );

    // 2) Write-through: capture writes kg.nq + stamp.
    let iri_a = record(&state_a, "Canonical test decision A");
    let text_a = std::fs::read_to_string(canonical::canonical_path(&dir_a)).expect("kg.nq");
    assert!(text_a.contains("Canonical test decision A"));
    assert!(canonical::stamp_path(&dir_a).exists());
    drop(state_a);

    // 3) Adoption: with kg.nq/stamp gone, reboot re-exports from the store,
    //    byte-identically (deterministic canonical sort).
    std::fs::remove_file(canonical::canonical_path(&dir_a)).unwrap();
    std::fs::remove_file(canonical::stamp_path(&dir_a)).unwrap();
    let state_a = AppState::bootstrap(&dir_a, &onto).expect("re-bootstrap A (adoption)");
    let readopted = std::fs::read_to_string(canonical::canonical_path(&dir_a)).expect("kg.nq");
    assert_eq!(text_a, readopted, "adoption re-export is byte-identical");
    drop(state_a);

    // 4) Fresh clone: a data dir holding ONLY kg.nq hydrates a working store.
    let dir_b = temp_dir("b");
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(canonical::canonical_path(&dir_b), &text_a).unwrap();
    let state_b = AppState::bootstrap(&dir_b, &onto).expect("bootstrap B (fresh clone)");
    assert!(
        contains_instance(&state_b, &iri_a),
        "cloned record is in B's store"
    );

    // 5) B moves forward: a second record lands in B's kg.nq.
    let iri_b = record(&state_b, "Canonical test decision B");
    let text_ab = std::fs::read_to_string(canonical::canonical_path(&dir_b)).unwrap();
    assert!(text_ab.contains("Canonical test decision B"));
    drop(state_b);

    // 6) Git-pull: C cloned at A's text, then kg.nq replaced by B's superset
    //    while C's store is in sync with its stamp → text is authoritative.
    let dir_c = temp_dir("c");
    std::fs::create_dir_all(&dir_c).unwrap();
    std::fs::write(canonical::canonical_path(&dir_c), &text_a).unwrap();
    let state_c = AppState::bootstrap(&dir_c, &onto).expect("bootstrap C");
    assert!(contains_instance(&state_c, &iri_a));
    assert!(!contains_instance(&state_c, &iri_b));
    drop(state_c);
    std::fs::write(canonical::canonical_path(&dir_c), &text_ab).unwrap();
    let state_c = AppState::bootstrap(&dir_c, &onto).expect("re-bootstrap C (pull)");
    assert!(
        contains_instance(&state_c, &iri_b),
        "pulled record hydrates into the store"
    );

    // 7) Divergence: C records locally (store+text+stamp advance), then the
    //    text is rolled back to the pre-C upstream and the stamp corrupted, so
    //    BOTH sides differ from the stamp → union merge, nothing lost, and the
    //    union is re-exported.
    let iri_c = record(&state_c, "Canonical test decision C");
    drop(state_c);
    std::fs::write(canonical::canonical_path(&dir_c), &text_ab).unwrap();
    std::fs::write(canonical::stamp_path(&dir_c), "not-a-real-hash").unwrap();
    let state_c = AppState::bootstrap(&dir_c, &onto).expect("re-bootstrap C (diverged)");
    for (iri, tag) in [(&iri_a, "A"), (&iri_b, "B"), (&iri_c, "C")] {
        assert!(
            contains_instance(&state_c, iri),
            "record {tag} survives the divergence merge"
        );
    }
    let text_union = std::fs::read_to_string(canonical::canonical_path(&dir_c)).unwrap();
    assert!(
        text_union.contains("Canonical test decision C"),
        "the union is re-exported to kg.nq"
    );
}
