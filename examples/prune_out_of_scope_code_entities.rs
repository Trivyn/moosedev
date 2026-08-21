//! Offline prune of minted CodeEntities that batch minting no longer manages.
//!
//! Narrowing the mint scope does not remove what a looser scope already minted:
//! those entities stay alive in the substrate, so they are not orphans, and they
//! are never planned, so they are not `unchanged`. `MintPlan::out_of_scope`
//! reports them; this binary is the only thing that deletes them.
//!
//!   cargo run --release --example prune_out_of_scope_code_entities -- [--verbose]
//!   cargo run --release --example prune_out_of_scope_code_entities -- --apply
//!
//! The daemon serving the store MUST be stopped for BOTH modes. Order per store:
//! dry-run, READ THE RETAINED BUCKETS, then --apply.
//!
//! Out-of-scope is necessary but NOT sufficient to delete. A lazily minted
//! private entity is permanently out of batch scope and may carry the only copy
//! of some recorded knowledge, so two further conjuncts must hold:
//!
//!   2. its asserted predicates are a subset of the shape minting itself writes
//!      — anything more means something was said about it;
//!   3. nothing references it, by IRI or by literal.
//!
//! Conjunct 2 is not redundant with 3. `link_code` orients edges through the
//! relation catalogue and frequently makes the CodeEntity the SUBJECT
//! (`isConcernedBy`, `playsRole`, `hasCriticality`), so a linked entity can have
//! zero INBOUND references — and the materialized inverse that would reveal it
//! is reasoner-derived, absent until `ensure_enriched` runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use moose::{RDFS_LABEL, RDF_TYPE};
use moosedev::code::substrate::Substrate;
use moosedev::graph::{AppState, CodeTerms, PROJECT_KG_GRAPH_IRI};
use moosedev::provenance::PROVENANCE_GRAPH_IRI;
use moosedev::{canonical, graph, runtime, validation};
use oxigraph::model::{
    GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Quad, Term, TermRef,
};

const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
/// Marks a literal as possibly embedding an entity IRI (e.g. a ProposedLink's
/// `proposesSubject`, which stores its subject as a plain string).
const ENTITY_IRI_MARKER: &str = "/kg/CodeEntity/";

struct Args {
    apply: bool,
    verbose: bool,
    data_dir: PathBuf,
    ontology_dir: PathBuf,
    repo_root: PathBuf,
}

/// One out-of-scope entity and why it may or may not be pruned.
struct Candidate {
    iri: String,
    symbol: String,
    name: Option<String>,
    path: Option<String>,
    /// Asserted predicates outside the mint shape.
    extra_predicates: BTreeSet<String>,
    /// (referring subject, predicate) references in the PROJECT graph. These
    /// block a prune: the project graph is the committed, exported,
    /// authoritative one.
    references: Vec<(String, String)>,
    /// (referring subject, predicate, graph) references from derived local
    /// graphs — MOOSE session traces above all. Reported, never blocking:
    /// those graphs are query history, are never exported to kg.nq, and the
    /// read side already skips nodes with no asserted type.
    derived_references: Vec<(String, String, String)>,
}

impl Candidate {
    fn prunable(&self) -> bool {
        self.extra_predicates.is_empty() && self.references.is_empty()
    }
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    println!(
        "store: {}  mode: {}",
        args.data_dir.join("kg").display(),
        if args.apply { "APPLY" } else { "DRY-RUN" }
    );
    ensure_daemon_stopped(&args.data_dir)?;

    let substrate = Substrate::load(&args.data_dir, &args.repo_root)
        .map_err(|e| anyhow::anyhow!("load code substrate: {e}\nrun `moosedev index` first"))?;
    let state = AppState::bootstrap(&args.data_dir, &args.ontology_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to open store at {}: {e}\n\
             A MOOSEDev backend likely holds the lock — stop it: kill $(cat {})",
            args.data_dir.join("kg").display(),
            runtime::pidfile_path_for(&args.data_dir).display()
        )
    })?;

    let terms = CodeTerms::resolve(&state)?;
    let components = graph::load_components(&state)?;
    let definitions = substrate.definitions();
    let plan = graph::plan_mint(&state, &definitions, &terms, &components, Some(&substrate))?;
    println!(
        "mint plan: create={} update={} unchanged={} orphaned={} out-of-scope={}",
        plan.create.len(),
        plan.update.len(),
        plan.unchanged,
        plan.orphaned.len(),
        plan.out_of_scope.len()
    );
    if plan.out_of_scope.is_empty() {
        println!("0 out-of-scope entities — nothing to prune (idempotent)");
        return Ok(());
    }

    let candidates = classify(&state, &terms, &plan.out_of_scope)?;
    report(&candidates, args.verbose);

    let prunable = candidates
        .iter()
        .filter(|candidate| candidate.prunable())
        .collect::<Vec<_>>();
    if prunable.is_empty() {
        println!("\nnothing is prunable; every out-of-scope entity is referenced or annotated");
        return Ok(());
    }

    let doomed = prunable
        .iter()
        .map(|candidate| candidate.iri.clone())
        .collect::<Vec<_>>();
    let quads = removable_quads(&state, &doomed)?;
    let (project, provenance) = quads
        .iter()
        .partition::<Vec<_>, _>(|quad| quad.graph_name.to_string().contains(PROJECT_KG_GRAPH_IRI));
    println!(
        "\nwould remove {} quad(s): project {} / provenance {}",
        quads.len(),
        project.len(),
        provenance.len()
    );

    if !args.apply {
        println!("\ndry-run only; review the RETAINED buckets above, then re-run with --apply");
        return Ok(());
    }

    let mut txn = state.store.start_transaction()?;
    for quad in &quads {
        txn.remove(quad.as_ref());
    }
    txn.commit()?;
    println!("removed {} quad(s) across {} entities", quads.len(), doomed.len());

    state.mark_inferred_stale();
    state.ensure_enriched();
    canonical::write_through(&state.store, &args.data_dir)?;

    let report = validation::validate_project(&state)?;
    println!("\n{}", validation::format_report(&report));
    if !report.conforms() {
        anyhow::bail!("post-prune validation failed");
    }
    Ok(())
}

/// Apply conjuncts 2 and 3 to every out-of-scope entity.
fn classify(
    state: &AppState,
    terms: &CodeTerms,
    out_of_scope: &[(String, String)],
) -> anyhow::Result<Vec<Candidate>> {
    let mint_shape = BTreeSet::from([
        RDF_TYPE.to_string(),
        RDFS_LABEL.to_string(),
        terms.has_substrate_symbol.clone(),
        terms.has_entity_kind.clone(),
        terms.has_code_name.clone(),
        terms.has_logical_path.clone(),
        terms.defined_in_path.clone(),
        terms.realizes.clone(),
    ]);
    let by_iri = out_of_scope
        .iter()
        .map(|(iri, symbol)| (iri.clone(), symbol.clone()))
        .collect::<BTreeMap<_, _>>();

    // Conjunct 3, in ONE pass over the store rather than a query per entity.
    let project_graph = format!("<{PROJECT_KG_GRAPH_IRI}>");
    let mut references: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut derived: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    let mut iri_bearing_literals: Vec<(String, String, String)> = Vec::new();
    for quad in state.store.quads_for_pattern(None, None, None, None) {
        let quad = quad?;
        match quad.object.as_ref() {
            TermRef::NamedNode(node) => {
                let iri = node.as_str();
                // A self-reference is impossible for the mint shape, but compare
                // by node rather than by rendered text so it stays impossible.
                let is_self = matches!(quad.subject.as_ref(), NamedOrBlankNodeRef::NamedNode(subject)
                    if subject.as_str() == iri);
                if by_iri.contains_key(iri) && !is_self {
                    let graph = quad.graph_name.to_string();
                    if graph == project_graph {
                        references
                            .entry(iri.to_string())
                            .or_default()
                            .push((quad.subject.to_string(), quad.predicate.to_string()));
                    } else {
                        derived.entry(iri.to_string()).or_default().push((
                            quad.subject.to_string(),
                            quad.predicate.to_string(),
                            graph,
                        ));
                    }
                }
            }
            TermRef::Literal(literal) if literal.value().contains(ENTITY_IRI_MARKER) => {
                iri_bearing_literals.push((
                    literal.value().to_string(),
                    quad.subject.to_string(),
                    quad.graph_name.to_string(),
                ));
            }
            _ => {}
        }
    }
    for (value, subject, graph) in &iri_bearing_literals {
        for iri in by_iri.keys() {
            if !value.contains(iri.as_str()) {
                continue;
            }
            // Graph-scoped exactly like an IRI reference. A literal in the
            // project graph is unshaped and invisible to SHACL — a ProposedLink
            // names its subject this way — so it blocks. The same string inside
            // a session trace is the recorded TEXT of a past answer, not an
            // assertion about the entity, and blocking on it would make every
            // entity that any query ever mentioned permanently unprunable.
            if *graph == project_graph {
                references
                    .entry(iri.clone())
                    .or_default()
                    .push((subject.clone(), "(literal)".to_string()));
            } else {
                derived.entry(iri.clone()).or_default().push((
                    subject.clone(),
                    "(literal)".to_string(),
                    graph.clone(),
                ));
            }
        }
    }

    let project = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let mut candidates = Vec::new();
    for (iri, symbol) in by_iri {
        let subject = NamedNode::new(&iri)?;
        let mut extra_predicates = BTreeSet::new();
        let (mut name, mut path) = (None, None);
        for quad in state.store.quads_for_pattern(
            Some(subject.as_ref().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(project)),
        ) {
            let quad = quad?;
            let predicate = quad.predicate.as_str().to_string();
            if predicate == terms.has_code_name {
                name = literal_value(&quad.object);
            } else if predicate == terms.defined_in_path {
                path = literal_value(&quad.object);
            }
            if !mint_shape.contains(&predicate) {
                extra_predicates.insert(predicate);
            }
        }
        let references = references.remove(&iri).unwrap_or_default();
        let derived_references = derived.remove(&iri).unwrap_or_default();
        candidates.push(Candidate {
            iri,
            symbol,
            name,
            path,
            extra_predicates,
            references,
            derived_references,
        });
    }
    Ok(candidates)
}

/// Every quad to remove: the entity's own assertions in both graphs, plus the
/// PROV activity minted for it. The `prov:Agent` is deterministic and shared by
/// every edit, so it is never removed.
fn removable_quads(state: &AppState, doomed: &[String]) -> anyhow::Result<Vec<Quad>> {
    let project = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let provenance = NamedNodeRef::new(PROVENANCE_GRAPH_IRI)?;
    let generated_by = NamedNodeRef::new(PROV_WAS_GENERATED_BY)?;
    let mut out = Vec::new();
    for iri in doomed {
        let subject = NamedNode::new(iri)?;
        for graph in [project, provenance] {
            for quad in state.store.quads_for_pattern(
                Some(subject.as_ref().into()),
                None,
                None,
                Some(GraphNameRef::NamedNode(graph)),
            ) {
                out.push(quad?);
            }
        }
        for quad in state.store.quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(generated_by),
            None,
            Some(GraphNameRef::NamedNode(provenance)),
        ) {
            let quad = quad?;
            let Term::NamedNode(activity) = quad.object else {
                continue;
            };
            for quad in state.store.quads_for_pattern(
                Some(activity.as_ref().into()),
                None,
                None,
                Some(GraphNameRef::NamedNode(provenance)),
            ) {
                out.push(quad?);
            }
        }
    }
    Ok(out)
}

fn report(candidates: &[Candidate], verbose: bool) {
    let (prunable, retained): (Vec<_>, Vec<_>) =
        candidates.iter().partition(|candidate| candidate.prunable());

    println!("\nPRUNE — out of scope, unannotated, unreferenced: {}", prunable.len());
    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for candidate in &prunable {
        *per_file
            .entry(candidate.path.as_deref().unwrap_or("(no path)"))
            .or_default() += 1;
    }
    for (path, count) in &per_file {
        println!("  {path}: {count}");
    }
    if verbose {
        for candidate in &prunable {
            println!("    {} {}", candidate.symbol, candidate.iri);
        }
    }

    // Both retained buckets print in full, always. They are the channel through
    // which the store says something the scope rule did not predict.
    let annotated = retained
        .iter()
        .filter(|candidate| !candidate.extra_predicates.is_empty())
        .collect::<Vec<_>>();
    println!("\nRETAINED — something is asserted about them: {}", annotated.len());
    for candidate in &annotated {
        println!(
            "  {} ({})\n    extra: {}",
            candidate.name.as_deref().unwrap_or(&candidate.symbol),
            candidate.iri,
            candidate
                .extra_predicates
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let referenced = retained
        .iter()
        .filter(|candidate| candidate.extra_predicates.is_empty())
        .collect::<Vec<_>>();
    println!("\nRETAINED — referenced elsewhere: {}", referenced.len());
    for candidate in &referenced {
        println!(
            "  {} ({})",
            candidate.name.as_deref().unwrap_or(&candidate.symbol),
            candidate.iri
        );
        for (subject, predicate) in &candidate.references {
            println!("    <- {subject} {predicate}");
        }
    }

    // Informational: derived graphs never block, but a prune that silently
    // strands references is a prune that lied about its blast radius.
    let derived = prunable
        .iter()
        .filter(|candidate| !candidate.derived_references.is_empty())
        .collect::<Vec<_>>();
    let rows: usize = derived
        .iter()
        .map(|candidate| candidate.derived_references.len())
        .sum();
    println!(
        "\nNOTE — {} pruned entit{} {} referenced by {} row(s) in derived local graphs \
         (MOOSE session traces). Those graphs are query history and are never exported to \
         kg.nq, so the references are dropped rather than repaired.",
        derived.len(),
        if derived.len() == 1 { "y" } else { "ies" },
        if derived.len() == 1 { "is" } else { "are" },
        rows
    );
    if verbose {
        for candidate in &derived {
            for (subject, predicate, graph) in &candidate.derived_references {
                println!("    {} <- {subject} {predicate} in {graph}", candidate.iri);
            }
        }
    }
}

fn literal_value(term: &Term) -> Option<String> {
    match term {
        Term::Literal(literal) => Some(literal.value().to_string()),
        _ => None,
    }
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
    let (mut apply, mut verbose) = (false, false);
    let (mut data_dir, mut ontology_dir, mut repo_root) = (None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
                .map(PathBuf::from)
        };
        match arg.as_str() {
            "--apply" => apply = true,
            "--verbose" => verbose = true,
            "--data-dir" => data_dir = Some(value("--data-dir")?),
            "--ontology-dir" => ontology_dir = Some(value("--ontology-dir")?),
            "--repo-root" => repo_root = Some(value("--repo-root")?),
            other => anyhow::bail!(
                "unknown argument {other:?}; expected --apply, --verbose, \
                 --data-dir PATH, --ontology-dir PATH, --repo-root PATH"
            ),
        }
    }
    Ok(Args {
        apply,
        verbose,
        data_dir: data_dir
            .or_else(|| std::env::var_os("MOOSEDEV_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(".moosedev")),
        ontology_dir: ontology_dir
            .or_else(|| std::env::var_os("MOOSEDEV_ONTOLOGY_DIR").map(PathBuf::from))
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")),
        repo_root: repo_root.unwrap_or(std::env::current_dir()?),
    })
}
