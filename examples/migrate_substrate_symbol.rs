//! Offline identity-property migration: `code:hasScipSymbol` → `code:hasSubstrateSymbol`.
//!
//! Slice-4 Phase 0 renamed the CodeEntity identity property because the
//! tree-sitter fallback introduces non-SCIP (`ts:`) identities and the property
//! name must not lie about its value space. This one-off rewrites the predicate
//! on every entity quad in the project graph. Dry-run by default:
//!
//!   cargo run --release --example migrate_substrate_symbol -- [--data-dir PATH]
//!   cargo run --release --example migrate_substrate_symbol -- --apply [--data-dir PATH]
//!
//! The daemon serving the store MUST be stopped for BOTH modes. Order per
//! store: dry-run, review, --apply. Idempotent: a migrated store matches zero
//! quads. Instance vectors are untouched (predicate IRIs are not embedded).

use std::path::{Path, PathBuf};

use moosedev::graph::{open_store, AppState, PROJECT_KG_GRAPH_IRI};
use moosedev::{canonical, runtime, validation};
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Quad};
use oxigraph::store::Store;

/// The only ontology term IRIs this binary may hardcode: they ARE the
/// migration data (Constraint 19bb4d8a exemption, per migrate_namespace).
const OLD_PREDICATE: &str = "https://trivyn.io/ontologies/software/code#hasScipSymbol";
const NEW_PREDICATE: &str = "https://trivyn.io/ontologies/software/code#hasSubstrateSymbol";

#[derive(Debug)]
struct Args {
    apply: bool,
    data_dir: PathBuf,
    ontology_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    println!(
        "store: {}  mode: {}",
        args.data_dir.join("kg").display(),
        if args.apply { "APPLY" } else { "DRY-RUN" }
    );

    ensure_daemon_stopped(&args.data_dir)?;

    // Stage 1: raw store, dropped before stage 2 reopens via bootstrap.
    {
        let kg_path = args.data_dir.join("kg");
        let store = if args.apply {
            open_store(&args.data_dir)
        } else {
            Store::open_read_only(&kg_path).map_err(Into::into)
        }
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to open store at {}: {e}\n\
                 A MOOSEDev backend likely holds the lock — stop it: kill $(cat {})",
                kg_path.display(),
                runtime::pidfile_path_for(&args.data_dir).display()
            )
        })?;

        let quads = matching_quads(&store)?;
        println!("{} quad(s) carry {OLD_PREDICATE}", quads.len());
        for quad in quads.iter().take(3) {
            println!("  sample subject: {}", quad.subject);
        }
        if quads.is_empty() {
            println!("0 quads matched — store already migrated (idempotent)");
            return Ok(());
        }
        if !args.apply {
            println!("\ndry-run only; re-run with --apply");
            return Ok(());
        }

        let new_predicate = NamedNode::new(NEW_PREDICATE)?;
        let mut txn = store.start_transaction()?;
        for old in &quads {
            let new = Quad::new(
                old.subject.clone(),
                new_predicate.clone(),
                old.object.clone(),
                old.graph_name.clone(),
            );
            txn.remove(old.as_ref());
            txn.insert(new.as_ref());
        }
        txn.commit()?;
        println!("rewrote {} quad(s)", quads.len());
        canonical::write_through(&store, &args.data_dir)?;
    }

    // Stage 2: normal bootstrap — new TTLs load; validation proves the shapes
    // (which now require hasSubstrateSymbol) hold over the migrated entities.
    let state = AppState::bootstrap(&args.data_dir, &args.ontology_dir)?;
    state.ensure_enriched();
    let report = validation::validate_project(&state)?;
    println!("\n{}", validation::format_report(&report));
    if !report.conforms() {
        anyhow::bail!("post-migration validation failed");
    }
    Ok(())
}

fn matching_quads(store: &Store) -> anyhow::Result<Vec<Quad>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(OLD_PREDICATE)?;
    let mut out = Vec::new();
    for quad in store.quads_for_pattern(
        None,
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        out.push(quad?);
    }
    Ok(out)
}

fn ensure_daemon_stopped(data_dir: &Path) -> anyhow::Result<()> {
    let socket = runtime::socket_path_for(data_dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        anyhow::bail!(
            "a MOOSEDev daemon is serving this store ({}) — stop it first: kill $(cat {})",
            socket.display(),
            runtime::pidfile_path_for(data_dir).display()
        );
    }
    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut apply = false;
    let mut data_dir = None;
    let mut ontology_dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--data-dir" => {
                data_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--data-dir requires a value")
                    })?));
            }
            "--ontology-dir" => {
                ontology_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--ontology-dir requires a value")
                    })?));
            }
            other => anyhow::bail!(
                "unknown argument {other:?}; expected --apply, --data-dir PATH, --ontology-dir PATH"
            ),
        }
    }
    let data_dir = data_dir
        .or_else(|| std::env::var_os("MOOSEDEV_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(".moosedev"));
    let ontology_dir = ontology_dir
        .or_else(|| std::env::var_os("MOOSEDEV_ONTOLOGY_DIR").map(PathBuf::from))
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"));
    Ok(Args {
        apply,
        data_dir,
        ontology_dir,
    })
}
