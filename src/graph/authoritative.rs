//! Lifecycle-filtered read views of the project store.
//!
//! Proposed and rejected records stay in the durable project graph for the
//! ratification inbox and the audit trail, but neither those records nor any
//! edge that touches them may reach an authoritative reader. The MCP `query`
//! tool reads a request-local view built here (issue #24); web chat reads one
//! too (issue #28), through [`LifecycleFilteredStore`], which also sends the
//! turn's own writes through to the live store.

use std::collections::HashSet;
use std::sync::Arc;

use moose::store_capability::{GraphReader, GraphWriter, Quad, QuadStream, ReadResults};
use moose::types::EngineError;
use oxigraph::model::{
    GraphName, GraphNameRef, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef,
};
use oxigraph::sparql::PreparedSparqlQuery;
use oxigraph::store::Store;

use super::lifecycle::is_hidden_from_authoritative_reads;
use super::state::AppState;
use super::PROJECT_KG_GRAPH_IRI;

/// The loaded domain and shape graphs. MOOSE derives its query vocabulary from
/// the same reader it uses for project data, so these are always copied whole.
pub const ONTOLOGY_GRAPH_IRIS: [&str; 6] = [
    crate::ontology::SE_DOMAIN_GRAPH_IRI,
    crate::ontology::SE_SHAPES_GRAPH_IRI,
    crate::ontology::ARCH_DOMAIN_GRAPH_IRI,
    crate::ontology::ARCH_SHAPES_GRAPH_IRI,
    crate::ontology::CODE_DOMAIN_GRAPH_IRI,
    crate::ontology::CODE_SHAPES_GRAPH_IRI,
];

/// Copy the store into a new in-memory store, leaving out every proposed or
/// rejected node and every quad that touches one.
///
/// `graphs` limits the copy to the named graphs listed; `None` copies the
/// whole dataset. Ontology graphs are copied unchanged; in every other graph a
/// quad whose subject or object is a hidden node is dropped. A node is hidden
/// when its lifecycle status in the project graph is proposed or rejected.
///
/// One read transaction pins both passes to the same snapshot: a proposal
/// committed between status discovery and copying must not enter the view
/// without having been classified as hidden.
pub fn authoritative_view(state: &AppState, graphs: Option<&[&str]>) -> anyhow::Result<Store> {
    let project_graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let status = NamedNodeRef::new(&state.capture.status)?;
    let transaction = state.store.start_transaction()?;

    let mut hidden = HashSet::new();
    for quad in transaction.quads_for_pattern(
        None,
        Some(status),
        None,
        Some(GraphNameRef::NamedNode(project_graph)),
    ) {
        let quad = quad?;
        let Term::Literal(value) = &quad.object else {
            continue;
        };
        if !is_hidden_from_authoritative_reads(value.value()) {
            continue;
        }
        if let NamedOrBlankNode::NamedNode(subject) = &quad.subject {
            hidden.insert(subject.as_str().to_string());
        }
    }

    let touches_hidden = |quad: &Quad| {
        let subject = match &quad.subject {
            NamedOrBlankNode::NamedNode(subject) => hidden.contains(subject.as_str()),
            NamedOrBlankNode::BlankNode(_) => false,
        };
        let object = match &quad.object {
            Term::NamedNode(object) => hidden.contains(object.as_str()),
            _ => false,
        };
        subject || object
    };
    let is_ontology = |graph: &GraphName| match graph {
        GraphName::NamedNode(graph) => ONTOLOGY_GRAPH_IRIS.contains(&graph.as_str()),
        _ => false,
    };

    let mut visible = Vec::new();
    let mut keep = |quad: Quad| {
        if is_ontology(&quad.graph_name) || !touches_hidden(&quad) {
            visible.push(quad);
        }
    };
    match graphs {
        Some(graphs) => {
            for graph_iri in graphs {
                let graph = NamedNodeRef::new(graph_iri)?;
                for quad in transaction.quads_for_pattern(
                    None,
                    None,
                    None,
                    Some(GraphNameRef::NamedNode(graph)),
                ) {
                    keep(quad?);
                }
            }
        }
        None => {
            for quad in transaction.quads_for_pattern(None, None, None, None) {
                keep(quad?);
            }
        }
    }
    drop(transaction);

    let store = Store::new()?;
    store.extend(visible)?;
    Ok(store)
}

/// A store for one MOOSE chat turn: reads come from a lifecycle-filtered view
/// ([`authoritative_view`] of the whole dataset); writes go to both the view
/// and the live store.
///
/// The view is what makes chat lifecycle-safe. The write-through is what keeps
/// it a working chat store: MOOSE persists session, clarification-overlay and
/// execution-trace RDF during the turn and reads it back, so every write must
/// be visible to later reads of this store (read-your-own-writes) and must
/// also land in the live store to outlast the turn.
///
/// Each write call is all-or-nothing, as [`GraphWriter`] requires. It is
/// applied to the in-memory view first, then to the live store; if the live
/// write fails, the view change is undone before the error is returned.
///
/// Two turns running at once each have their own view, so neither sees the
/// other's mid-turn writes until its next turn builds a fresh view.
pub struct LifecycleFilteredStore<W: GraphWriter = Store> {
    view: Store,
    live: W,
}

impl LifecycleFilteredStore<Store> {
    /// The store for one web chat turn over `state`'s project store.
    pub fn for_chat_turn(state: &AppState) -> anyhow::Result<Self> {
        Ok(Self::new(
            authoritative_view(state, None)?,
            state.store.clone(),
        ))
    }
}

impl<W: GraphWriter> LifecycleFilteredStore<W> {
    /// Pair a read view with the live writer its writes must also reach.
    pub fn new(view: Store, live: W) -> Self {
        Self { view, live }
    }

    fn view_contains(&self, quad: &Quad) -> Result<bool, EngineError> {
        self.view
            .contains(quad)
            .map_err(|e| EngineError::InternalError(format!("chat view read failed: {e}")))
    }

    /// Restore quads to the view after a failed live write. Best effort: the
    /// live store is unchanged either way, and the original error is the one
    /// the caller needs.
    fn restore(&self, quads: Vec<Quad>) {
        if let Err(e) = self.view.extend(quads) {
            tracing::warn!("chat view rollback failed: {e}");
        }
    }
}

impl<W: GraphWriter> GraphReader for LifecycleFilteredStore<W> {
    fn quads_for_pattern(
        &self,
        subject: Option<NamedOrBlankNodeRef<'_>>,
        predicate: Option<NamedNodeRef<'_>>,
        object: Option<TermRef<'_>>,
        graph_name: Option<GraphNameRef<'_>>,
    ) -> QuadStream<'_> {
        GraphReader::quads_for_pattern(&self.view, subject, predicate, object, graph_name)
    }

    fn execute_query(&self, query: PreparedSparqlQuery) -> Result<ReadResults, EngineError> {
        GraphReader::execute_query(&self.view, query)
    }

    fn reader_handle(&self) -> Arc<dyn GraphReader> {
        // A handle on the view itself (`Store` clones share their data), so a
        // read moved to a blocking thread sees the same filtered dataset and
        // this turn's writes.
        Arc::new(self.view.clone())
    }
}

impl<W: GraphWriter> GraphWriter for LifecycleFilteredStore<W> {
    fn insert_quads(&self, quads: &[Quad]) -> Result<(), EngineError> {
        if quads.is_empty() {
            return Ok(());
        }
        let mut added = Vec::new();
        for quad in quads {
            if !self.view_contains(quad)? {
                added.push(quad.clone());
            }
        }
        GraphWriter::insert_quads(&self.view, quads)?;
        if let Err(e) = self.live.insert_quads(quads) {
            if let Err(undo) = GraphWriter::remove_quads(&self.view, &added) {
                tracing::warn!("chat view rollback failed: {undo}");
            }
            return Err(e);
        }
        Ok(())
    }

    fn remove_quads(&self, quads: &[Quad]) -> Result<(), EngineError> {
        if quads.is_empty() {
            return Ok(());
        }
        let mut removed = Vec::new();
        for quad in quads {
            if self.view_contains(quad)? {
                removed.push(quad.clone());
            }
        }
        GraphWriter::remove_quads(&self.view, quads)?;
        if let Err(e) = self.live.remove_quads(quads) {
            self.restore(removed);
            return Err(e);
        }
        Ok(())
    }

    fn clear_graph(&self, graph_name: NamedNodeRef<'_>) -> Result<(), EngineError> {
        let cleared = self
            .view
            .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph_name)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EngineError::InternalError(format!("chat view read failed: {e}")))?;
        GraphWriter::clear_graph(&self.view, graph_name)?;
        if let Err(e) = self.live.clear_graph(graph_name) {
            self.restore(cleared);
            return Err(e);
        }
        Ok(())
    }
}
