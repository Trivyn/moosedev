//! Ratification queue: proposed links (v2.1), proposed records (v2.2), and
//! proposed judgments (judgment stratum).
//!
//! Three kinds of entry share one queue:
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
//! * **Judgment** — a `ProposedLink` whose `proposesTargetIri` names a
//!   role/criticality individual (its presence IS the kind marker). Proposed
//!   by the classifier with a confidence and an escalation disposition; accept
//!   materializes `entity playsRole/hasCriticality individual` via the
//!   shape-validated [`relate`], and the accepted node remains as the edge's
//!   ratification provenance. Judgments NEVER count toward [`pending_count`]
//!   (the nudge) — escalation is inbox prominence, not a demand.

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
const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const PROPOSED: &str = "proposed";
const ACCEPTED: &str = "accepted";
const REJECTED: &str = "rejected";

/// Confidence-gate dispositions for judgment proposals (inbox prominence only).
pub const ESCALATED: &str = "escalated";
pub const AUTO_HELD: &str = "auto-held";

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
    /// A `code:ProposedLink` carrying a pending entity→role/criticality edge
    /// (marked by a `proposesTargetIri` literal).
    Judgment,
}

/// The outcome of accepting a queue entry.
#[derive(Debug, Clone)]
pub enum AcceptOutcome {
    /// A link was materialized onto this entity.
    Link(LinkCodeOutcome),
    /// A proposed record was ratified in place.
    Record { iri: String, title: String },
    /// A judgment edge was materialized.
    Judgment {
        entity_iri: String,
        predicate_local: String,
        target_iri: String,
    },
}

/// One pending or resolved queue entry, read back from the graph. Link fields
/// (`subject_iri`/`predicate_local`/`target_symbol`/`target_path`) are empty
/// for `Record` entries; `record_class` is set only for them; `target_iri`,
/// `confidence`, and `escalation` are set only for `Judgment` entries.
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
    /// Role/criticality individual IRI, for `Judgment` entries.
    pub target_iri: String,
    /// Classifier confidence literal (e.g. "0.75"), for `Judgment` entries.
    pub confidence: Option<String>,
    /// `escalated` or `auto-held`, for `Judgment` entries.
    pub escalation: Option<String>,
    /// The subject's human name — the record's title for `Link` entries, the
    /// entity's code name for `Judgment` entries. A human triages by name,
    /// never by IRI.
    pub subject_name: String,
    /// The subject entity's defining file, for `Judgment` entries.
    pub subject_path: String,
    /// Humanized target: the logical path (e.g. `graph::proposals`) for `Link`
    /// entries, the individual's local name for `Judgment` entries.
    pub target_display: String,
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
    target_iri: String,
    confidence: String,
    escalation: String,
    /// CodeEntity display properties, for humanizing judgment subjects.
    has_code_name: String,
    defined_in_path: String,
}

impl ProposalTerms {
    fn resolve(state: &AppState) -> anyhow::Result<Self> {
        Ok(Self {
            class: state.resolve_code_class("ProposedLink")?,
            subject: state.resolve_code_datatype_property("proposesSubject")?,
            predicate: state.resolve_code_datatype_property("proposesPredicate")?,
            target_symbol: state.resolve_code_datatype_property("proposesTargetSymbol")?,
            target_path: state.resolve_code_datatype_property("proposesTargetPath")?,
            target_iri: state.resolve_code_datatype_property("proposesTargetIri")?,
            confidence: state.resolve_code_datatype_property("hasConfidence")?,
            escalation: state.resolve_code_datatype_property("hasEscalation")?,
            has_code_name: state.resolve_code_datatype_property("hasCodeName")?,
            defined_in_path: state.resolve_code_datatype_property("definedInPath")?,
        })
    }
}

/// Enqueue a proposed link. Writes literals only — no real edge exists until
/// [`accept_proposal`], which re-resolves `target_symbol` at HEAD. Idempotent
/// against inbox noise: when an identical (subject, predicate, target symbol)
/// proposal is already PENDING, its IRI is returned instead of minting a
/// duplicate (repeated session captures must not multiply queue entries).
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
    if let Some(existing) = scan_link_proposals(state, Some(PROPOSED))?
        .into_iter()
        .find(|p| {
            p.kind == ProposalKind::Link
                && p.subject_iri == subject_iri
                && p.predicate_local == predicate_local
                && p.target_symbol == target_symbol
        })
    {
        return Ok(existing.iri);
    }
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

/// Enqueue a proposed judgment: `entity --playsRole/hasCriticality--> target`
/// with the classifier's confidence, escalation disposition, and rule-trace
/// evidence. Literals only — the edge is materialized by [`accept_proposal`]
/// via the shape-validated `relate`, never here.
#[allow(clippy::too_many_arguments)]
pub fn propose_judgment(
    state: &AppState,
    entity_iri: &str,
    predicate_local: &str,
    target_iri: &str,
    confidence: f64,
    escalation: &str,
    evidence: &str,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        matches!(escalation, ESCALATED | AUTO_HELD),
        "escalation must be {ESCALATED:?} or {AUTO_HELD:?}, got {escalation:?}"
    );
    let terms = ProposalTerms::resolve(state)?;
    let iri = mint_instance_iri("ProposedLink");
    let node = NamedNode::new(&iri)?;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let label = format!("{predicate_local} → {}", local_name(target_iri));
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
        lit(&terms.subject, entity_iri)?,
        lit(&terms.predicate, predicate_local)?,
        lit(&terms.target_iri, target_iri)?,
        Quad::new(
            node.clone(),
            NamedNode::new(&terms.confidence)?,
            Literal::new_typed_literal(format!("{confidence:.2}"), NamedNode::new(XSD_DECIMAL)?),
            graph.clone(),
        ),
        lit(&terms.escalation, escalation)?,
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
        .map_err(|e| anyhow::anyhow!("propose_judgment transaction: {e}"))?;
    for quad in &quads {
        txn.insert(quad.as_ref());
    }
    txn.commit()
        .map_err(|e| anyhow::anyhow!("propose_judgment commit: {e}"))?;
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
        // A proposesTargetIri literal is what makes a ProposedLink a judgment.
        let target_iri = first_literal(&state.store, &iri, &terms.target_iri).unwrap_or_default();
        let kind = if target_iri.is_empty() {
            ProposalKind::Link
        } else {
            ProposalKind::Judgment
        };
        let subject_iri = first_literal(&state.store, &iri, &terms.subject).unwrap_or_default();
        let target_symbol =
            first_literal(&state.store, &iri, &terms.target_symbol).unwrap_or_default();
        // Humanize both kinds: a triager needs names, never UUIDs or raw
        // SCIP symbols.
        let (subject_name, subject_path, target_display) = match kind {
            ProposalKind::Judgment => (
                first_literal(&state.store, &subject_iri, &terms.has_code_name)
                    .unwrap_or_else(|| local_name(&subject_iri).to_string()),
                first_literal(&state.store, &subject_iri, &terms.defined_in_path)
                    .unwrap_or_default(),
                local_name(&target_iri).to_string(),
            ),
            _ => (
                // The subject of a link is a knowledge record: show its title.
                first_literal(&state.store, &subject_iri, &state.capture.title)
                    .or_else(|| first_literal(&state.store, &subject_iri, moose::RDFS_LABEL))
                    .unwrap_or_else(|| local_name(&subject_iri).to_string()),
                String::new(),
                crate::code::substrate::symbols::logical_path(&target_symbol)
                    .unwrap_or_else(|| target_symbol.clone()),
            ),
        };
        out.push(ProposalSummary {
            kind,
            label: first_literal(&state.store, &iri, moose::RDFS_LABEL).unwrap_or_default(),
            subject_iri,
            predicate_local: first_literal(&state.store, &iri, &terms.predicate)
                .unwrap_or_default(),
            target_symbol: first_literal(&state.store, &iri, &terms.target_symbol)
                .unwrap_or_default(),
            target_path: first_literal(&state.store, &iri, &terms.target_path).unwrap_or_default(),
            record_class: None,
            target_iri,
            confidence: first_literal(&state.store, &iri, &terms.confidence),
            escalation: first_literal(&state.store, &iri, &terms.escalation),
            subject_name,
            subject_path,
            target_display,
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
            target_iri: String::new(),
            confidence: None,
            escalation: None,
            subject_name: String::new(),
            subject_path: String::new(),
            target_display: String::new(),
            evidence: first_literal(&state.store, &iri, &state.capture.description),
            status: PROPOSED.to_string(),
            iri,
        });
    }
    Ok(out)
}

/// Count pending (`proposed`) proposals that warrant a nudge: links and
/// records only. Judgments — escalated or auto-held — NEVER count here
/// (never-nudge amendment of Consequence `be097082`): escalation is inbox
/// prominence, and a machine's classification backlog must not page a human.
pub fn pending_count(state: &AppState) -> anyhow::Result<usize> {
    Ok(list_proposals(state, Some(PROPOSED))?
        .iter()
        .filter(|proposal| proposal.kind != ProposalKind::Judgment)
        .count())
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

            // A link must not resurrect a record a human already declined: a
            // rejected/deprecated subject would otherwise re-enter dossiers
            // and why-coverage through the materialized edge.
            let subject_status =
                first_literal(&state.store, &subject, &state.capture.status).unwrap_or_default();
            if subject_status.eq_ignore_ascii_case(REJECTED)
                || subject_status.eq_ignore_ascii_case("deprecated")
            {
                anyhow::bail!(
                    "cannot accept {proposal_iri}: its subject record {subject} is {subject_status}; \
                     reject this link instead"
                );
            }

            let outcome = link_code(
                state,
                &subject,
                &predicate,
                &CodeSelector::Symbol(target_symbol),
                agent,
            )
            .with_context(|| format!("cannot accept proposal {proposal_iri}"))?;

            set_status(state, proposal_iri, ACCEPTED)?;
            stamp_resolver(state, proposal_iri, agent);
            state.note_project_write();
            Ok(AcceptOutcome::Link(outcome))
        }
        ProposalKind::Record => {
            set_status(state, proposal_iri, ACCEPTED)?;
            stamp_resolver(state, proposal_iri, agent);
            state.note_project_write();
            Ok(AcceptOutcome::Record {
                iri: proposal_iri.to_string(),
                title: first_literal(&state.store, proposal_iri, &state.capture.title)
                    .or_else(|| first_literal(&state.store, proposal_iri, moose::RDFS_LABEL))
                    .unwrap_or_default(),
            })
        }
        ProposalKind::Judgment => {
            let entity_iri = first_literal(&state.store, proposal_iri, &terms.subject)
                .ok_or_else(|| anyhow::anyhow!("judgment {proposal_iri} has no subject"))?;
            let predicate_local = first_literal(&state.store, proposal_iri, &terms.predicate)
                .ok_or_else(|| anyhow::anyhow!("judgment {proposal_iri} has no predicate"))?;
            let target_iri = first_literal(&state.store, proposal_iri, &terms.target_iri)
                .ok_or_else(|| anyhow::anyhow!("judgment {proposal_iri} has no target IRI"))?;

            // Shape-validated, idempotent; a wrong-range target fails here and
            // the judgment stays pending (honest skip).
            super::lifecycle::relate(state, &entity_iri, &predicate_local, &target_iri)
                .with_context(|| format!("cannot accept judgment {proposal_iri}"))?;

            set_status(state, proposal_iri, ACCEPTED)?;
            stamp_resolver(state, proposal_iri, agent);
            state.note_project_write();
            Ok(AcceptOutcome::Judgment {
                entity_iri,
                predicate_local,
                target_iri,
            })
        }
    }
}

/// Recategorize a pending judgment: the human corrects the classifier's
/// target (spec §6's "edit" affordance). The original proposal is rejected
/// (preserved for audit with its rule trace), and a HUMAN-authored judgment
/// with the corrected target is minted and immediately accepted — the human
/// assertion is the ratification, no second round-trip. Errors before any
/// write when the entry is not a pending judgment or the target is not a
/// taxonomy individual of the judgment's axis.
pub fn recategorize_judgment(
    state: &AppState,
    proposal_iri: &str,
    new_target_local: &str,
    agent: &str,
) -> anyhow::Result<AcceptOutcome> {
    let terms = ProposalTerms::resolve(state)?;
    let kind = require_pending(state, proposal_iri, &terms)?;
    anyhow::ensure!(
        kind == ProposalKind::Judgment,
        "{proposal_iri} is not a judgment proposal; only judgments can be recategorized"
    );

    let subject = first_literal(&state.store, proposal_iri, &terms.subject)
        .ok_or_else(|| anyhow::anyhow!("judgment {proposal_iri} has no subject"))?;
    let predicate = first_literal(&state.store, proposal_iri, &terms.predicate)
        .ok_or_else(|| anyhow::anyhow!("judgment {proposal_iri} has no predicate"))?;
    let old_target = first_literal(&state.store, proposal_iri, &terms.target_iri)
        .map(|iri| local_name(&iri).to_string())
        .unwrap_or_default();

    // The corrected target must belong to the judgment's own axis.
    let new_target_iri = match predicate.as_str() {
        "playsRole" if super::taxonomy::ROLE_LOCALS.contains(&new_target_local) => {
            super::taxonomy::role_iri(new_target_local)
        }
        "hasCriticality" if super::taxonomy::CRITICALITY_LOCALS.contains(&new_target_local) => {
            super::taxonomy::criticality_iri(new_target_local)
        }
        _ => anyhow::bail!(
            "{new_target_local:?} is not a valid {predicate} target (roles: {:?}; criticalities: {:?})",
            super::taxonomy::ROLE_LOCALS,
            super::taxonomy::CRITICALITY_LOCALS
        ),
    };
    super::taxonomy::ensure_taxonomy_individuals(state)?;

    let old_evidence =
        first_literal(&state.store, proposal_iri, &state.capture.description).unwrap_or_default();
    let evidence = format!(
        "recategorized by {agent} from '{old_target}' (classifier evidence: {old_evidence})"
    );
    // Human-authored: confidence 1.0; escalation is moot once accepted.
    let corrected = propose_judgment(
        state,
        &subject,
        &predicate,
        &new_target_iri,
        1.0,
        ESCALATED,
        &evidence,
        agent,
        Utc::now(),
    )?;
    let outcome = accept_proposal(state, &corrected, agent)
        .with_context(|| format!("cannot materialize the recategorized judgment {corrected}"))?;
    reject_proposal(state, proposal_iri, agent)?;
    Ok(outcome)
}

/// Reject a pending queue entry: flip to rejected. A `Link` never creates an
/// edge; a `Record` keeps its content. Both are preserved for audit
/// (invariant #6), and the rejecter is stamped as edit provenance.
///
/// Rejecting a `Record` cascade-rejects its own pending link proposals: a
/// declined record's links are dead by definition, and leaving them pending
/// would let a later accept resurrect the record into dossiers.
pub fn reject_proposal(state: &AppState, proposal_iri: &str, agent: &str) -> anyhow::Result<()> {
    let terms = ProposalTerms::resolve(state)?;
    let kind = require_pending(state, proposal_iri, &terms)?;
    set_status(state, proposal_iri, REJECTED)?;
    stamp_resolver(state, proposal_iri, agent);
    if kind == ProposalKind::Record {
        for dependent in scan_link_proposals(state, Some(PROPOSED))? {
            if dependent.kind == ProposalKind::Link && dependent.subject_iri == proposal_iri {
                set_status(state, &dependent.iri, REJECTED)?;
                stamp_resolver(state, &dependent.iri, agent);
            }
        }
    }
    state.note_project_write();
    Ok(())
}

/// Best-effort: stamp who resolved (accepted/rejected) a queue entry as PROV
/// edit provenance on the node — the node's `author` literal stays the
/// PROPOSER (e.g. the classifier); this records the human decision.
fn stamp_resolver(state: &AppState, proposal_iri: &str, agent: &str) {
    if let Err(e) = crate::provenance::record_provenance(&state.store, proposal_iri, agent) {
        tracing::warn!("failed to stamp resolver provenance on {proposal_iri}: {e}");
    }
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
        // A proposesTargetIri literal marks the judgment kind.
        if first_literal(&state.store, iri, &terms.target_iri).is_some() {
            ProposalKind::Judgment
        } else {
            ProposalKind::Link
        }
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

/// One judgment (proposed or ratified) about a code entity, read back from its
/// queue node — the node doubles as the materialized edge's provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgmentSummary {
    /// The ProposedLink node carrying this judgment.
    pub proposal_iri: String,
    /// `playsRole` or `hasCriticality`.
    pub predicate_local: String,
    /// Local name of the target individual (e.g. `boundary`, `high`).
    pub target_local: String,
    /// `proposed` (advisory) or `accepted` (ratified; the edge exists).
    pub status: String,
    /// Classifier confidence literal, e.g. "0.75".
    pub confidence: Option<String>,
    /// `escalated` or `auto-held`.
    pub escalation: Option<String>,
    pub author: String,
    pub timestamp: String,
}

/// All non-rejected judgments grouped by their subject entity IRI — the bulk
/// read the dossier and code lens use (one scan per request, not per entity).
pub fn judgments_by_subject(
    state: &AppState,
) -> anyhow::Result<std::collections::BTreeMap<String, Vec<JudgmentSummary>>> {
    let terms = ProposalTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let class = NamedNodeRef::new(&terms.class)?;
    let mut out: std::collections::BTreeMap<String, Vec<JudgmentSummary>> =
        std::collections::BTreeMap::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(rdf_type),
        Some(class.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(node) = quad.subject else {
            continue;
        };
        let iri = node.as_str();
        let Some(target_iri) = first_literal(&state.store, iri, &terms.target_iri) else {
            continue; // link proposal, not a judgment
        };
        let status = first_literal(&state.store, iri, &state.capture.status).unwrap_or_default();
        if status == REJECTED {
            continue;
        }
        let Some(subject) = first_literal(&state.store, iri, &terms.subject) else {
            continue;
        };
        out.entry(subject).or_default().push(JudgmentSummary {
            proposal_iri: iri.to_string(),
            predicate_local: first_literal(&state.store, iri, &terms.predicate).unwrap_or_default(),
            target_local: local_name(&target_iri).to_string(),
            status,
            confidence: first_literal(&state.store, iri, &terms.confidence),
            escalation: first_literal(&state.store, iri, &terms.escalation),
            author: first_literal(&state.store, iri, &state.capture.author).unwrap_or_default(),
            timestamp: first_literal(&state.store, iri, &state.capture.timestamp)
                .unwrap_or_default(),
        });
    }
    for judgments in out.values_mut() {
        judgments.sort_by(|a, b| {
            a.predicate_local
                .cmp(&b.predicate_local)
                .then_with(|| a.proposal_iri.cmp(&b.proposal_iri))
        });
    }
    Ok(out)
}

/// Non-rejected judgments for one entity.
pub fn judgments_for_entity(
    state: &AppState,
    entity_iri: &str,
) -> anyhow::Result<Vec<JudgmentSummary>> {
    Ok(judgments_by_subject(state)?
        .remove(entity_iri)
        .unwrap_or_default())
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
