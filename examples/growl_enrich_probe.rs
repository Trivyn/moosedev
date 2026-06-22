//! Read-only prototype: run GROWL enrich over the dogfood project A-box + the CURRENT
//! domain ontologies (loaded fresh from disk so the just-added `owl:inverseOf` axioms are
//! guaranteed present, regardless of what a running serve loaded at startup), and print
//! the inferred-edge delta. Proves the declared inverses pay off before any write-path
//! wiring. ZERO mutation of the dogfood store (opened read-only).
//!
//!   cargo run --example growl_enrich_probe [-- <kg_path> <ontology_dir>]
//!
//! defaults: kg_path=.moosedev/kg  ontology_dir=ontologies

use std::collections::BTreeMap;
use std::path::Path;

use moosedev::graph::PROJECT_KG_GRAPH_IRI;
use moosedev::ontology::{
    load_turtle, ARCH_DOMAIN_GRAPH_IRI, ARCH_DOMAIN_TTL, SE_DOMAIN_GRAPH_IRI, SE_DOMAIN_TTL,
};
use moosedev::reasoning::enrich_delta;
use oxigraph::model::{GraphNameRef, NamedNodeRef};
use oxigraph::store::Store;

/// Last path/fragment segment of an IRI.
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// `<…/Requirement/b3ba8285-…>` -> `Requirement/b3ba8285` for readable samples.
fn short(term: &impl std::fmt::Display) -> String {
    let s = term.to_string();
    let s = s.trim_start_matches('<').trim_end_matches('>');
    let mut it = s.rsplitn(3, '/');
    match (it.next(), it.next()) {
        (Some(last), Some(mid)) => {
            let last = last.get(..8).unwrap_or(last);
            format!("{mid}/{last}")
        }
        _ => s.to_string(),
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let kg_path = args.next().unwrap_or_else(|| ".moosedev/kg".to_string());
    let onto_dir = args.next().unwrap_or_else(|| "ontologies".to_string());
    let onto_dir = Path::new(&onto_dir);

    // 1. Copy the project A-box out of the dogfood store (read-only secondary — coexists
    //    with a live serve) into a fresh in-memory store.
    let src = Store::open_read_only(&kg_path)
        .map_err(|e| anyhow::anyhow!("open {kg_path} read-only: {e}"))?;
    let mem = Store::new()?;
    let project = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let mut abox = 0usize;
    for q in src.quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(project))) {
        let quad = q?;
        mem.insert(&quad)?;
        abox += 1;
    }

    // 2. Load the CURRENT domain ontologies from disk (guarantees the just-added inverses
    //    are present, independent of any running serve's startup snapshot).
    load_turtle(&mem, &onto_dir.join(SE_DOMAIN_TTL), SE_DOMAIN_GRAPH_IRI)?;
    load_turtle(&mem, &onto_dir.join(ARCH_DOMAIN_TTL), ARCH_DOMAIN_GRAPH_IRI)?;

    // 3. Enrich (no writes — returns the inferred delta placed in the project graph).
    let delta = enrich_delta(
        &mem,
        PROJECT_KG_GRAPH_IRI,
        &[SE_DOMAIN_GRAPH_IRI, ARCH_DOMAIN_GRAPH_IRI],
    )?;

    // 4. Report: histogram by predicate local-name + a few non-type samples.
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    for q in &delta {
        *hist
            .entry(local_name(q.predicate.as_str()).to_string())
            .or_default() += 1;
    }

    println!("project A-box : {abox} triples in <{PROJECT_KG_GRAPH_IRI}>");
    println!("enrich delta  : {} inferred edges\n", delta.len());
    println!("by predicate (local name):");
    for (p, n) in &hist {
        println!("  {n:>4}  {p}");
    }

    println!("\nsample inferred edges (excluding rdf:type):");
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    for q in delta
        .iter()
        .filter(|q| q.predicate.as_str() != rdf_type)
        .take(15)
    {
        println!(
            "  {:<28} --{}--> {}",
            short(&q.subject),
            local_name(q.predicate.as_str()),
            short(&q.object),
        );
    }
    Ok(())
}
