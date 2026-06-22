//! Audit `rdfs:label` shapes against the MOOSE label-designator contract.
//!
//! Reports how many labels in a store are *content-shaped* — a sentence or claim
//! stuffed into a name field, which Core demotes out of name-resolution and the
//! BM25F title boost — versus proper short *designators*. This is the first-class,
//! whole-store version of the write-time length warning in the record handlers,
//! and the driver/verifier for a relabel migration.
//!
//! The store is opened READ-ONLY (a secondary RocksDB handle), so this is safe to
//! run while a `--serve` holds the primary write lock on the same store.
//!
//! Usage:
//!   cargo run --release --example audit_labels [STORE_KG_PATH] [GRAPH_IRI]
//! Defaults: `.moosedev/kg` (the dogfood store) and `PROJECT_KG_GRAPH_IRI`.

use moose::entity_index::audit_label_shapes;
use moose::types::LabelShapeConfig;
use moosedev::graph::PROJECT_KG_GRAPH_IRI;
use oxigraph::store::Store;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let kg_path = args.next().unwrap_or_else(|| ".moosedev/kg".to_string());
    let graph_iri = args
        .next()
        .unwrap_or_else(|| PROJECT_KG_GRAPH_IRI.to_string());

    let store = Store::open_read_only(&kg_path)
        .map_err(|e| anyhow::anyhow!("open {kg_path} read-only: {e}"))?;

    let cfg = LabelShapeConfig::default();
    println!(
        "auditing <{graph_iri}> in {kg_path}\nthresholds: content if >{}c OR >{}w OR sentence-break",
        cfg.max_designator_chars, cfg.max_designator_words
    );

    audit_label_shapes(&store, &[graph_iri], &cfg).print();
    Ok(())
}
