//! Deterministic Story route and beat planning over grounded project knowledge.

use super::checks::{build_checks, build_record_kind_checks};
use super::grounding::*;
use super::model::*;
use super::resolution::*;
use super::*;

pub fn generate_story_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    subject: &StoryRecipeSubject,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    match subject {
        StoryRecipeSubject::Entity { iri } => {
            let entity = match recipe {
                Some(_) => index.recipe_entity(state, iri)?,
                None => index.resolve_entity(state, iri)?,
            };
            if entity.kind == "SystemComponent" {
                generate_symbolic_with_index(state, index, &entity, recipe, include_checks)
            } else {
                generate_entity_story(state, index, &entity, recipe, include_checks)
            }
        }
        StoryRecipeSubject::Topic { query } => {
            validate_topic(query)
                .map_err(|error| anyhow::Error::new(StorySubjectInvalid(error.to_string())))?;
            generate_topic_story(state, index, query, recipe, include_checks)
        }
    }
}

fn generate_entity_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    entity: &StoryCandidate,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    let records = entity_records(state, entity)?;
    let code = entity_code(state, &index.code_entities, &entity.iri)?;
    let current = index.resolve_entity(state, &entity.iri).is_ok();
    let (beats, gaps) = match recipe {
        Some(recipe) => generic_recipe_beats(state, recipe, Some(entity), &records, &code)?,
        None => generic_generated_beats(state, entity, &records, &code),
    };
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
        include_checks,
    )
}

fn generate_topic_story(
    state: &AppState,
    index: &StoryResolutionIndex,
    query: &str,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    let mut records = topic_records(state, query)?;
    if records.is_empty() && recipe.is_none() {
        return Err(anyhow::Error::new(StorySubjectInvalid(format!(
            "no current project knowledge matches Story topic {query:?}"
        ))));
    }
    let mut code = code_for_records(state, &index.code_entities, &records)?;
    if let Some(recipe) = recipe {
        extend_topic_recipe_anchors(state, index, recipe, &mut records, &mut code)?;
    }
    let topic = StoryCandidate {
        iri: String::new(),
        kind: "Topic".to_string(),
        label: query.trim().to_string(),
        description: None,
    };
    let (beats, gaps) = match recipe {
        Some(recipe) => generic_recipe_beats(state, recipe, None, &records, &code)?,
        None => generic_generated_beats(state, &topic, &records, &code),
    };
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
        include_checks,
    )
}

fn extend_topic_recipe_anchors(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
    records: &mut Vec<RecordData>,
    code: &mut Vec<StoryCodeAnchor>,
) -> anyhow::Result<()> {
    for beat in &recipe.beats {
        for iri in &beat.record_iris {
            if let Some(record) = record_data(state, iri)? {
                if in_working_set(&record.evidence.status) {
                    records.push(record);
                }
            }
        }
        for symbol in &beat.code_symbols {
            if let Some(anchor) = index.code_by_symbol.get(symbol) {
                code.push(anchor.clone());
            }
        }
    }
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
    mut gaps: Vec<StoryGap>,
    subject_is_current: bool,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    if !subject_is_current {
        gaps.push(StoryGap {
            id: "subject-drift".to_string(),
            title: "Story subject is unresolved".to_string(),
            detail: "The saved Story subject is no longer current authoritative project knowledge."
                .to_string(),
            beat_intent: None,
        });
    }
    for beat in &beats {
        if let Some(detail) = &beat.gap {
            gaps.push(StoryGap {
                id: format!("gap-{}", beat.id),
                title: format!("Missing evidence for {}", beat.title),
                detail: detail.clone(),
                beat_intent: Some(beat.intent.clone()),
            });
        }
    }
    let (checks, viable_check_count) = if subject_is_current {
        build_record_kind_checks(state, &subject, &beats, include_checks)?
    } else {
        (Vec::new(), 0)
    };
    if viable_check_count == 0 {
        gaps.push(StoryGap {
            id: "checks-unavailable".to_string(),
            title: "Comprehension check unavailable".to_string(),
            detail: "Current authoritative knowledge does not provide two distinct, unambiguous options for a symbolic check."
                .to_string(),
            beat_intent: None,
        });
    }
    dedupe_gaps(&mut gaps);
    let (title, goal, trust_state, recipe_id) = story_headline(recipe, &subject_label);
    Ok(StoryRun {
        recipe_id,
        trust_state,
        narration_mode: NarrationMode::Symbolic,
        narration_outcome: NarrationOutcome::NotRequested,
        title,
        subject,
        goal,
        overview: format!(
            "An evidence-backed route through {subject_label}. Each claim links back to current project knowledge, and missing context is shown as a gap."
        ),
        beats,
        gaps,
        checks,
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

pub(super) fn generate_symbolic_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    component: &StoryCandidate,
    recipe: Option<&StoryRecipe>,
    include_checks: bool,
) -> anyhow::Result<StoryRun> {
    let records = component_records(state, &component.iri)?;
    let code = component_code(state, &component.iri)?;
    let (beats, mut gaps) = match recipe {
        Some(recipe) => recipe_beats(state, recipe, component, &index.code_by_symbol)?,
        None => {
            let component_status =
                first_literal(&state.store, &component.iri, &state.capture.status)
                    .unwrap_or_else(|| "unknown".to_string());
            generated_beats(component, &component_status, &records, &code)
        }
    };
    if recipe.is_none() {
        let pending = pending_component_record_count(state, &component.iri)?;
        if pending > 0 {
            gaps.push(StoryGap {
                id: "pending-knowledge".to_string(),
                title: "Pending knowledge is not authoritative".to_string(),
                detail: format!(
                    "{pending} proposed record(s) concern this component and await ratification; they are not used as Story evidence."
                ),
                beat_intent: None,
            });
        }
    }
    let subject_is_current = index.components_by_iri.contains_key(&component.iri);
    if !subject_is_current {
        gaps.push(StoryGap {
            id: "subject-drift".to_string(),
            title: "Story subject is unresolved".to_string(),
            detail: format!(
                "Component {} is no longer a current authoritative SystemComponent.",
                component.iri
            ),
            beat_intent: None,
        });
    }
    for beat in &beats {
        if let Some(detail) = &beat.gap {
            gaps.push(StoryGap {
                id: format!("gap-{}", beat.id),
                title: format!("Missing evidence for {}", beat.title),
                detail: detail.clone(),
                beat_intent: Some(beat.intent.clone()),
            });
        }
    }
    dedupe_gaps(&mut gaps);
    let (checks, viable_check_count) = if subject_is_current {
        build_checks(
            state,
            component,
            &beats,
            index,
            &records,
            &code,
            include_checks,
        )?
    } else {
        // A drifted recipe remains readable for recovery, but it cannot issue
        // an authoritative quiz about a subject outside the working set.
        (Vec::new(), 0)
    };
    if viable_check_count == 0 {
        gaps.push(StoryGap {
            id: "checks-unavailable".to_string(),
            title: "Comprehension check unavailable".to_string(),
            detail: "Current authoritative knowledge does not provide two distinct, unambiguous options for a symbolic relationship check.".to_string(),
            beat_intent: None,
        });
        dedupe_gaps(&mut gaps);
    }
    let (title, goal, trust_state, recipe_id) = match recipe {
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
            format!("The story of {}", component.label),
            format!("Understand {} before changing it", component.label),
            StoryTrustState::Generated,
            None,
        ),
    };
    Ok(StoryRun {
        recipe_id,
        trust_state,
        narration_mode: NarrationMode::Symbolic,
        narration_outcome: NarrationOutcome::NotRequested,
        title,
        subject: StorySubject::Entity {
            iri: component.iri.clone(),
            kind: "SystemComponent".to_string(),
            label: component.label.clone(),
        },
        goal,
        overview: format!(
            "A five-part, evidence-backed route through {}. Gaps are shown rather than filled by inference.",
            component.label
        ),
        beats,
        gaps,
        checks,
    })
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

pub(super) fn friendly_record_kind(kind: &str) -> &str {
    match kind {
        "Requirement" => "requirement",
        "ArchitecturalDecision" => "architecture decision",
        "Constraint" => "constraint",
        "Pattern" => "recommended pattern",
        "AntiPattern" => "practice to avoid",
        "Lesson" => "lesson learned",
        "Consequence" => "recorded consequence",
        "Rationale" => "recorded rationale",
        "CodeEntity" => "code entity",
        "SystemComponent" => "system component",
        "InformationRecord" => "project record",
        other => other,
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

fn generic_recipe_beats(
    state: &AppState,
    recipe: &StoryRecipe,
    entity: Option<&StoryCandidate>,
    current_records: &[RecordData],
    current_code: &[StoryCodeAnchor],
) -> anyhow::Result<(Vec<StoryBeat>, Vec<StoryGap>)> {
    let records = current_records
        .iter()
        .map(|record| (record.evidence.iri.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let code = current_code
        .iter()
        .map(|anchor| (anchor.symbol.as_str(), anchor))
        .collect::<BTreeMap<_, _>>();
    let mut gaps = Vec::new();
    let mut beats = Vec::new();
    for spec in &recipe.beats {
        let mut evidence = Vec::new();
        for iri in &spec.record_iris {
            match records.get(iri.as_str()) {
                Some(record) => evidence.push(record.evidence.clone()),
                None => gaps.push(StoryGap {
                    id: format!("record-{}-{}", spec.id, gaps.len()),
                    title: "Story record is unavailable for this subject".to_string(),
                    detail: format!("Record {iri} is missing, retired, or outside the current Story subject neighborhood."),
                    beat_intent: Some(spec.intent.clone()),
                }),
            }
        }
        let mut anchors = Vec::new();
        for symbol in &spec.code_symbols {
            match code.get(symbol.as_str()) {
                Some(anchor) => anchors.push((*anchor).clone()),
                None => gaps.push(StoryGap {
                    id: format!("code-{}-{}", spec.id, gaps.len()),
                    title: "Code anchor is unavailable for this subject".to_string(),
                    detail: format!("Symbol {symbol} is missing or outside the current Story subject neighborhood."),
                    beat_intent: Some(spec.intent.clone()),
                }),
            }
        }
        if spec.intent == StoryIntent::Boundary {
            if let Some(entity) = entity {
                if story_entity_is_current(state, &entity.iri, &entity.kind)? {
                    evidence.insert(0, entity_subject_evidence(state, entity));
                }
            }
        }
        let boundary_detail = if spec.intent == StoryIntent::Boundary {
            entity.and_then(|subject| subject.description.clone())
        } else {
            None
        };
        beats.push(make_beat(
            &spec.id,
            &spec.title,
            spec.intent.clone(),
            evidence,
            anchors,
            boundary_detail,
            spec.curator_note.clone(),
        ));
    }
    Ok((beats, gaps))
}
