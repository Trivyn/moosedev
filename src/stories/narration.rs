//! Optional bounded LLM prose sensor with whole-run symbolic fallback.

use super::grounding::truncate_utf8;
use super::model::*;
use super::*;

/// Optionally rewrite only the already-grounded beat prose. Any sensor error,
/// timeout, malformed JSON, citation mismatch, or incomplete response preserves
/// the complete symbolic run.
pub async fn narrate_with_llm(state: &AppState, mut run: StoryRun, assist_level: u8) -> StoryRun {
    if assist_level == 0 {
        return run;
    }
    if !state.llm_configured || state.engine_config.llm_assist_level == LlmAssistLevel::PureSymbolic
    {
        run.narration_outcome = NarrationOutcome::Unconfigured;
        return run;
    }
    if !narration_evidence_is_current(&run) {
        run.narration_outcome = NarrationOutcome::Ineligible;
        return run;
    }
    let eligible = run
        .beats
        .iter()
        .filter(|beat| !beat.evidence.is_empty() || !beat.code_anchors.is_empty())
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        run.narration_outcome = NarrationOutcome::Ineligible;
        return run;
    }
    let Some(prompt) = build_narration_prompt(state, &run, &eligible) else {
        run.narration_outcome = NarrationOutcome::Ineligible;
        return run;
    };
    let llm = state.llm.with_fresh_usage();
    let params = LlmParams {
        temperature: Some(0.0),
        ..LlmParams::default()
    };
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        llm.chat_completion(&state.model, &prompt, Some(&params)),
    )
    .await;
    match response {
        Err(_) => {
            run.narration_outcome = NarrationOutcome::Timeout;
            run
        }
        Ok(Err(_)) => {
            run.narration_outcome = NarrationOutcome::ProviderError;
            run
        }
        Ok(Ok(raw)) => match apply_narration_response(run.clone(), &raw) {
            Some(narrated) => narrated,
            None => {
                run.narration_outcome = NarrationOutcome::InvalidResponse;
                run
            }
        },
    }
}

pub(super) fn narration_evidence_is_current(run: &StoryRun) -> bool {
    !run.gaps.iter().any(|gap| gap.id == "subject-drift")
        && run
            .beats
            .iter()
            .flat_map(|beat| &beat.evidence)
            .all(|evidence| in_working_set(&evidence.status))
}

pub(super) fn build_narration_prompt(
    state: &AppState,
    run: &StoryRun,
    eligible: &[&StoryBeat],
) -> Option<String> {
    let (subject_label, subject_kind) = match &run.subject {
        StorySubject::Entity { label, kind, .. } => (label.as_str(), kind.as_str()),
        StorySubject::Topic { label, .. } => (label.as_str(), "Topic"),
    };
    let prompt_beats = eligible
        .iter()
        .map(|beat| {
            let evidence = beat
                .evidence
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let description = first_literal(
                        &state.store,
                        &item.iri,
                        &state.capture.description,
                    )
                    .map(|value| truncate_utf8(&value, MAX_LLM_FIELD_BYTES).to_string());
                    serde_json::json!({
                        "id": format!("e{index}"),
                        "title": truncate_utf8(&item.title, MAX_LLM_FIELD_BYTES),
                        "kind": truncate_utf8(&item.kind, MAX_LLM_FIELD_BYTES),
                        "status": truncate_utf8(&item.status, MAX_LLM_FIELD_BYTES),
                        "description": description,
                    })
                })
                .chain(beat.code_anchors.iter().enumerate().map(|(index, item)| {
                    serde_json::json!({
                        "id": format!("c{index}"),
                        "code": truncate_utf8(&item.label, MAX_LLM_FIELD_BYTES),
                        "path": item.path.as_deref().map(|path| truncate_utf8(path, MAX_LLM_FIELD_BYTES)),
                    })
                }))
                .collect::<Vec<_>>();
            serde_json::json!({
                "beat_id": beat.id,
                "title": truncate_utf8(&beat.title, MAX_LLM_FIELD_BYTES),
                "intent": beat.intent,
                "symbolic_extract": truncate_utf8(&beat.narrative, MAX_LLM_FIELD_BYTES),
                "evidence": evidence,
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "story": {
            "title": truncate_utf8(&run.title, MAX_LLM_FIELD_BYTES),
            "subject_label": truncate_utf8(subject_label, MAX_LLM_FIELD_BYTES),
            "subject_kind": truncate_utf8(subject_kind, MAX_LLM_FIELD_BYTES),
            "goal": truncate_utf8(&run.goal, MAX_LLM_FIELD_BYTES),
        },
        "beats": prompt_beats,
    });
    let prompt = format!(
        "Rewrite each Story beat for a maintainer who is new to this codebase, using ONLY its supplied evidence. \
         Write two to four short, connected sentences per beat in plain language. Explain why the evidence matters \
         in the context of the Story subject and goal before naming implementation details; briefly define unavoidable \
         project-specific terms; avoid type-name lists, shorthand, headings, and bullet points. \
         Evidence lifecycle status is authoritative; never describe non-current evidence as current. \
         Return strict JSON {{\"beats\":[{{\"beat_id\":string,\"text\":string,\"citation_ids\":[string]}}]}}. \
         Include every beat once, in order; cite at least one supplied evidence id per beat. Grounded input: {}",
        serde_json::to_string(&payload).unwrap_or_default()
    );
    (prompt.len() <= MAX_LLM_PROMPT_BYTES).then_some(prompt)
}

pub(super) fn apply_narration_response(mut run: StoryRun, raw: &str) -> Option<StoryRun> {
    let parsed = parse_narration_response(raw)?;
    let eligible = run
        .beats
        .iter()
        .filter(|beat| !beat.evidence.is_empty() || !beat.code_anchors.is_empty())
        .collect::<Vec<_>>();
    if parsed.beats.len() != eligible.len() {
        return None;
    }
    let mut replacements = BTreeMap::new();
    for (expected, narrated) in eligible.iter().zip(parsed.beats) {
        if narrated.beat_id != expected.id
            || narrated.text.trim().is_empty()
            || narrated.text.len() > 1_200
            || narrated.citation_ids.is_empty()
        {
            return None;
        }
        let allowed = (0..expected.evidence.len())
            .map(|index| format!("e{index}"))
            .chain((0..expected.code_anchors.len()).map(|index| format!("c{index}")))
            .collect::<BTreeSet<_>>();
        let citations = narrated.citation_ids.iter().collect::<BTreeSet<_>>();
        if citations.len() != narrated.citation_ids.len()
            || narrated
                .citation_ids
                .iter()
                .any(|citation| !allowed.contains(citation))
        {
            return None;
        }
        replacements.insert(narrated.beat_id, narrated.text.trim().to_string());
    }
    for beat in &mut run.beats {
        if let Some(text) = replacements.remove(&beat.id) {
            beat.narrative = text;
        }
    }
    run.narration_mode = NarrationMode::Llm;
    run.narration_outcome = NarrationOutcome::Succeeded;
    Some(run)
}

/// Parse strict JSON first, then repair the common JSON-like syntax emitted by
/// local models. Repair is only syntactic: the typed schema and closed-evidence
/// validation in `apply_narration_response` remain authoritative.
fn parse_narration_response(raw: &str) -> Option<NarrationResponse> {
    const MAX_LLM_RESPONSE_BYTES: usize = 64 * 1024;

    if raw.len() > MAX_LLM_RESPONSE_BYTES {
        return None;
    }
    let trimmed = raw.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        let repaired = jsonrepair::repair_json(trimmed, &jsonrepair::Options::default()).ok()?;
        serde_json::from_str(&repaired).ok()
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarrationResponse {
    beats: Vec<NarratedBeat>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarratedBeat {
    beat_id: String,
    text: String,
    citation_ids: Vec<String>,
}
