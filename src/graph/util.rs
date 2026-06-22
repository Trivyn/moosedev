//! Shared RDF, SPARQL, IRI, and ontology-vocabulary helpers for `crate::graph`.
//! These are intentionally small primitives used by sibling graph modules.

use moose::types::{CompactVocabulary, VocabularyEntry};
use oxigraph::model::{NamedNode, NamedNodeRef, Term};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

/// Find a vocabulary entry's full IRI by its local name — the one place the code
/// looks a term up in the loaded ontology, keeping the volatile namespace out.
pub(crate) fn iri_by_local_name(entries: &[VocabularyEntry], local: &str) -> Option<String> {
    entries
        .iter()
        .find(|e| e.local_name == local)
        .map(|e| e.iri.clone())
}

/// Resolve a datatype property's full IRI from the loaded vocabulary by local name.
pub(crate) fn datatype_property_iri(
    vocab: &CompactVocabulary,
    local: &str,
) -> anyhow::Result<String> {
    iri_by_local_name(&vocab.datatype_properties, local).ok_or_else(|| {
        anyhow::anyhow!("architecture ontology is missing datatype property {local:?}")
    })
}

/// Resolve an object property's (relation's) full IRI from the loaded vocabulary
/// by local name — the relation analogue of [`datatype_property_iri`], keeping the
/// volatile namespace out of the code.
pub(crate) fn object_property_iri(
    vocab: &CompactVocabulary,
    local: &str,
) -> anyhow::Result<String> {
    iri_by_local_name(&vocab.object_properties, local).ok_or_else(|| {
        anyhow::anyhow!("architecture ontology is missing object property {local:?}")
    })
}

/// Mint a fresh instance IRI for a class local name, e.g.
/// `https://moosedev.dev/kg/ArchitecturalDecision/<uuid>`.
pub fn mint_instance_iri(class_local: &str) -> String {
    format!(
        "https://moosedev.dev/kg/{}/{}",
        class_local,
        uuid::Uuid::new_v4()
    )
}

/// `rdfs:subClassOf` — class-subsumption predicate (moose's const set omits it).
pub(crate) const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
pub(crate) const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
pub(crate) const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
pub(crate) const SH_TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";
pub(crate) const SH_PROPERTY: &str = "http://www.w3.org/ns/shacl#property";
pub(crate) const SH_PATH: &str = "http://www.w3.org/ns/shacl#path";
pub(crate) const SH_CLASS: &str = "http://www.w3.org/ns/shacl#class";
pub(crate) const SH_OR: &str = "http://www.w3.org/ns/shacl#or";

/// True if `class_iri` equals `ancestor_iri` or is a transitive `rdfs:subClassOf`
/// of it, per the loaded ontology. Bounded, cycle-safe walk over subClassOf edges
/// in any graph — class axioms live in the ontology graphs, not the project graph.
pub(crate) fn is_subclass_of(store: &Store, class_iri: &str, ancestor_iri: &str) -> bool {
    let sub_class_of = NamedNodeRef::new_unchecked(RDFS_SUBCLASS_OF);
    let mut stack = vec![class_iri.to_string()];
    let mut seen = std::collections::HashSet::new();
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

pub(crate) fn run_sparql<'a>(store: &'a Store, sparql: &str) -> anyhow::Result<QueryResults<'a>> {
    let prepared = SparqlEvaluator::new()
        .parse_query(sparql)
        .map_err(|e| anyhow::anyhow!("graph query parse failed: {e}\n{sparql}"))?;
    prepared
        .on_store(store)
        .execute()
        .map_err(|e| anyhow::anyhow!("graph query failed: {e}"))
}

pub(crate) fn iri_value(term: Option<&Term>) -> Option<String> {
    match term {
        Some(Term::NamedNode(node)) => Some(node.as_str().to_string()),
        _ => None,
    }
}

pub(crate) fn any_subclass_of(store: &Store, actual: &[String], expected: &[String]) -> bool {
    actual
        .iter()
        .any(|a| expected.iter().any(|e| is_subclass_of(store, a, e)))
}

pub(crate) fn class_list(classes: &[String]) -> String {
    if classes.is_empty() {
        "<none>".to_string()
    } else {
        classes
            .iter()
            .map(|iri| local_name(iri).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub(crate) fn unique_classes(classes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for class in classes {
        if !out.contains(&class) {
            out.push(class);
        }
    }
    out
}

/// Extract the local name of an IRI (after the last `/` or `#`).
pub(crate) fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}
