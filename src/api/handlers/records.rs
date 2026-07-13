use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};

use crate::api::error::ApiError;
use crate::api::models::{RecordDetailResponse, RecordIncomingEdge, RecordOutgoingEdge};
use crate::graph::{
    asserted_project_types, first_literal, local_name, AppState, PROJECT_KG_GRAPH_IRI,
};

pub async fn get_record(
    State(state): State<Arc<AppState>>,
    Path(uuid): Path<String>,
) -> Result<Json<RecordDetailResponse>, ApiError> {
    if uuid.is_empty() || uuid.contains('/') {
        return Err(ApiError::bad_request("invalid record uuid"));
    }

    let iri = record_iri_for_uuid(&state, &uuid)
        .ok_or_else(|| ApiError::not_found(format!("record {uuid:?} not found")))?;
    let record = NamedNode::new(&iri)
        .map_err(|e| ApiError::internal(format!("invalid stored record IRI {iri:?}: {e}")))?;

    let kind = asserted_project_types(&state, &record)
        .into_iter()
        .next()
        .map(|class| local_name(&class).to_string())
        .unwrap_or_else(|| "Record".to_string());
    let title = record_title_for(&state, &iri);

    Ok(Json(RecordDetailResponse {
        iri: iri.clone(),
        kind,
        title,
        description: first_literal(&state.store, &iri, &state.capture.description),
        status: first_literal(&state.store, &iri, &state.capture.status),
        timestamp: first_literal(&state.store, &iri, &state.capture.timestamp),
        author: first_literal(&state.store, &iri, &state.capture.author),
        outgoing: outgoing_edges(&state, &record),
        incoming: incoming_edges(&state, &record),
    }))
}

pub(crate) fn record_iri_for_uuid(state: &AppState, uuid: &str) -> Option<String> {
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let rdf_type = NamedNodeRef::new_unchecked(moose::RDF_TYPE);
    let suffix = format!("/{uuid}");
    let mut matches = state
        .store
        .quads_for_pattern(
            None,
            Some(rdf_type),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|quad| match quad.subject {
            oxigraph::model::NamedOrBlankNode::NamedNode(subject)
                if subject.as_str().ends_with(&suffix) =>
            {
                Some(subject.as_str().to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    matches.into_iter().next()
}

fn outgoing_edges(state: &AppState, record: &NamedNode) -> Vec<RecordOutgoingEdge> {
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut edges = state
        .store
        .quads_for_pattern(
            Some(record.as_ref().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|quad| {
            let Term::NamedNode(target) = quad.object else {
                return None;
            };
            if !is_object_property(state, quad.predicate.as_str()) {
                return None;
            }
            let target_iri = target.as_str().to_string();
            Some(RecordOutgoingEdge {
                predicate: local_name(quad.predicate.as_str()).to_string(),
                target_label: label_for(state, &target_iri),
                target_kind: kind_for(state, &target),
                target_iri,
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        (left.predicate.as_str(), left.target_iri.as_str())
            .cmp(&(right.predicate.as_str(), right.target_iri.as_str()))
    });
    edges
}

fn incoming_edges(state: &AppState, record: &NamedNode) -> Vec<RecordIncomingEdge> {
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let mut edges = state
        .store
        .quads_for_pattern(
            None,
            None,
            Some(record.as_ref().into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|quad| {
            let oxigraph::model::NamedOrBlankNode::NamedNode(source) = quad.subject else {
                return None;
            };
            if !is_object_property(state, quad.predicate.as_str()) {
                return None;
            }
            let source_iri = source.as_str().to_string();
            Some(RecordIncomingEdge {
                predicate: local_name(quad.predicate.as_str()).to_string(),
                source_label: label_for(state, &source_iri),
                source_kind: kind_for(state, &source),
                source_iri,
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        (left.predicate.as_str(), left.source_iri.as_str())
            .cmp(&(right.predicate.as_str(), right.source_iri.as_str()))
    });
    edges
}

fn is_object_property(state: &AppState, predicate_iri: &str) -> bool {
    // Intent-link predicates (realizes/satisfies/embodies) live in the code
    // vocabulary; record links (concerns/constrains/...) in the architecture
    // vocabulary. Edges from either must survive the filter.
    state
        .arch_vocab
        .object_properties
        .iter()
        .chain(state.code_vocab.object_properties.iter())
        .any(|property| property.iri == predicate_iri)
}

fn label_for(state: &AppState, iri: &str) -> String {
    first_literal(&state.store, iri, moose::RDFS_LABEL)
        .unwrap_or_else(|| local_name(iri).to_string())
}

fn record_title_for(state: &AppState, iri: &str) -> String {
    first_literal(&state.store, iri, &state.capture.title)
        .or_else(|| first_literal(&state.store, iri, moose::RDFS_LABEL))
        .unwrap_or_else(|| local_name(iri).to_string())
}

fn kind_for(state: &AppState, node: &NamedNode) -> String {
    asserted_project_types(state, node)
        .into_iter()
        .next()
        .map(|class| local_name(&class).to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}
