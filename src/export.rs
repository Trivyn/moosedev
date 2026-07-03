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
        quads.extend(graph_quads(store, graph_iri)?);
    }

    quads.sort_by_cached_key(canonical_quad_key);

    let text = serialize_quads(&quads, format)?;
    Ok(GraphDump {
        text,
        quad_count: quads.len(),
        graphs: graph_iris.into_iter().map(str::to_string).collect(),
    })
}

/// Canonical dump of the project graph for the committed `.moosedev/kg.nq`:
/// sorted N-Quads MINUS reasoner-materialized quads. Inferred edges are tagged
/// only by reification in the (local, uncommitted) provenance graph, so shipping
/// them in the text would strand them untagged — and un-droppable — on the
/// importing machine; they are always re-derived locally by lazy enrichment.
pub fn export_canonical_project(store: &Store) -> anyhow::Result<GraphDump> {
    export_canonical(store, PROJECT_KG_GRAPH_IRI)
}

/// [`export_canonical_project`] generalized over the data graph, so tests can
/// exercise the inferred-quad exclusion on scratch graphs.
pub(crate) fn export_canonical(store: &Store, data_graph_iri: &str) -> anyhow::Result<GraphDump> {
    let inferred: BTreeSet<(String, String, String, String)> =
        crate::provenance::reasoner_inferred_data_quads(store, data_graph_iri)?
            .iter()
            .map(canonical_quad_key)
            .collect();

    let mut quads: Vec<Quad> = Vec::new();
    let mut blank_node_count = 0usize;
    for quad in graph_quads(store, data_graph_iri)? {
        if inferred.contains(&canonical_quad_key(&quad)) {
            continue;
        }
        if quad_has_blank_node(&quad) {
            blank_node_count += 1;
        }
        quads.push(quad);
    }
    if blank_node_count > 0 {
        // Blank nodes re-mint on every parse, so a patch import of this text
        // would never be idempotent. Records use named UUID IRIs by design.
        tracing::warn!(
            "canonical export: {blank_node_count} quad(s) in {data_graph_iri} carry blank nodes; \
             patch imports of this text will duplicate them"
        );
    }

    quads.sort_by_cached_key(canonical_quad_key);
    let text = serialize_quads(&quads, ExportFormat::NQuads)?;
    Ok(GraphDump {
        text,
        quad_count: quads.len(),
        graphs: vec![data_graph_iri.to_string()],
    })
}

fn graph_quads(store: &Store, graph_iri: &str) -> anyhow::Result<Vec<Quad>> {
    let graph = NamedNode::new(graph_iri)
        .map_err(|e| anyhow::anyhow!("invalid export graph IRI {graph_iri:?}: {e}"))?;
    store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .map(|q| q.map_err(anyhow::Error::from))
        .collect()
}

fn quad_has_blank_node(quad: &Quad) -> bool {
    use oxigraph::model::{NamedOrBlankNode, Term};
    matches!(quad.subject, NamedOrBlankNode::BlankNode(_))
        || matches!(quad.object, Term::BlankNode(_))
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use oxigraph::model::Literal;

    /// The committed canonical dump must carry only asserted knowledge: after
    /// GROWL materializes an inferred inverse edge into the data graph, the
    /// plain graph read sees it but the canonical export filters it out (its
    /// reification tag lives in the local provenance graph and is not shipped).
    #[test]
    fn canonical_export_excludes_reasoner_materialized_quads() -> anyhow::Result<()> {
        let store = Store::new()?;
        let data = NamedNode::new("urn:test:data")?;
        let onto = NamedNode::new("urn:test:onto")?;

        let a = NamedNode::new("urn:test:a")?;
        let b = NamedNode::new("urn:test:b")?;
        let concerns = NamedNode::new("urn:test:concerns")?;
        let is_concerned_by = NamedNode::new("urn:test:isConcernedBy")?;
        let inverse_of = NamedNode::new("http://www.w3.org/2002/07/owl#inverseOf")?;
        let rdf_type = NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")?;
        let object_property = NamedNode::new("http://www.w3.org/2002/07/owl#ObjectProperty")?;
        let label = NamedNode::new("http://www.w3.org/2000/01/rdf-schema#label")?;

        // A-box: a concerns b, both labeled (labels keep them in the reasoner's
        // asserted-subject scope, as every real record is).
        store.insert(&Quad::new(
            a.clone(),
            concerns.clone(),
            b.clone(),
            data.clone(),
        ))?;
        store.insert(&Quad::new(
            a.clone(),
            label.clone(),
            Literal::new_simple_literal("a"),
            data.clone(),
        ))?;
        store.insert(&Quad::new(
            b.clone(),
            label,
            Literal::new_simple_literal("b"),
            data.clone(),
        ))?;
        // T-box: concerns owl:inverseOf isConcernedBy.
        store.insert(&Quad::new(
            concerns.clone(),
            inverse_of,
            is_concerned_by.clone(),
            onto.clone(),
        ))?;
        store.insert(&Quad::new(
            concerns,
            rdf_type.clone(),
            object_property.clone(),
            onto.clone(),
        ))?;
        store.insert(&Quad::new(
            is_concerned_by,
            rdf_type,
            object_property,
            onto.clone(),
        ))?;

        let materialized =
            crate::reasoning::enrich(&store, "urn:test:data", &["urn:test:onto"], Utc::now())?;
        assert_eq!(materialized, 1, "the inverse edge is materialized");

        // The inferred edge sits in the data graph alongside the asserted quads…
        assert_eq!(graph_quads(&store, "urn:test:data")?.len(), 4);

        // …but the canonical dump excludes it.
        let dump = export_canonical(&store, "urn:test:data")?;
        assert_eq!(dump.quad_count, 3, "only the asserted quads are exported");
        assert!(!dump.text.contains("isConcernedBy"));
        assert!(dump.text.contains("urn:test:concerns"));
        Ok(())
    }
}
