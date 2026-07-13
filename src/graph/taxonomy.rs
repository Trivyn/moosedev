//! Role/criticality taxonomy individuals (judgment stratum).
//!
//! The six roles (AD `b5b4762b`) and three criticality levels are project-graph
//! individuals with **deterministic** IRIs, so every project's kg.nq names them
//! identically and judgment edges diff cleanly across clones. They live in the
//! project graph (not the ontology TTL) because relation-endpoint validation
//! reads asserted types from `kg/project` only.
//!
//! `standard` criticality is the implicit derived default — the classifier
//! never proposes it — but the individual exists so a human can assert it
//! explicitly via `relate` when they want the judgment on record.

use oxigraph::model::{GraphName, Literal, NamedNode, NamedNodeRef, Quad};

use super::state::AppState;
use super::PROJECT_KG_GRAPH_IRI;

/// Role local names, per the ratified taxonomy (AD `b5b4762b`).
pub const ROLE_LOCALS: &[&str] = &[
    "core-algorithm",
    "domain-logic",
    "boundary",
    "glue",
    "boilerplate",
    "generated",
];

/// Criticality local names; `standard` is the implicit default.
pub const CRITICALITY_LOCALS: &[&str] = &["high", "standard", "low"];

/// Deterministic IRI of a role individual.
pub fn role_iri(local: &str) -> String {
    format!("https://moosedev.dev/kg/CodeRole/{local}")
}

/// Deterministic IRI of a criticality individual.
pub fn criticality_iri(local: &str) -> String {
    format!("https://moosedev.dev/kg/Criticality/{local}")
}

/// Idempotently seed the taxonomy individuals (type + label each). Returns how
/// many individuals were newly created; zero writes when all exist.
pub fn ensure_taxonomy_individuals(state: &AppState) -> anyhow::Result<usize> {
    let role_class = state.resolve_code_class("CodeRole")?;
    let criticality_class = state.resolve_code_class("Criticality")?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;

    let mut pending: Vec<Quad> = Vec::new();
    let mut created = 0usize;
    let mut seed = |iri: String, class_iri: &str, label: &str| -> anyhow::Result<()> {
        let node = NamedNode::new(&iri)?;
        let class = NamedNode::new(class_iri)?;
        let exists = state
            .store
            .quads_for_pattern(
                Some(node.as_ref().into()),
                Some(rdf_type),
                Some(class.as_ref().into()),
                Some(oxigraph::model::GraphNameRef::NamedNode(graph)),
            )
            .next()
            .is_some();
        if exists {
            return Ok(());
        }
        created += 1;
        let graph_name = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
        pending.push(Quad::new(
            node.clone(),
            NamedNode::new(moose::RDF_TYPE)?,
            class,
            graph_name.clone(),
        ));
        pending.push(Quad::new(
            node,
            NamedNode::new(moose::RDFS_LABEL)?,
            Literal::new_simple_literal(label),
            graph_name,
        ));
        Ok(())
    };

    for local in ROLE_LOCALS {
        seed(role_iri(local), &role_class, local)?;
    }
    for local in CRITICALITY_LOCALS {
        seed(criticality_iri(local), &criticality_class, local)?;
    }

    if pending.is_empty() {
        return Ok(0);
    }
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("taxonomy seed transaction: {e}"))?;
    for quad in &pending {
        txn.insert(quad.as_ref());
    }
    txn.commit()
        .map_err(|e| anyhow::anyhow!("taxonomy seed commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    state.note_project_write();
    Ok(created)
}
