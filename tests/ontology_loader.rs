//! Verifies the ontology-agnostic loader: the shipped domain ontologies + SHACL
//! shape graphs load into an oxigraph store and yield a MOOSE `CompactVocabulary`
//! for the architecture domain containing the v1 typed classes and the relations
//! the NLQ query walks. Loading by MOOSEDev-owned graph IRI (not the TTL
//! namespace) is what keeps the code decoupled from the regenerated ontology.

use std::path::Path;

use moosedev::ontology;
use oxigraph::store::Store;

#[test]
fn loads_ontologies_and_extracts_architecture_vocabulary() {
    let store = Store::new().expect("in-memory store");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    let vocab = ontology::load_ontologies(&store, &dir).expect("load shipped ontologies");

    // The architecture domain's typed classes must surface — these are what
    // `record_important_decision` types instances against (resolved by local name).
    let locals: Vec<&str> = vocab
        .classes
        .iter()
        .map(|c| c.local_name.as_str())
        .collect();
    for expected in [
        "ArchitecturalDecision",
        "Lesson",
        "Constraint",
        "AntiPattern",
    ] {
        assert!(
            locals.contains(&expected),
            "expected architecture class {expected} in extracted vocab; got {locals:?}"
        );
    }

    // The relations the NLQ query traverses (concerns, isMotivatedBy, hasRationale, …).
    assert!(
        vocab.object_properties.len() >= 3,
        "expected several architecture relations; got {}",
        vocab.object_properties.len()
    );

    // The capture predicates are resolved by local name at bootstrap, so they
    // must be present as datatype properties in the extracted vocabulary.
    let dt_locals: Vec<&str> = vocab
        .datatype_properties
        .iter()
        .map(|e| e.local_name.as_str())
        .collect();
    for expected in [
        "hasTitle",
        "hasDescription",
        "hasLifecycleStatus",
        "hasAuthor",
        "hasTimestamp",
    ] {
        assert!(
            dt_locals.contains(&expected),
            "expected datatype property {expected}; got {dt_locals:?}"
        );
    }
}
