//! Opaque comprehension-check issuance and graph-current grading.

use super::grounding::*;
use super::model::*;
use super::resolution::*;
use super::*;

pub(super) fn build_record_kind_checks(
    state: &AppState,
    subject: &StorySubject,
    beats: &[StoryBeat],
    include_checks: bool,
) -> anyhow::Result<(Vec<StoryCheck>, usize)> {
    let displayed = beats
        .iter()
        .flat_map(|beat| beat.evidence.iter().map(move |evidence| (beat, evidence)))
        .filter(|(_, evidence)| {
            evidence.kind != "SystemComponent"
                && evidence.kind != "CodeEntity"
                && evidence.kind != "Entity"
        })
        .collect::<Vec<_>>();
    let all_records = relevant_context_snapshot(state, None, 96, false)?
        .into_iter()
        .filter_map(|item| record_data(state, &item.iri).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut used_kinds = BTreeSet::new();
    let mut viable = 0;
    for (beat, correct) in displayed {
        if !used_kinds.insert(correct.kind.clone()) {
            continue;
        }
        let facts = all_records.iter().map(|record| CheckOptionFact {
            id: record.evidence.iri.clone(),
            label: record.evidence.title.clone(),
            matches_target: record.evidence.kind == correct.kind,
        });
        let Some((correct_id, options)) =
            unambiguous_check_options(std::slice::from_ref(&correct.iri), facts)
        else {
            continue;
        };
        if push_check(
            state,
            &mut checks,
            CheckSpec {
                kind: CheckKind::RecordKind,
                component_iri: "",
                beat_id: &beat.id,
                correct_option_id: &correct_id,
                correct_kind: Some(&correct.kind),
                subject_entity: match subject {
                    StorySubject::Entity { iri, kind, .. } => Some((iri, kind)),
                    StorySubject::Topic { .. } => None,
                },
                question: format!(
                    "Which item is the {} shown in this Story?",
                    friendly_record_kind(&correct.kind)
                ),
                options,
            },
            include_checks,
        ) {
            viable += 1;
        }
        if viable == 2 {
            break;
        }
    }
    Ok((checks, viable))
}

pub(super) fn build_checks(
    state: &AppState,
    component: &StoryCandidate,
    beats: &[StoryBeat],
    index: &StoryResolutionIndex,
    component_records: &[RecordData],
    component_code: &[StoryCodeAnchor],
    include_checks: bool,
) -> anyhow::Result<(Vec<StoryCheck>, usize)> {
    let mut checks = Vec::new();
    let mut viable_check_count = 0;
    let target_record_ids = component_records
        .iter()
        .map(|record| record.evidence.iri.clone())
        .collect::<BTreeSet<_>>();
    let displayed_record_ids = beats
        .iter()
        .flat_map(|beat| beat.evidence.iter())
        .filter(|evidence| {
            evidence.kind != "SystemComponent" && target_record_ids.contains(&evidence.iri)
        })
        .map(|evidence| evidence.iri.clone())
        .collect::<Vec<_>>();
    let record_facts = records_for_components(state, &index.components)?
        .into_iter()
        .chain(component_records.iter().cloned())
        .map(|record| CheckOptionFact {
            matches_target: target_record_ids.contains(&record.evidence.iri),
            id: record.evidence.iri,
            label: record.evidence.title,
        });
    if let Some((correct_id, options)) =
        unambiguous_check_options(&displayed_record_ids, record_facts)
    {
        let beat_id = beats
            .iter()
            .find(|beat| {
                beat.evidence
                    .iter()
                    .any(|evidence| evidence.iri == correct_id)
            })
            .map(|beat| beat.id.as_str())
            .unwrap_or_default();
        if push_check(
            state,
            &mut checks,
            CheckSpec {
                kind: CheckKind::Concerns,
                component_iri: &component.iri,
                beat_id,
                correct_option_id: &correct_id,
                correct_kind: None,
                subject_entity: None,
                question: format!("Which accepted record is linked to {}?", component.label),
                options,
            },
            include_checks,
        ) {
            viable_check_count += 1;
        }
    }
    if viable_check_count < 2 {
        let target_code_ids = component_code
            .iter()
            .filter_map(|anchor| anchor.entity_iri.clone())
            .collect::<BTreeSet<_>>();
        let displayed_code_ids = beats
            .iter()
            .flat_map(|beat| beat.code_anchors.iter())
            .filter_map(|anchor| anchor.entity_iri.as_ref())
            .filter(|entity| target_code_ids.contains(*entity))
            .cloned()
            .collect::<Vec<_>>();
        let code_facts = index
            .code_entities
            .iter()
            .chain(component_code.iter())
            .filter_map(|anchor| {
                anchor.entity_iri.as_ref().map(|entity| CheckOptionFact {
                    matches_target: target_code_ids.contains(entity),
                    id: entity.clone(),
                    label: anchor.label.clone(),
                })
            });
        if let Some((correct_id, options)) =
            unambiguous_check_options(&displayed_code_ids, code_facts)
        {
            if let Some(beat_id) = beats.iter().find_map(|beat| {
                beat.code_anchors
                    .iter()
                    .any(|anchor| anchor.entity_iri.as_deref() == Some(correct_id.as_str()))
                    .then_some(beat.id.as_str())
            }) {
                if push_check(
                    state,
                    &mut checks,
                    CheckSpec {
                        kind: CheckKind::Realizes,
                        component_iri: &component.iri,
                        beat_id,
                        correct_option_id: &correct_id,
                        correct_kind: None,
                        subject_entity: None,
                        question: format!("Which code entity realizes {}?", component.label),
                        options,
                    },
                    include_checks,
                ) {
                    viable_check_count += 1;
                }
            }
        }
    }
    // Keep the API bounded even if future generators add more check kinds.
    checks.truncate(2);
    Ok((checks, viable_check_count.min(2)))
}

#[derive(Debug, Clone)]
pub(super) struct CheckOptionFact {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) matches_target: bool,
}

pub(super) fn unambiguous_check_options(
    displayed_correct_ids: &[String],
    facts: impl IntoIterator<Item = CheckOptionFact>,
) -> Option<(String, Vec<StoryCheckOption>)> {
    let mut facts_by_id = BTreeMap::<String, CheckOptionFact>::new();
    for fact in facts {
        if fact.id.is_empty() || normalize_visible_label(&fact.label).is_empty() {
            continue;
        }
        facts_by_id
            .entry(fact.id.clone())
            .and_modify(|current| current.matches_target |= fact.matches_target)
            .or_insert(fact);
    }

    let mut groups = BTreeMap::<String, Vec<CheckOptionFact>>::new();
    for fact in facts_by_id.into_values() {
        groups
            .entry(normalize_visible_label(&fact.label))
            .or_default()
            .push(fact);
    }
    for group in groups.values_mut() {
        group.sort_by(|left, right| left.id.cmp(&right.id));
    }

    let correct = displayed_correct_ids.iter().find_map(|displayed_id| {
        groups.values().find_map(|group| {
            let candidate = group.iter().find(|fact| &fact.id == displayed_id)?;
            (candidate.matches_target && group.iter().all(|fact| fact.matches_target))
                .then(|| candidate.clone())
        })
    })?;
    let mut options = vec![StoryCheckOption {
        id: correct.id.clone(),
        label: correct.label,
    }];
    for group in groups.values() {
        if group.iter().all(|fact| !fact.matches_target) {
            let candidate = &group[0];
            options.push(StoryCheckOption {
                id: candidate.id.clone(),
                label: candidate.label.clone(),
            });
            if options.len() == MAX_CHECK_OPTIONS {
                break;
            }
        }
    }
    (options.len() >= 2).then_some((correct.id, options))
}

struct CheckSpec<'a> {
    kind: CheckKind,
    component_iri: &'a str,
    beat_id: &'a str,
    correct_option_id: &'a str,
    correct_kind: Option<&'a str>,
    subject_entity: Option<(&'a str, &'a str)>,
    question: String,
    options: Vec<StoryCheckOption>,
}

fn push_check(
    state: &AppState,
    checks: &mut Vec<StoryCheck>,
    mut spec: CheckSpec<'_>,
    include_check: bool,
) -> bool {
    if !nontrivial_options(&mut spec.options) {
        return false;
    }
    if include_check {
        let (correct_option_token, options, option_entities) =
            opaque_options(spec.options, spec.correct_option_id, uuid::Uuid::new_v4);
        let handle = register_check(
            state,
            CheckGrant {
                kind: spec.kind,
                component_iri: spec.component_iri.to_string(),
                beat_id: spec.beat_id.to_string(),
                correct_option_token,
                option_entities,
                correct_entity_iri: spec.correct_option_id.to_string(),
                correct_kind: spec.correct_kind.map(str::to_string),
                subject_entity: spec
                    .subject_entity
                    .map(|(iri, kind)| (iri.to_string(), kind.to_string())),
                expires_at: std::time::Instant::now() + CHECK_TTL,
            },
        );
        checks.push(StoryCheck {
            id: handle,
            question: spec.question,
            options,
        });
    }
    true
}

pub(super) fn opaque_options(
    options: Vec<StoryCheckOption>,
    correct_option_id: &str,
    mut random_uuid: impl FnMut() -> uuid::Uuid,
) -> (String, Vec<StoryCheckOption>, BTreeMap<String, String>) {
    let mut correct_option_token = String::new();
    let mut option_entities = BTreeMap::new();
    let mut randomized = options
        .into_iter()
        .map(|option| {
            let token = random_uuid().to_string();
            if option.id == correct_option_id {
                correct_option_token = token.clone();
            }
            option_entities.insert(token.clone(), option.id);
            (
                random_uuid(),
                StoryCheckOption {
                    id: token,
                    label: option.label,
                },
            )
        })
        .collect::<Vec<_>>();
    randomized.sort_by_key(|(order, _)| *order);
    (
        correct_option_token,
        randomized.into_iter().map(|(_, option)| option).collect(),
        option_entities,
    )
}

pub(super) fn register_check(state: &AppState, grant: CheckGrant) -> String {
    let handle = uuid::Uuid::new_v4().to_string();
    let now = std::time::Instant::now();
    let mut registry = state
        .story_checks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expired = registry
        .grants
        .iter()
        .filter(|(_, grant)| grant.expires_at <= now)
        .map(|(handle, _)| handle.clone())
        .collect::<Vec<_>>();
    for handle in expired {
        registry.grants.remove(&handle);
        remember_retired(&mut registry, handle, RetiredCheckKind::Expired);
    }
    if registry.grants.len() >= MAX_CHECK_GRANTS {
        if let Some(oldest) = registry
            .grants
            .iter()
            .min_by_key(|(_, grant)| grant.expires_at)
            .map(|(handle, _)| handle.clone())
        {
            registry.grants.remove(&oldest);
            remember_retired(&mut registry, oldest, RetiredCheckKind::Evicted);
        }
    }
    registry.grants.insert(handle.clone(), grant);
    handle
}

fn remember_retired(registry: &mut StoryCheckRegistry, handle: String, kind: RetiredCheckKind) {
    if registry.retired.len() >= MAX_RETIRED_CHECK_HANDLES {
        registry.retired.pop_front();
    }
    registry.retired.push_back((handle, kind));
}

pub(super) fn nontrivial_options(options: &mut Vec<StoryCheckOption>) -> bool {
    // Callers put the correct answer first. Stable retention therefore keeps
    // it while removing duplicate IDs/visible labels, then admits at most two
    // distinct distractors before opaque randomization.
    let mut seen_ids = BTreeSet::new();
    let mut seen_labels = BTreeSet::new();
    options.retain(|option| {
        let label = normalize_visible_label(&option.label);
        !option.id.is_empty()
            && !label.is_empty()
            && seen_ids.insert(option.id.clone())
            && seen_labels.insert(label)
    });
    options.truncate(MAX_CHECK_OPTIONS);
    options.len() >= 2
}

fn normalize_visible_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn records_for_components(
    state: &AppState,
    components: &[ComponentEntry],
) -> anyhow::Result<Vec<RecordData>> {
    let mut out = Vec::new();
    for component in components {
        let Some(iri) = component.iri.as_deref() else {
            continue;
        };
        out.extend(component_records(state, iri)?);
    }
    out.sort_by(|left, right| left.evidence.iri.cmp(&right.evidence.iri));
    out.dedup_by(|left, right| left.evidence.iri == right.evidence.iri);
    Ok(out)
}

pub fn grade_check(
    state: &AppState,
    check_id: &str,
    selected: &[String],
) -> anyhow::Result<GradeResult> {
    if uuid::Uuid::parse_str(check_id).is_err() {
        return Err(StoryCheckError::Malformed.into());
    }
    let grant = {
        let mut registry = state
            .story_checks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(grant) = registry.grants.get(check_id).cloned() else {
            let error = registry
                .retired
                .iter()
                .rev()
                .find(|(handle, _)| handle == check_id)
                .map(|(_, kind)| match kind {
                    RetiredCheckKind::Expired => StoryCheckError::Expired,
                    RetiredCheckKind::Evicted => StoryCheckError::Evicted,
                })
                .unwrap_or(StoryCheckError::Unknown);
            return Err(error.into());
        };
        if grant.expires_at <= std::time::Instant::now() {
            registry.grants.remove(check_id);
            remember_retired(
                &mut registry,
                check_id.to_string(),
                RetiredCheckKind::Expired,
            );
            return Err(StoryCheckError::Expired.into());
        }
        grant
    };
    if selected.len() != 1
        || selected
            .iter()
            .any(|token| !grant.option_entities.contains_key(token))
    {
        return Err(StoryCheckError::ForeignOption.into());
    }
    if let Some((subject_iri, subject_kind)) = &grant.subject_entity {
        if !story_entity_is_current(state, subject_iri, subject_kind)? {
            return Err(StoryCheckError::Stale.into());
        }
    }
    if !matches!(grant.kind, CheckKind::RecordKind)
        && !component_iri_is_current(state, &grant.component_iri)?
    {
        return Err(StoryCheckError::Stale.into());
    }
    let endpoint_is_current = |entity: &str| -> anyhow::Result<bool> {
        match grant.kind {
            CheckKind::Concerns => Ok(record_data(state, entity)?
                .is_some_and(|record| in_working_set(&record.evidence.status))),
            CheckKind::Realizes => code_entity_is_current(state, entity),
            CheckKind::RecordKind => Ok(record_data(state, entity)?
                .is_some_and(|record| in_working_set(&record.evidence.status))),
        }
    };
    for entity in grant.option_entities.values() {
        if !endpoint_is_current(entity)? {
            return Err(StoryCheckError::Stale.into());
        }
    }
    let relationship_is_current = |entity: &str| match grant.kind {
        CheckKind::Concerns => record_concerns_component(state, entity, &grant.component_iri),
        CheckKind::Realizes => code_realizes_component(state, entity, &grant.component_iri),
        CheckKind::RecordKind => Ok(record_data(state, entity)?.is_some_and(|record| {
            grant.correct_kind.as_deref() == Some(record.evidence.kind.as_str())
        })),
    };
    let mut distractor_became_valid = false;
    for (token, entity) in &grant.option_entities {
        if token != &grant.correct_option_token && relationship_is_current(entity)? {
            distractor_became_valid = true;
            break;
        }
    }
    if !relationship_is_current(&grant.correct_entity_iri)? || distractor_became_valid {
        return Err(StoryCheckError::Stale.into());
    }
    let correct = selected == [grant.correct_option_token.as_str()];
    let evidence_iris = if correct {
        vec![grant.correct_entity_iri.clone()]
    } else {
        vec![grant.correct_entity_iri]
    };
    Ok(GradeResult {
        correct,
        feedback: if correct {
            "Correct. The selected relationship is present in current authoritative project knowledge."
                .to_string()
        } else {
            "Not quite. Revisit the evidence linked to this Story beat.".to_string()
        },
        revisit_beat_id: (!correct).then_some(grant.beat_id),
        evidence_iris,
    })
}
