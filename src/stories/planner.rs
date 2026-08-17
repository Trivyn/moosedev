//! Deterministic Story route and beat planning over grounded project knowledge.

use super::checks::{
    bind_prepared_check_sections, issue_prepared_checks, prepare_checks,
    prepare_record_kind_checks, PreparedStoryCheck,
};
use super::grounding::{
    build_story_document, code_for_records, component_code, component_records, dedupe_code_anchors,
    dedupe_gaps, entity_code, entity_records, pending_component_record_count, record_data,
    sort_dedupe_records, story_recipe_priority_iris, story_subject_closure_iris_with_priority,
    topic_records, truncate_utf8, ComponentExpansion, RecordData, MAX_STORY_ENTITIES,
};
use super::model::{
    friendly_record_kind, validate_topic, NarrationMode, NarrationOutcome, NarrationStrategy,
    StoryBeat, StoryCandidate, StoryCodeAnchor, StoryEvidence, StoryGap, StoryIntent, StoryRecipe,
    StoryRecipeSubject, StoryRun, StorySectionKind, StoryStatus, StorySubject, StoryTrustState,
};
use super::repository::{StorySubjectInvalid, STORY_SCHEMA_VERSION};
use super::resolution::StoryResolutionIndex;
use crate::graph::{first_literal, in_working_set, AppState};

const MAX_ANCHORS_PER_BEAT: usize = MAX_STORY_ENTITIES;

pub fn generate_consistent_story(
    state: &AppState,
    subject: &StoryRecipeSubject,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    for _ in 0..3 {
        let generation = state.project_write_generation();
        let index = StoryResolutionIndex::build(state)?;
        // Quiz grants are side effects. Build and validate a complete symbolic
        // projection first so discarded retry attempts never mint handles.
        let mut run = generate_story_with_index(state, &index, subject, recipe)?;
        if state.project_write_generation() != generation {
            continue;
        }
        if !include_checks {
            return Ok(run);
        }
        let prepared = prepare_checks_for_stable_story(state, &index, &run)?;
        if state.project_write_generation() != generation {
            continue;
        }
        run.checks = issue_prepared_checks(state, prepared);
        return Ok(run);
    }
    anyhow::bail!("project knowledge changed repeatedly while generating the Story; retry")
}

pub(super) fn prepare_checks_for_stable_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    run: &StoryRun,
) -> anyhow::Result<Vec<PreparedStoryCheck>> {
    if run.gaps.iter().any(|gap| gap.id == "subject-drift") {
        return Ok(Vec::new());
    }
    let mut prepared = match &run.subject {
        StorySubject::Entity { iri, kind, label } if kind == "SystemComponent" => {
            let component = StoryCandidate {
                iri: iri.clone(),
                kind: kind.clone(),
                label: label.clone(),
                description: None,
            };
            // The beats choose the displayed correct answer. Distractor truth
            // must be checked against the complete current component sets, or
            // a valid but undisplayed entity could be presented as false.
            let records = component_records(state, iri)?;
            let code = component_code(state, iri)?;
            prepare_checks(state, &component, &run.beats, index, &records, &code)?
        }
        _ => prepare_record_kind_checks(state, &run.subject, &run.beats)?,
    };
    bind_prepared_check_sections(&mut prepared, &run.narrative);
    Ok(prepared)
}

pub(super) fn generate_story_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    subject: &StoryRecipeSubject,
    recipe: Option<&StoryRecipe>,
) -> anyhow::Result<StoryRun> {
    match subject {
        StoryRecipeSubject::Entity { iri } => {
            let entity = match recipe {
                Some(_) => index.recipe_entity(state, iri)?,
                None => index.resolve_entity(state, iri)?,
            };
            if entity.kind == "SystemComponent" {
                generate_component_story(state, index, &entity, recipe)
            } else {
                generate_entity_story(state, index, &entity, recipe)
            }
        }
        StoryRecipeSubject::Topic { query } => {
            validate_topic(query)
                .map_err(|error| anyhow::Error::new(StorySubjectInvalid(error.to_string())))?;
            generate_topic_story(state, index, query, recipe)
        }
    }
}

fn generate_entity_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    entity: &StoryCandidate,
    recipe: Option<&StoryRecipe>,
) -> anyhow::Result<StoryRun> {
    let mut records = entity_records(state, entity)?;
    let mut code = entity_code(
        state,
        &index.code_entities,
        &entity.iri,
        if entity.kind == "CodeEntity" {
            ComponentExpansion::SubjectOnly
        } else {
            ComponentExpansion::Full
        },
    )?;
    let mut focus_gaps = Vec::new();
    if let Some(recipe) = recipe {
        apply_recipe_focus(
            state,
            index,
            recipe,
            &mut records,
            &mut code,
            &mut focus_gaps,
        )?;
    }
    let current = index.resolve_entity(state, &entity.iri).is_ok();
    let (beats, mut gaps) = generic_generated_beats(state, entity, &records, &code);
    gaps.extend(focus_gaps);
    finish_generic_story(
        state,
        entity.label.clone(),
        StorySubject::Entity {
            iri: entity.iri.clone(),
            kind: entity.kind.clone(),
            label: entity.label.clone(),
        },
        recipe,
        beats,
        gaps,
        current,
    )
}

fn generate_topic_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    query: &str,
    recipe: Option<&StoryRecipe>,
) -> anyhow::Result<StoryRun> {
    let mut records = topic_records(state, query)?;
    if records.is_empty() && recipe.is_none() {
        return Err(anyhow::Error::new(StorySubjectInvalid(format!(
            "no current project knowledge matches Story topic {query:?}"
        ))));
    }
    let mut code = code_for_records(state, &index.code_entities, &records)?;
    let mut focus_gaps = Vec::new();
    if let Some(recipe) = recipe {
        apply_recipe_focus(
            state,
            index,
            recipe,
            &mut records,
            &mut code,
            &mut focus_gaps,
        )?;
    }
    let topic = StoryCandidate {
        iri: String::new(),
        kind: "Topic".to_string(),
        label: query.trim().to_string(),
        description: None,
    };
    let (beats, mut gaps) = generic_generated_beats(state, &topic, &records, &code);
    gaps.extend(focus_gaps);
    finish_generic_story(
        state,
        topic.label.clone(),
        StorySubject::Topic {
            query: query.trim().to_string(),
            label: topic.label.clone(),
        },
        recipe,
        beats,
        gaps,
        true,
    )
}

fn apply_recipe_focus(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
    records: &mut Vec<RecordData>,
    code: &mut Vec<StoryCodeAnchor>,
    gaps: &mut Vec<StoryGap>,
) -> anyhow::Result<()> {
    let priority = story_recipe_priority_iris(state, recipe)?;
    let closure =
        story_subject_closure_iris_with_priority(state, recipe.resolved_subject()?, &priority)?;
    for iri in &recipe.focus.include_record_iris {
        if !closure.contains(iri) {
            gaps.push(StoryGap {
                id: format!("outside-focus-record-{}", gaps.len()),
                title: "Included Story evidence is outside this subject".to_string(),
                detail: format!(
                    "Record {iri} is not part of the subject's typed evidence closure."
                ),
                section_kind: None,
            });
            continue;
        }
        match record_data(state, iri)? {
            Some(record) if record.evidence.status.eq_ignore_ascii_case("proposed") => {
                gaps.push(StoryGap {
                    id: format!("proposed-focus-record-{}", gaps.len()),
                    title: "Included knowledge is awaiting ratification".to_string(),
                    detail: format!(
                        "Record {iri} is proposed and is shown only as a knowledge gap."
                    ),
                    section_kind: None,
                });
            }
            Some(record) if in_working_set(&record.evidence.status) => records.push(record),
            Some(_) => {
                // Historical anchors remain available to the deterministic
                // dossier/timeline, but never support present-tense prose or checks.
            }
            None => gaps.push(StoryGap {
                id: format!("missing-focus-record-{}", gaps.len()),
                title: "Included Story evidence is unavailable".to_string(),
                detail: format!("Record {iri} cannot be resolved as typed project knowledge."),
                section_kind: None,
            }),
        }
    }
    for symbol in &recipe.focus.include_code_symbols {
        match index.code_by_symbol.get(symbol) {
            Some(anchor)
                if anchor
                    .entity_iri
                    .as_ref()
                    .is_some_and(|iri| closure.contains(iri)) =>
            {
                code.push(anchor.clone())
            }
            Some(_) => gaps.push(StoryGap {
                id: format!("outside-focus-code-{}", gaps.len()),
                title: "Included Story code is outside this subject".to_string(),
                detail: format!(
                    "Code symbol {symbol} is not part of the subject's typed evidence closure."
                ),
                section_kind: Some(StorySectionKind::Implementation),
            }),
            None => gaps.push(StoryGap {
                id: format!("missing-focus-code-{}", gaps.len()),
                title: "Included Story code is unavailable".to_string(),
                detail: format!("Code symbol {symbol} cannot be resolved."),
                section_kind: Some(StorySectionKind::Implementation),
            }),
        }
    }
    records.retain(|record| {
        !recipe
            .focus
            .exclude_record_iris
            .contains(&record.evidence.iri)
    });
    code.retain(|anchor| !recipe.focus.exclude_code_symbols.contains(&anchor.symbol));
    sort_dedupe_records(records);
    *code = dedupe_code_anchors(std::mem::take(code));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_generic_story(
    state: &AppState,
    subject_label: String,
    subject: StorySubject,
    recipe: Option<&StoryRecipe>,
    beats: Vec<StoryBeat>,
    gaps: Vec<StoryGap>,
    subject_is_current: bool,
) -> anyhow::Result<StoryRun> {
    let prepared_checks = if subject_is_current {
        prepare_record_kind_checks(state, &subject, &beats)?
    } else {
        Vec::new()
    };
    finish_story(
        state,
        StoryAssembly {
            subject_label,
            subject,
            recipe,
            beats,
            gaps,
            subject_is_current,
            subject_drift_detail:
                "The saved Story subject is no longer current authoritative project knowledge."
                    .to_string(),
            prepared_checks,
            unavailable_check_detail: "Current authoritative knowledge does not provide two distinct, unambiguous options for a symbolic check.",
        },
    )
}

struct StoryAssembly<'a> {
    subject_label: String,
    subject: StorySubject,
    recipe: Option<&'a StoryRecipe>,
    beats: Vec<StoryBeat>,
    gaps: Vec<StoryGap>,
    subject_is_current: bool,
    subject_drift_detail: String,
    prepared_checks: Vec<PreparedStoryCheck>,
    unavailable_check_detail: &'static str,
}

fn finish_story(state: &AppState, mut assembly: StoryAssembly<'_>) -> anyhow::Result<StoryRun> {
    if !assembly.subject_is_current {
        assembly.gaps.push(StoryGap {
            id: "subject-drift".to_string(),
            title: "Story subject is unresolved".to_string(),
            detail: assembly.subject_drift_detail,
            section_kind: None,
        });
    }
    for beat in &assembly.beats {
        if let Some(detail) = &beat.gap {
            assembly.gaps.push(StoryGap {
                id: format!("gap-{}", beat.id),
                title: format!("Missing evidence for {}", beat.title),
                detail: detail.clone(),
                section_kind: Some((&beat.intent).into()),
            });
        }
    }
    let (title, goal, trust_state, recipe_id) =
        story_headline(assembly.recipe, &assembly.subject_label);
    let document =
        build_story_document(state, &assembly.subject, &assembly.beats, assembly.recipe)?;
    bind_prepared_check_sections(&mut assembly.prepared_checks, &document.narrative);
    if assembly.prepared_checks.is_empty() {
        assembly.gaps.push(StoryGap {
            id: "checks-unavailable".to_string(),
            title: "Comprehension check unavailable".to_string(),
            detail: assembly.unavailable_check_detail.to_string(),
            section_kind: None,
        });
    }
    if document.coverage.truncated {
        assembly.gaps.push(StoryGap {
            id: "closure-truncated".to_string(),
            title: "Story evidence was bounded".to_string(),
            detail: "The connected evidence exceeded the safe Story closure limit; this account does not claim completeness.".to_string(),
            section_kind: None,
        });
    }
    if document.coverage.proposed_count > 0
        && !assembly
            .gaps
            .iter()
            .any(|gap| gap.id == "proposed-knowledge")
    {
        assembly.gaps.push(StoryGap {
            id: "proposed-knowledge".to_string(),
            title: "Proposed knowledge is not authoritative".to_string(),
            detail: format!(
                "{} connected proposed record(s) are shown as gaps and are not used to narrate current project truth.",
                document.coverage.proposed_count
            ),
            section_kind: None,
        });
    }
    dedupe_gaps(&mut assembly.gaps);
    Ok(StoryRun {
        schema_version: STORY_SCHEMA_VERSION,
        recipe_id,
        trust_state,
        narration_mode: NarrationMode::Symbolic,
        narration_strategy: NarrationStrategy::Symbolic,
        narration_outcome: NarrationOutcome::NotRequested,
        narration_failure_reason: None,
        narration_coverage: None,
        title,
        subject: assembly.subject,
        goal,
        curator_context: assembly
            .recipe
            .and_then(|recipe| recipe.curator_context.clone()),
        brief: document.brief,
        narrative: document.narrative,
        timeline: document.timeline,
        evidence: document.evidence,
        code_anchors: document.code_anchors,
        coverage: document.coverage,
        gaps: assembly.gaps,
        checks: Vec::new(),
        beats: assembly.beats,
    })
}

fn story_headline(
    recipe: Option<&StoryRecipe>,
    subject_label: &str,
) -> (String, String, StoryTrustState, Option<String>) {
    match recipe {
        Some(recipe) => (
            recipe.title.clone(),
            recipe.goal.clone(),
            match recipe.status {
                StoryStatus::Draft => StoryTrustState::Draft,
                StoryStatus::Published => StoryTrustState::Published,
            },
            Some(recipe.id.clone()),
        ),
        None => (
            format!("The story of {subject_label}"),
            format!("Understand {subject_label} before changing it"),
            StoryTrustState::Generated,
            None,
        ),
    }
}

pub(super) fn generate_component_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    component: &StoryCandidate,
    recipe: Option<&StoryRecipe>,
) -> anyhow::Result<StoryRun> {
    let mut records = component_records(state, &component.iri)?;
    let mut code = component_code(state, &component.iri)?;
    let mut focus_gaps = Vec::new();
    if let Some(recipe) = recipe {
        apply_recipe_focus(
            state,
            index,
            recipe,
            &mut records,
            &mut code,
            &mut focus_gaps,
        )?;
    }
    let component_status = first_literal(&state.store, &component.iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    let (beats, mut gaps) = generated_beats(component, &component_status, &records, &code);
    gaps.extend(focus_gaps);
    if recipe.is_none() {
        let pending = pending_component_record_count(state, &component.iri)?;
        if pending > 0 {
            gaps.push(StoryGap {
                id: "pending-knowledge".to_string(),
                title: "Pending knowledge is not authoritative".to_string(),
                detail: format!(
                    "{pending} proposed record(s) concern this component and await ratification; they are not used as Story evidence."
                ),
                section_kind: None,
            });
        }
    }
    let subject_is_current = index.components_by_iri.contains_key(&component.iri);
    let prepared_checks = if subject_is_current {
        prepare_checks(state, component, &beats, index, &records, &code)?
    } else {
        // A drifted recipe remains readable for recovery, but it cannot issue
        // an authoritative quiz about a subject outside the working set.
        Vec::new()
    };
    let subject = StorySubject::Entity {
        iri: component.iri.clone(),
        kind: "SystemComponent".to_string(),
        label: component.label.clone(),
    };
    finish_story(
        state,
        StoryAssembly {
            subject_label: component.label.clone(),
            subject,
            recipe,
            beats,
            gaps,
            subject_is_current,
            subject_drift_detail: format!(
                "Component {} is no longer a current authoritative SystemComponent.",
                component.iri
            ),
            prepared_checks,
            unavailable_check_detail: "Current authoritative knowledge does not provide two distinct, unambiguous options for a symbolic relationship check.",
        },
    )
}

pub(super) fn generated_beats(
    component: &StoryCandidate,
    component_status: &str,
    records: &[RecordData],
    code: &[StoryCodeAnchor],
) -> (Vec<StoryBeat>, Vec<StoryGap>) {
    let purpose_records = record_data_of_kinds(records, &["Requirement"]);
    let governance_records =
        record_data_of_kinds(records, &["ArchitecturalDecision", "Constraint", "Pattern"]);
    let risk_records = record_data_of_kinds(
        records,
        &["Lesson", "AntiPattern", "Consequence", "Rationale"],
    );
    let boundary_evidence = vec![StoryEvidence {
        iri: component.iri.clone(),
        title: component.label.clone(),
        kind: "SystemComponent".to_string(),
        status: component_status.to_string(),
    }];
    let mut beats = vec![
        make_beat(
            "purpose",
            "Why it exists",
            StoryIntent::Purpose,
            purpose_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "boundary",
            "Where its boundary lies",
            StoryIntent::Boundary,
            boundary_evidence,
            vec![],
            component.description.clone(),
            None,
        ),
        make_beat(
            "core-code",
            "Code to understand first",
            StoryIntent::CoreCode,
            vec![],
            code.iter().take(MAX_ANCHORS_PER_BEAT).cloned().collect(),
            None,
            None,
        ),
        make_beat(
            "governance",
            "Decisions and constraints",
            StoryIntent::Governance,
            governance_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "risk",
            "Risks and lessons",
            StoryIntent::Risk,
            risk_records
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
    ];
    apply_extractive_narrative_to(&mut beats, "purpose", &purpose_records);
    apply_extractive_narrative_to(&mut beats, "governance", &governance_records);
    apply_extractive_narrative_to(&mut beats, "risk", &risk_records);
    (beats, Vec::new())
}

fn apply_extractive_narrative_to(beats: &mut [StoryBeat], beat_id: &str, records: &[&RecordData]) {
    if let Some(beat) = beats.iter_mut().find(|beat| beat.id == beat_id) {
        apply_extractive_narrative(beat, records);
    }
}

pub(super) fn make_beat(
    id: &str,
    title: &str,
    intent: StoryIntent,
    evidence: Vec<StoryEvidence>,
    code_anchors: Vec<StoryCodeAnchor>,
    detail: Option<String>,
    curator_note: Option<String>,
) -> StoryBeat {
    let gap = if evidence.is_empty() && code_anchors.is_empty() {
        Some(format!(
            "No current authoritative {} evidence is linked to this Story subject.",
            intent.id()
        ))
    } else {
        None
    };
    let narrative = symbolic_narrative(&evidence, &code_anchors, detail.as_deref());
    StoryBeat {
        id: id.to_string(),
        title: title.to_string(),
        intent,
        narrative,
        evidence,
        code_anchors,
        curator_note,
        gap,
    }
}

fn symbolic_narrative(
    evidence: &[StoryEvidence],
    code: &[StoryCodeAnchor],
    detail: Option<&str>,
) -> String {
    let base = if !code.is_empty() {
        format!(
            "Start with {}. These are the code locations the project graph connects to this part of the Story.",
            code.iter()
                .map(|anchor| anchor.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else if evidence.is_empty() {
        "The project graph does not currently contain authoritative evidence for this part of the Story."
            .to_string()
    } else {
        let sources = evidence
            .iter()
            .map(|item| format!("{} ({})", item.title, friendly_record_kind(&item.kind)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "This part of the Story is grounded in {sources}. Open the linked sources below for the full project wording."
        )
    };
    match detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{detail}\n\n{base}"),
        None => base,
    }
}

fn record_data_of_kinds<'a>(records: &'a [RecordData], kinds: &[&str]) -> Vec<&'a RecordData> {
    records
        .iter()
        .filter(|record| kinds.contains(&record.evidence.kind.as_str()))
        .take(MAX_ANCHORS_PER_BEAT)
        .collect()
}

fn apply_extractive_narrative(beat: &mut StoryBeat, records: &[&RecordData]) {
    let claims = records
        .iter()
        .take(3)
        .filter_map(|record| {
            record
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(|description| {
                    let clipped = truncate_utf8(description, 240);
                    format!("{} says: {}", record.evidence.title, clipped)
                })
        })
        .collect::<Vec<_>>();
    if !claims.is_empty() {
        let introduction = if claims.len() == 1 {
            "One current project record explains this:".to_string()
        } else {
            format!(
                "{} current project records explain this from different angles:",
                claims.len()
            )
        };
        beat.narrative = format!("{introduction}\n\n{}", claims.join("\n\n"));
    }
}

fn generic_generated_beats(
    state: &AppState,
    subject: &StoryCandidate,
    records: &[RecordData],
    code: &[StoryCodeAnchor],
) -> (Vec<StoryBeat>, Vec<StoryGap>) {
    let purpose = record_data_of_kinds(records, &["Requirement", "Rationale"]);
    let governance =
        record_data_of_kinds(records, &["ArchitecturalDecision", "Constraint", "Pattern"]);
    let risk = record_data_of_kinds(records, &["Lesson", "AntiPattern", "Consequence"]);
    let context = records
        .iter()
        .filter(|record| {
            !purpose
                .iter()
                .any(|item| item.evidence.iri == record.evidence.iri)
                && !governance
                    .iter()
                    .any(|item| item.evidence.iri == record.evidence.iri)
                && !risk
                    .iter()
                    .any(|item| item.evidence.iri == record.evidence.iri)
        })
        .take(MAX_ANCHORS_PER_BEAT)
        .collect::<Vec<_>>();
    let boundary_evidence = (!subject.iri.is_empty())
        .then(|| entity_subject_evidence(state, subject))
        .into_iter()
        .collect();
    let mut beats = vec![
        make_beat(
            "purpose",
            "Why it matters",
            StoryIntent::Purpose,
            purpose
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "boundary",
            "What this subject includes",
            StoryIntent::Boundary,
            boundary_evidence,
            vec![],
            subject.description.clone(),
            None,
        ),
        make_beat(
            "core-code",
            "Where it appears in code",
            StoryIntent::CoreCode,
            context
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            code.iter().take(MAX_ANCHORS_PER_BEAT).cloned().collect(),
            None,
            None,
        ),
        make_beat(
            "governance",
            "Decisions and constraints",
            StoryIntent::Governance,
            governance
                .iter()
                .map(|record| record.evidence.clone())
                .collect(),
            vec![],
            None,
            None,
        ),
        make_beat(
            "risk",
            "Risks and lessons",
            StoryIntent::Risk,
            risk.iter().map(|record| record.evidence.clone()).collect(),
            vec![],
            None,
            None,
        ),
    ];
    apply_extractive_narrative_to(&mut beats, "purpose", &purpose);
    apply_extractive_narrative_to(&mut beats, "core-code", &context);
    apply_extractive_narrative_to(&mut beats, "governance", &governance);
    apply_extractive_narrative_to(&mut beats, "risk", &risk);
    (beats, Vec::new())
}

fn entity_subject_evidence(state: &AppState, subject: &StoryCandidate) -> StoryEvidence {
    StoryEvidence {
        iri: subject.iri.clone(),
        title: subject.label.clone(),
        kind: subject.kind.clone(),
        status: first_literal(&state.store, &subject.iri, &state.capture.status)
            .unwrap_or_else(|| "unknown".to_string()),
    }
}
