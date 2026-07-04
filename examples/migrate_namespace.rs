//! Offline ontology-namespace migration for MOOSEDev stores.
//!
//! Rewrites instance-data IRIs after the TTL namespace rename
//! (`…/software/architecture/domain/` → `…/software/architecture#`, engineering
//! likewise). Dry-run by default:
//!
//!   cargo run --release --example migrate_namespace -- [--data-dir PATH]
//!   cargo run --release --example migrate_namespace -- --apply [--data-dir PATH]
//!
//! The daemon serving the store MUST be stopped for BOTH modes — a concurrent
//! read-only open of a RocksDB store with a live writer is undefined behavior.
//! Order per store: dry-run, review, --apply. After --apply the first daemon
//! boot re-embeds instance-vectors.db and rebuilds ontology-vectors.db
//! (fingerprint flip), so expect one slower start. moose_sessions.db may hold
//! stale class IRIs in chat snapshots; those expire with the 7-day session TTL.

use std::path::{Path, PathBuf};

use moosedev::graph::{open_store, AppState};
use moosedev::{canonical, export, ontology, runtime, validation};
use oxigraph::model::{GraphNameRef, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use oxigraph::store::Store;
use sha2::{Digest, Sha256};

/// (old_prefix, new_prefix) — the only ontology term IRIs this binary may
/// hardcode: they ARE the migration data. Local names are unchanged.
const MAPPINGS: &[(&str, &str)] = &[
    (
        "https://trivyn.io/ontologies/software/architecture/domain/",
        "https://trivyn.io/ontologies/software/architecture#",
    ),
    (
        "https://trivyn.io/ontologies/software/engineering/domain/",
        "https://trivyn.io/ontologies/software/engineering#",
    ),
];

/// Graphs wholesale-reloaded from TTLs at every boot — pointless to rewrite.
fn excluded_graphs() -> [&'static str; 5] {
    [
        ontology::SE_DOMAIN_GRAPH_IRI,
        ontology::SE_SHAPES_GRAPH_IRI,
        ontology::ARCH_DOMAIN_GRAPH_IRI,
        ontology::ARCH_SHAPES_GRAPH_IRI,
        moose::MOOSE_ONTOLOGY_GRAPH,
    ]
}

fn is_excluded_graph(iri: &str) -> bool {
    excluded_graphs().contains(&iri)
}

#[derive(Debug)]
struct Args {
    apply: bool,
    data_dir: PathBuf,
    ontology_dir: PathBuf,
}

struct GraphChanges {
    graph_label: String,
    changes: Vec<(Quad, Quad)>,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    println!(
        "store: {}  mode: {}",
        args.data_dir.join("kg").display(),
        if args.apply {
            "APPLY (exclusive)"
        } else {
            "DRY-RUN (read-only)"
        }
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

        let action = preflight_canonical(&store, &args.data_dir)?;
        println!("canonical preflight: {action:?}");
        if matches!(
            action,
            canonical::StartupAction::HydrateReplace | canonical::StartupAction::MergeDiverged
        ) {
            if args.apply {
                anyhow::bail!(
                    "kg.nq and the store have diverged ({action:?}); start the daemon once to \
                     reconcile, stop it, then re-run"
                );
            }
            println!("WARNING: --apply will refuse until the daemon reconciles kg.nq");
        }

        let plan = plan_sweep(&store)?;
        let total: usize = plan.iter().map(|g| g.changes.len()).sum();
        for g in &plan {
            println!("{}: {} quads would change", g.graph_label, g.changes.len());
        }
        for (old, new) in plan
            .iter()
            .flat_map(|g| g.changes.iter())
            .filter_map(|(o, n)| sample_iri_pair(o, n))
            .take(5)
        {
            println!("  sample: {old} -> {new}");
        }
        println!("total: {total}");

        if total == 0 {
            println!("0 quads matched — store already migrated (idempotent)");
            return Ok(());
        }
        if !args.apply {
            println!("\ndry-run only; re-run with --apply");
            return Ok(());
        }

        let applied = apply_sweep(&store, &plan)?;
        println!("rewrote {applied} quads");
        canonical::write_through(&store, &args.data_dir)?;
        delete_instance_vectors(&args.data_dir);
    }

    // Stage 2: normal bootstrap — canonical sync (should be Nothing/StampOnly),
    // new TTLs load, enrichment re-materializes under the new namespace.
    let state = AppState::bootstrap(&args.data_dir, &args.ontology_dir)?;
    state.ensure_enriched();
    let report = validation::validate_project(&state)?;
    println!("\n{}", validation::format_report(&report));
    if !report.conforms() {
        anyhow::bail!("post-migration validation failed");
    }

    let residual = count_old_iris_all_graphs(&state.store)?;
    if residual > 0 {
        anyhow::bail!("residual scan found {residual} old-namespace IRIs — migration incomplete");
    }
    println!("residual scan: 0 old-namespace IRIs — migration complete");
    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut apply = false;
    let mut data_dir: Option<PathBuf> = None;
    let mut ontology_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--data-dir requires a value"))?,
                ));
            }
            "--ontology-dir" => {
                ontology_dir = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--ontology-dir requires a value"))?,
                ));
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

/// Both modes require the daemon down: RocksDB read-only open concurrent with
/// a live writer is documented undefined behavior.
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

// --- pure rewrite core -------------------------------------------------------

fn rewrite_iri(iri: &str) -> Option<String> {
    for (old, new) in MAPPINGS {
        if let Some(rest) = iri.strip_prefix(old) {
            return Some(format!("{new}{rest}"));
        }
    }
    None
}

fn rewrite_named_node(node: &NamedNode) -> Option<NamedNode> {
    rewrite_iri(node.as_str()).map(NamedNode::new_unchecked)
}

fn rewrite_subject(subject: &NamedOrBlankNode) -> Option<NamedOrBlankNode> {
    match subject {
        NamedOrBlankNode::NamedNode(n) => rewrite_named_node(n).map(NamedOrBlankNode::NamedNode),
        NamedOrBlankNode::BlankNode(_) => None,
    }
}

/// Named nodes are rewritten; literals and blank nodes never are. RDF 1.2
/// quoted-triple terms (provenance reification tags) are rewritten RECURSIVELY —
/// leaving them stale would orphan reasoner-inference tags, and the untagged
/// materializations would then leak into kg.nq as asserted knowledge.
fn rewrite_term(term: &Term) -> Option<Term> {
    match term {
        Term::NamedNode(n) => rewrite_named_node(n).map(Term::NamedNode),
        Term::Triple(t) => {
            let subject = rewrite_subject(&t.subject);
            let predicate = rewrite_named_node(&t.predicate);
            let object = rewrite_term(&t.object);
            if subject.is_none() && predicate.is_none() && object.is_none() {
                return None;
            }
            Some(Term::Triple(Box::new(Triple::new(
                subject.unwrap_or_else(|| t.subject.clone()),
                predicate.unwrap_or_else(|| t.predicate.clone()),
                object.unwrap_or_else(|| t.object.clone()),
            ))))
        }
        _ => None,
    }
}

/// `None` = quad unchanged. The graph name is never rewritten (verified: all
/// graph names are moosedev.dev/… or the moose pipeline ontology).
fn rewrite_quad(q: &Quad) -> Option<Quad> {
    let subject = rewrite_subject(&q.subject);
    let predicate = rewrite_named_node(&q.predicate);
    let object = rewrite_term(&q.object);
    if subject.is_none() && predicate.is_none() && object.is_none() {
        return None;
    }
    Some(Quad::new(
        subject.unwrap_or_else(|| q.subject.clone()),
        predicate.unwrap_or_else(|| q.predicate.clone()),
        object.unwrap_or_else(|| q.object.clone()),
        q.graph_name.clone(),
    ))
}

// --- sweep -------------------------------------------------------------------

fn plan_sweep(store: &Store) -> anyhow::Result<Vec<GraphChanges>> {
    let mut out = Vec::new();
    let mut graph_names: Vec<String> = Vec::new();
    for g in store.named_graphs() {
        let g = g?;
        let NamedOrBlankNode::NamedNode(name) = g else {
            continue;
        };
        let iri = name.as_str().to_string();
        if rewrite_iri(&iri).is_some() {
            anyhow::bail!("graph NAME {iri} matches an old prefix — unexpected, aborting");
        }
        if !is_excluded_graph(&iri) {
            graph_names.push(iri);
        }
    }
    graph_names.sort();

    for iri in graph_names {
        let graph = NamedNode::new(&iri)?;
        let changes = collect_changes(
            store,
            GraphNameRef::NamedNode(graph.as_ref()),
        )?;
        if !changes.is_empty() {
            out.push(GraphChanges {
                graph_label: iri,
                changes,
            });
        }
    }
    let default_changes = collect_changes(store, GraphNameRef::DefaultGraph)?;
    if !default_changes.is_empty() {
        out.push(GraphChanges {
            graph_label: "(default graph)".to_string(),
            changes: default_changes,
        });
    }
    Ok(out)
}

fn collect_changes(store: &Store, graph: GraphNameRef<'_>) -> anyhow::Result<Vec<(Quad, Quad)>> {
    let mut changes = Vec::new();
    for q in store.quads_for_pattern(None, None, None, Some(graph)) {
        let q = q?;
        if let Some(new) = rewrite_quad(&q) {
            changes.push((q, new));
        }
    }
    Ok(changes)
}

fn apply_sweep(store: &Store, plan: &[GraphChanges]) -> anyhow::Result<usize> {
    let mut total = 0;
    for g in plan {
        let mut txn = store.start_transaction()?;
        for (old, new) in &g.changes {
            txn.remove(old.as_ref());
            txn.insert(new.as_ref());
        }
        txn.commit()?;
        total += g.changes.len();
    }
    store.flush()?;
    Ok(total)
}

fn sample_iri_pair(old: &Quad, new: &Quad) -> Option<(String, String)> {
    if old.predicate != new.predicate {
        return Some((
            old.predicate.as_str().to_string(),
            new.predicate.as_str().to_string(),
        ));
    }
    if let (Term::NamedNode(o), Term::NamedNode(n)) = (&old.object, &new.object) {
        if o != n {
            return Some((o.as_str().to_string(), n.as_str().to_string()));
        }
    }
    None
}

/// Residual assertion after stage 2: no old-prefix IRI anywhere, ontology
/// graphs included (they now hold the reloaded new-namespace TTL content).
fn count_old_iris_all_graphs(store: &Store) -> anyhow::Result<usize> {
    let mut count = 0;
    for q in store.quads_for_pattern(None, None, None, None) {
        if rewrite_quad(&q?).is_some() {
            count += 1;
        }
    }
    Ok(count)
}

// --- canonical pre-flight ----------------------------------------------------

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Refuse to mutate a store whose canonical text has diverged: stage 2's
/// startup sync would otherwise hydrate OLD-namespace text back over the
/// rewritten store.
fn preflight_canonical(
    store: &Store,
    data_dir: &Path,
) -> anyhow::Result<canonical::StartupAction> {
    let dump = export::export_canonical_project(store)?;
    let store_hash = sha256_hex(&dump.text);
    let text = match std::fs::read_to_string(canonical::canonical_path(data_dir)) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => anyhow::bail!("read kg.nq: {e}"),
    };
    let text_hash = text.as_deref().map(sha256_hex);
    let stamp = std::fs::read_to_string(canonical::stamp_path(data_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(canonical::decide(
        text_hash.as_deref(),
        &store_hash,
        dump.quad_count == 0,
        stamp.as_deref(),
    ))
}

fn delete_instance_vectors(data_dir: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let path = data_dir.join(format!("instance-vectors.db{suffix}"));
        match std::fs::remove_file(&path) {
            Ok(()) => println!("deleted {} (rebuilds on next daemon boot)", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => println!("warning: could not delete {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moosedev::graph::PROJECT_KG_GRAPH_IRI;
    use oxigraph::model::{GraphName, Literal};

    const OLD_ARCH: &str = "https://trivyn.io/ontologies/software/architecture/domain/";
    const NEW_ARCH: &str = "https://trivyn.io/ontologies/software/architecture#";

    #[test]
    fn rewrite_iri_maps_both_prefixes_and_preserves_local_name() {
        assert_eq!(
            rewrite_iri(&format!("{OLD_ARCH}ArchitecturalDecision")).as_deref(),
            Some(format!("{NEW_ARCH}ArchitecturalDecision").as_str())
        );
        assert_eq!(
            rewrite_iri("https://trivyn.io/ontologies/software/engineering/domain/Component")
                .as_deref(),
            Some("https://trivyn.io/ontologies/software/engineering#Component")
        );
        assert!(rewrite_iri("https://moosedev.dev/kg/Lesson/abc").is_none());
        assert!(rewrite_iri("https://trivyn.io/ontologies/software/architecture/mapping/x").is_none());
        // Idempotency at the pure level: already-migrated IRIs never match.
        assert!(rewrite_iri(&format!("{NEW_ARCH}Constraint")).is_none());
    }

    #[test]
    fn mappings_table_is_well_formed() {
        for (old, new) in MAPPINGS {
            assert!(NamedNode::new(format!("{new}X")).is_ok());
            for (other_old, _) in MAPPINGS {
                assert!(
                    !new.starts_with(other_old),
                    "rewrite could cascade: {new} starts with {other_old}"
                );
            }
            assert!(!old.starts_with('#'));
        }
    }

    #[test]
    fn rewrite_quad_touches_only_named_nodes() {
        let g = GraphName::NamedNode(NamedNode::new_unchecked(PROJECT_KG_GRAPH_IRI));
        let s = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
            "https://moosedev.dev/kg/Lesson/abc",
        ));
        let typed = Quad::new(
            s.clone(),
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            Term::NamedNode(NamedNode::new_unchecked(format!("{OLD_ARCH}Lesson"))),
            g.clone(),
        );
        let rewritten = rewrite_quad(&typed).expect("type object should rewrite");
        assert_eq!(
            rewritten.object,
            Term::NamedNode(NamedNode::new_unchecked(format!("{NEW_ARCH}Lesson")))
        );
        assert_eq!(rewritten.subject, s);

        // A literal whose text mentions the old prefix is prose — untouched.
        let literal = Quad::new(
            s,
            NamedNode::new_unchecked(format!("{OLD_ARCH}hasDescription")),
            Term::Literal(Literal::new_simple_literal(format!(
                "see {OLD_ARCH}Lesson for details"
            ))),
            g,
        );
        let rewritten = rewrite_quad(&literal).expect("predicate should rewrite");
        assert!(matches!(&rewritten.object, Term::Literal(l)
            if l.value().contains(OLD_ARCH)));
    }

    #[test]
    fn rewrite_term_recurses_into_quoted_triples() {
        // Mirrors the provenance reification shape (src/provenance/mod.rs:266-277).
        let inner = Triple::new(
            NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
                "https://moosedev.dev/kg/ArchitecturalDecision/x",
            )),
            NamedNode::new_unchecked(format!("{OLD_ARCH}isMotivatedBy")),
            Term::NamedNode(NamedNode::new_unchecked(
                "https://moosedev.dev/kg/Requirement/y",
            )),
        );
        let term = Term::Triple(Box::new(inner));
        let rewritten = rewrite_term(&term).expect("embedded predicate should rewrite");
        let Term::Triple(t) = rewritten else {
            panic!("expected quoted triple");
        };
        assert_eq!(t.predicate.as_str(), format!("{NEW_ARCH}isMotivatedBy"));
    }

    #[test]
    fn graph_exclusion_covers_ontology_and_moose_graphs() {
        for g in excluded_graphs() {
            assert!(is_excluded_graph(g));
        }
        assert!(!is_excluded_graph(PROJECT_KG_GRAPH_IRI));
        assert!(!is_excluded_graph(moosedev::provenance::PROVENANCE_GRAPH_IRI));
    }

    #[test]
    fn sweep_on_memory_store_rewrites_and_is_idempotent() {
        let store = Store::new().unwrap();
        let project = GraphName::NamedNode(NamedNode::new_unchecked(PROJECT_KG_GRAPH_IRI));
        let provenance = GraphName::NamedNode(NamedNode::new_unchecked(
            moosedev::provenance::PROVENANCE_GRAPH_IRI,
        ));
        let ontology_graph =
            GraphName::NamedNode(NamedNode::new_unchecked(ontology::ARCH_DOMAIN_GRAPH_IRI));
        let record = NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
            "https://moosedev.dev/kg/Lesson/abc",
        ));
        let rdf_type =
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");

        store
            .insert(
                Quad::new(
                    record.clone(),
                    rdf_type.clone(),
                    Term::NamedNode(NamedNode::new_unchecked(format!("{OLD_ARCH}Lesson"))),
                    project.clone(),
                )
                .as_ref(),
            )
            .unwrap();
        store
            .insert(
                Quad::new(
                    NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
                        "https://moosedev.dev/kg/Reifier/r1",
                    )),
                    NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies"),
                    Term::Triple(Box::new(Triple::new(
                        record.clone(),
                        NamedNode::new_unchecked(format!("{OLD_ARCH}learnedFrom")),
                        Term::NamedNode(NamedNode::new_unchecked(
                            "https://moosedev.dev/kg/ArchitecturalDecision/z",
                        )),
                    ))),
                    provenance.clone(),
                )
                .as_ref(),
            )
            .unwrap();
        store
            .insert(
                Quad::new(
                    NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
                        "{OLD_ARCH}Lesson"
                    ))),
                    rdf_type,
                    Term::NamedNode(NamedNode::new_unchecked(
                        "http://www.w3.org/2002/07/owl#Class",
                    )),
                    ontology_graph,
                )
                .as_ref(),
            )
            .unwrap();

        let plan = plan_sweep(&store).unwrap();
        let total: usize = plan.iter().map(|g| g.changes.len()).sum();
        assert_eq!(total, 2, "project type quad + provenance reified quad");
        apply_sweep(&store, &plan).unwrap();

        // Project graph rewritten.
        let project_quads: Vec<_> = store
            .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(
                NamedNode::new_unchecked(PROJECT_KG_GRAPH_IRI).as_ref(),
            )))
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(project_quads.iter().all(|q| rewrite_quad(q).is_none()));

        // Provenance quoted triple rewritten.
        let prov_quads: Vec<_> = store
            .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(
                NamedNode::new_unchecked(moosedev::provenance::PROVENANCE_GRAPH_IRI).as_ref(),
            )))
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(prov_quads.iter().all(|q| rewrite_quad(q).is_none()));

        // Excluded ontology graph untouched.
        let ont_quads: Vec<_> = store
            .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(
                NamedNode::new_unchecked(ontology::ARCH_DOMAIN_GRAPH_IRI).as_ref(),
            )))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ont_quads.len(), 1);
        assert!(rewrite_quad(&ont_quads[0]).is_some(), "still old-namespace");

        // Idempotent: a second sweep proposes nothing.
        let second = plan_sweep(&store).unwrap();
        assert_eq!(second.iter().map(|g| g.changes.len()).sum::<usize>(), 0);
    }

    #[test]
    fn plan_sweep_does_not_mutate() {
        let store = Store::new().unwrap();
        let project = GraphName::NamedNode(NamedNode::new_unchecked(PROJECT_KG_GRAPH_IRI));
        store
            .insert(
                Quad::new(
                    NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
                        "https://moosedev.dev/kg/Lesson/abc",
                    )),
                    NamedNode::new_unchecked(format!("{OLD_ARCH}hasTitle")),
                    Term::Literal(Literal::new_simple_literal("t")),
                    project,
                )
                .as_ref(),
            )
            .unwrap();
        let before: Vec<_> = store
            .quads_for_pattern(None, None, None, None)
            .collect::<Result<_, _>>()
            .unwrap();
        let _ = plan_sweep(&store).unwrap();
        let after: Vec<_> = store
            .quads_for_pattern(None, None, None, None)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(before, after);
    }
}
