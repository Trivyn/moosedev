//! Dogfood probe for the capture-gap fix, run against a COPY of the real project
//! store (a separate RocksDB lock, so it coexists with a live serve). On the actual
//! dogfood graph it shows: (1) the non-blocking under-linked advisory that
//! `validate_against_architecture` now surfaces, and (2) real ranked,
//! ontology-legal link suggestions. ZERO writes to the live store.
//!
//!   cp -r .moosedev /tmp/moosedev-capgap-demo && rm -f /tmp/moosedev-capgap-demo/*.sock
//!   cargo run --release --example capgap_probe -- /tmp/moosedev-capgap-demo ontologies

use std::collections::BTreeMap;
use std::path::Path;

use moosedev::graph::{self, AppState};
use moosedev::validation;

/// `<…/ArchitecturalDecision/af8ef3b1-…>` -> `ArchitecturalDecision/af8ef3b1`.
fn short(iri: &str) -> String {
    let s = iri.trim_start_matches('<').trim_end_matches('>');
    let mut it = s.rsplitn(3, '/');
    match (it.next(), it.next()) {
        (Some(last), Some(mid)) => format!("{mid}/{}", last.get(..8).unwrap_or(last)),
        _ => s.to_string(),
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let data_dir = args
        .next()
        .unwrap_or_else(|| "/tmp/moosedev-capgap-demo".to_string());
    let onto_dir = args.next().unwrap_or_else(|| "ontologies".to_string());

    let mut state = AppState::bootstrap(Path::new(&data_dir), Path::new(&onto_dir))?;

    // (1) Under-linked advisory — pure SPARQL over the shapes' sh:or branches.
    let under = graph::under_linked_records(&state, usize::MAX);
    println!("== under-linked records (shapes say each SHOULD carry a link) ==");
    println!("total under-linked: {}", under.len());
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    for u in &under {
        *by_class
            .entry(format!(
                "{} (missing {})",
                u.class_local, u.missing_predicate
            ))
            .or_default() += 1;
    }
    for (k, n) in &by_class {
        println!("  {n:>3}  {k}");
    }

    // (2) validate — conformance is unaffected; advisories are surfaced alongside.
    let report = validation::validate_project(&state)?;
    println!("\n== validate_against_architecture ==");
    println!(
        "Conforms: {}   Violations: {}   Advisories (SHOULD, non-blocking): {}",
        report.conforms(),
        report.violations.len(),
        report.advisories.len()
    );

    // (3) Real link suggestions — needs the dense retrieval index (embedding model).
    println!("\n== link suggestions (hybrid retrieval × SHACL legality, suggest-only) ==");
    match state.build_instance_index().await {
        Ok(n) => {
            println!("(dense index ready: {n} record vectors)\n");
            for u in under.iter().take(6) {
                let sugg = graph::suggest_links_for_record(
                    &state,
                    &u.iri,
                    3,
                    graph::dense_floor(),
                    Some(u.missing_predicate.as_str()),
                )
                .await;
                println!(
                    "• {} ({}) — should have {}",
                    short(&u.iri),
                    u.class_local,
                    u.missing_predicate
                );
                if sugg.is_empty() {
                    println!("    (no confident candidate)");
                }
                for s in sugg {
                    // Oriented subject→object, matching the `relate` it would emit.
                    println!(
                        "    \"{}\" ({}) --{}--> \"{}\" ({})  [score {:.3}]",
                        clip(&s.subject_title, 48),
                        s.subject_kind,
                        s.predicate_local,
                        clip(&s.object_title, 48),
                        s.object_kind,
                        s.score
                    );
                }
            }
        }
        Err(e) => println!("  (dense index unavailable — embedding backbone didn't load: {e})"),
    }
    Ok(())
}
