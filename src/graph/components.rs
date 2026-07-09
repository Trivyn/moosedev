//! Project `SystemComponent` path coverage helpers.
//!
//! Components declare the repository paths they own with `coversPath` literals
//! in the project KG. This module is the shared read/match layer for tools that
//! need to turn a source path into the most specific owning component.

use std::collections::BTreeSet;

use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term,
};
use oxigraph::store::Store;

use super::capture::asserted_project_types;
use super::context::first_literal;
use super::state::AppState;
use super::util::datatype_property_iri;
use super::PROJECT_KG_GRAPH_IRI;

/// A SystemComponent from the project KG together with the repo paths it
/// covers (`coversPath` literals; trailing '/' = directory prefix, otherwise
/// an exact file path).
#[derive(Debug, Clone)]
pub struct ComponentEntry {
    /// None only for planned-but-not-yet-minted components (seeding flows
    /// like backfill_concerns); `load_components` always sets it.
    pub iri: Option<String>,
    pub name: String,
    pub covers_paths: BTreeSet<String>,
}

/// Result of declaring new path coverage for a `SystemComponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareOutcome {
    pub component_iri: String,
    pub component_name: String,
    pub added: Vec<String>,
    pub already_covered: Vec<String>,
}

/// Load all minted `SystemComponent` records and their `coversPath` values from
/// the project graph.
///
/// Ontology terms are resolved by local name from the loaded architecture
/// vocabulary here, so callers do not need to know the current ontology
/// namespace or full term IRIs.
pub fn load_components(state: &AppState) -> anyhow::Result<Vec<ComponentEntry>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let system_component = state.resolve_class("SystemComponent")?;
    let class = NamedNodeRef::new(&system_component)?;
    let covers_path = datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let mut out = Vec::new();

    // SystemComponents are identified by their rdf:type in the project graph.
    // The ontology resolver above supplies the full class IRI at runtime.
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

        // rdfs:label is canonical for display/search; capture.title is kept as
        // a compatibility fallback for older or partially seeded records.
        let name = first_literal(&state.store, &iri, moose::RDFS_LABEL)
            .or_else(|| first_literal(&state.store, &iri, &state.capture.title))
            .unwrap_or_else(|| iri.clone());
        let covers_paths = literal_values(&state.store, &iri, &covers_path)?;
        out.push(ComponentEntry {
            iri: Some(iri),
            name,
            covers_paths,
        });
    }

    // Deterministic ordering keeps downstream mint plans and dry-run reports
    // stable regardless of store iteration order.
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.iri.cmp(&b.iri)));
    Ok(out)
}

/// Return the most specific component that covers `path`.
///
/// `coversPath` values ending in `/` are directory prefixes. Values without a
/// trailing slash are exact file paths. If several entries match, the longest
/// matching `coversPath` wins.
pub fn best_component_for_path<'a>(
    path: &str,
    components: &'a [ComponentEntry],
) -> Option<&'a ComponentEntry> {
    let mut best: Option<(&ComponentEntry, usize)> = None;
    for component in components {
        for covers_path in &component.covers_paths {
            let matched = if covers_path.ends_with('/') {
                path.starts_with(covers_path)
            } else {
                path == covers_path
            };
            if matched && best.is_none_or(|(_, len)| covers_path.len() > len) {
                best = Some((component, covers_path.len()));
            }
        }
    }
    best.map(|(component, _)| component)
}

/// Declare repository path coverage for a minted `SystemComponent`.
///
/// `paths` are repo-relative. Values ending in `/` cover a directory prefix;
/// values without a trailing slash cover one exact file path. Re-declaring an
/// existing value is a successful no-op.
pub fn declare_component_paths(
    state: &AppState,
    component: &str,
    paths: &[String],
) -> anyhow::Result<DeclareOutcome> {
    let requested = validate_paths(paths)?;
    let components = load_components(state)?;
    let resolved = resolve_component(state, &components, component)?;
    let subject = NamedNode::new(&resolved.iri)
        .map_err(|e| anyhow::anyhow!("invalid component IRI {}: {e}", resolved.iri))?;
    let system_component = state.resolve_class("SystemComponent")?;
    let asserted = asserted_project_types(state, &subject);
    if !asserted.iter().any(|class| class == &system_component) {
        anyhow::bail!(
            "target {} is not a SystemComponent; asserted project types: {}",
            resolved.iri,
            if asserted.is_empty() {
                "(none)".to_string()
            } else {
                asserted.join(", ")
            }
        );
    }

    let covers_path = datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let existing = literal_values(&state.store, &resolved.iri, &covers_path)?;
    let mut already_covered = Vec::new();
    let mut added = Vec::new();
    for path in requested {
        if existing.contains(&path) {
            already_covered.push(path);
        } else {
            added.push(path);
        }
    }
    already_covered.sort();
    added.sort();

    if !added.is_empty() {
        let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI)?;
        let predicate = NamedNode::new(&covers_path)?;
        let mut txn = state
            .store
            .start_transaction()
            .map_err(|e| anyhow::anyhow!("declare component paths transaction: {e}"))?;
        for path in &added {
            let quad = Quad::new(
                subject.clone(),
                predicate.clone(),
                Literal::new_simple_literal(path),
                GraphName::NamedNode(graph.clone()),
            );
            txn.insert(quad.as_ref());
        }
        txn.commit()
            .map_err(|e| anyhow::anyhow!("declare component paths commit: {e}"))?;
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    }

    Ok(DeclareOutcome {
        component_iri: resolved.iri,
        component_name: resolved.name,
        added,
        already_covered,
    })
}

/// Collect literal objects for one predicate on one subject in the project KG.
///
/// Non-literal objects are ignored because `coversPath` is a datatype property
/// and only literal values participate in path matching.
fn literal_values(
    store: &Store,
    subject_iri: &str,
    predicate_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    let mut out = BTreeSet::new();
    for q in store.quads_for_pattern(
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

#[derive(Debug, Clone)]
struct ResolvedComponent {
    iri: String,
    name: String,
}

fn resolve_component(
    state: &AppState,
    components: &[ComponentEntry],
    component: &str,
) -> anyhow::Result<ResolvedComponent> {
    if let Ok(node) = NamedNode::new(component) {
        let iri = node.as_str().to_string();
        if has_project_subject_quads(state, &node)? {
            let name = components
                .iter()
                .find(|entry| entry.iri.as_deref() == Some(iri.as_str()))
                .map(|entry| entry.name.clone())
                .or_else(|| first_literal(&state.store, &iri, moose::RDFS_LABEL))
                .or_else(|| first_literal(&state.store, &iri, &state.capture.title))
                .unwrap_or_else(|| iri.clone());
            return Ok(ResolvedComponent { iri, name });
        }
    }

    let matches = components
        .iter()
        .filter(|entry| entry.name == component)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [entry] => Ok(ResolvedComponent {
            iri: entry
                .iri
                .clone()
                .expect("load_components sets component IRI"),
            name: entry.name.clone(),
        }),
        [] => {
            let names = components
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "unknown SystemComponent {component:?}; known components from load_components: {}",
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            )
        }
        many => {
            let iris = many
                .iter()
                .filter_map(|entry| entry.iri.as_deref())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "ambiguous SystemComponent label {component:?}; matching IRIs: {}",
                iris.join(", ")
            )
        }
    }
}

fn has_project_subject_quads(state: &AppState, subject: &NamedNode) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

fn validate_paths(paths: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for raw in paths {
        let path = raw.trim();
        if path.is_empty() {
            anyhow::bail!("component coverage path must not be empty");
        }
        if path.starts_with('/') {
            anyhow::bail!("component coverage path {path:?} must be repo-relative");
        }
        if path.starts_with("./") {
            anyhow::bail!("component coverage path {path:?} must not start with ./");
        }
        if path.contains('\\') {
            anyhow::bail!("component coverage path {path:?} must use forward slashes");
        }
        if path.split('/').any(|segment| segment == "..") {
            anyhow::bail!("component coverage path {path:?} must not contain .. segments");
        }
        out.insert(path.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, paths: &[&str]) -> ComponentEntry {
        ComponentEntry {
            iri: Some(format!("urn:{name}")),
            name: name.to_string(),
            covers_paths: paths.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let components = vec![
            component("broad graph", &["src/"]),
            component("graph layer", &["src/graph/"]),
        ];
        let best = best_component_for_path("src/graph/capture.rs", &components).unwrap();
        assert_eq!(best.name, "graph layer");
    }

    #[test]
    fn exact_file_matches_only_exact_path() {
        let runtime = vec![component("runtime", &["src/runtime.rs"])];
        assert_eq!(
            best_component_for_path("src/runtime.rs", &runtime)
                .unwrap()
                .name,
            "runtime"
        );
        assert!(best_component_for_path("src/runtime.rs.bak", &runtime).is_none());
        assert!(best_component_for_path("src/runtime", &runtime).is_none());

        let graph = vec![component("graph", &["src/graph"])];
        assert!(best_component_for_path("src/graph/capture.rs", &graph).is_none());
    }

    #[test]
    fn miss_returns_none() {
        let components = vec![component("graph", &["src/graph/"])];
        assert!(best_component_for_path("../moose/src/core.rs", &components).is_none());
    }
}
