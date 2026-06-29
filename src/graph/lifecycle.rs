//! Lifecycle mutations for recorded knowledge: supersede, retract, and relate.
//! These operations preserve history and write graph edges in transactions.

use chrono::{DateTime, Utc};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad};

use super::capture::{
    capture_instance_quads, require_information_record, CaptureStamp, RecordInput,
};
use super::context::first_literal;
use super::relations::validate_relation_endpoints;
use super::state::AppState;
use super::util::{local_name, mint_instance_iri};
use super::PROJECT_KG_GRAPH_IRI;

/// Lifecycle statuses retired from the current working set.
pub const RETIRED_STATUSES: &[&str] = &["superseded", "deprecated"];

pub fn is_retired(status: &str) -> bool {
    RETIRED_STATUSES
        .iter()
        .any(|retired| status.eq_ignore_ascii_case(retired))
}

/// A decision change: the replacement to record, the decision it supersedes, and
/// the rationale (the *why*) for the change.
pub struct SupersedeInput {
    pub superseded_iri: String,
    pub new: RecordInput,
    pub rationale: String,
}

/// IRIs minted/affected by a supersede.
pub struct SupersedeOutcome {
    pub new_iri: String,
    pub rationale_iri: String,
    pub superseded_iri: String,
}

/// Record a new knowledge item that supersedes an existing one, capture *why* it
/// changed as a linked `Rationale`, and mark the old item `superseded` — preserving
/// it as history (it is never deleted). The replacement is recorded with the SAME
/// class as the superseded item (type-preserving), so the caller's `new.class_*`
/// fields are ignored. Atomic: the new item, the `Rationale` node, the
/// `supersedes`/`hasRationale` edges, and the old item's status change all commit
/// in one transaction; the entity index is invalidated once on success. The
/// superseded subject must already be an `InformationRecord` (or subclass) in the
/// project graph — else this errors and writes nothing.
pub fn supersede_decision(
    state: &AppState,
    input: &SupersedeInput,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<SupersedeOutcome> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);

    // Precondition: the superseded subject must be a recorded knowledge item — an
    // instance of :InformationRecord (or a subclass). We then mint the replacement
    // with that SAME class (type-preserving): a Requirement is superseded by a
    // Requirement, a Constraint by a Constraint, and so on. This prevents nonsense
    // cross-kind supersedes and keeps the supersedes/hasRationale edges on a class
    // whose ontology domain is :InformationRecord. (Previously hardcoded to
    // ArchitecturalDecision, which blocked superseding any other knowledge class.)
    let old_subject = NamedNode::new(&input.superseded_iri)
        .map_err(|e| anyhow::anyhow!("invalid superseded IRI {}: {e}", input.superseded_iri))?;
    let superseded_class = require_information_record(state, &old_subject)
        .map_err(|e| anyhow::anyhow!("cannot supersede {}: {e}", input.superseded_iri))?;
    let superseded_local = local_name(&superseded_class).to_string();

    // Resolve relation + class IRIs from the loaded ontology (by local name).
    let supersedes_pred = state.resolve_object_property("supersedes")?;
    let has_rationale_pred = state.resolve_object_property("hasRationale")?;
    let rationale_class = state.resolve_class("Rationale")?;

    let new_iri = mint_instance_iri(&superseded_local);
    let rationale_iri = mint_instance_iri("Rationale");
    let timestamp = when.to_rfc3339();

    // The Rationale node (the why): its description carries the reason; its title
    // is derived from the new decision's title so it reads well in listings.
    let new_title = input
        .new
        .properties
        .iter()
        .find(|(p, _)| p == &state.capture.title)
        .map(|(_, v)| v.as_str())
        .unwrap_or("decision");
    let rationale_title = format!("Rationale: {new_title}");
    let rationale_literals = vec![
        (moose::RDFS_LABEL.to_string(), rationale_title.clone()),
        (state.capture.title.clone(), rationale_title),
        (state.capture.description.clone(), input.rationale.clone()),
    ];
    // A superseding decision (and its rationale) is the now-current record, so
    // default the lifecycle status to "accepted".
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: "accepted",
    };
    let rationale_quads = capture_instance_quads(
        &state.store,
        &rationale_iri,
        &rationale_class,
        &rationale_literals,
        &[],
        &stamp,
    )?;

    // The new decision: caller literals + edges to the rationale and the old one.
    // (The caller may still override status via `new.properties`.)
    let new_edges = vec![
        (has_rationale_pred, rationale_iri.clone()),
        (supersedes_pred, input.superseded_iri.clone()),
    ];
    let new_quads = capture_instance_quads(
        &state.store,
        &new_iri,
        &superseded_class,
        &input.new.properties,
        &new_edges,
        &stamp,
    )?;

    // Flip the OLD decision's lifecycle status to "superseded": remove all its
    // existing status quads and assert the new one. Nothing else on the old
    // instance is touched — it remains as the historical record.
    let old_status_quads: Vec<Quad> = state
        .store
        .quads_for_pattern(
            Some(old_subject.as_ref().into()),
            Some(NamedNodeRef::new(&state.capture.status)?),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .collect();
    let superseded_status = Quad::new(
        old_subject.clone(),
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal("superseded"),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // One atomic transaction: insert the new decision + rationale + the old's new
    // status, and remove the old's prior status quads.
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("supersede transaction: {e}"))?;
    txn.extend(rationale_quads.iter().map(Quad::as_ref));
    txn.extend(new_quads.iter().map(Quad::as_ref));
    for quad in &old_status_quads {
        txn.remove(quad.as_ref());
    }
    txn.insert(superseded_status.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("supersede commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(SupersedeOutcome {
        new_iri,
        rationale_iri,
        superseded_iri: input.superseded_iri.clone(),
    })
}

/// IRIs affected by a retract: the record withdrawn and the `Rationale` minted.
pub struct RetractOutcome {
    pub retracted_iri: String,
    pub rationale_iri: String,
}

/// Retract a recorded knowledge item in place: flip its lifecycle status to
/// `deprecated` (so it drops out of the current working set, while the record and
/// all its other triples are preserved as history) and attach a `Rationale`
/// capturing *why* it was withdrawn. Unlike [`supersede_decision`], no replacement
/// is minted — this is the "this entry should no longer apply" transition (e.g. a
/// duplicate, or a decision abandoned without a successor). Atomic: the `Rationale`
/// node, the `hasRationale` edge, and the status change commit in one transaction;
/// the entity index is invalidated once on success. The subject must already be an
/// `InformationRecord` (or subclass) in the project graph — else this errors and
/// writes nothing.
pub fn retract_decision(
    state: &AppState,
    target_iri: &str,
    rationale: &str,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<RetractOutcome> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let subject = NamedNode::new(target_iri)
        .map_err(|e| anyhow::anyhow!("invalid target IRI {target_iri}: {e}"))?;

    // Precondition: only recorded knowledge items can be retracted (writes nothing
    // on failure, since this returns before the transaction).
    require_information_record(state, &subject)
        .map_err(|e| anyhow::anyhow!("cannot retract {target_iri}: {e}"))?;

    let has_rationale_pred = state.resolve_object_property("hasRationale")?;
    let rationale_class = state.resolve_class("Rationale")?;
    let rationale_iri = mint_instance_iri("Rationale");
    let timestamp = when.to_rfc3339();

    // Title the Rationale after the retracted record so it reads well in listings.
    let target_title = first_literal(&state.store, target_iri, &state.capture.title)
        .unwrap_or_else(|| "record".to_string());
    let rationale_title = format!("Rationale: retract {target_title}");
    let rationale_literals = vec![
        (moose::RDFS_LABEL.to_string(), rationale_title.clone()),
        (state.capture.title.clone(), rationale_title),
        (state.capture.description.clone(), rationale.to_string()),
    ];
    // The rationale is itself a current record.
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: "accepted",
    };
    let rationale_quads = capture_instance_quads(
        &state.store,
        &rationale_iri,
        &rationale_class,
        &rationale_literals,
        &[],
        &stamp,
    )?;

    // The hasRationale edge hangs off the retracted record itself — unlike a
    // supersede, there is no successor record to carry it.
    let rationale_edge = Quad::new(
        subject.clone(),
        NamedNode::new(&has_rationale_pred)?,
        NamedNode::new(&rationale_iri)?,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // Flip the target's lifecycle status to "deprecated": remove its existing
    // status quads and assert the new one. Nothing else on the record is touched.
    let old_status_quads: Vec<Quad> = state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new(&state.capture.status)?),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .collect();
    let deprecated_status = Quad::new(
        subject.clone(),
        NamedNode::new(&state.capture.status)?,
        Literal::new_simple_literal("deprecated"),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );

    // One atomic transaction: insert the rationale + its edge + the new status, and
    // remove the prior status quads.
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("retract transaction: {e}"))?;
    txn.extend(rationale_quads.iter().map(Quad::as_ref));
    txn.insert(rationale_edge.as_ref());
    for quad in &old_status_quads {
        txn.remove(quad.as_ref());
    }
    txn.insert(deprecated_status.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("retract commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(RetractOutcome {
        retracted_iri: target_iri.to_string(),
        rationale_iri,
    })
}

/// The edge written by [`relate`]: subject, the resolved predicate IRI, object.
pub struct RelateOutcome {
    pub subject_iri: String,
    pub predicate_iri: String,
    pub object_iri: String,
}

/// Assert a typed relationship edge between two existing recorded knowledge items
/// — e.g. an `AntiPattern` `violates` a `Constraint`, or an `ArchitecturalDecision`
/// `isMotivatedBy` a `Requirement` / `concerns` a component. The predicate is an
/// object property resolved from the loaded ontology by local name (keeping the
/// volatile namespace out of the code and rejecting ad-hoc, untyped edges). Both
/// endpoints must already be `InformationRecord`s (or subclasses) in the project
/// graph — else this errors and writes nothing. Atomic and idempotent: one quad is
/// inserted in a transaction (re-asserting an existing edge is a no-op) and the
/// entity index is invalidated once on success. This is the primitive that turns
/// capture from a typed *list* into a traversable *graph*: the ontology already
/// declares these relations (`supersedes`, `violates`, `isMotivatedBy`, …), but
/// only `supersede_decision` ever wrote one before.
pub fn relate(
    state: &AppState,
    subject_iri: &str,
    predicate_local: &str,
    object_iri: &str,
) -> anyhow::Result<RelateOutcome> {
    let subject = NamedNode::new(subject_iri)
        .map_err(|e| anyhow::anyhow!("invalid subject IRI {subject_iri}: {e}"))?;
    let object = NamedNode::new(object_iri)
        .map_err(|e| anyhow::anyhow!("invalid object IRI {object_iri}: {e}"))?;

    // Resolve the relation IRI from the ontology by local name. Restricting to a
    // declared object property keeps the graph well-typed and the namespace out of
    // the code (decouple-code-from-ontology-ttl).
    let predicate_iri = state.resolve_object_property(predicate_local).map_err(|e| {
        anyhow::anyhow!(
            "unknown relationship {predicate_local:?} (not an object property in the architecture ontology): {e}"
        )
    })?;

    // Preconditions: endpoint classes must satisfy the predicate's SHACL shape
    // contract. Checked before the transaction, so a bad edge writes nothing.
    validate_relation_endpoints(state, &subject, &predicate_iri, &object)?;

    let edge = Quad::new(
        subject,
        NamedNode::new(&predicate_iri)?,
        object,
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
    );
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("relate transaction: {e}"))?;
    txn.insert(edge.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("relate commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok(RelateOutcome {
        subject_iri: subject_iri.to_string(),
        predicate_iri,
        object_iri: object_iri.to_string(),
    })
}
