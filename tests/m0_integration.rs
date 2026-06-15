//! M0 integration checks: prove the two flagged MOOSE/oxigraph integration
//! unknowns are resolved —
//!   1. a persistent on-disk oxigraph `Store` survives a close/reopen (the
//!      durable project KG depends on this), and
//!   2. `moose::initialize` works when called from outside the moose crate
//!      (it resolves `MOOSE-Pipeline.ttl` via moose's own `CARGO_MANIFEST_DIR`).

use oxigraph::model::{GraphName, NamedNode, Quad};
use oxigraph::store::Store;

#[test]
fn moose_initializes_and_store_persists_across_reopen() {
    let dir = std::env::temp_dir().join(format!("moosedev-m0-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let test_graph = NamedNode::new("urn:moosedev:test").unwrap();

    // First open: initialize MOOSE and write one quad, then drop the store
    // (releases the on-disk lock) so we can reopen it below.
    {
        let store = Store::open(&dir).expect("open persistent oxigraph store");

        let cache = moose::initialize(&store).expect("moose::initialize from moosedev");
        assert!(
            !cache.stages.is_empty(),
            "MOOSE pipeline ontology should load stages"
        );

        store
            .insert(&Quad::new(
                NamedNode::new("urn:moosedev:s").unwrap(),
                NamedNode::new("urn:moosedev:p").unwrap(),
                NamedNode::new("urn:moosedev:o").unwrap(),
                GraphName::NamedNode(test_graph.clone()),
            ))
            .expect("insert test quad");
        store.flush().expect("flush store to disk");
    }

    // Reopen: the quad we wrote must still be present → durable persistence works.
    {
        let store = Store::open(&dir).expect("reopen persistent oxigraph store");
        let count = store
            .quads_for_pattern(None, None, None, Some(test_graph.as_ref().into()))
            .count();
        assert_eq!(count, 1, "test quad should persist across store reopen");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
