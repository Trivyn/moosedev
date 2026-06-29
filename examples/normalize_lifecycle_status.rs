//! Normalize legacy default lifecycle statuses.
//!
//! Top-level ArchitecturalDecision and Requirement records that still carry the
//! old default status `proposed` are rewritten to `accepted`. Auxiliary records
//! such as Alternative, Consequence, and Rationale are deliberately left alone.
//!
//! DRY-RUN by default: opens the store read-only and reports the rows it would
//! change. Pass `--apply` to mutate, which opens the store exclusively. Run this
//! against a copied store first.
//!
//! Usage:
//!   cargo run --release --example normalize_lifecycle_status -- [STORE_KG_PATH] [--apply]
//! Defaults: .moosedev/kg

use std::collections::BTreeSet;

use moosedev::graph::PROJECT_KG_GRAPH_IRI;
use oxigraph::model::{GraphName, Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::store::Store;

const TARGET_CLASSES: &[&str] = &["ArchitecturalDecision", "Requirement"];
const STATUS_LOCAL: &str = "hasLifecycleStatus";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|arg| arg == "--apply");
    let kg_path = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .unwrap_or(".moosedev/kg");

    println!(
        "store: {kg_path}  mode: {}",
        if apply {
            "APPLY (exclusive)"
        } else {
            "DRY-RUN (read-only)"
        }
    );

    let store = if apply {
        Store::open(kg_path)?
    } else {
        Store::open_read_only(kg_path)?
    };
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;

    let mut targets = BTreeSet::new();
    for quad in store
        .quads_for_pattern(None, Some(rdf_type), None, Some(graph.into()))
        .collect::<Result<Vec<_>, _>>()?
    {
        let Term::NamedNode(class) = &quad.object else {
            continue;
        };
        if !TARGET_CLASSES.contains(&local_name(class.as_str())) {
            continue;
        }
        if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = &quad.subject {
            targets.insert(subject.as_str().to_string());
        }
    }

    let mut changed = 0usize;
    for iri in targets {
        let subject = NamedNode::new(&iri)?;
        let proposed_statuses: Vec<Quad> = store
            .quads_for_pattern(
                Some(subject.as_ref().into()),
                None,
                None,
                Some(graph.into()),
            )
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|quad| {
                local_name(quad.predicate.as_str()) == STATUS_LOCAL
                    && matches!(&quad.object, Term::Literal(lit) if lit.value() == "proposed")
            })
            .collect();

        if proposed_statuses.is_empty() {
            continue;
        }

        changed += 1;
        println!("  proposed -> accepted: {iri}");
        if apply {
            let status_predicate = proposed_statuses[0].predicate.clone();
            for quad in &proposed_statuses {
                store.remove(quad)?;
            }
            store.insert(&Quad::new(
                subject,
                status_predicate,
                Literal::new_simple_literal("accepted"),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
            ))?;
        }
    }

    if apply {
        store.flush()?;
    }

    println!(
        "{} {changed} records",
        if apply {
            "APPLIED"
        } else {
            "DRY-RUN would change"
        }
    );
    Ok(())
}

fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}
