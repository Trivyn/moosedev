use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::Json;

use crate::api::error::ApiError;
use crate::api::models::{RequirementDetailResponse, RequirementListResponse};
use crate::graph::AppState;
use crate::requirements::{generate_requirement_set, RequirementGenerationOptions};

pub async fn list_requirements(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RequirementListResponse>, ApiError> {
    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())?;
    let requirements = set.summaries();
    Ok(Json(RequirementListResponse {
        generated_at: set.generated_at,
        graph_requirements: set.graph_requirements,
        requirement_files: set.requirement_files,
        index_filename: set.index_filename,
        warnings: set.warnings,
        requirements,
    }))
}

pub async fn get_requirement(
    State(state): State<Arc<AppState>>,
    Path(num): Path<String>,
) -> Result<Json<RequirementDetailResponse>, ApiError> {
    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())?;
    let requirement = set
        .find_by_num(&num)
        .ok_or_else(|| ApiError::not_found(format!("Requirement {num:?} not found")))?;
    Ok(Json(RequirementDetailResponse {
        summary: requirement.summary(),
        markdown: requirement.markdown.clone(),
    }))
}

pub async fn download_requirement_archive(
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())?;
    let archive = set.zip_archive()?;
    let headers = [
        (CONTENT_TYPE, "application/zip".to_string()),
        (
            CONTENT_DISPOSITION,
            "attachment; filename=\"moosedev-requirements.zip\"".to_string(),
        ),
    ];
    Ok((headers, archive))
}
