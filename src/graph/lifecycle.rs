//! Lifecycle mutations for recorded knowledge: supersede, retract, and relate.
//! These operations preserve history and write graph edges in transactions.

use chrono::{DateTime, Utc};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad};

use super::capture::{
    capture_instance_quads, plan_relation_args, require_information_record, AppliedEdge,
    CaptureStamp, RecordInput,
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

/// Whether a lifecycle status admits a record into the authoritative working
/// set. `proposed` records live in the ratification inbox, not in recall —
/// unratified knowledge must never influence an agent as if it were the
/// maintainers' recorded truth — and `rejected`/retired records were declined
/// or replaced. Records with no status literal (pre-lifecycle legacy) stay in.
pub fn in_working_set(status: &str) -> bool {
    !(status.eq_ignore_ascii_case("proposed")
        || status.eq_ignore_ascii_case("rejected")
        || is_retired(status))
}

/// A decision change: the replacement to record, the decision it supersedes, and
/// the rationale (the *why*) for the change.
pub struct SupersedeInput {
    pub superseded_iri: String,
    pub new: RecordInput,
    pub rationale: String,
}

/// IRIs minted/affected by a supersede.
#[derive(Debug, Clone)]
pub struct SupersedeOutcome {
    pub new_iri: String,
    pub rationale_iri: String,
    pub superseded_iri: String,
}

/// A supersede plus the caller-authored replacement relations and the prior
/// semantic relations that remain attached only to the retired record.
#[derive(Debug, Clone)]
pub struct SupersedeWithRelationsOutcome {
    pub lifecycle: SupersedeOutcome,
    pub applied_edges: Vec<AppliedEdge>,
    pub not_carried_edges: Vec<AppliedEdge>,
}

#[derive(Clone, Copy)]
enum SemanticRelationReport {
    Disabled,
    IncludeOmissions,
}

/// Record a new knowledge item that supersedes an existing one, capture *why* it
/// changed as a linked `Rationale`, and mark the old item `superseded` — preserving
/// it as history (it is never deleted). The replacement is recorded with the SAME
/// class as the superseded item (type-preserving), so the caller's `new.class_*`
/// fields are ignored. Atomic: the new item, the `Rationale` node, the
/// `supersedes`/`isSupersededBy`/`hasRationale` edges, and the old item's status
/// change all commit in one transaction; the entity index is invalidated once
/// on success. The superseded subject must already be an `InformationRecord`
/// (or subclass) in the project graph — else this errors and writes nothing.
pub fn supersede_decision(
    state: &AppState,
    input: &SupersedeInput,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<SupersedeOutcome> {
    Ok(supersede_decision_inner(
        state,
        input,
        &[],
        SemanticRelationReport::Disabled,
        author,
        when,
    )?
    .lifecycle)
}

/// Relation-aware form of [`supersede_decision`]. Caller-supplied relations are
/// resolved and validated before the lifecycle transaction, then asserted on the
/// replacement in that same transaction. No relation is inherited implicitly.
///
/// The outcome reports semantic outgoing relations found on the superseded
/// record but not explicitly reasserted. The snapshot includes current GROWL
/// inverse materialization and excludes lifecycle/rationale bookkeeping.
pub fn supersede_decision_with_relation_args(
    state: &AppState,
    input: &SupersedeInput,
    relations: &[(String, String)],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<SupersedeWithRelationsOutcome> {
    supersede_decision_inner(
        state,
        input,
        relations,
        SemanticRelationReport::IncludeOmissions,
        author,
        when,
    )
}

fn supersede_decision_inner(
    state: &AppState,
    input: &SupersedeInput,
    relations: &[(String, String)],
    relation_report: SemanticRelationReport,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<SupersedeWithRelationsOutcome> {
    if matches!(relation_report, SemanticRelationReport::IncludeOmissions) {
        // The review list must include useful inverse relations. Refresh the
        // closure before reading the old record; the post-write hook marks it
        // stale again. Legacy callers deliberately skip this whole-graph read.
        state.try_ensure_enriched()?;
    }
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

    // Inline relation validation needs the replacement's real, inherited class,
    // not the ignored placeholders carried by SupersedeInput::new.
    let effective_new = RecordInput {
        class_iri: superseded_class.clone(),
        class_local: superseded_local.clone(),
        properties: input.new.properties.clone(),
    };
    reject_managed_relation_args(state, relations)?;
    let (planned_edges, applied_edges) = plan_relation_args(state, &effective_new, relations)?;

    // Resolve relation + class IRIs from the loaded ontology (by local name).
    let supersedes_pred = state.resolve_object_property("supersedes")?;
    let is_superseded_by_pred = state.resolve_object_property("isSupersededBy")?;
    let has_rationale_pred = state.resolve_object_property("hasRationale")?;
    let rationale_class = state.resolve_class("Rationale")?;

    let not_carried_edges = match relation_report {
        SemanticRelationReport::Disabled => Vec::new(),
        SemanticRelationReport::IncludeOmissions => {
            let mut edges = semantic_outgoing_edges(state, &old_subject);
            edges.retain(|old| {
                !applied_edges.iter().any(|applied| {
                    old.predicate_local == applied.predicate_local
                        && old.object_iri == applied.object_iri
                })
            });
            edges
        }
    };

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
    let mut new_edges = planned_edges;
    for edge in [
        (has_rationale_pred, rationale_iri.clone()),
        (supersedes_pred, input.superseded_iri.clone()),
    ] {
        if !new_edges.contains(&edge) {
            new_edges.push(edge);
        }
    }
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
    // Persist the declared OWL inverse explicitly. Project-graph consumers such
    // as the ADR renderer traverse from the retired record to its successor and
    // must remain correct even when enrichment has not run or is unavailable.
    let successor_link = Quad::new(
        old_subject.clone(),
        NamedNode::new(is_superseded_by_pred)?,
        NamedNode::new(&new_iri)?,
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
    txn.insert(successor_link.as_ref());
    txn.commit()
        .map_err(|e| anyhow::anyhow!("supersede commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    // The transaction changed the evidence from which GROWL derives inverses.
    // Mark it stale at the commit boundary, before any caller can await or do
    // follow-up work; the MCP post-write hook handles export/memo bookkeeping.
    state.mark_inferred_stale();

    Ok(SupersedeWithRelationsOutcome {
        lifecycle: SupersedeOutcome {
            new_iri,
            rationale_iri,
            superseded_iri: input.superseded_iri.clone(),
        },
        applied_edges,
        not_carried_edges,
    })
}

fn reject_managed_relation_args(
    state: &AppState,
    relations: &[(String, String)],
) -> anyhow::Result<()> {
    let managed = [
        "supersedes",
        "isSupersededBy",
        "hasRationale",
        "isRationaleFor",
    ]
    .into_iter()
    .map(|local| state.resolve_object_property(local))
    .collect::<anyhow::Result<Vec<_>>>()?;

    for (predicate_local, _) in relations {
        if state
            .resolve_object_property(predicate_local)
            .is_ok_and(|predicate_iri| managed.contains(&predicate_iri))
        {
            anyhow::bail!(
                "relationship {predicate_local:?} is managed by supersede_decision and cannot be supplied in `relations`"
            );
        }
    }
    Ok(())
}

/// Current semantic object-property edges asserted or inferred from `subject`.
/// Architecture/code vocabularies define the reportable domain surface; this
/// avoids leaking generic alignment superproperties into repair suggestions.
fn semantic_outgoing_edges(state: &AppState, subject: &NamedNode) -> Vec<AppliedEdge> {
    const LIFECYCLE_RELATIONS: &[&str] = &[
        "supersedes",
        "isSupersededBy",
        "hasRationale",
        "isRationaleFor",
    ];

    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut edges = state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .filter_map(|quad| {
            let object = match quad.object {
                oxigraph::model::Term::NamedNode(object) => object,
                _ => return None,
            };
            let predicate_iri = quad.predicate.as_str();
            let is_domain_relation = state
                .arch_vocab
                .object_properties
                .iter()
                .chain(state.code_vocab.object_properties.iter())
                .any(|property| property.iri == predicate_iri);
            if !is_domain_relation {
                return None;
            }
            let predicate_local = local_name(predicate_iri).to_string();
            if LIFECYCLE_RELATIONS.contains(&predicate_local.as_str()) {
                return None;
            }
            Some(AppliedEdge {
                predicate_local,
                object_iri: object.as_str().to_string(),
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        (&left.predicate_local, &left.object_iri).cmp(&(&right.predicate_local, &right.object_iri))
    });
    edges.dedup_by(|left, right| {
        left.predicate_local == right.predicate_local && left.object_iri == right.object_iri
    });
    edges
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
