//! Retract a reviewed list of records in one run.
//!
//! The list is a TSV file, one record per line: `<record IRI>\t<rationale>`.
//! Blank lines and lines starting with `#` are skipped. Each record goes
//! through `graph::retract_decision`, the same write as the `retract_decision`
//! tool: its status becomes `deprecated` and a `Rationale` carrying the given
//! text is attached. The record and its edges stay, as history.
//!
//! DRY-RUN by default: reports each record's title and current status and
//! refuses the run if any IRI is not an `accepted` record. Pass `--apply` to
//! retract, re-export `kg.nq` and validate. The daemon serving the data dir
//! MUST be stopped first.
//!
//! Usage:
//!   cargo run --release --example retract_batch -- <DATA_DIR> <LIST.tsv> [--apply] [--author NAME]

use std::path::{Path, PathBuf};

use chrono::Utc;
use moosedev::graph::{self, AppState};
use oxigraph::model::{NamedNodeRef, Term};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|arg| arg == "--apply");
    let author = args
        .iter()
        .position(|arg| arg == "--author")
        .and_then(|at| args.get(at + 1))
        .cloned()
        .unwrap_or_else(|| "claude-code".to_string());
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(at, arg)| !arg.starts_with("--") && (*at == 0 || args[at - 1] != "--author"))
        .map(|(_, arg)| arg)
        .collect();
    let [data_dir, list] = positional[..] else {
        anyhow::bail!("usage: retract_batch <DATA_DIR> <LIST.tsv> [--apply] [--author NAME]");
    };
    let data_dir = PathBuf::from(data_dir);

    let mut entries = Vec::new();
    for (number, line) in std::fs::read_to_string(list)?.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((iri, rationale)) = line.split_once('\t') else {
            anyhow::bail!("{list}:{}: expected `<IRI>\\t<rationale>`", number + 1);
        };
        entries.push((iri.trim().to_string(), rationale.trim().to_string()));
    }

    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&data_dir, &ontology_dir)?;
    println!(
        "data dir: {}  records: {}  mode: {}",
        data_dir.display(),
        entries.len(),
        if apply { "APPLY" } else { "DRY-RUN" }
    );

    let mut refused = 0;
    for (iri, _) in &entries {
        let status = literal(&state, iri, &state.capture.status);
        let title = literal(&state, iri, &state.capture.title).unwrap_or_default();
        let ok = status.as_deref() == Some("accepted");
        if !ok {
            refused += 1;
        }
        println!(
            "{} {iri}  [{}]  {title}",
            if ok { " " } else { "!" },
            status.as_deref().unwrap_or("no status")
        );
    }
    if refused > 0 {
        anyhow::bail!("{refused} record(s) are not accepted; fix the list (marked `!`)");
    }
    if !apply {
        println!("dry-run only; re-run with --apply");
        return Ok(());
    }

    let now = Utc::now();
    for (iri, rationale) in &entries {
        let outcome = graph::retract_decision(&state, iri, rationale, &author, now)?;
        println!("retracted {iri}  rationale {}", outcome.rationale_iri);
    }
    state.ensure_enriched();
    moosedev::canonical::write_through(&state.store, &data_dir)?;
    let report = moosedev::validation::validate_project(&state)?;
    println!("\n{}", moosedev::validation::format_report(&report));
    Ok(())
}

/// The record's first literal value for `predicate` in the project graph.
fn literal(state: &AppState, iri: &str, predicate: &str) -> Option<String> {
    let graph_name = NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).ok()?;
    state
        .store
        .quads_for_pattern(
            Some(NamedNodeRef::new(iri).ok()?.into()),
            Some(NamedNodeRef::new(predicate).ok()?),
            None,
            Some(graph_name.into()),
        )
        .flatten()
        .find_map(|quad| match quad.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
}
