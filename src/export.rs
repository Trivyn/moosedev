//! Text export for the durable project knowledge graph.
//!
//! N-Quads is the canonical dataset export. N-Triples is deterministic after
//! dropping graph names. Turtle is human-readable and intentionally not a
//! byte-canonical version-control format.

use std::collections::BTreeSet;

use oxigraph::io::{RdfFormat, RdfSerializer};
use oxigraph::model::{GraphNameRef, NamedNode, Quad};
use oxigraph::store::Store;

use crate::graph::PROJECT_KG_GRAPH_IRI;
use crate::provenance::PROVENANCE_GRAPH_IRI;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ExportFormat {
    #[default]
    NQuads,
    NTriples,
    Turtle,
}

impl ExportFormat {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "nq" | "n-quads" | "nquads" => Ok(Self::NQuads),
            "nt" | "n-triples" | "ntriples" => Ok(Self::NTriples),
            "ttl" | "turtle" => Ok(Self::Turtle),
            other => anyhow::bail!("unknown export format {other:?}; valid values: nq, nt, ttl"),
        }
    }

    pub fn rdf_format(self) -> RdfFormat {
        match self {
            Self::NQuads => RdfFormat::NQuads,
            Self::NTriples => RdfFormat::NTriples,
            Self::Turtle => RdfFormat::Turtle,
        }
    }

    pub fn extension(self) -> &'static str {
        self.rdf_format().file_extension()
    }

    pub fn media_type(self) -> &'static str {
        self.rdf_format().media_type()
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ExportScope {
    #[default]
    Project,
    Provenance,
    All,
}

impl ExportScope {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "project" => Ok(Self::Project),
            "provenance" | "prov" => Ok(Self::Provenance),
            "all" => Ok(Self::All),
            other => anyhow::bail!(
                "unknown export graph {other:?}; valid values: project, provenance, all"
            ),
        }
    }

    pub fn graph_iris(self) -> Vec<&'static str> {
        match self {
            Self::Project => vec![PROJECT_KG_GRAPH_IRI],
            Self::Provenance => vec![PROVENANCE_GRAPH_IRI],
            Self::All => vec![PROJECT_KG_GRAPH_IRI, PROVENANCE_GRAPH_IRI],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Provenance => "provenance",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GraphDump {
    pub text: String,
    pub quad_count: usize,
    pub graphs: Vec<String>,
}

pub fn export_graph(
    store: &Store,
    scope: ExportScope,
    format: ExportFormat,
) -> anyhow::Result<GraphDump> {
    let graph_iris = scope.graph_iris();
    let mut quads = Vec::new();

    for graph_iri in &graph_iris {
        let graph = NamedNode::new(*graph_iri)
            .map_err(|e| anyhow::anyhow!("invalid export graph IRI {graph_iri:?}: {e}"))?;
        for quad in store.quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        ) {
            quads.push(quad?);
        }
    }

    quads.sort_by_cached_key(canonical_quad_key);

    let text = serialize_quads(&quads, format)?;
    Ok(GraphDump {
        text,
        quad_count: quads.len(),
        graphs: graph_iris.into_iter().map(str::to_string).collect(),
    })
}

fn canonical_quad_key(quad: &Quad) -> (String, String, String, String) {
    (
        quad.graph_name.to_string(),
        quad.subject.to_string(),
        quad.predicate.to_string(),
        quad.object.to_string(),
    )
}

fn serialize_quads(quads: &[Quad], format: ExportFormat) -> anyhow::Result<String> {
    let mut out = Vec::new();
    let mut serializer = RdfSerializer::from_format(format.rdf_format()).for_writer(&mut out);

    match format {
        ExportFormat::NQuads => {
            for quad in quads {
                serializer.serialize_quad(quad.as_ref())?;
            }
        }
        ExportFormat::NTriples | ExportFormat::Turtle => {
            let mut seen = BTreeSet::new();
            for quad in quads {
                if !seen.insert(canonical_triple_key(quad)) {
                    continue;
                }
                serializer.serialize_triple(quad.as_ref())?;
            }
        }
    }
    serializer.finish()?;
    String::from_utf8(out).map_err(|e| anyhow::anyhow!("graph dump was not UTF-8: {e}"))
}

fn canonical_triple_key(quad: &Quad) -> (String, String, String) {
    (
        quad.subject.to_string(),
        quad.predicate.to_string(),
        quad.object.to_string(),
    )
}
