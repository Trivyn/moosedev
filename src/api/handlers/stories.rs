use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;
use crate::graph::AppState;
use crate::stories::{
    enrich_summary, generate_symbolic_with_index, grade_check, narrate_with_llm, recipe_has_drift,
    validate_story_id, GradeResult, ResolveOutcome, StoryCandidate, StoryCheckError, StoryConflict,
    StoryCorrupt, StoryInternal, StoryNotFound, StoryRecipe, StoryRepository, StoryResolutionIndex,
    StoryRun, StorySummary,
};

#[derive(Debug, Serialize)]
pub struct StoryListResponse {
    pub stories: Vec<StorySummary>,
}

#[derive(Debug, Serialize)]
pub struct StoryRecipeResponse {
    pub recipe: StoryRecipe,
}

#[derive(Debug, Deserialize)]
pub struct GenerateStoryRequest {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub component_iri: Option<String>,
    #[serde(default)]
    pub recipe_id: Option<String>,
    #[serde(default)]
    pub fresh: bool,
    #[serde(default)]
    pub assist_level: u8,
    #[serde(default = "default_include_checks")]
    pub include_checks: bool,
}

fn default_include_checks() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GenerateStoryResponse {
    Story {
        story: StoryRun,
    },
    Ambiguous {
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        recipe_id: Option<String>,
        candidates: Vec<StoryCandidate>,
    },
}

#[derive(Debug, Deserialize)]
pub struct GradeStoryRequest {
    pub check_id: String,
    #[serde(default)]
    pub selected_option_ids: Vec<String>,
}

pub async fn list_stories(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StoryListResponse>, ApiError> {
    let repository = repository(&state);
    let recipes = repository.list_recipes()?;
    let index = StoryResolutionIndex::build(&state)?;
    let mut stories = Vec::with_capacity(recipes.len());
    for recipe in &recipes {
        let mut summary = StorySummary::from(recipe);
        enrich_summary(&state, &index, recipe, &mut summary)?;
        stories.push(summary);
    }
    Ok(Json(StoryListResponse { stories }))
}

pub async fn get_story(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<StoryRecipeResponse>, ApiError> {
    validate_story_id(&id).map_err(|error| ApiError::bad_request(error.to_string()))?;
    let recipe = repository(&state)
        .get(&id)?
        .ok_or_else(|| ApiError::not_found(format!("Story {id:?} not found")))?;
    Ok(Json(StoryRecipeResponse { recipe }))
}

pub async fn put_story(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(recipe): Json<StoryRecipe>,
) -> Result<Json<StoryRecipeResponse>, ApiError> {
    validate_story_id(&id).map_err(|error| ApiError::bad_request(error.to_string()))?;
    let recipe = repository(&state)
        .save_checked(&id, recipe, |candidate| {
            if candidate.status == crate::stories::StoryStatus::Published
                && checked_recipe_has_drift(&state, candidate)?
            {
                anyhow::bail!(
                    "a published Story may reference only current authoritative components, records, and code anchors"
                );
            }
            Ok(())
        })
        .map_err(story_write_error)?;
    Ok(Json(StoryRecipeResponse { recipe }))
}

pub async fn publish_story(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(expected): Json<PublishStoryRequest>,
) -> Result<Json<StoryRecipeResponse>, ApiError> {
    validate_story_id(&id).map_err(|error| ApiError::bad_request(error.to_string()))?;
    let repository = repository(&state);
    let recipe = repository
        .publish_checked(&id, &expected.updated_at, |draft| {
            if checked_recipe_has_drift(&state, draft)? {
                anyhow::bail!(
                    "Story has missing, retired, unaccepted, or unresolved anchors and cannot be published"
                );
            }
            Ok(())
        })
        .map_err(story_write_error)?;
    Ok(Json(StoryRecipeResponse { recipe }))
}

#[derive(Debug, Deserialize)]
pub struct PublishStoryRequest {
    pub updated_at: String,
}

pub async fn generate_story(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateStoryRequest>,
) -> Result<Json<GenerateStoryResponse>, ApiError> {
    if request.assist_level > 1 {
        return Err(ApiError::bad_request("assist_level must be 0 or 1"));
    }
    let repository = repository(&state);
    let index = StoryResolutionIndex::build(&state)?;
    let mut recipe = match request.recipe_id.as_deref() {
        Some(id) => {
            validate_story_id(id).map_err(|error| ApiError::bad_request(error.to_string()))?;
            Some(
                repository
                    .get(id)?
                    .ok_or_else(|| ApiError::not_found(format!("Story {id:?} not found")))?,
            )
        }
        None => None,
    };
    if let (Some(recipe), Some(explicit_component)) =
        (recipe.as_ref(), request.component_iri.as_deref())
    {
        if explicit_component != recipe.subject_component_iri {
            return Err(ApiError::bad_request(
                "component_iri cannot override a curated Story recipe subject",
            ));
        }
    }
    // An explicit component selects a generated Story. A curated recipe keeps
    // its exact saved subject; mismatch was rejected immediately above.
    let selector = request
        .component_iri
        .as_deref()
        .or_else(|| {
            recipe
                .as_ref()
                .map(|recipe| recipe.subject_component_iri.as_str())
        })
        .or(request.prompt.as_deref())
        .ok_or_else(|| ApiError::bad_request("prompt, component_iri, or recipe_id is required"))?;
    let prompt = request
        .prompt
        .clone()
        .unwrap_or_else(|| selector.to_string());
    let component = match index.resolve_component(selector) {
        Ok(ResolveOutcome::Resolved(component)) => component,
        Ok(ResolveOutcome::Ambiguous(candidates)) => {
            return Ok(Json(GenerateStoryResponse::Ambiguous {
                prompt,
                recipe_id: request.recipe_id.clone(),
                candidates,
            }));
        }
        // Curated recipes retain their exact historical subject so drift can
        // be rendered honestly. Any explicit subject mismatch was rejected
        // before resolution above.
        Err(error) => match recipe.as_ref() {
            Some(recipe) => index.recipe_subject(&recipe.subject_component_iri),
            None => return Err(ApiError::bad_request(error.to_string())),
        },
    };
    if recipe.is_none() && !request.fresh {
        recipe = repository.published_for_subject(&component.iri)?;
    }
    let story = generate_symbolic_with_index(
        &state,
        &index,
        &component,
        recipe.as_ref(),
        request.include_checks,
    )?;
    let story = narrate_with_llm(&state, story, request.assist_level).await;
    Ok(Json(GenerateStoryResponse::Story { story }))
}

pub async fn grade_story_check(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GradeStoryRequest>,
) -> Result<Json<GradeResult>, ApiError> {
    // Runs are intentionally ephemeral; the opaque project-scoped grant is
    // revalidated against current graph state before grading.
    let result =
        grade_check(&state, &request.check_id, &request.selected_option_ids).map_err(|error| {
            if let Some(kind) = error.downcast_ref::<StoryCheckError>() {
                return match kind {
                    StoryCheckError::Malformed => ApiError::bad_request(error.to_string()),
                    StoryCheckError::Unknown
                    | StoryCheckError::Expired
                    | StoryCheckError::Evicted => ApiError::not_found(error.to_string()),
                    StoryCheckError::ForeignOption => ApiError::bad_request(error.to_string()),
                    StoryCheckError::Stale => ApiError::conflict(error.to_string()),
                };
            }
            ApiError::internal(error.to_string())
        })?;
    Ok(Json(result))
}

fn repository(state: &AppState) -> StoryRepository {
    StoryRepository::new(&state.project_root())
}

fn checked_recipe_has_drift(state: &AppState, recipe: &StoryRecipe) -> anyhow::Result<bool> {
    recipe_has_drift(state, recipe).map_err(|error| {
        anyhow::Error::new(StoryInternal(format!(
            "validate Story against current project graph: {error}"
        )))
    })
}

fn story_write_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<StoryConflict>().is_some() {
        ApiError::conflict(error.to_string())
    } else if error.downcast_ref::<StoryNotFound>().is_some() {
        ApiError::not_found(error.to_string())
    } else if error.downcast_ref::<StoryCorrupt>().is_some()
        || error.downcast_ref::<StoryInternal>().is_some()
        || error.chain().any(|cause| cause.is::<std::io::Error>())
    {
        ApiError::internal(error.to_string())
    } else {
        ApiError::bad_request(error.to_string())
    }
}
