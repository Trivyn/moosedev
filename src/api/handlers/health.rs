use std::sync::Arc;

use axum::{extract::State, Json};

use crate::api::models::HealthResponse;
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        project_graph: PROJECT_KG_GRAPH_IRI.to_string(),
        data_dir: state.data_dir.display().to_string(),
    })
}
