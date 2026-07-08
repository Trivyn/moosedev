//! Project `SystemComponent` path coverage helpers.
//!
//! Components declare the repository paths they own with `coversPath` literals
//! in the project KG. This module is the shared read/match layer for tools that
//! need to turn a source path into the most specific owning component.

use std::collections::BTreeSet;

use oxigraph::model::{GraphNameRef, NamedNodeRef, NamedOrBlankNode, Term};
use oxigraph::store::Store;

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
