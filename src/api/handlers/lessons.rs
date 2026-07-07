use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::Json;

use crate::api::error::ApiError;
use crate::api::models::{LessonDetailResponse, LessonListResponse};
use crate::graph::AppState;
use crate::lessons::{generate_lesson_set, LessonGenerationOptions};

pub async fn list_lessons(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LessonListResponse>, ApiError> {
    let set = generate_lesson_set(&state, LessonGenerationOptions::default())?;
    let lessons = set.summaries();
    Ok(Json(LessonListResponse {
        generated_at: set.generated_at,
        graph_lessons: set.graph_lessons,
        lesson_files: set.lesson_files,
        index_filename: set.index_filename,
        warnings: set.warnings,
        lessons,
    }))
}

pub async fn get_lesson(
    State(state): State<Arc<AppState>>,
    Path(num): Path<String>,
) -> Result<Json<LessonDetailResponse>, ApiError> {
    let set = generate_lesson_set(&state, LessonGenerationOptions::default())?;
    let lesson = set
        .find_by_num(&num)
        .ok_or_else(|| ApiError::not_found(format!("Lesson {num:?} not found")))?;
    Ok(Json(LessonDetailResponse {
        summary: lesson.summary(),
        markdown: lesson.markdown.clone(),
    }))
}

pub async fn download_lesson_archive(
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let set = generate_lesson_set(&state, LessonGenerationOptions::default())?;
    let archive = set.zip_archive()?;
    let headers = [
        (CONTENT_TYPE, "application/zip".to_string()),
        (
            CONTENT_DISPOSITION,
            "attachment; filename=\"moosedev-lessons.zip\"".to_string(),
        ),
    ];
    Ok((headers, archive))
}
