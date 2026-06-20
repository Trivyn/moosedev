use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::api::error::ApiError;
use crate::export::{self, ExportFormat, ExportScope};
use crate::graph::AppState;

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub format: Option<String>,
    pub graph: Option<String>,
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
