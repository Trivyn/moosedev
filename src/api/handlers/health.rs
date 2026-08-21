use std::sync::Arc;

use axum::{extract::State, Json};

use crate::api::models::HealthResponse;
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let project_root = state.project_root();
    let project_name = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_string();
    // Canonical so cross-process consumers can use it as backend IDENTITY: the
    // hooks and `--status`/`ui` compare it against their own project's data
    // dir before trusting a (possibly crash-stale) `http.addr` — a daemon may
    // have been started with a relative `./.moosedev`.
    let data_dir =
        std::fs::canonicalize(&state.data_dir).unwrap_or_else(|_| state.data_dir.clone());

    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        project_graph: PROJECT_KG_GRAPH_IRI.to_string(),
        data_dir: data_dir.display().to_string(),
        project_name,
        project_root: project_root.display().to_string(),
        llm_configured: state.llm_configured,
        llm_assist_level: format!("{:?}", state.engine_config.llm_assist_level),
    })
}
