//! Ratification queue: proposed links (v2.1) and proposed records (v2.2).
//!
//! Two kinds of entry share one queue:
//!
//! * **Link** — [`propose_link`] writes a `code:ProposedLink` node that carries a
//!   pending link as literals (the subject record IRI, the predicate local name,
//!   and the target substrate symbol and path) plus a `proposed` lifecycle
//!   status — never the real link edge. So a pending proposal is invisible to
//!   dossiers and the why-coverage metric (which walk only the real link
//!   predicates) until it is ratified. Accept materializes the real edge via
//!   [`link_code`]; reject flips the status and never creates an edge. Both
//!   preserve the node (and its evidence) as audit history.
//! * **Record** — an ordinary `InformationRecord` subclass instance sitting at
//!   lifecycle status `proposed` (e.g. from grounded capture). Its queue
//!   membership *is* its status: accept flips it to `accepted`, reject to
//!   `rejected`. Resolved records leave the queue view and are browsable as
//!   normal records; only `proposed` ones are queue entries.

use anyhow::Context;
use chrono::{DateTime, Utc};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term,
};

use super::capture::require_information_record;
use super::context::first_literal;
use super::link_code::{link_code, CodeSelector, LinkCodeOutcome};
use super::state::AppState;
use super::util::{local_name, mint_instance_iri};
use super::PROJECT_KG_GRAPH_IRI;

// A W3C standard datatype, not a (volatile) ontology namespace, so hardcoding it
// is not a Constraint 19bb4d8a violation — mirrors src/graph/capture.rs.
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
const PROPOSED: &str = "proposed";
const ACCEPTED: &str = "accepted";
const REJECTED: &str = "rejected";

/// Cluster-satellite classes: nodes that hang off a parent decision
/// (`weighs`→Alternative, `resultsIn`→Consequence, rationale links) and are
/// never independently ratified — their lifecycle rides the parent. They are
/// not queue entries even when a legacy default left them at `proposed`.
const SATELLITE_CLASSES: &[&str] = &["Alternative", "Consequence", "Rationale"];

/// Kind of ratification-queue entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalKind {
    /// A `code:ProposedLink` node carrying a pending record→entity edge.
    Link,
    /// An `InformationRecord` subclass instance at status `proposed`.
    Record,
}

/// The outcome of accepting a queue entry.
#[derive(Debug, Clone)]
pub enum AcceptOutcome {
    /// A link was materialized onto this entity.
    Link(LinkCodeOutcome),
    /// A proposed record was ratified in place.
    Record { iri: String, title: String },
}

/// One pending or resolved queue entry, read back from the graph. Link fields
/// (`subject_iri`/`predicate_local`/`target_symbol`/`target_path`) are empty
/// for `Record` entries; `record_class` is set only for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalSummary {
    pub iri: String,
    pub kind: ProposalKind,
    pub label: String,
    pub subject_iri: String,
    pub predicate_local: String,
    pub target_symbol: String,
    pub target_path: String,
    /// Local class name for `Record` entries (e.g. `ArchitecturalDecision`).
    pub record_class: Option<String>,
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

/// List queue entries, optionally filtered by lifecycle status (e.g.
/// "proposed"). `Link` entries are listed at every status (the resolved node
/// is the audit trail); `Record` entries appear only while `proposed` — once
/// resolved they are ordinary records, not queue history.
pub fn list_proposals(
    state: &AppState,
    status_filter: Option<&str>,
) -> anyhow::Result<Vec<ProposalSummary>> {
    let mut out = scan_link_proposals(state, status_filter)?;
    if status_filter.is_none_or(|s| s == PROPOSED) {
        out.extend(scan_record_proposals(state)?);
    }
    out.sort_by(|a, b| a.iri.cmp(&b.iri));
    Ok(out)
}

/// All `code:ProposedLink` nodes, optionally filtered by status.
fn scan_link_proposals(
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
            kind: ProposalKind::Link,
            label: first_literal(&state.store, &iri, moose::RDFS_LABEL).unwrap_or_default(),
            subject_iri: first_literal(&state.store, &iri, &terms.subject).unwrap_or_default(),
            predicate_local: first_literal(&state.store, &iri, &terms.predicate)
                .unwrap_or_default(),
            target_symbol: first_literal(&state.store, &iri, &terms.target_symbol)
                .unwrap_or_default(),
            target_path: first_literal(&state.store, &iri, &terms.target_path).unwrap_or_default(),
            record_class: None,
            evidence: first_literal(&state.store, &iri, &state.capture.description),
            status,
            iri,
        });
    }
    Ok(out)
}

/// Every `InformationRecord` subclass instance sitting at status `proposed`.
fn scan_record_proposals(state: &AppState) -> anyhow::Result<Vec<ProposalSummary>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let status_pred = NamedNodeRef::new(&state.capture.status)?;
    let proposed: Term = Literal::new_simple_literal(PROPOSED).into();
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(status_pred),
        Some(proposed.as_ref()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        // Only knowledge records qualify — ProposedLink is not an
        // InformationRecord subclass, so link nodes never double-list here.
        let Ok(class_iri) = require_information_record(state, &subject) else {
            continue;
        };
        if SATELLITE_CLASSES.contains(&local_name(&class_iri)) {
            continue;
        }
        let iri = subject.as_str().to_string();
        out.push(ProposalSummary {
            kind: ProposalKind::Record,
            label: first_literal(&state.store, &iri, &state.capture.title)
                .or_else(|| first_literal(&state.store, &iri, moose::RDFS_LABEL))
                .unwrap_or_default(),
            subject_iri: String::new(),
            predicate_local: String::new(),
            target_symbol: String::new(),
            target_path: String::new(),
            record_class: Some(local_name(&class_iri).to_string()),
            evidence: first_literal(&state.store, &iri, &state.capture.description),
            status: PROPOSED.to_string(),
            iri,
        });
    }
    Ok(out)
}

/// Count pending (`proposed`) proposals — the ratification-queue depth.
pub fn pending_count(state: &AppState) -> anyhow::Result<usize> {
    Ok(list_proposals(state, Some(PROPOSED))?.len())
}

/// Accept a pending queue entry. For a `Link`, materialize the real edge, then
/// flip to accepted — if the target symbol no longer resolves at HEAD,
/// [`link_code`] errors and the proposal stays pending (an honest skip, not a
/// silent broken link). For a `Record`, ratify it in place (`proposed` →
/// `accepted`); its queued links, if any, remain separate entries.
pub fn accept_proposal(
    state: &AppState,
    proposal_iri: &str,
    agent: &str,
) -> anyhow::Result<AcceptOutcome> {
    let terms = ProposalTerms::resolve(state)?;
    match require_pending(state, proposal_iri, &terms)? {
        ProposalKind::Link => {
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
            Ok(AcceptOutcome::Link(outcome))
        }
        ProposalKind::Record => {
            set_status(state, proposal_iri, ACCEPTED)?;
            state.note_project_write();
            Ok(AcceptOutcome::Record {
                iri: proposal_iri.to_string(),
                title: first_literal(&state.store, proposal_iri, &state.capture.title)
                    .or_else(|| first_literal(&state.store, proposal_iri, moose::RDFS_LABEL))
                    .unwrap_or_default(),
            })
        }
    }
}

/// Reject a pending queue entry: flip to rejected. A `Link` never creates an
/// edge; a `Record` keeps its content. Both are preserved for audit
/// (invariant #6).
pub fn reject_proposal(state: &AppState, proposal_iri: &str, _agent: &str) -> anyhow::Result<()> {
    let terms = ProposalTerms::resolve(state)?;
    require_pending(state, proposal_iri, &terms)?;
    set_status(state, proposal_iri, REJECTED)?;
    state.note_project_write();
    Ok(())
}

/// Precondition: `iri` is a queue entry (a `code:ProposedLink`, or an
/// `InformationRecord` subclass) currently at status `proposed`. Returns its
/// kind; writes nothing on failure.
fn require_pending(
    state: &AppState,
    iri: &str,
    terms: &ProposalTerms,
) -> anyhow::Result<ProposalKind> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let node = NamedNode::new(iri)?;
    let is_link = state
        .store
        .quads_for_pattern(
            Some(node.as_ref().into()),
            Some(NamedNodeRef::new(moose::RDF_TYPE)?),
            Some(NamedNodeRef::new(&terms.class)?.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .is_some();
    let kind = if is_link {
        ProposalKind::Link
    } else if let Ok(class_iri) = require_information_record(state, &node) {
        if SATELLITE_CLASSES.contains(&local_name(&class_iri)) {
            anyhow::bail!(
                "{iri} is a cluster-satellite record ({}); its lifecycle rides its parent decision and it cannot be ratified independently",
                local_name(&class_iri)
            );
        }
        ProposalKind::Record
    } else {
        anyhow::bail!("{iri} is not a ratification-queue entry (ProposedLink or knowledge record)");
    };
    let status = first_literal(&state.store, iri, &state.capture.status);
    if status.as_deref() != Some(PROPOSED) {
        anyhow::bail!(
            "proposal {iri} is not pending (status {})",
            status.as_deref().unwrap_or("<none>")
        );
    }
    Ok(kind)
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
