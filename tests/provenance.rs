//! PROV-O edit provenance: recording an instance, then writing + reading back
//! its edit provenance (who asserted it, when, via which activity). Hermetic.

use std::path::Path;

use moosedev::graph::{self, AppState, RecordInput};
use moosedev::provenance;

#[test]
fn records_and_reads_edit_provenance() {
    let dir = std::env::temp_dir().join(format!("moosedev-prov-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = graph::record_instance(
        &state,
        &RecordInput {
            class_iri,
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![(moose::RDFS_LABEL.to_string(), "Adopt rmcp".to_string())],
        },
    )
    .expect("record decision");

    // Nothing recorded yet.
    assert!(provenance::read_provenance(&state.store, &iri)
        .unwrap()
        .is_none());

    provenance::record_provenance(&state.store, &iri, "test-agent").expect("write provenance");

    let p = provenance::read_provenance(&state.store, &iri)
        .unwrap()
        .expect("provenance should be present");
    assert_eq!(p.agent, "test-agent");
    assert!(!p.time.is_empty(), "should carry a timestamp");
    assert!(
        p.activity.starts_with("https://moosedev.dev/kg/Activity/"),
        "activity IRI: {}",
        p.activity
    );

    let _ = std::fs::remove_dir_all(&dir);
}
