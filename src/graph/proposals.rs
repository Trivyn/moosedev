//! Ratification queue: proposed record→code-entity links (v2.1).
//!
//! [`propose_link`] writes a `code:ProposedLink` node that carries a pending link
//! as literals (the subject record IRI, the predicate local name, and the target
//! substrate symbol and path) plus a `proposed` lifecycle status — never the real
//! link edge. So a pending proposal is invisible to dossiers and the why-coverage
//! metric (which walk only the real link predicates) until it is ratified.
//! [`accept_proposal`] materializes the real edge via [`link_code`] and flips the
//! status to `accepted`; [`reject_proposal`] flips to `rejected` and never creates
//! an edge. Both preserve the node (and its evidence) as audit history.

use anyhow::Context;
use chrono::{DateTime, Utc};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad,
};

use super::context::first_literal;
use super::link_code::{link_code, CodeSelector, LinkCodeOutcome};
use super::state::AppState;
use super::util::mint_instance_iri;
use super::PROJECT_KG_GRAPH_IRI;

// A W3C standard datatype, not a (volatile) ontology namespace, so hardcoding it
// is not a Constraint 19bb4d8a violation — mirrors src/graph/capture.rs.
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
const PROPOSED: &str = "proposed";
const ACCEPTED: &str = "accepted";
const REJECTED: &str = "rejected";

/// One pending or resolved link proposal, read back from the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalSummary {
    pub iri: String,
    pub label: String,
    pub subject_iri: String,
    pub predicate_local: String,
    pub target_symbol: String,
    pub target_path: String,
    pub evidence: Option<String>,
    pub status: String,
}

/// Resolved `code:ProposedLink` term IRIs (Constraint 19bb4d8a: by local name).
struct ProposalTerms {
    class: String,
    subject: String,
    predicate: String,
    target_symbol: String,
    target_path: String,
}

impl ProposalTerms {
    fn resolve(state: &AppState) -> anyhow::Result<Self> {
        Ok(Self {
            class: state.resolve_code_class("ProposedLink")?,
            subject: state.resolve_code_datatype_property("proposesSubject")?,
            predicate: state.resolve_code_datatype_property("proposesPredicate")?,
            target_symbol: state.resolve_code_datatype_property("proposesTargetSymbol")?,
            target_path: state.resolve_code_datatype_property("proposesTargetPath")?,
        })
    }
}

/// Enqueue a proposed link. Writes literals only — no real edge exists until
/// [`accept_proposal`], which re-resolves `target_symbol` at HEAD.
#[allow(clippy::too_many_arguments)]
pub fn propose_link(
    state: &AppState,
    subject_iri: &str,
    predicate_local: &str,
    target_symbol: &str,
    target_path: &str,
    evidence: &str,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    let terms = ProposalTerms::resolve(state)?;
    let iri = mint_instance_iri("ProposedLink");
    let node = NamedNode::new(&iri)?;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let label = format!("{predicate_local} → {target_symbol}");
    let timestamp = when.to_rfc3339();

    let lit = |predicate: &str, value: &str| -> anyhow::Result<Quad> {
        Ok(Quad::new(
            node.clone(),
            NamedNode::new(predicate)?,
            Literal::new_simple_literal(value),
            graph.clone(),
        ))
    };

    let quads = vec![
        Quad::new(
            node.clone(),
            NamedNode::new(moose::RDF_TYPE)?,
            NamedNode::new(&terms.class)?,
            graph.clone(),
        ),
        lit(moose::RDFS_LABEL, &label)?,
        lit(&terms.subject, subject_iri)?,
        lit(&terms.predicate, predicate_local)?,
        lit(&terms.target_symbol, target_symbol)?,
        lit(&terms.target_path, target_path)?,
        lit(&state.capture.description, evidence)?,
        lit(&state.capture.author, author)?,
        lit(&state.capture.status, PROPOSED)?,
        Quad::new(
            node.clone(),
            NamedNode::new(&state.capture.timestamp)?,
            Literal::new_typed_literal(&timestamp, NamedNode::new(XSD_DATETIME)?),
            graph.clone(),
        ),
    ];

    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("propose_link transaction: {e}"))?;
    for quad in &quads {
        txn.insert(quad.as_ref());
    }
    txn.commit()
        .map_err(|e| anyhow::anyhow!("propose_link commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    state.note_project_write();
    Ok(iri)
}

/// List proposals, optionally filtered by lifecycle status (e.g. "proposed").
pub fn list_proposals(
    state: &AppState,
    status_filter: Option<&str>,
) -> anyhow::Result<Vec<ProposalSummary>> {
    let terms = ProposalTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let class = NamedNodeRef::new(&terms.class)?;
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(rdf_type),
        Some(class.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        let iri = subject.as_str().to_string();
        let status = first_literal(&state.store, &iri, &state.capture.status).unwrap_or_default();
        if let Some(want) = status_filter {
            if status != want {
                continue;
            }
        }
        out.push(ProposalSummary {
            label: first_literal(&state.store, &iri, moose::RDFS_LABEL).unwrap_or_default(),
            subject_iri: first_literal(&state.store, &iri, &terms.subject).unwrap_or_default(),
            predicate_local: first_literal(&state.store, &iri, &terms.predicate)
                .unwrap_or_default(),
            target_symbol: first_literal(&state.store, &iri, &terms.target_symbol)
                .unwrap_or_default(),
            target_path: first_literal(&state.store, &iri, &terms.target_path).unwrap_or_default(),
            evidence: first_literal(&state.store, &iri, &state.capture.description),
            status,
            iri,
        });
    }
    out.sort_by(|a, b| a.iri.cmp(&b.iri));
    Ok(out)
}

/// Count pending (`proposed`) proposals — the ratification-queue depth.
pub fn pending_count(state: &AppState) -> anyhow::Result<usize> {
    Ok(list_proposals(state, Some(PROPOSED))?.len())
}

/// Accept a pending proposal: materialize the real edge, then flip to accepted.
/// If the target symbol no longer resolves at HEAD, [`link_code`] errors and the
/// proposal stays pending (an honest skip, not a silent broken link).
pub fn accept_proposal(
    state: &AppState,
    proposal_iri: &str,
    agent: &str,
) -> anyhow::Result<LinkCodeOutcome> {
    let terms = ProposalTerms::resolve(state)?;
    require_pending(state, proposal_iri, &terms)?;

    let subject = first_literal(&state.store, proposal_iri, &terms.subject)
        .ok_or_else(|| anyhow::anyhow!("proposal {proposal_iri} has no subject"))?;
    let predicate = first_literal(&state.store, proposal_iri, &terms.predicate)
        .ok_or_else(|| anyhow::anyhow!("proposal {proposal_iri} has no predicate"))?;
    let target_symbol = first_literal(&state.store, proposal_iri, &terms.target_symbol)
        .ok_or_else(|| anyhow::anyhow!("proposal {proposal_iri} has no target symbol"))?;

    let outcome = link_code(
        state,
        &subject,
        &predicate,
        &CodeSelector::Symbol(target_symbol),
        agent,
    )
    .with_context(|| format!("cannot accept proposal {proposal_iri}"))?;

    set_status(state, proposal_iri, ACCEPTED)?;
    state.note_project_write();
    Ok(outcome)
}

/// Reject a pending proposal: flip to rejected, never create an edge. The node
/// and its evidence are preserved for audit (invariant #6).
pub fn reject_proposal(state: &AppState, proposal_iri: &str, _agent: &str) -> anyhow::Result<()> {
    let terms = ProposalTerms::resolve(state)?;
    require_pending(state, proposal_iri, &terms)?;
    set_status(state, proposal_iri, REJECTED)?;
    state.note_project_write();
    Ok(())
}

/// Precondition: `iri` is a `code:ProposedLink` currently at status `proposed`.
/// Writes nothing on failure (returns before any transaction).
fn require_pending(state: &AppState, iri: &str, terms: &ProposalTerms) -> anyhow::Result<()> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let node = NamedNode::new(iri)?;
    let is_proposal = state
        .store
        .quads_for_pattern(
            Some(node.as_ref().into()),
            Some(NamedNodeRef::new(moose::RDF_TYPE)?),
            Some(NamedNodeRef::new(&terms.class)?.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .is_some();
    if !is_proposal {
        anyhow::bail!("{iri} is not a ProposedLink");
    }
    let status = first_literal(&state.store, iri, &state.capture.status);
    if status.as_deref() != Some(PROPOSED) {
        anyhow::bail!(
            "proposal {iri} is not pending (status {})",
            status.as_deref().unwrap_or("<none>")
        );
    }
    Ok(())
}

/// Swap a subject's lifecycle status literal in one transaction (the
/// `retract_decision` idiom): remove existing status quads, insert the new one.
fn set_status(state: &AppState, subject_iri: &str, new_status: &str) -> anyhow::Result<()> {
    let project_graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNode::new(subject_iri)?;
    let old: Vec<Quad> = state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new(&state.capture.status)?),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .collect();
    let new_quad = Quad::new(
        subject.clone(),
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal(new_status),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("set_status transaction: {e}"))?;
    for quad in &old {
        txn.remove(quad.as_ref());
    }
    txn.insert(new_quad.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("set_status commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    Ok(())
}
