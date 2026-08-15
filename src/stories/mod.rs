//! Human-readable subsystem Stories projected from authoritative project knowledge.
//!
//! Recipes are version-controlled presentation metadata under `stories/`. Story
//! runs are read-only projections: this module never writes the project graph.

mod checks;
mod grounding;
mod model;
mod narration;
mod planner;
mod repository;
mod resolution;

pub use checks::grade_check;
pub use model::{
    GradeResult, NarrationFailureReason, NarrationMode, NarrationOutcome, NarrationStrategy,
    ResolveOutcome, StoryBeatRecipe, StoryCandidate, StoryCheck, StoryCheckError, StoryCheckOption,
    StoryCodeAnchor, StoryCoverage, StoryEvidenceDetail, StoryEvidenceRelation, StoryFocus,
    StoryGap, StoryIntent, StoryLiteralProperty, StoryNarrationCoverage, StoryNarrativeSection,
    StoryParagraph, StoryRecipe, StoryRecipeSubject, StoryRelationDirection, StoryRun,
    StorySectionKind, StoryStatus, StorySubject, StorySummary, StoryTimelineEvent, StoryTrustState,
};
pub use narration::{narrate_with_llm, narration_prompt_token_budget};
pub use planner::generate_consistent_story;
pub use repository::{
    validate_story_id, StoryConflict, StoryCorrupt, StoryInternal, StoryNotFound, StoryRepository,
    StorySubjectInvalid,
};
pub use resolution::{enrich_summary, recipe_has_drift, story_subjects, StoryResolutionIndex};

pub(crate) use model::StoryCheckRegistry;
pub(crate) use narration::StoryNarrationCache;

#[cfg(test)]
use checks::*;
#[cfg(test)]
use grounding::*;
#[cfg(test)]
use model::{CheckGrant, CheckKind, StoryEvidence};
#[cfg(test)]
use narration::{
    apply_packet_response_for_test, build_narration_packet_for_test, narration_evidence_is_eligible,
};
#[cfg(test)]
use planner::*;
#[cfg(test)]
use repository::{
    next_revision, revision_value, validate_recipe, validate_refs, MAX_FOCUS_REFS,
    STORY_SCHEMA_VERSION,
};

#[cfg(test)]
mod tests;
