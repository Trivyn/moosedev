use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::Json;

use crate::api::error::ApiError;
use crate::api::models::{ConstraintDetailResponse, ConstraintListResponse};
use crate::constraints::{generate_constraint_set, ConstraintGenerationOptions};
use crate::graph::AppState;

pub async fn list_constraints(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ConstraintListResponse>, ApiError> {
    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())?;
    let constraints = set.summaries();
    Ok(Json(ConstraintListResponse {
        generated_at: set.generated_at,
        graph_constraints: set.graph_constraints,
        constraint_files: set.constraint_files,
        index_filename: set.index_filename,
        warnings: set.warnings,
        constraints,
    }))
}

pub async fn get_constraint(
    State(state): State<Arc<AppState>>,
    Path(num): Path<String>,
) -> Result<Json<ConstraintDetailResponse>, ApiError> {
    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())?;
    let constraint = set
        .find_by_num(&num)
        .ok_or_else(|| ApiError::not_found(format!("Constraint {num:?} not found")))?;
    Ok(Json(ConstraintDetailResponse {
        summary: constraint.summary(),
        markdown: constraint.markdown.clone(),
    }))
}

pub async fn download_constraint_archive(
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let set = generate_constraint_set(&state, ConstraintGenerationOptions::default())?;
    let archive = set.zip_archive()?;
    let headers = [
        (CONTENT_TYPE, "application/zip".to_string()),
        (
            CONTENT_DISPOSITION,
            "attachment; filename=\"moosedev-constraints.zip\"".to_string(),
        ),
    ];
    Ok((headers, archive))
}
