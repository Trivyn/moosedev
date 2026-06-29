//! Typed record capture and inline relation assertion.
//! The write path stays atomic: relation legality is checked before any record is persisted.

use chrono::{DateTime, Utc};
use moose::kg::{
    assert_instance, AssertionLiteral, DatatypeAssertion, InstanceAssertion, ObjectAssertion,
};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, Quad, Term};
use oxigraph::store::Store;

use super::context::resolve_record_exact_all;
use super::relations::{validate_relation_for_subject_types, EdgeDirection};
use super::state::{
    AppState, CapturePredicates, DEFAULT_LIFECYCLE_STATUS, LABEL_PROPERTY_LOCAL, XSD_DATETIME,
};
use super::util::{is_subclass_of, local_name, mint_instance_iri};
use super::PROJECT_KG_GRAPH_IRI;

/// A validated knowledge item to record: a resolved class plus its literal
/// property assertions as `(predicate_iri, value)` pairs. Domain-neutral — the
/// caller maps its fields to predicates, so new knowledge classes need no change
/// to the writer below.
pub struct RecordInput {
    pub class_iri: String,
    pub class_local: String,
    pub properties: Vec<(String, String)>,
}

/// Record a typed knowledge instance into the durable project KG via MOOSE's
/// cache-coherent assertion primitive. Returns the minted subject IRI.
pub fn record_instance(
    state: &AppState,
    input: &RecordInput,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    record_instance_with_relations(state, input, &[], author, when)
}

/// Like [`record_instance`], but also writes IRI-valued relations
/// `(predicate_iri, object_iri)` — e.g. `isMotivatedBy`, `supersedes`. This is the
/// enabling layer for typed links between records (invariant #2): the writer
/// previously always passed an empty `object_props` slice, so no relation could be
/// captured. Resolve `predicate_iri` from the ontology via
/// [`AppState::resolve_object_property`].
pub fn record_instance_with_relations(
    state: &AppState,
    input: &RecordInput,
    object_props: &[(String, String)],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<String> {
    let subject = mint_instance_iri(&input.class_local);
    let timestamp = when.to_rfc3339();
    let literal_props = normalize_capture_literal_props(
        &state.store,
        &state.capture,
        &input.class_iri,
        &input.properties,
    );
    let mut datatype_props: Vec<DatatypeAssertion> = literal_props
        .iter()
        .map(|(predicate, value)| DatatypeAssertion {
            predicate_iri: predicate.as_str(),
            literal: AssertionLiteral::Simple(value.as_str()),
        })
        .collect();
    if !has_literal_property(&literal_props, &state.capture.author) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.author.as_str(),
            literal: AssertionLiteral::Simple(author),
        });
    }
    if !has_literal_property(&literal_props, &state.capture.timestamp) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.timestamp.as_str(),
            literal: AssertionLiteral::Typed {
                value: timestamp.as_str(),
                datatype_iri: XSD_DATETIME,
            },
        });
    }
    if !has_literal_property(&literal_props, &state.capture.status) {
        datatype_props.push(DatatypeAssertion {
            predicate_iri: state.capture.status.as_str(),
            literal: AssertionLiteral::Simple(DEFAULT_LIFECYCLE_STATUS),
        });
    }

    let object_assertions: Vec<ObjectAssertion> = object_props
        .iter()
        .map(|(predicate, object)| ObjectAssertion {
            predicate_iri: predicate.as_str(),
            object_iri: object.as_str(),
        })
        .collect();

    let assertion = InstanceAssertion {
        graph_iri: PROJECT_KG_GRAPH_IRI,
        subject_iri: &subject,
        class_iri: &input.class_iri,
        datatype_props: &datatype_props,
        object_props: &object_assertions,
    };

    assert_instance(&state.store, &state.entity_index, &assertion, None)
        .map_err(|e| anyhow::anyhow!("assert_instance: {e:?}"))?;
    Ok(subject)
}

/// A forward relation written by [`record_instance_with_relation_args`].
#[derive(Debug, Clone)]
pub struct AppliedEdge {
    pub predicate_local: String,
    pub object_iri: String,
}

type PlannedObjectProps = Vec<(String, String)>;
type PlannedEdges = Vec<AppliedEdge>;

/// Result of a capture that may also assert inline relations.
#[derive(Debug, Clone)]
pub struct RecordOutcome {
    pub iri: String,
    pub applied_edges: Vec<AppliedEdge>,
}

/// Like [`record_instance`], but also asserts forward inline relations from the new
/// record (subject = the record being created). Each `(predicate_local, target)`
/// is resolved and SHACL-validated against the new record's class *before* any
/// write, so an invalid relation fails the whole capture — no orphan record is left
/// behind (validation precedes the single `assert_instance`). `target` is an
/// existing record IRI or its exact (normalized) title; an ambiguous title is
/// rejected (Req 5565038e). Identical `(predicate, object)` pairs are deduped.
pub fn record_instance_with_relation_args(
    state: &AppState,
    input: &RecordInput,
    relations: &[(String, String)],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<RecordOutcome> {
    let (object_props, applied_edges) = plan_relation_args(state, input, relations)?;
    let iri = record_instance_with_relations(state, input, &object_props, author, when)?;
    Ok(RecordOutcome { iri, applied_edges })
}

fn plan_relation_args(
    state: &AppState,
    input: &RecordInput,
    relations: &[(String, String)],
) -> anyhow::Result<(PlannedObjectProps, PlannedEdges)> {
    let subject_types = std::slice::from_ref(&input.class_iri);
    let mut object_props: Vec<(String, String)> = Vec::new();
    let mut applied_edges: Vec<AppliedEdge> = Vec::new();

    for (predicate_local, target) in relations {
        let predicate_iri = state.resolve_object_property(predicate_local).map_err(|e| {
            anyhow::anyhow!(
                "unknown relationship {predicate_local:?} (not an object property in the architecture ontology): {e}"
            )
        })?;
        let object_iri = resolve_relation_target(state, target)?;
        let object = NamedNode::new(&object_iri)
            .map_err(|e| anyhow::anyhow!("invalid target IRI {object_iri:?}: {e}"))?;
        validate_relation_for_subject_types(
            state,
            subject_types,
            &input.class_local,
            &predicate_iri,
            &object,
        )?;
        if object_props
            .iter()
            .any(|(p, o)| p == &predicate_iri && o == &object_iri)
        {
            continue; // dedup identical (predicate, object) pairs
        }
        applied_edges.push(AppliedEdge {
            predicate_local: predicate_local.clone(),
            object_iri: object_iri.clone(),
        });
        object_props.push((predicate_iri, object_iri));
    }

    Ok((object_props, applied_edges))
}

/// One inline cluster slot for [`record_decision_with_cluster`]: the `predicate_local`
/// that links the decision to a freshly-minted node of `range_class_local`, plus the
/// caller-supplied `labels` (one minted node per non-empty label).
pub struct ClusterSlot<'a> {
    pub predicate_local: &'a str,
    pub range_class_local: &'a str,
    pub labels: &'a [String],
}

/// A node minted-and-linked as part of a decision's cluster (surfaced in the response).
#[derive(Debug, Clone)]
pub struct MintedClusterNode {
    pub predicate_local: String,
    pub iri: String,
}

/// Record a decision (with its inline `relations`, atomically as in
/// [`record_instance_with_relation_args`]), then mint each cluster slot's labels as
/// typed nodes linked from the decision via the slot predicate — e.g.
/// `weighs`→`Alternative`, `resultsIn`→`Consequence`. This lets a decision carry its
/// decision-specific cluster in ONE capture call instead of the mint-then-`relate`
/// dance that left `weighs`/`resultsIn` empty in practice (small models don't do the
/// multi-call follow-up).
///
/// Each slot's `predicate_local`→`range_class_local` legality is checked against the
/// SHACL catalogue BEFORE anything is written, so a bad mapping (or cluster fields on a
/// class the shape doesn't allow that edge from) fails the whole call with no record
/// left behind. Minted nodes reuse [`record_instance`] + the SHACL-validated
/// [`relate`](super::lifecycle::relate); each label is written as both `rdfs:label` and
/// the class title property, and the node uses the default lifecycle status.
pub fn record_decision_with_cluster(
    state: &AppState,
    input: &RecordInput,
    relations: &[(String, String)],
    cluster: &[ClusterSlot<'_>],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<(RecordOutcome, Vec<MintedClusterNode>)> {
    // Up-front: resolve + legality-check every non-empty slot before recording
    // anything, so an illegal field/class mapping writes no record at all.
    let mut planned: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for slot in cluster {
        let labels: Vec<String> = slot
            .labels
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if labels.is_empty() {
            continue;
        }
        let range_class_iri = state.resolve_class(slot.range_class_local)?;
        let legal =
            state
                .catalogue
                .legal_predicates(&state.store, &input.class_iri, &range_class_iri);
        if !legal.iter().any(|e| {
            e.predicate_local == slot.predicate_local && e.direction == EdgeDirection::Forward
        }) {
            anyhow::bail!(
                "{} cannot carry {}: not a legal {}->{} edge in the architecture ontology",
                input.class_local,
                slot.predicate_local,
                input.class_local,
                slot.range_class_local
            );
        }
        let predicate_iri = state.resolve_object_property(slot.predicate_local)?;
        planned.push((
            slot.predicate_local.to_string(),
            predicate_iri,
            range_class_iri,
            labels,
        ));
    }

    let (decision_edges, applied_edges) = plan_relation_args(state, input, relations)?;
    let decision_iri = mint_instance_iri(&input.class_local);
    let timestamp = when.to_rfc3339();
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: DEFAULT_LIFECYCLE_STATUS,
    };

    let mut quads = capture_instance_quads(
        &state.store,
        &decision_iri,
        &input.class_iri,
        &input.properties,
        &decision_edges,
        &stamp,
    )?;
    let mut minted: Vec<MintedClusterNode> = Vec::new();
    for (predicate_local, predicate_iri, range_class_iri, labels) in &planned {
        let range_class_local = local_name(range_class_iri).to_string();
        for label in labels {
            let aux_iri = mint_instance_iri(&range_class_local);
            let aux_input = RecordInput {
                class_iri: range_class_iri.clone(),
                class_local: range_class_local.clone(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), label.clone()),
                    (state.capture.title.clone(), label.clone()),
                ],
            };
            quads.extend(capture_instance_quads(
                &state.store,
                &aux_iri,
                range_class_iri,
                &aux_input.properties,
                &[],
                &stamp,
            )?);
            quads.push(Quad::new(
                NamedNode::new(&decision_iri)?,
                NamedNode::new(predicate_iri)?,
                NamedNode::new(&aux_iri)?,
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?),
            ));
            minted.push(MintedClusterNode {
                predicate_local: predicate_local.clone(),
                iri: aux_iri,
            });
        }
    }

    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("cluster capture transaction: {e}"))?;
    txn.extend(quads.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("cluster capture commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    Ok((
        RecordOutcome {
            iri: decision_iri,
            applied_edges,
        },
        minted,
    ))
}

/// Resolve an inline-relation target to an existing record IRI. Accepts an exact
/// project record IRI, or an exact (normalized) title — rejecting "not found" and
/// "ambiguous title" so a typo or a duplicate title can't silently mislink.
fn resolve_relation_target(state: &AppState, target: &str) -> anyhow::Result<String> {
    // An exact IRI of an existing record wins (titles with spaces fail IRI parse
    // and fall through to title resolution).
    if let Ok(node) = NamedNode::new(target) {
        if require_information_record(state, &node).is_ok() {
            return Ok(target.to_string());
        }
    }
    let matches = resolve_record_exact_all(state, target);
    match matches.len() {
        0 => anyhow::bail!(
            "relation target {target:?} matches no recorded item (by IRI or exact title)"
        ),
        1 => Ok(matches.into_iter().next().unwrap().0),
        n => anyhow::bail!(
            "relation target {target:?} is ambiguous — {n} records share that title; pass the IRI instead"
        ),
    }
}

/// Check whether the caller already supplied a property so write-path defaults do
/// not duplicate explicit values.
fn has_literal_property(literal_props: &[(String, String)], predicate_iri: &str) -> bool {
    literal_props
        .iter()
        .any(|(predicate, _)| predicate == predicate_iri)
}

/// Mirror the canonical `rdfs:label` value into the class-specific datatype
/// property identified by the ontology's `labelProperty` annotation. This keeps
/// retrieval label-driven while satisfying shapes such as
/// `SystemComponent.hasComponentName minCount 1`.
fn normalize_capture_literal_props(
    store: &Store,
    capture: &CapturePredicates,
    class_iri: &str,
    literal_props: &[(String, String)],
) -> Vec<(String, String)> {
    let label_mirror_property = class_label_mirror_property_iri(store, capture, class_iri);
    let title_value = literal_props
        .iter()
        .find(|(predicate, _)| predicate == &label_mirror_property)
        .or_else(|| {
            literal_props
                .iter()
                .find(|(predicate, _)| predicate == &capture.title)
        })
        .map(|(_, value)| value.clone());

    let mut out = Vec::with_capacity(literal_props.len() + 1);
    for (predicate, value) in literal_props {
        if predicate == &capture.title && label_mirror_property != capture.title {
            continue;
        }
        out.push((predicate.clone(), value.clone()));
    }

    if !has_literal_property(&out, &label_mirror_property) {
        if let Some(title) = title_value {
            out.push((label_mirror_property, title));
        }
    }

    out
}

/// Read the datatype property that mirrors `rdfs:label` for a class. If the
/// class has no direct annotation, preserve the existing `hasTitle` behavior.
fn class_label_mirror_property_iri(
    store: &Store,
    capture: &CapturePredicates,
    class_iri: &str,
) -> String {
    let Ok(class) = NamedNode::new(class_iri) else {
        return capture.title.clone();
    };
    store
        .quads_for_pattern(Some(class.as_ref().into()), None, None, None)
        .flatten()
        .find_map(|q| {
            if local_name(q.predicate.as_str()) != LABEL_PROPERTY_LOCAL {
                return None;
            }
            match q.object {
                Term::NamedNode(label_property) => Some(label_property.as_str().to_string()),
                _ => None,
            }
        })
        .unwrap_or_else(|| capture.title.clone())
}

/// Verify `subject` is a recorded knowledge item — an instance of
/// `:InformationRecord` (or a subclass) in the project graph — and return its
/// class IRI. The lifecycle tools (`supersede_decision`, `retract_decision`)
/// share this precondition so they never mutate a non-record subject, and the
/// returned class lets a supersede mint its replacement type-preservingly.
pub(crate) fn require_information_record(
    state: &AppState,
    subject: &NamedNode,
) -> anyhow::Result<String> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let info_record_class = state.resolve_class("InformationRecord")?;
    state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new_unchecked(moose::RDF_TYPE)),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::NamedNode(t) => Some(t.as_str().to_string()),
            _ => None,
        })
        .find(|t| is_subclass_of(&state.store, t, &info_record_class))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} is not a recorded knowledge item (InformationRecord) in the project graph",
                subject.as_str()
            )
        })
}

/// Asserted rdf:type classes for a project-graph subject. No inference is
/// performed here; callers compare with `is_subclass_of` against ontology axioms.
pub(crate) fn asserted_project_types(state: &AppState, subject: &NamedNode) -> Vec<String> {
    let project_graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(NamedNodeRef::new_unchecked(moose::RDF_TYPE)),
            None,
            Some(GraphNameRef::NamedNode(project_graph)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::NamedNode(t) => Some(t.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// The write-path "stamp" applied to a captured instance: the capture predicate
/// IRIs plus the author, timestamp, and lifecycle status defaults to add when the
/// caller didn't supply them.
pub(crate) struct CaptureStamp<'a> {
    pub(crate) capture: &'a CapturePredicates,
    pub(crate) author: &'a str,
    pub(crate) timestamp: &'a str,
    pub(crate) status: &'a str,
}

/// Build the owned quads for one capture instance in the project graph: its type,
/// the caller's literal props, its IRI-valued relations, and the write-path
/// defaults (author, typed timestamp, lifecycle status) when the caller didn't
/// supply them. Returns quads rather than asserting so a supersede can commit
/// several instances *plus* a status change in one transaction. The default set
/// mirrors `record_instance_with_relations` — keep the two in sync.
pub(crate) fn capture_instance_quads(
    store: &Store,
    subject_iri: &str,
    class_iri: &str,
    literal_props: &[(String, String)],
    object_props: &[(String, String)],
    stamp: &CaptureStamp<'_>,
) -> anyhow::Result<Vec<Quad>> {
    let capture = stamp.capture;
    let author = stamp.author;
    let timestamp_rfc3339 = stamp.timestamp;
    let status = stamp.status;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let subject = NamedNode::new(subject_iri)
        .map_err(|e| anyhow::anyhow!("invalid subject IRI {subject_iri}: {e}"))?;
    let literal_props = normalize_capture_literal_props(store, capture, class_iri, literal_props);

    let mut quads = vec![Quad::new(
        subject.clone(),
        NamedNode::new(moose::RDF_TYPE)?,
        NamedNode::new(class_iri)
            .map_err(|e| anyhow::anyhow!("invalid class IRI {class_iri}: {e}"))?,
        graph.clone(),
    )];
    for (predicate, value) in &literal_props {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(predicate)?,
            Literal::new_simple_literal(value.as_str()),
            graph.clone(),
        ));
    }
    for (predicate, object) in object_props {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(predicate)?,
            NamedNode::new(object)?,
            graph.clone(),
        ));
    }

    // Write-path defaults, only when the caller didn't supply them (mirrors
    // `record_instance_with_relations`). Timestamp is typed xsd:dateTime to satisfy
    // the InformationRecord shape; author/status are plain strings.
    let supplied = |p: &str| literal_props.iter().any(|(k, _)| k == p);
    if !supplied(&capture.author) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.author)?,
            Literal::new_simple_literal(author),
            graph.clone(),
        ));
    }
    if !supplied(&capture.timestamp) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.timestamp)?,
            Literal::new_typed_literal(timestamp_rfc3339, NamedNode::new(XSD_DATETIME)?),
            graph.clone(),
        ));
    }
    if !supplied(&capture.status) {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(&capture.status)?,
            Literal::new_simple_literal(status),
            graph,
        ));
    }
    Ok(quads)
}
