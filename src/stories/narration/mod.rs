//! Optional LLM prose sensor over a deterministic, bounded Story packet.

mod cache;
mod packet;
mod response;

pub use cache::StoryNarrationCache;
pub(super) use packet::narration_evidence_is_eligible;
pub use packet::narration_prompt_token_budget;

#[cfg(test)]
pub(super) use packet::build_narration_packet_for_test;
#[cfg(test)]
pub(super) use response::apply_packet_response_for_test;

use cache::CacheStart;
use packet::{build_narration_packet, estimate_tokens, NarrationPacket};
use response::{validate_packet_response, ValidationFailure};

use super::model::{
    NarrationFailureReason, NarrationMode, NarrationOutcome, NarrationStrategy,
    StoryNarrativeSection, StoryRun,
};
use crate::graph::AppState;
use moose::types::{LlmAssistLevel, LlmParams};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NARRATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct NarrationValue {
    narrative: Vec<StoryNarrativeSection>,
}

#[derive(Clone)]
struct NarrationFailure {
    outcome: NarrationOutcome,
    reason: Option<NarrationFailureReason>,
    category: &'static str,
}

impl NarrationFailure {
    fn provider(error: &moose::types::EngineError) -> Self {
        let unsupported = error
            .to_string()
            .contains("does not support required JSON-schema output");
        Self {
            outcome: NarrationOutcome::ProviderError,
            reason: unsupported.then_some(NarrationFailureReason::StructuredOutputUnsupported),
            category: if unsupported {
                "structured_output_unsupported"
            } else {
                "provider_error"
            },
        }
    }

    fn invalid(reason: NarrationFailureReason, category: &'static str) -> Self {
        Self {
            outcome: NarrationOutcome::InvalidResponse,
            reason: Some(reason),
            category,
        }
    }
}

/// Narration is presentation-only. The deterministic subject, dossier,
/// chronology, gaps, code anchors, checks, and coverage are never accepted
/// back from the model.
pub async fn narrate_with_llm(state: &AppState, mut run: StoryRun, assist_level: u8) -> StoryRun {
    if assist_level == 0 {
        return run;
    }
    if !state.llm_configured || state.engine_config.llm_assist_level == LlmAssistLevel::PureSymbolic
    {
        run.narration_outcome = NarrationOutcome::Unconfigured;
        return run;
    }
    if !narration_evidence_is_eligible(&run) {
        run.narration_outcome = NarrationOutcome::Ineligible;
        return run;
    }
    let packet = match build_narration_packet(state, &run, state.llm_context_window_tokens) {
        Ok(packet) => packet,
        Err(reason) => {
            run.narration_outcome = NarrationOutcome::Ineligible;
            run.narration_failure_reason = Some(reason);
            return run;
        }
    };
    run.narration_coverage = Some(packet.coverage.clone());
    let key = packet.cache_key(state);
    let started = Instant::now();
    let (cache_status, result) = match state.story_narrations.begin(&key) {
        CacheStart::Hit(value) => ("hit", Ok(value)),
        CacheStart::Follower(flight) => ("coalesced", flight.wait().await),
        CacheStart::Leader(flight) => {
            let result = synthesize_packet(state, &run, &packet).await.map(Arc::new);
            state.story_narrations.finish(&key, &flight, result.clone());
            ("miss", result)
        }
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(value) => {
            tracing::info!(
                stage = "story_synthesis",
                cache = cache_status,
                prompt_bytes = packet.prompt.len(),
                estimated_prompt_tokens = estimate_tokens(packet.prompt.len()),
                source_groups = packet.coverage.source_groups,
                included_entities = packet.coverage.included_entities,
                eligible_entities = packet.coverage.eligible_entities,
                duration_ms = elapsed_ms,
                outcome = "succeeded",
                "Story LLM narration completed"
            );
            run.narrative = value.narrative.clone();
            run.narration_mode = NarrationMode::Llm;
            run.narration_strategy = NarrationStrategy::SinglePass;
            run.narration_outcome = NarrationOutcome::Succeeded;
            run.narration_failure_reason = None;
        }
        Err(failure) => {
            tracing::warn!(
                stage = "story_synthesis",
                cache = cache_status,
                prompt_bytes = packet.prompt.len(),
                estimated_prompt_tokens = estimate_tokens(packet.prompt.len()),
                source_groups = packet.coverage.source_groups,
                duration_ms = elapsed_ms,
                outcome = failure.category,
                "Story LLM narration kept the symbolic article"
            );
            run.narration_outcome = failure.outcome;
            run.narration_failure_reason = failure.reason;
        }
    }
    run
}

async fn synthesize_packet(
    state: &AppState,
    run: &StoryRun,
    packet: &NarrationPacket,
) -> Result<NarrationValue, NarrationFailure> {
    let llm = state.llm.with_fresh_usage();
    let params = LlmParams {
        temperature: Some(0.0),
        ..LlmParams::default()
    };
    let response = tokio::time::timeout(
        NARRATION_TIMEOUT,
        llm.chat_completion_json_schema(
            &state.model,
            &packet.prompt,
            Some(&params),
            "moosedev_story_narration",
            packet.schema.clone(),
        ),
    )
    .await
    .map_err(|_| NarrationFailure {
        outcome: NarrationOutcome::Timeout,
        reason: None,
        category: "timeout",
    })?
    .map_err(|error| NarrationFailure::provider(&error))?;
    let narrative = validate_packet_response(
        run,
        &response,
        &packet.citations_by_source,
        &packet.sections_by_source,
    )
    .map_err(ValidationFailure::into_narration_failure)?;
    let (prompt_tokens, completion_tokens) = llm.take_usage();
    tracing::info!(
        stage = "story_synthesis",
        prompt_tokens,
        completion_tokens,
        "Story LLM token usage"
    );
    Ok(NarrationValue { narrative })
}
