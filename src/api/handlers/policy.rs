//! `POST /api/v1/policy` and `POST /api/v1/capture` — thin adapters over
//! `policy::evaluate_and_fire` and the fire-telemetry journal.
//!
//! `/policy` returns the same [`PolicyDecision`] JSON the MCP `evaluate_policy`
//! tool returns, so every host surface sees one verdict format. `/capture` is
//! the automatic session-checkpoint surface for shell-hook adapters (Claude
//! Code, opencode), which speak HTTP rather than a bidirectional MCP pipe. It
//! journals to `fires.jsonl` and NEVER writes the graph: a session's final
//! message is a status report, not a decision (Lesson `641c1811`), so only
//! deliberate calls (`record_important_decision`, the MCP
//! `capture_decision_point` tool) mint records — AD `007dce15`.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;
use crate::graph::AppState;
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

/// Request body for `POST /api/v1/capture` — an automatic session checkpoint.
/// Unknown fields (e.g. the retired `since_unix_seconds`, `requirement`) are
/// ignored so older deployed adapters keep working.
#[derive(Deserialize)]
pub struct CaptureRequest {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub files: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Serialize)]
pub struct CaptureResponse {
    pub outcome: &'static str,
}

/// Automatic session checkpoint: append one journal line to `fires.jsonl`.
/// Never writes the graph — deliberate capture is the only minting path.
pub async fn capture_decision_point(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CaptureRequest>,
) -> Result<Json<CaptureResponse>, ApiError> {
    let summary = req
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if summary.is_none() && req.files.is_empty() {
        return Err(ApiError::bad_request(
            "nothing to journal: no summary and no changed files",
        ));
    }
    append_fire(
        &state.data_dir,
        &FireEvent {
            ts: Utc::now().to_rfc3339(),
            verb: "capture",
            host: req.host,
            entity: None,
            decision: "journaled".to_string(),
            records_cited: vec![],
            summary,
            files: req.files,
        },
    )
    .map_err(|e| ApiError::internal(format!("failed to journal capture: {e}")))?;
    Ok(Json(CaptureResponse {
        outcome: "journaled",
    }))
}
