//! `POST /api/v1/policy` and `POST /api/v1/capture` — thin adapters over
//! `policy::evaluate_and_fire` and `graph::capture_decision_point`.
//!
//! `/policy` returns the same [`PolicyDecision`] JSON the MCP `evaluate_policy`
//! tool returns, so every host surface sees one verdict format. `/capture` is
//! the grounded-capture surface for shell-hook adapters (Claude Code), which
//! speak HTTP rather than a bidirectional MCP pipe.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;
use crate::graph::{self, AppState};
use crate::policy::fires::{append_fire, FireEvent};
use crate::policy::{evaluate_and_fire, PolicyDecision, PolicyEvent};

/// Request body: the host identity plus the flattened tagged event, e.g.
/// `{"host":"opencode","kind":"edit_proposed","file":"src/x.rs","anchor":"fn x"}`.
#[derive(Deserialize)]
pub struct PolicyEvalRequest {
    /// Host adapter identity for fire telemetry.
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(flatten)]
    pub event: PolicyEvent,
}

fn default_host() -> String {
    "http".to_string()
}

/// Evaluate one host event; acted decisions append fire telemetry.
pub async fn evaluate_policy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PolicyEvalRequest>,
) -> Result<Json<PolicyDecision>, ApiError> {
    let repo_root = std::env::current_dir()
        .map_err(|e| ApiError::internal(format!("cannot resolve daemon repo root: {e}")))?;
    let decision = evaluate_and_fire(&state, &repo_root, &req.event, &req.host)?;
    Ok(Json(decision))
}

/// Request body for `POST /api/v1/capture` — a grounded decision point.
#[derive(Deserialize)]
pub struct CaptureRequest {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub files: Vec<String>,
    pub summary: Option<String>,
    /// Existing Requirement (IRI or exact title) for `isMotivatedBy`;
    /// unresolvable values fail the capture — never invented.
    pub requirement: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    /// Author to attribute; defaults to the host identity.
    pub author: Option<String>,
}

#[derive(Serialize)]
pub struct CaptureResponse {
    pub record_iri: String,
    pub title: String,
    pub status: String,
    pub proposed_links: Vec<String>,
    pub unanchored: Vec<String>,
}

/// Grounded capture: mint a `proposed` record + queued links, fire telemetry.
pub async fn capture_decision_point(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CaptureRequest>,
) -> Result<Json<CaptureResponse>, ApiError> {
    let author = req.author.clone().unwrap_or_else(|| req.host.clone());
    let captured = graph::capture_decision_point(
        &state,
        &req.files,
        req.summary.as_deref(),
        req.requirement.as_deref(),
        &req.entities,
        &author,
        Utc::now(),
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    if let Err(e) = state.index_record(&captured.record_iri).await {
        tracing::warn!("dense index update failed for {}: {e}", captured.record_iri);
    }
    append_fire(
        &state.data_dir,
        &FireEvent {
            ts: Utc::now().to_rfc3339(),
            verb: "capture",
            host: req.host,
            entity: None,
            decision: "proposed".to_string(),
            records_cited: vec![captured.record_iri.clone()],
        },
    );

    Ok(Json(CaptureResponse {
        record_iri: captured.record_iri,
        title: captured.title,
        status: "proposed".to_string(),
        proposed_links: captured.proposed_links,
        unanchored: captured.unanchored,
    }))
}
