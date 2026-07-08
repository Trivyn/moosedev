//! Ontology loading + resolution for MOOSEDev.
//!
//! Loads MOOSEDev's domain ontologies (Turtle) into the durable RDF store,
//! exposes the domain MOOSE [`CompactVocabulary`] values, and implements
//! MOOSE's [`OntologyResolver`] so the query pipeline knows which ontology
//! graphs (and SHACL shape graphs) are aligned to the project knowledge graph.
//!
//! The ontology *content* is produced by Trivyn's ontology generator; MOOSEDev
//! only consumes it. Three domains ship: a general software-engineering backbone
//! (`software-engineering.ttl`), a software-architecture domain layered on top
//! (`software-architecture.ttl`) — the classes MOOSEDev captures into — and a
//! code layer (`software-code.ttl`). Each domain has a companion SHACL shapes
//! graph. This loader is **content-agnostic**.

use std::path::Path;

use async_trait::async_trait;
use moose::traits::OntologyResolver;
use moose::types::{CompactVocabulary, EngineError};
use moose::vocabulary::extract_compact_vocabulary;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::NamedNodeRef;
use oxigraph::store::Store;

/// Named-graph IRIs the shipped ontologies are loaded into. These are
/// MOOSEDev-owned *container* IRIs, deliberately independent of the ontologies'
/// own term namespaces: a class keeps its TTL IRI (e.g.
/// `<https://trivyn.io/…/domain/ArchitecturalDecision>`) regardless of which
/// graph holds it, so the ontology can be regenerated under a different
/// namespace without touching this code. Everything the code needs (class and
/// property IRIs) is read back out of the loaded vocabulary by local name.
pub const SE_DOMAIN_GRAPH_IRI: &str = "https://moosedev.dev/kg/ontology/software-engineering";
pub const SE_SHAPES_GRAPH_IRI: &str =
    "https://moosedev.dev/kg/ontology/software-engineering/shapes";
pub const ARCH_DOMAIN_GRAPH_IRI: &str = "https://moosedev.dev/kg/ontology/software-architecture";
pub const ARCH_SHAPES_GRAPH_IRI: &str =
    "https://moosedev.dev/kg/ontology/software-architecture/shapes";
pub const CODE_DOMAIN_GRAPH_IRI: &str = "https://moosedev.dev/kg/ontology/software-code";
pub const CODE_SHAPES_GRAPH_IRI: &str = "https://moosedev.dev/kg/ontology/software-code/shapes";

/// File names of the shipped ontologies, relative to the ontology directory.
pub const SE_DOMAIN_TTL: &str = "software-engineering.ttl";
pub const SE_SHAPES_TTL: &str = "software-engineering_shapes.ttl";
pub const ARCH_DOMAIN_TTL: &str = "software-architecture.ttl";
pub const ARCH_SHAPES_TTL: &str = "software-architecture_shapes.ttl";
pub const CODE_DOMAIN_TTL: &str = "software-code.ttl";
pub const CODE_SHAPES_TTL: &str = "software-code_shapes.ttl";

/// Vocabularies extracted from the loaded domain ontologies, by local name.
pub struct DomainVocabularies {
    /// Architecture domain — the classes MOOSEDev captures into.
    pub arch: CompactVocabulary,
    /// Code domain — CodeEntity + its properties and the intent links.
    pub code: CompactVocabulary,
}

/// Parse a Turtle file and load it into `store` under the named graph `graph_iri`.
///
/// Mirrors how the MOOSE engine ingests its own pipeline ontology
/// (`moose::initialize`): an oxigraph `RdfParser` with the default graph set,
/// fed the raw bytes.
pub fn load_turtle(store: &Store, path: &Path, graph_iri: &str) -> anyhow::Result<()> {
    let graph = NamedNodeRef::new(graph_iri)
        .map_err(|e| anyhow::anyhow!("invalid graph IRI {graph_iri}: {e}"))?;
    let existing: Vec<_> = store
        .quads_for_pattern(None, None, None, Some(graph.into()))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("clear ontology graph {graph_iri}: {e}"))?;
    for quad in existing {
        store
            .remove(&quad)
            .map_err(|e| anyhow::anyhow!("clear ontology graph {graph_iri}: {e}"))?;
    }
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read ontology {}: {e}", path.display()))?;
    let parser = RdfParser::from_format(RdfFormat::Turtle).with_default_graph(graph);
    store
        .load_from_reader(parser, bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("load ontology {}: {e}", path.display()))?;
    Ok(())
}

/// Load all shipped ontologies (domains + SHACL shape graphs) from `dir` into
/// their named graphs, and return compact vocabularies for the domain modules
/// MOOSEDev resolves terms from. The software-engineering backbone is loaded so
/// the alignment/query layers can see it (via the resolver), but capture is
/// scoped to the architecture domain.
pub fn load_ontologies(store: &Store, dir: &Path) -> anyhow::Result<DomainVocabularies> {
    load_turtle(store, &dir.join(SE_DOMAIN_TTL), SE_DOMAIN_GRAPH_IRI)?;
    load_turtle(store, &dir.join(SE_SHAPES_TTL), SE_SHAPES_GRAPH_IRI)?;
    load_turtle(store, &dir.join(ARCH_DOMAIN_TTL), ARCH_DOMAIN_GRAPH_IRI)?;
    load_turtle(store, &dir.join(ARCH_SHAPES_TTL), ARCH_SHAPES_GRAPH_IRI)?;
    load_turtle(store, &dir.join(CODE_DOMAIN_TTL), CODE_DOMAIN_GRAPH_IRI)?;
    load_turtle(store, &dir.join(CODE_SHAPES_TTL), CODE_SHAPES_GRAPH_IRI)?;
    let arch = extract_compact_vocabulary(store, ARCH_DOMAIN_GRAPH_IRI, None).map_err(|e| {
        anyhow::anyhow!("extract_compact_vocabulary({ARCH_DOMAIN_GRAPH_IRI}): {e:?}")
    })?;
    let code = extract_compact_vocabulary(store, CODE_DOMAIN_GRAPH_IRI, None).map_err(|e| {
        anyhow::anyhow!("extract_compact_vocabulary({CODE_DOMAIN_GRAPH_IRI}): {e:?}")
    })?;
    Ok(DomainVocabularies { arch, code })
}

/// MOOSE [`OntologyResolver`] for MOOSEDev: the project KG (data graph) is
/// aligned to both shipped domain ontologies, with their SHACL shape graphs
/// supplying declared cardinality/range to MOOSE's `schema_shape` layer. Mapping
/// graphs use the trait default (none) — the cross-domain bridges
/// (`SystemComponent ⊑ se:Component`, …) are inline in the domain graph.
#[derive(Debug, Clone, Default)]
pub struct MooseDevOntologyResolver;

impl MooseDevOntologyResolver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl OntologyResolver for MooseDevOntologyResolver {
    async fn get_aligned_ontologies(
        &self,
        _data_graphs: &[String],
    ) -> Result<Vec<String>, EngineError> {
        Ok(vec![
            SE_DOMAIN_GRAPH_IRI.to_string(),
            ARCH_DOMAIN_GRAPH_IRI.to_string(),
            CODE_DOMAIN_GRAPH_IRI.to_string(),
        ])
    }

    async fn get_shape_graphs(&self, _data_graphs: &[String]) -> Result<Vec<String>, EngineError> {
        Ok(vec![
            SE_SHAPES_GRAPH_IRI.to_string(),
            ARCH_SHAPES_GRAPH_IRI.to_string(),
            CODE_SHAPES_GRAPH_IRI.to_string(),
        ])
    }
}
