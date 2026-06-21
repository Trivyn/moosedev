//! Relabel content-shaped `rdfs:label` values to short designator handles.
//!
//! Reads a JSON map `{ "<iri>": "<new handle>", ... }` and, for each record in the
//! project graph, replaces its `rdfs:label` with the handle (preserving any language
//! tag). `hasTitle` is left untouched, so the full original title survives there and,
//! being content-shaped, forfeits its BM25F boost anyway — no information is lost.
//!
//! DRY-RUN by default: opens the store READ-ONLY (safe alongside a live `--serve`),
//! reads nothing-destructive, and reports coverage. Pass `--apply` to mutate, which
//! opens the store EXCLUSIVELY and therefore requires the serve stopped. Back up first.
//!
//! Usage:
//!   cargo run --release --example relabel_apply -- [MAP_JSON] [STORE_KG_PATH] [--apply]
//! Defaults: /tmp/final_map.json and .moosedev/kg

use moosedev::graph::PROJECT_KG_GRAPH_IRI;
use oxigraph::model::{Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::store::Store;
use std::collections::BTreeMap;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let map_path = pos.first().map(|s| s.as_str()).unwrap_or("/tmp/final_map.json");
    let kg_path = pos.get(1).map(|s| s.as_str()).unwrap_or(".moosedev/kg");

    let map: BTreeMap<String, String> =
        serde_json::from_reader(std::fs::File::open(map_path)?)?;
    println!("map: {} entries from {map_path}\nstore: {kg_path}  mode: {}",
             map.len(), if apply { "APPLY (exclusive)" } else { "DRY-RUN (read-only)" });

    let store = if apply { Store::open(kg_path)? } else { Store::open_read_only(kg_path)? };
    let label = NamedNodeRef::new(RDFS_LABEL)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;

    let (mut updated, mut missing, mut multi) = (0usize, 0usize, 0usize);
    for (iri, handle) in &map {
        let subj = NamedNode::new(iri)?;
        let existing: Vec<Quad> = store
            .quads_for_pattern(Some(subj.as_ref().into()), Some(label), None, Some(graph.into()))
            .collect::<Result<_, _>>()?;
        if existing.is_empty() {
            missing += 1;
            eprintln!("  MISSING rdfs:label: {iri}");
            continue;
        }
        if existing.len() > 1 {
            multi += 1;
            eprintln!("  {} label quads (will collapse to 1): {iri}", existing.len());
        }
        let new_obj: Term = match &existing[0].object {
            Term::Literal(l) => match l.language() {
                Some(lang) => Literal::new_language_tagged_literal(handle, lang)?.into(),
                None => Literal::new_simple_literal(handle).into(),
            },
            _ => Literal::new_simple_literal(handle).into(),
        };
        if apply {
            for q in &existing {
                store.remove(q)?;
            }
            store.insert(&Quad::new(subj, label.into_owned(), new_obj, graph.into_owned()))?;
        }
        updated += 1;
    }
    if apply {
        store.flush()?;
    }

    println!(
        "\n{} {updated} records | missing label: {missing} | multi-label: {multi}",
        if apply { "APPLIED →" } else { "DRY-RUN would update" }
    );
    if !apply {
        println!("(nothing written — rerun with --apply, serve stopped, to mutate)");
    }
    Ok(())
}
