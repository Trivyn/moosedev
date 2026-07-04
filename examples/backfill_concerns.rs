//! Offline concerns backfill for the live project KG.
//!
//! Dry-run by default:
//!
//!   cargo run --release --example backfill_concerns --
//!   cargo run --release --example backfill_concerns -- --apply --author James

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use moosedev::{canonical, runtime, validation};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term,
};

const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

#[derive(Debug, Clone)]
struct ComponentSeed {
    name: &'static str,
    paths: &'static [&'static str],
}

const COMPONENT_SEEDS: &[ComponentSeed] = &[
    ComponentSeed {
        name: "MCP tool surface & server runtime",
        paths: &["src/mcp/", "src/runtime.rs", "src/main.rs"],
    },
    ComponentSeed {
        name: "graph/store layer",
        paths: &[
            "src/graph/",
            "src/canonical.rs",
            "src/reasoning/",
            "src/provenance/",
            "src/validation.rs",
            "src/sparql.rs",
            "src/export.rs",
            "src/graph_import.rs",
        ],
    },
    ComponentSeed {
        name: "HTTP API",
        paths: &["src/api/"],
    },
    ComponentSeed {
        name: "web UI",
        paths: &["ui/"],
    },
    ComponentSeed {
        name: "NLQ & retrieval pipeline",
        paths: &["src/llm/", "src/vectors/", "src/alignment/"],
    },
    ComponentSeed {
        name: "ontology artifacts",
        paths: &["ontologies/", "src/ontology/"],
    },
    ComponentSeed {
        name: "benchmark harness",
        paths: &["bench/"],
    },
    ComponentSeed {
        name: "documentation & ADR mirror",
        paths: &[
            "docs/",
            "spec/",
            "src/adrs.rs",
            "scripts/generate-adrs-from-graph.py",
        ],
    },
    ComponentSeed {
        name: "maintenance tools",
        paths: &["examples/", "scripts/"],
    },
];

#[derive(Debug)]
struct Args {
    apply: bool,
    author: String,
    data_dir: PathBuf,
    ontology_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct Component {
    iri: Option<String>,
    name: String,
    covers_paths: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct Record {
    iri: String,
    kind: String,
    title: String,
    description: String,
    has_concerns: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedLink {
    record_iri: String,
    record_kind: String,
    record_title: String,
    component_name: String,
    component_iri: Option<String>,
    via_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchOutcome {
    links: Vec<PlannedLink>,
    unmatched_tokens: BTreeSet<String>,
}

#[derive(Debug)]
struct Resolved {
    system_component: String,
    information_record: String,
    concerns: String,
    covers_path: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let state = match runtime::build_state(&args.data_dir, &args.ontology_dir).await {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "failed to open MOOSEDev state at {}: {e}\n\
                 Stop the daemon first, for example: kill $(cat {}/moosedev-serve.pid)",
                args.data_dir.display(),
                args.data_dir.display()
            );
            return Err(e);
        }
    };
    let resolved = resolve_terms(&state)?;

    let existing = read_components(&state, &resolved)?;
    let (mut components, seed_actions) = seed_plan(existing);
    for action in &seed_actions {
        println!("{action}");
    }

    if args.apply {
        apply_seed_actions(&state, &resolved, &seed_actions, &args.author)?;
        components = read_components(&state, &resolved)?;
    }

    let records = read_information_records(&state, &resolved)?;
    let outcome = match_records_to_components(&records, &components);
    for link in &outcome.links {
        println!(
            "LINK {} \"{}\" --concerns--> {} [via {}]",
            link.record_kind, link.record_title, link.component_name, link.via_token
        );
    }
    if !outcome.unmatched_tokens.is_empty() {
        println!("\nUNMATCHED path tokens:");
        for token in &outcome.unmatched_tokens {
            println!("  {token}");
        }
    }

    let skipped = records.iter().filter(|r| r.has_concerns).count();
    println!(
        "\ncounts: components={} seed-actions={} records={} skipped-already-concerned={} planned-links={} unmatched-tokens={}",
        components.len(),
        seed_actions.len(),
        records.len(),
        skipped,
        outcome.links.len(),
        outcome.unmatched_tokens.len()
    );

    if !args.apply {
        println!("\ndry-run only; re-run with --apply after ratifying this plan");
        return Ok(());
    }

    let mut failures = Vec::new();
    let by_name: BTreeMap<String, String> = components
        .iter()
        .filter_map(|c| c.iri.as_ref().map(|iri| (c.name.clone(), iri.clone())))
        .collect();
    for mut link in outcome.links {
        if link.component_iri.is_none() {
            link.component_iri = by_name.get(&link.component_name).cloned();
        }
        let Some(component_iri) = link.component_iri.as_deref() else {
            failures.push(format!(
                "{} -> {}: component IRI unavailable",
                link.record_iri, link.component_name
            ));
            continue;
        };
        if let Err(e) = graph::relate(&state, &link.record_iri, "concerns", component_iri) {
            failures.push(format!(
                "{} -> {}: {e}",
                link.record_iri, link.component_name
            ));
        }
    }
    if !failures.is_empty() {
        println!("\nedge failures:");
        for failure in &failures {
            println!("  {failure}");
        }
    }

    state.ensure_enriched();
    canonical::write_through(&state.store, &args.data_dir)?;
    let report = validation::validate_project(&state)?;
    println!("\n{}", validation::format_report(&report));
    if !report.conforms() {
        anyhow::bail!("post-backfill validation failed");
    }
    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut apply = false;
    let mut author = "backfill_concerns".to_string();
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
    Ok(Args {
        apply,
        author,
        data_dir,
        ontology_dir,
    })
}

fn resolve_terms(state: &AppState) -> anyhow::Result<Resolved> {
    let covers_path = state
        .arch_vocab
        .datatype_properties
        .iter()
        .find(|entry| entry.local_name == "coversPath")
        .map(|entry| entry.iri.clone())
        .ok_or_else(|| anyhow::anyhow!("architecture ontology is missing coversPath"))?;
    Ok(Resolved {
        system_component: state.resolve_class("SystemComponent")?,
        information_record: state.resolve_class("InformationRecord")?,
        concerns: state.resolve_object_property("concerns")?,
        covers_path,
    })
}

fn read_components(state: &AppState, resolved: &Resolved) -> anyhow::Result<Vec<Component>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let class = NamedNodeRef::new(&resolved.system_component)?;
    let mut out = Vec::new();
    for q in state.store.quads_for_pattern(
        None,
        Some(rdf_type),
        Some(class.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let NamedOrBlankNode::NamedNode(subject) = q.subject else {
            continue;
        };
        let iri = subject.as_str().to_string();
        let name = first_literal_any(
            state,
            &iri,
            &[moose::RDFS_LABEL, state.capture.title.as_str()],
        )
        .unwrap_or_else(|| iri.clone());
        let covers_paths = literal_values(state, &iri, &resolved.covers_path)?;
        out.push(Component {
            iri: Some(iri),
            name,
            covers_paths,
        });
    }
    Ok(out)
}

fn seed_plan(existing: Vec<Component>) -> (Vec<Component>, Vec<String>) {
    let mut by_name: BTreeMap<String, Component> = existing
        .into_iter()
        .map(|component| (component.name.clone(), component))
        .collect();
    let mut actions = Vec::new();
    for seed in COMPONENT_SEEDS {
        match by_name.get_mut(seed.name) {
            Some(component) => {
                for path in seed.paths {
                    if component.covers_paths.insert((*path).to_string()) {
                        actions.push(format!("WOULD ADD coversPath {} -> {}", seed.name, path));
                    }
                }
            }
            None => {
                actions.push(format!(
                    "WOULD MINT SystemComponent \"{}\" coversPath [{}]",
                    seed.name,
                    seed.paths.join(", ")
                ));
                by_name.insert(
                    seed.name.to_string(),
                    Component {
                        iri: None,
                        name: seed.name.to_string(),
                        covers_paths: seed.paths.iter().map(|p| (*p).to_string()).collect(),
                    },
                );
            }
        }
    }
    (by_name.into_values().collect(), actions)
}

fn apply_seed_actions(
    state: &AppState,
    resolved: &Resolved,
    actions: &[String],
    author: &str,
) -> anyhow::Result<()> {
    if actions.is_empty() {
        return Ok(());
    }
    let existing = read_components(state, resolved)?;
    let existing_by_name: BTreeMap<String, Component> = existing
        .into_iter()
        .map(|component| (component.name.clone(), component))
        .collect();
    for seed in COMPONENT_SEEDS {
        match existing_by_name.get(seed.name) {
            Some(component) => {
                let missing: Vec<&str> = seed
                    .paths
                    .iter()
                    .copied()
                    .filter(|path| !component.covers_paths.contains(*path))
                    .collect();
                if missing.is_empty() {
                    continue;
                }
                let subject = component
                    .iri
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("component {} lacks IRI", component.name))?;
                insert_covers_paths(state, subject, &resolved.covers_path, &missing)?;
            }
            None => {
                let mut properties = vec![
                    (moose::RDFS_LABEL.to_string(), seed.name.to_string()),
                    (state.capture.title.clone(), seed.name.to_string()),
                ];
                for path in seed.paths {
                    properties.push((resolved.covers_path.clone(), (*path).to_string()));
                }
                graph::record_instance(
                    state,
                    &RecordInput {
                        class_iri: resolved.system_component.clone(),
                        class_local: "SystemComponent".to_string(),
                        properties,
                    },
                    author,
                    Utc::now(),
                )?;
            }
        }
    }
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    Ok(())
}

fn insert_covers_paths(
    state: &AppState,
    subject_iri: &str,
    covers_path_iri: &str,
    paths: &[&str],
) -> anyhow::Result<()> {
    let subject = NamedNode::new(subject_iri)?;
    let predicate = NamedNode::new(covers_path_iri)?;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let mut txn = state.store.start_transaction()?;
    for path in paths {
        txn.insert(
            Quad::new(
                subject.clone(),
                predicate.clone(),
                Literal::new_simple_literal(*path),
                graph.clone(),
            )
            .as_ref(),
        );
    }
    txn.commit()?;
    Ok(())
}

fn read_information_records(state: &AppState, resolved: &Resolved) -> anyhow::Result<Vec<Record>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for q in state.store.quads_for_pattern(
        None,
        Some(rdf_type),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let NamedOrBlankNode::NamedNode(subject) = q.subject else {
            continue;
        };
        let Term::NamedNode(class_node) = q.object else {
            continue;
        };
        let class_iri = class_node.as_str();
        if !is_subclass_of(&state.store, class_iri, &resolved.information_record) {
            continue;
        }
        let iri = subject.as_str().to_string();
        if !seen.insert(iri.clone()) {
            continue;
        }
        let title = first_literal_any(
            state,
            &iri,
            &[moose::RDFS_LABEL, state.capture.title.as_str()],
        )
        .unwrap_or_else(|| iri.clone());
        let description = first_literal_any(state, &iri, &[state.capture.description.as_str()])
            .unwrap_or_default();
        let has_concerns = has_outgoing(state, &iri, &resolved.concerns)?;
        out.push(Record {
            iri,
            kind: local_name(class_iri).to_string(),
            title,
            description,
            has_concerns,
        });
    }
    Ok(out)
}

fn literal_values(
    state: &AppState,
    subject_iri: &str,
    predicate_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    let mut out = BTreeSet::new();
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let Term::Literal(literal) = q.object {
            out.insert(literal.value().to_string());
        }
    }
    Ok(out)
}

fn first_literal_any(state: &AppState, subject_iri: &str, predicates: &[&str]) -> Option<String> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).ok()?;
    let subject = NamedNodeRef::new(subject_iri).ok()?;
    for predicate_iri in predicates {
        let predicate = NamedNodeRef::new(predicate_iri).ok()?;
        for q in state
            .store
            .quads_for_pattern(
                Some(subject.into()),
                Some(predicate),
                None,
                Some(GraphNameRef::NamedNode(graph)),
            )
            .flatten()
        {
            if let Term::Literal(literal) = q.object {
                return Some(literal.value().to_string());
            }
        }
    }
    None
}

fn has_outgoing(state: &AppState, subject_iri: &str, predicate_iri: &str) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

fn match_records_to_components(records: &[Record], components: &[Component]) -> MatchOutcome {
    let mut links = Vec::new();
    let mut unmatched_tokens = BTreeSet::new();
    for record in records.iter().filter(|record| !record.has_concerns) {
        let text = format!("{} {}", record.title, record.description);
        let tokens = extract_path_tokens(&text);
        let mut by_component: BTreeMap<String, PlannedLink> = BTreeMap::new();
        for token in tokens {
            match best_component_for_token(&token, components) {
                Some(component) => {
                    by_component
                        .entry(component.name.clone())
                        .or_insert(PlannedLink {
                            record_iri: record.iri.clone(),
                            record_kind: record.kind.clone(),
                            record_title: record.title.clone(),
                            component_name: component.name.clone(),
                            component_iri: component.iri.clone(),
                            via_token: token,
                        });
                }
                None => {
                    unmatched_tokens.insert(token);
                }
            }
        }
        links.extend(by_component.into_values());
    }
    links.sort_by(|a, b| {
        a.record_kind
            .cmp(&b.record_kind)
            .then(a.record_title.cmp(&b.record_title))
            .then(a.component_name.cmp(&b.component_name))
    });
    MatchOutcome {
        links,
        unmatched_tokens,
    }
}

fn best_component_for_token<'a>(token: &str, components: &'a [Component]) -> Option<&'a Component> {
    let mut best: Option<(&Component, usize)> = None;
    for component in components {
        for path in &component.covers_paths {
            let matched = if path.ends_with('/') {
                token.starts_with(path)
            } else {
                token == path
            };
            if matched && best.is_none_or(|(_, len)| path.len() > len) {
                best = Some((component, path.len()));
            }
        }
    }
    best.map(|(component, _)| component)
}

fn extract_path_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ','
            )
        {
            push_clean_token(&mut out, &current);
            current.clear();
        } else {
            current.push(ch);
        }
    }
    push_clean_token(&mut out, &current);
    out
}

fn push_clean_token(out: &mut Vec<String>, raw: &str) {
    let mut token = raw.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
        )
    });
    if let Some(stripped) = token.strip_prefix("./") {
        token = stripped;
    }
    if let Some((prefix, suffix)) = token.rsplit_once(':') {
        if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            token = prefix;
        }
    }
    token = token.trim_end_matches('.');
    if token.contains('/') && !out.iter().any(|existing| existing == token) {
        out.push(token.to_string());
    }
}

fn is_subclass_of(store: &oxigraph::store::Store, class_iri: &str, ancestor_iri: &str) -> bool {
    let sub_class_of = NamedNodeRef::new_unchecked(RDFS_SUBCLASS_OF);
    let mut stack = vec![class_iri.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(cur) = stack.pop() {
        if cur == ancestor_iri {
            return true;
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Ok(node) = NamedNode::new(&cur) else {
            continue;
        };
        for q in store
            .quads_for_pattern(Some(node.as_ref().into()), Some(sub_class_of), None, None)
            .flatten()
        {
            if let Term::NamedNode(parent) = q.object {
                stack.push(parent.as_str().to_string());
            }
        }
    }
    false
}

fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, paths: &[&str]) -> Component {
        Component {
            iri: Some(format!("urn:{name}")),
            name: name.to_string(),
            covers_paths: paths.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    #[test]
    fn extracts_path_tokens_without_regex() {
        let tokens = extract_path_tokens(
            "Touched `./src/graph/capture.rs:337`, ontologies/software-architecture.ttl, and (ui/src/App.tsx).",
        );
        assert_eq!(
            tokens,
            vec![
                "src/graph/capture.rs",
                "ontologies/software-architecture.ttl",
                "ui/src/App.tsx"
            ]
        );
    }

    #[test]
    fn longest_prefix_wins() {
        let components = vec![
            component("broad graph", &["src/"]),
            component("graph layer", &["src/graph/"]),
        ];
        let best = best_component_for_token("src/graph/capture.rs", &components).unwrap();
        assert_eq!(best.name, "graph layer");
    }

    #[test]
    fn reports_unmatched_tokens() {
        let records = vec![Record {
            iri: "urn:record".to_string(),
            kind: "Lesson".to_string(),
            title: "MOOSE engine path".to_string(),
            description: "Closed-source path ../moose/src/core.rs is outside this map".to_string(),
            has_concerns: false,
        }];
        let outcome = match_records_to_components(&records, &[component("graph", &["src/graph/"])]);
        assert!(outcome.links.is_empty());
        assert!(outcome.unmatched_tokens.contains("../moose/src/core.rs"));
    }

    #[test]
    fn skips_already_concerned_records() {
        let records = vec![Record {
            iri: "urn:record".to_string(),
            kind: "Lesson".to_string(),
            title: "Already linked".to_string(),
            description: "src/graph/capture.rs".to_string(),
            has_concerns: true,
        }];
        let outcome = match_records_to_components(&records, &[component("graph", &["src/graph/"])]);
        assert!(outcome.links.is_empty());
        assert!(outcome.unmatched_tokens.is_empty());
    }
}
