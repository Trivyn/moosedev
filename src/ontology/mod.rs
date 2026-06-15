//! Ontology loading + resolution for MOOSEDev.
//!
//! Loads MOOSEDev's domain ontologies (Turtle) into the durable RDF store,
//! exposes their MOOSE [`CompactVocabulary`], and implements MOOSE's
//! [`OntologyResolver`] so the query pipeline knows which ontology graph is
//! aligned to the project knowledge graph.
//!
//! The ontology *content* is produced by Trivyn's ontology generator (see
//! `spec/architecture-ontology-brief.md`); MOOSEDev only consumes it. v1 ships
//! a minimal stub (`ontologies/architecture.ttl`) until the generated ontology
//! lands — this loader is **content-agnostic**.

use std::path::Path;

use async_trait::async_trait;
use moose::traits::OntologyResolver;
use moose::types::{CompactVocabulary, EngineError};
use moose::vocabulary::extract_compact_vocabulary;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::NamedNodeRef;
use oxigraph::store::Store;

/// Named-graph IRI the architecture ontology is loaded into.
pub const ARCHITECTURE_GRAPH_IRI: &str = "https://moosedev.dev/ontologies/architecture";

/// Default on-disk location of the architecture ontology, relative to the crate root.
pub const DEFAULT_ARCHITECTURE_TTL: &str = "ontologies/architecture.ttl";

/// Parse a Turtle file and load it into `store` under the named graph `graph_iri`.
///
/// Mirrors how the MOOSE engine ingests its own pipeline ontology
/// (`moose::initialize`): an oxigraph `RdfParser` with the default graph set,
/// fed the raw bytes.
pub fn load_turtle(store: &Store, path: &Path, graph_iri: &str) -> anyhow::Result<()> {
    let graph = NamedNodeRef::new(graph_iri)
        .map_err(|e| anyhow::anyhow!("invalid graph IRI {graph_iri}: {e}"))?;
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read ontology {}: {e}", path.display()))?;
    let parser = RdfParser::from_format(RdfFormat::Turtle).with_default_graph(graph);
    store
        .load_from_reader(parser, bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("load ontology {}: {e}", path.display()))?;
    Ok(())
}

/// Load the architecture ontology from `path` and return its MOOSE
/// [`CompactVocabulary`] — the classes/properties the alignment and query
/// pipelines consult.
pub fn load_architecture(store: &Store, path: &Path) -> anyhow::Result<CompactVocabulary> {
    load_turtle(store, path, ARCHITECTURE_GRAPH_IRI)?;
    extract_compact_vocabulary(store, ARCHITECTURE_GRAPH_IRI, None)
        .map_err(|e| anyhow::anyhow!("extract_compact_vocabulary({ARCHITECTURE_GRAPH_IRI}): {e:?}"))
}

/// MOOSE [`OntologyResolver`] for MOOSEDev: the project KG (data graph) is
/// aligned to the architecture ontology graph. Mapping/shape graphs use the
/// trait defaults (none) for v1.
#[derive(Debug, Clone)]
pub struct MooseDevOntologyResolver {
    pub architecture_graph_iri: String,
}

impl MooseDevOntologyResolver {
    pub fn new() -> Self {
        Self {
            architecture_graph_iri: ARCHITECTURE_GRAPH_IRI.to_string(),
        }
    }
}

impl Default for MooseDevOntologyResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OntologyResolver for MooseDevOntologyResolver {
    async fn get_aligned_ontologies(
        &self,
        _data_graphs: &[String],
    ) -> Result<Vec<String>, EngineError> {
        Ok(vec![self.architecture_graph_iri.clone()])
    }
}
