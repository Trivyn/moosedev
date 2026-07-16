//! Citation-seed migration (v2.1): upgrade prose code references in v1 records
//! into *proposed* record→CodeEntity links, queued for the ratification inbox.
//!
//! Every candidate token is accounted for — proposed or explained (zero silent
//! drops). Dry-run by default; `--apply` enqueues `ProposedLink`s at status
//! `proposed`. Per the migration discipline (skills/temporal-episode-capture.md),
//! we resolve the SYMBOL at HEAD and never trust historical line numbers.
//!
//!   cargo run --release --example seed_citations --
//!   cargo run --release --example seed_citations -- --apply --author James
//!
//! Requires a fresh substrate (`moosedev index`) and a stopped daemon (the mint
//! store lock): `kill $(cat .moosedev/moosedev-serve.pid)`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use moosedev::code::substrate::{DefinitionEntry, Substrate};
use moosedev::graph::{self, AppState, PROJECT_KG_GRAPH_IRI};
use moosedev::{canonical, runtime};
use oxigraph::model::{GraphNameRef, NamedNodeRef, NamedOrBlankNode, Term};

struct Args {
    apply: bool,
    author: String,
    data_dir: PathBuf,
    ontology_dir: PathBuf,
    repo_root: PathBuf,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut apply = false;
    let mut author = "seed_citations".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--author" => {
                author = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--author requires a value"))?;
            }
            other => anyhow::bail!("unknown argument {other:?}; expected --apply or --author NAME"),
        }
    }
    let data_dir = std::env::var_os("MOOSEDEV_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".moosedev"));
    let ontology_dir = std::env::var_os("MOOSEDEV_ONTOLOGY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"));
    let repo_root = std::env::current_dir()?;
    Ok(Args {
        apply,
        author,
        data_dir,
        ontology_dir,
        repo_root,
    })
}

struct Record {
    iri: String,
    title: String,
    description: String,
}

/// Substrate definitions indexed for name/qualified-name lookup.
struct SymbolIndex {
    by_name: BTreeMap<String, Vec<DefinitionEntry>>,
    by_qualified: BTreeMap<String, DefinitionEntry>,
}

enum Resolution {
    /// A confident match: propose a link to this entity.
    Entity {
        symbol: String,
        path: String,
        name: String,
    },
    /// Accounted for, but not proposed — with the reason.
    Skip(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let state = match runtime::build_state(&args.data_dir, &args.ontology_dir).await {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "failed to open MOOSEDev state at {}: {e}\n\
                 Stop the daemon first: kill $(cat {}/moosedev-serve.pid)",
                args.data_dir.display(),
                args.data_dir.display()
            );
            return Err(e);
        }
    };
    state.load_substrate(&args.repo_root);
    let substrate = state
        .substrate()
        .context("code substrate is not loaded; run `moosedev index` first")?;
    let index = build_symbol_index(&substrate);
    let records = read_records(&state)?;

    let mut proposals: Vec<(String, String, String, String, String)> = Vec::new(); // subj, symbol, path, token, name
    let mut skips: Vec<(String, String, String)> = Vec::new(); // subj, token, reason
    let mut seed_records = 0usize;

    for record in &records {
        let tokens = extract_code_tokens(&format!("{} {}", record.title, record.description));
        if tokens.is_empty() {
            continue;
        }
        seed_records += 1;
        let mut targeted = BTreeSet::new();
        for token in tokens {
            match resolve_token(&token, &index) {
                Resolution::Entity { symbol, path, name } => {
                    // Dedupe multiple citations of the same entity within one record.
                    if targeted.insert(symbol.clone()) {
                        proposals.push((record.iri.clone(), symbol, path, token, name));
                    }
                }
                Resolution::Skip(reason) => skips.push((record.iri.clone(), token, reason)),
            }
        }
    }

    for (subj, _symbol, path, token, name) in &proposals {
        println!("PROPOSE  {subj}\n         --concerns--> {name}  ({path})  [cited as `{token}`]");
    }
    if !skips.is_empty() {
        println!("\nSKIPPED (accounted for, not proposed):");
        for (subj, token, reason) in &skips {
            println!("  `{token}` in {subj}: {reason}");
        }
    }
    println!(
        "\ncounts: records={} seed-records={} proposals={} skipped-candidates={}",
        records.len(),
        seed_records,
        proposals.len(),
        skips.len()
    );

    if !args.apply {
        println!("\ndry-run only; re-run with --apply to enqueue proposals for ratification");
        return Ok(());
    }

    let now = Utc::now();
    let mut enqueued = 0usize;
    for (subj, symbol, path, token, _name) in &proposals {
        match graph::propose_link(
            &state,
            subj,
            "concerns",
            symbol,
            path,
            &format!("cited in prose as `{token}`"),
            &args.author,
            now,
        ) {
            Ok(_) => enqueued += 1,
            Err(e) => eprintln!("  propose failed for {subj} -> {symbol}: {e}"),
        }
    }
    canonical::write_through(&state.store, &args.data_dir)?;
    println!(
        "\nenqueued {enqueued} proposals at status=proposed — ratify them in the workbench inbox"
    );
    Ok(())
}

/// Read the knowledge records whose prose may cite code (title + description).
fn read_records(state: &AppState) -> anyhow::Result<Vec<Record>> {
    const KINDS: &[&str] = &[
        "ArchitecturalDecision",
        "Constraint",
        "Lesson",
        "Requirement",
        "Pattern",
        "AntiPattern",
    ];
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for kind in KINDS {
        let Ok(class_iri) = state.resolve_class(kind) else {
            continue;
        };
        let class = NamedNodeRef::new(&class_iri)?;
        for quad in state.store.quads_for_pattern(
            None,
            Some(rdf_type),
            Some(class.into()),
            Some(GraphNameRef::NamedNode(graph)),
        ) {
            let quad = quad?;
            let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
                continue;
            };
            let iri = subject.as_str().to_string();
            if !seen.insert(iri.clone()) {
                continue;
            }
            let title = first_literal(state, &iri, &state.capture.title)
                .or_else(|| first_literal(state, &iri, moose::RDFS_LABEL))
                .unwrap_or_default();
            let description =
                first_literal(state, &iri, &state.capture.description).unwrap_or_default();
            out.push(Record {
                iri,
                title,
                description,
            });
        }
    }
    Ok(out)
}

fn build_symbol_index(substrate: &Substrate) -> SymbolIndex {
    let mut by_name: BTreeMap<String, Vec<DefinitionEntry>> = BTreeMap::new();
    let mut by_qualified: BTreeMap<String, DefinitionEntry> = BTreeMap::new();
    for def in substrate.definitions() {
        if def.is_module {
            continue; // modules are file-level, handled by concerns-to-component
        }
        if let Some(name) = &def.display_name {
            by_name.entry(name.clone()).or_default().push(def.clone());
        }
        if let Some(qualified) = qualified_of(&def) {
            by_qualified.entry(qualified).or_insert(def);
        }
    }
    SymbolIndex {
        by_name,
        by_qualified,
    }
}

/// A clean qualified name (e.g. `runtime::build_server`) for simple slash
/// descriptors; `None` for method/impl descriptors that don't render cleanly.
fn qualified_of(def: &DefinitionEntry) -> Option<String> {
    let descriptor = def.normalized_symbol.rsplit(' ').next()?;
    if descriptor.contains('#') || descriptor.contains('[') {
        return None;
    }
    let trimmed = descriptor.trim_end_matches(['(', ')', '.']);
    (!trimmed.is_empty()).then(|| trimmed.replace('/', "::"))
}

/// Extract high-signal code references — the single-token backtick spans that
/// this repo's prose uses for symbols and paths.
fn extract_code_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_tick = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if in_tick {
                let token = current.trim();
                if !token.is_empty() {
                    out.push(token.to_string());
                }
                current.clear();
            }
            in_tick = !in_tick;
        } else if in_tick {
            current.push(ch);
        }
    }
    out
}

fn resolve_token(token: &str, index: &SymbolIndex) -> Resolution {
    let token = token.trim();
    if token.chars().any(char::is_whitespace) {
        return Resolution::Skip("multi-token span (phrase or CLI invocation)".to_string());
    }
    let cleaned = token
        .trim_start_matches('&')
        .trim_end_matches(['(', ')', '.', ',', ';', ':']);
    if cleaned.is_empty() {
        return Resolution::Skip("no identifier".to_string());
    }

    // A file path (has '/' and an extension, no '::') — component-level, not an entity.
    if cleaned.contains('/') && cleaned.contains('.') && !cleaned.contains("::") {
        let is_file_line = token
            .rsplit(':')
            .next()
            .is_some_and(|tail| !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()))
            && token.contains(':');
        return if is_file_line {
            Resolution::Skip(
                "file:line — historical position; resolve symbols not lines".to_string(),
            )
        } else {
            Resolution::Skip("file path — component-level (concerns handles this)".to_string())
        };
    }

    if cleaned.contains("::") {
        if let Some(entry) = index.by_qualified.get(cleaned) {
            return entity(entry);
        }
        let last = cleaned.rsplit("::").next().unwrap_or(cleaned);
        return match index.by_name.get(last) {
            Some(defs) if defs.len() == 1 => entity(&defs[0]),
            Some(defs) => {
                Resolution::Skip(format!("ambiguous: {} defs named `{last}`", defs.len()))
            }
            None => Resolution::Skip("no substrate match".to_string()),
        };
    }

    // A bare, single-word, all-lowercase identifier is too generic to link
    // confidently: common English words (topic, relations, links, stale,
    // consequences) collide with real symbol names but read as prose. Require a
    // distinctive name — multi-part snake_case or CamelCase — for a bare token.
    if !cleaned.contains('_') && !cleaned.chars().any(char::is_uppercase) {
        return Resolution::Skip(
            "single-word lowercase identifier — too generic to link confidently".to_string(),
        );
    }
    match index.by_name.get(cleaned) {
        Some(defs) if defs.len() == 1 => entity(&defs[0]),
        Some(defs) => Resolution::Skip(format!("ambiguous: {} defs named `{cleaned}`", defs.len())),
        None => Resolution::Skip("no substrate match".to_string()),
    }
}

fn entity(entry: &DefinitionEntry) -> Resolution {
    Resolution::Entity {
        symbol: entry.normalized_symbol.clone(),
        path: entry.file.clone(),
        name: entry
            .display_name
            .clone()
            .unwrap_or_else(|| entry.normalized_symbol.clone()),
    }
}

fn first_literal(state: &AppState, subject_iri: &str, predicate_iri: &str) -> Option<String> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).ok()?;
    let subject = NamedNodeRef::new(subject_iri).ok()?;
    let predicate = NamedNodeRef::new(predicate_iri).ok()?;
    state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .find_map(|quad| match quad.object {
            Term::Literal(literal) => Some(literal.value().to_string()),
            _ => None,
        })
}
