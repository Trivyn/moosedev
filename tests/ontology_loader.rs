//! Verifies the ontology-agnostic loader: the stub architecture ontology loads
//! into an oxigraph store and yields a MOOSE `CompactVocabulary` containing the
//! v1 typed classes and relations. When the generated ontology replaces the
//! stub, this test continues to pass unchanged (content-agnostic plumbing).

use std::path::Path;

use moosedev::ontology;
use oxigraph::store::Store;

#[test]
fn loads_architecture_ontology_and_extracts_vocabulary() {
    let store = Store::new().expect("in-memory store");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(ontology::DEFAULT_ARCHITECTURE_TTL);

    let vocab = ontology::load_architecture(&store, &path).expect("load architecture ontology");

    // The central typed class must surface in the extracted vocabulary —
    // this is what `record_important_decision` will type instances against.
    let has_decision = vocab
        .classes
        .iter()
        .any(|c| c.iri.ends_with("ArchitecturalDecision"));
    assert!(
        has_decision,
        "ArchitecturalDecision should appear in the extracted vocabulary; got classes: {:?}",
        vocab.classes.iter().map(|c| &c.iri).collect::<Vec<_>>()
    );

    // At least one walkable relation (object property) must be extracted —
    // these are the edges the NLQ query traverses.
    assert!(
        !vocab.object_properties.is_empty(),
        "expected at least one object property (e.g. arch:concerns)"
    );
}
