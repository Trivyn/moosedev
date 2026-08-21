//! Repair supersessions whose lifecycle status was never flipped.
//!
//! A record that is the object of a `supersedes` edge must carry
//! `hasLifecycleStatus "superseded"`. Stores written before the inverse/status
//! write landed (commit ad45a97, 2026-07-16), or linked with a bare `relate`
//! instead of `supersede_decision`, can hold a `supersedes` edge whose target
//! is still `accepted` — so recall returns a replaced decision as current.
//! This flips exactly those targets; nothing else is touched.
//!
//! The `supersedes` edge is the evidence and is left alone, as is the
//! reasoner-materialized `isSupersededBy` inverse (GROWL re-derives it, and
//! `export_canonical` strips inferred quads from the text by design).
//!
//! Idempotent: a repaired store matches zero rows.
//!
//! DRY-RUN by default: opens the store read-only and reports the rows it would
//! change. Pass `--apply` to mutate, which opens the store exclusively — the
//! daemon serving the store MUST be stopped first. Run against a copy first.
//!
//! Usage:
//!   cargo run --release --example repair_unflipped_supersessions -- [STORE_KG_PATH] [--apply]
//! Defaults: .moosedev/kg

use std::collections::BTreeSet;

use moosedev::graph::PROJECT_KG_GRAPH_IRI;
use oxigraph::model::{GraphName, Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::store::Store;

const SUPERSEDES_LOCAL: &str = "supersedes";
const STATUS_LOCAL: &str = "hasLifecycleStatus";
const RETIRED: &[&str] = &["superseded", "deprecated"];

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

    // Every record claimed as superseded by some other record.
    let mut claimed = BTreeSet::new();
    for quad in store
        .quads_for_pattern(None, None, None, Some(graph.into()))
        .collect::<Result<Vec<_>, _>>()?
    {
        if local_name(quad.predicate.as_str()) != SUPERSEDES_LOCAL {
            continue;
        }
        if let Term::NamedNode(target) = &quad.object {
            claimed.insert(target.as_str().to_string());
        }
    }

    let mut changed = 0usize;
    for iri in claimed {
        let subject = NamedNode::new(&iri)?;
        let statuses: Vec<Quad> = store
            .quads_for_pattern(
                Some(subject.as_ref().into()),
                None,
                None,
                Some(graph.into()),
            )
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|quad| local_name(quad.predicate.as_str()) == STATUS_LOCAL)
            .collect();

        // Already retired (or carries no status literal to correct) — leave it.
        if statuses.is_empty()
            || statuses.iter().any(
                |quad| matches!(&quad.object, Term::Literal(lit) if RETIRED.contains(&lit.value())),
            )
        {
            continue;
        }

        let was = statuses
            .iter()
            .filter_map(|quad| match &quad.object {
                Term::Literal(lit) => Some(lit.value()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", ");

        changed += 1;
        println!("  {was} -> superseded: {iri}");
        if apply {
            let status_predicate = statuses[0].predicate.clone();
            for quad in &statuses {
                store.remove(quad)?;
            }
            store.insert(&Quad::new(
                subject,
                status_predicate,
                Literal::new_simple_literal("superseded"),
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
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}
