//! RDF import for MOOSEDev-owned knowledge graphs.
//!
//! N-Quads is the dataset backup/restore format because it preserves named
//! graph identity. Turtle and N-Triples are graph formats, so imports in those
//! formats are loaded into one selected target graph.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphName, GraphNameRef, NamedNode, NamedNodeRef, Quad};
use oxigraph::store::Store;
use serde::Serialize;

use crate::export::ExportScope;
use crate::graph::PROJECT_KG_GRAPH_IRI;
use crate::provenance::PROVENANCE_GRAPH_IRI;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportFormat {
    #[default]
    Turtle,
    NTriples,
    NQuads,
}

impl ImportFormat {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "ttl" | "turtle" => Ok(Self::Turtle),
            "nt" | "n-triples" | "ntriples" => Ok(Self::NTriples),
            "nq" | "n-quads" | "nquads" => Ok(Self::NQuads),
            other => anyhow::bail!("unknown import format {other:?}; valid values: ttl, nt, nq"),
        }
    }

    fn rdf_format(self) -> RdfFormat {
        match self {
            Self::Turtle => RdfFormat::Turtle,
            Self::NTriples => RdfFormat::NTriples,
            Self::NQuads => RdfFormat::NQuads,
        }
    }

    fn is_dataset(self) -> bool {
        self == Self::NQuads
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    #[default]
    Patch,
    Replace,
}

impl ImportMode {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "patch" => Ok(Self::Patch),
            "replace" | "restore" | "full" => Ok(Self::Replace),
            other => anyhow::bail!("unknown import mode {other:?}; valid values: patch, replace"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphImport {
    pub format: ImportFormat,
    pub mode: ImportMode,
    pub graphs: Vec<String>,
    pub parsed_quad_count: usize,
    pub duplicate_input_count: usize,
    pub inserted_quad_count: usize,
    pub skipped_existing_count: usize,
    pub removed_quad_count: usize,
}

impl GraphImport {
    pub fn project_changed(&self) -> bool {
        self.graphs
            .iter()
            .any(|graph| graph == PROJECT_KG_GRAPH_IRI)
            && (self.inserted_quad_count > 0 || self.removed_quad_count > 0)
    }
}

pub fn import_graph(
    store: &Store,
    scope: ExportScope,
    format: ImportFormat,
    mode: ImportMode,
    text: &str,
) -> anyhow::Result<GraphImport> {
    if text.trim().is_empty() {
        anyhow::bail!("import body must not be empty");
    }

    let parsed = parse_quads(scope, format, text)?;
    if parsed.is_empty() {
        anyhow::bail!("import contained no RDF triples/quads");
    }

    let (imported, duplicate_input_count) = deduplicate_quads(parsed);
    let parsed_quad_count = imported.len();

    match mode {
        ImportMode::Patch => patch_graph(
            store,
            scope,
            format,
            imported,
            parsed_quad_count,
            duplicate_input_count,
        ),
        ImportMode::Replace => replace_graph(
            store,
            scope,
            format,
            imported,
            parsed_quad_count,
            duplicate_input_count,
        ),
    }
}

fn parse_quads(scope: ExportScope, format: ImportFormat, text: &str) -> anyhow::Result<Vec<Quad>> {
    if !format.is_dataset() && scope == ExportScope::All {
        anyhow::bail!(
            "ttl/nt imports require graph=project or graph=provenance; use format=nq for graph=all"
        );
    }

    let parser = RdfParser::from_format(format.rdf_format());
    let quads: Vec<Quad> = if format.is_dataset() {
        parser
            .for_slice(text.as_bytes())
            .map(|quad| quad.map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        let graph_iri = single_graph_iri(scope)?;
        let graph = NamedNodeRef::new(graph_iri)
            .map_err(|e| anyhow::anyhow!("invalid import graph IRI {graph_iri}: {e}"))?;
        parser
            .with_default_graph(graph)
            .for_slice(text.as_bytes())
            .map(|quad| quad.map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()?
    };

    validate_import_graphs(scope, format, &quads)?;
    Ok(quads)
}

fn deduplicate_quads(quads: Vec<Quad>) -> (Vec<Quad>, usize) {
    let raw_quad_count = quads.len();
    let mut unique = BTreeMap::new();
    for quad in quads {
        unique.entry(canonical_quad_key(&quad)).or_insert(quad);
    }
    let duplicate_input_count = raw_quad_count.saturating_sub(unique.len());
    (unique.into_values().collect(), duplicate_input_count)
}

fn patch_graph(
    store: &Store,
    scope: ExportScope,
    format: ImportFormat,
    imported: Vec<Quad>,
    parsed_quad_count: usize,
    duplicate_input_count: usize,
) -> anyhow::Result<GraphImport> {
    let mut additions = Vec::new();
    let mut skipped_existing_count = 0;

    for quad in imported {
        if quad_exists(store, &quad)? {
            skipped_existing_count += 1;
        } else {
            additions.push(quad);
        }
    }

    if !additions.is_empty() {
        let mut txn = store
            .start_transaction()
            .map_err(|e| anyhow::anyhow!("import patch transaction: {e}"))?;
        txn.extend(additions.iter().map(Quad::as_ref));
        txn.commit()
            .map_err(|e| anyhow::anyhow!("import patch commit: {e}"))?;
    }

    Ok(GraphImport {
        format,
        mode: ImportMode::Patch,
        graphs: graph_iris(scope),
        parsed_quad_count,
        duplicate_input_count,
        inserted_quad_count: additions.len(),
        skipped_existing_count,
        removed_quad_count: 0,
    })
}

fn replace_graph(
    store: &Store,
    scope: ExportScope,
    format: ImportFormat,
    imported: Vec<Quad>,
    parsed_quad_count: usize,
    duplicate_input_count: usize,
) -> anyhow::Result<GraphImport> {
    let removals = scoped_quads(store, scope)?;

    let mut txn = store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("import replace transaction: {e}"))?;
    for quad in &removals {
        txn.remove(quad.as_ref());
    }
    txn.extend(imported.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("import replace commit: {e}"))?;

    Ok(GraphImport {
        format,
        mode: ImportMode::Replace,
        graphs: graph_iris(scope),
        parsed_quad_count,
        duplicate_input_count,
        inserted_quad_count: imported.len(),
        skipped_existing_count: 0,
        removed_quad_count: removals.len(),
    })
}

fn validate_import_graphs(
    scope: ExportScope,
    format: ImportFormat,
    quads: &[Quad],
) -> anyhow::Result<()> {
    let allowed: BTreeSet<&'static str> = scope.graph_iris().into_iter().collect();

    for quad in quads {
        let graph_iri = match &quad.graph_name {
            GraphName::NamedNode(node) => node.as_str(),
            GraphName::DefaultGraph => {
                if format.is_dataset() {
                    anyhow::bail!("nq imports must use named project/provenance graphs, not the default graph");
                }
                continue;
            }
            GraphName::BlankNode(_) => {
                anyhow::bail!("imports must not target blank-node graph names");
            }
        };

        if graph_iri != PROJECT_KG_GRAPH_IRI && graph_iri != PROVENANCE_GRAPH_IRI {
            anyhow::bail!("import graph {graph_iri:?} is not MOOSEDev-owned; valid graphs are project and provenance");
        }
        if !allowed.contains(graph_iri) {
            anyhow::bail!(
                "import graph {graph_iri:?} is outside the selected scope {}",
                scope.label()
            );
        }
    }

    Ok(())
}

fn scoped_quads(store: &Store, scope: ExportScope) -> anyhow::Result<Vec<Quad>> {
    let mut quads = Vec::new();
    for graph_iri in scope.graph_iris() {
        let graph = NamedNode::new(graph_iri)
            .map_err(|e| anyhow::anyhow!("invalid scope graph IRI {graph_iri:?}: {e}"))?;
        for quad in store.quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        ) {
            quads.push(quad?);
        }
    }
    Ok(quads)
}

fn quad_exists(store: &Store, quad: &Quad) -> anyhow::Result<bool> {
    Ok(store
        .quads_for_pattern(
            Some(quad.subject.as_ref()),
            Some(quad.predicate.as_ref()),
            Some(quad.object.as_ref()),
            Some(quad.graph_name.as_ref()),
        )
        .next()
        .transpose()?
        .is_some())
}

fn single_graph_iri(scope: ExportScope) -> anyhow::Result<&'static str> {
    match scope {
        ExportScope::Project => Ok(PROJECT_KG_GRAPH_IRI),
        ExportScope::Provenance => Ok(PROVENANCE_GRAPH_IRI),
        ExportScope::All => anyhow::bail!("graph formats require a single target graph"),
    }
}

fn graph_iris(scope: ExportScope) -> Vec<String> {
    scope.graph_iris().into_iter().map(str::to_string).collect()
}

fn canonical_quad_key(quad: &Quad) -> String {
    quad.to_string()
}
