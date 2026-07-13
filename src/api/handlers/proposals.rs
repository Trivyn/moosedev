use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::api::error::ApiError;
use crate::api::models::{ProposalActionResponse, ProposalDto, ProposalListResponse};
use crate::graph::{self, local_name, AppState};

use super::records::record_iri_for_uuid;

#[derive(Deserialize)]
pub struct ProposalQuery {
    /// Optional lifecycle-status filter (e.g. `proposed`).
    pub status: Option<String>,
}

/// `GET /api/v1/proposals[?status=proposed]` — the ratification inbox.
pub async fn list_proposals(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProposalQuery>,
) -> Result<Json<ProposalListResponse>, ApiError> {
    let items = graph::list_proposals(&state, query.status.as_deref())?;
    let proposals = items
        .into_iter()
        .map(|p| ProposalDto {
            id: local_name(&p.iri).to_string(),
            iri: p.iri,
            label: p.label,
            subject_iri: p.subject_iri,
            predicate: p.predicate_local,
            target_symbol: p.target_symbol,
            target_path: p.target_path,
            evidence: p.evidence,
            status: p.status,
        })
        .collect();
    Ok(Json(ProposalListResponse { proposals }))
}

/// `POST /api/v1/proposals/{id}/accept` — ratify: materialize the real edge.
pub async fn accept_proposal(
    State(state): State<Arc<AppState>>,
    Path(uuid): Path<String>,
) -> Result<Json<ProposalActionResponse>, ApiError> {
    let iri = resolve_proposal(&state, &uuid)?;
    let outcome = graph::accept_proposal(&state, &iri, "workbench")
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(ProposalActionResponse {
        id: uuid,
        status: "accepted".to_string(),
        entity_iri: Some(outcome.entity_iri),
        entity_name: Some(outcome.entity_name),
    }))
}

/// `POST /api/v1/proposals/{id}/reject` — decline: never creates an edge.
pub async fn reject_proposal(
    State(state): State<Arc<AppState>>,
    Path(uuid): Path<String>,
) -> Result<Json<ProposalActionResponse>, ApiError> {
    let iri = resolve_proposal(&state, &uuid)?;
    graph::reject_proposal(&state, &iri, "workbench")
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(ProposalActionResponse {
        id: uuid,
        status: "rejected".to_string(),
        entity_iri: None,
        entity_name: None,
    }))
}

fn resolve_proposal(state: &AppState, uuid: &str) -> Result<String, ApiError> {
    if uuid.is_empty() || uuid.contains('/') {
        return Err(ApiError::bad_request("invalid proposal id"));
    }
    record_iri_for_uuid(state, uuid)
        .ok_or_else(|| ApiError::not_found(format!("proposal {uuid:?} not found")))
}
