use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{extract::State, Json};

use crate::api::models::HealthResponse;
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let project_root = infer_project_root(&state.data_dir);
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

fn infer_project_root(data_dir: &Path) -> PathBuf {
    let data_dir_was_relative = data_dir.is_relative();
    let data_dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());

    if is_conventional_data_dir(&data_dir) {
        if let Some(parent) = data_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            return parent.to_path_buf();
        }
    }

    if let Some(root) = git_ancestor(&data_dir) {
        return root;
    }

    if data_dir_was_relative {
        if let Ok(cwd) = std::env::current_dir() {
            if let Some(root) = git_ancestor(&cwd) {
                return root;
            }
        }
    }

    data_dir
}

fn is_conventional_data_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".moosedev" || name == "data")
}

fn git_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}
