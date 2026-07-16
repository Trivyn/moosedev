use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::Json;

use crate::adrs::{generate_adr_set_cached, AdrGenerationOptions};
use crate::api::error::ApiError;
use crate::api::models::{AdrDetailResponse, AdrListResponse};
use crate::graph::AppState;

pub async fn list_adrs(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AdrListResponse>, ApiError> {
    let set = generate_adr_set_cached(&state, AdrGenerationOptions::default())?;
    let adrs = set.summaries();
    Ok(Json(AdrListResponse {
        generated_at: set.generated_at.clone(),
        graph_decisions: set.graph_decisions,
        adr_files: set.adr_files,
        index_filename: set.index_filename.clone(),
        warnings: set.warnings.clone(),
        adrs,
    }))
}

pub async fn get_adr(
    State(state): State<Arc<AppState>>,
    Path(num): Path<String>,
) -> Result<Json<AdrDetailResponse>, ApiError> {
    let set = generate_adr_set_cached(&state, AdrGenerationOptions::default())?;
    let adr = set
        .find_by_num(&num)
        .ok_or_else(|| ApiError::not_found(format!("ADR {num:?} not found")))?;
    Ok(Json(AdrDetailResponse {
        summary: adr.summary(),
        markdown: adr.markdown.clone(),
    }))
}

pub async fn download_adr_archive(
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let set = generate_adr_set_cached(&state, AdrGenerationOptions::default())?;
    let archive = set.zip_archive()?;
    let headers = [
        (CONTENT_TYPE, "application/zip".to_string()),
        (
            CONTENT_DISPOSITION,
            "attachment; filename=\"moosedev-adrs.zip\"".to_string(),
        ),
    ];
    Ok((headers, archive))
}
