use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::api::error::ApiError;
use crate::export::{self, ExportFormat, ExportScope};
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};
use crate::graph_import::{self, ImportFormat, ImportMode};

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub format: Option<String>,
    pub graph: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    pub format: Option<String>,
    pub graph: Option<String>,
    pub mode: Option<String>,
}

pub async fn export_graph(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ExportQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let format = match query.format.as_deref() {
        Some(raw) => ExportFormat::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => ExportFormat::default(),
    };
    let scope = match query.graph.as_deref() {
        Some(raw) => ExportScope::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => ExportScope::default(),
    };

    let dump = export::export_graph(&state.store, scope, format)?;
    let filename = format!("moosedev-{}.{}", scope.label(), format.extension());
    let headers = [
        (CONTENT_TYPE, format.media_type().to_string()),
        (
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ),
    ];

    Ok((headers, dump.text))
}

pub async fn import_graph(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ImportQuery>,
    body: String,
) -> Result<impl IntoResponse, ApiError> {
    let format = match query.format.as_deref() {
        Some(raw) => ImportFormat::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => ImportFormat::default(),
    };
    let scope = match query.graph.as_deref() {
        Some(raw) => ExportScope::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => ExportScope::default(),
    };
    let mode = match query.mode.as_deref() {
        Some(raw) => ImportMode::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?,
        None => ImportMode::default(),
    };

    let outcome = graph_import::import_graph(&state.store, scope, format, mode, &body)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if outcome.project_changed() {
        // Import bypasses the typed capture path, so refresh the read-side caches
        // the capture path normally invalidates for project-graph writes.
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
        state.mark_inferred_stale();
    }

    Ok(axum::Json(outcome))
}
