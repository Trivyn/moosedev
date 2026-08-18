//! Opaque comprehension-check issuance and graph-current grading.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::graph::{in_working_set, relevant_context_snapshot, AppState, ComponentEntry};

use super::grounding::{
    code_entity_is_current, code_realizes_component, component_iri_is_current, component_records,
    edge_targets, record_concerns_component, record_data, record_supersedes, record_weighs,
    story_entity_is_current, RecordData,
};
use super::model::{
    friendly_record_kind, CheckGrant, CheckKind, GradeResult, RetiredCheckKind, StoryBeat,
    StoryCandidate, StoryCheck, StoryCheckError, StoryCheckOption, StoryCheckRegistry,
    StoryCodeAnchor, StoryNarrativeSection, StorySubject,
};
use super::resolution::StoryResolutionIndex;

pub(super) const MAX_CHECK_GRANTS: usize = 1_024;
const MAX_RETIRED_CHECK_HANDLES: usize = 1_024;
pub(super) const MAX_CHECK_OPTIONS: usize = 3;
pub(super) const CHECK_TTL: Duration = Duration::from_secs(30 * 60);

pub(super) fn prepare_record_kind_checks(
    state: &AppState,
    subject: &StorySubject,
    beats: &[StoryBeat],
) -> anyhow::Result<Vec<PreparedStoryCheck>> {
    let displayed = beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .filter(|evidence| {
            evidence.kind != "SystemComponent"
                && evidence.kind != "CodeEntity"
                && evidence.kind != "Entity"
        })
        .collect::<Vec<_>>();
    let shown_evidence_iris = beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .map(|evidence| evidence.iri.clone())
        .collect::<BTreeSet<_>>();
    let all_records = relevant_context_snapshot(state, None, 96, false)?
        .into_iter()
        .filter_map(|item| record_data(state, &item.iri).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut used_kinds = BTreeSet::new();
    for correct in displayed {
        if !used_kinds.insert(correct.kind.clone()) {
            continue;
        }
        let facts = all_records.iter().map(|record| CheckOptionFact {
            id: record.evidence.iri.clone(),
            label: record.evidence.title.clone(),
            matches_target: record.evidence.kind == correct.kind,
            kind: record.evidence.kind.clone(),
            shown_in_story: shown_evidence_iris.contains(&record.evidence.iri),
        });
        let Some((correct_id, options)) =
            unambiguous_check_options(std::slice::from_ref(&correct.iri), facts)
        else {
            continue;
        };
        prepare_check(
            &mut checks,
            CheckSpec {
                kind: CheckKind::RecordKind,
                counterpart_iri: "",
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
        );
        if checks.len() == 2 {
            break;
        }
    }
    Ok(checks)
}

/// Checks that ask WHY, from the relationships the Story is built to explain:
/// which record replaced a retired one, and which approach a decision rejected.
///
/// These probe the reasoning a reader is meant to carry away, where the
/// membership questions only probe which end of an edge something sits on. Both
/// are emitted only when the Story's OWN evidence contains the pair — a question
/// about material the reader never saw is a lookup, not a check.
pub(super) fn prepare_relationship_checks(
    state: &AppState,
    subject: &StorySubject,
    beats: &[StoryBeat],
) -> anyhow::Result<Vec<PreparedStoryCheck>> {
    let shown = beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .filter(|evidence| evidence.kind != "SystemComponent" && evidence.kind != "CodeEntity")
        .collect::<Vec<_>>();
    let shown_iris = shown
        .iter()
        .map(|evidence| evidence.iri.clone())
        .collect::<BTreeSet<_>>();
    let subject_entity = match subject {
        StorySubject::Entity { iri, kind, .. } => Some((iri.as_str(), kind.as_str())),
        StorySubject::Topic { .. } => None,
    };
    let mut checks = Vec::new();

    // "Which record replaced X?" — anchored on the CURRENT record, which is what
    // a Story shows. The retired record it replaced is named in the question but
    // is not itself an option: working-set filtering keeps it out of the
    // evidence, and every option must be something the reader could have seen.
    for successor in &shown {
        let superseded = edge_targets(state, &successor.iri, "supersedes")?;
        let Some(retired) = superseded.first() else {
            continue;
        };
        let Some(retired_data) = record_data(state, retired)? else {
            continue;
        };
        let facts = shown.iter().map(|evidence| CheckOptionFact {
            matches_target: evidence.iri == successor.iri,
            shown_in_story: true,
            kind: evidence.kind.clone(),
            id: evidence.iri.clone(),
            label: evidence.title.clone(),
        });
        let Some((correct_id, options)) =
            unambiguous_check_options(std::slice::from_ref(&successor.iri), facts)
        else {
            continue;
        };
        if prepare_check(
            &mut checks,
            CheckSpec {
                kind: CheckKind::Supersedes,
                counterpart_iri: retired,
                correct_option_id: &correct_id,
                correct_kind: None,
                subject_entity,
                question: format!(
                    "Which record replaced \u{201c}{}\u{201d}?",
                    retired_data.evidence.title
                ),
                options,
            },
        ) {
            break;
        }
    }

    // "Which approach did X reject?" — the rationale a Story exists to carry.
    for decision in &shown {
        let alternatives = edge_targets(state, &decision.iri, "weighs")?;
        let Some(rejected) = alternatives.first() else {
            continue;
        };
        let Some(rejected_data) = record_data(state, rejected)? else {
            continue;
        };
        let mut facts = vec![CheckOptionFact {
            matches_target: true,
            shown_in_story: shown_iris.contains(rejected),
            kind: rejected_data.evidence.kind.clone(),
            id: rejected_data.evidence.iri.clone(),
            label: rejected_data.evidence.title.clone(),
        }];
        // Distractors: approaches OTHER decisions in this Story rejected. A
        // reader who followed the reasoning knows which decision weighed which.
        for other in shown.iter().filter(|other| other.iri != decision.iri) {
            for candidate in edge_targets(state, &other.iri, "weighs")? {
                if candidate == *rejected {
                    continue;
                }
                if let Some(data) = record_data(state, &candidate)? {
                    facts.push(CheckOptionFact {
                        matches_target: false,
                        shown_in_story: shown_iris.contains(&candidate),
                        kind: data.evidence.kind.clone(),
                        id: data.evidence.iri.clone(),
                        label: data.evidence.title.clone(),
                    });
                }
            }
        }
        let Some((correct_id, options)) =
            unambiguous_check_options(std::slice::from_ref(rejected), facts)
        else {
            continue;
        };
        if prepare_check(
            &mut checks,
            CheckSpec {
                kind: CheckKind::Weighs,
                counterpart_iri: &decision.iri,
                correct_option_id: &correct_id,
                correct_kind: None,
                subject_entity,
                question: format!(
                    "Which approach did \u{201c}{}\u{201d} reject?",
                    decision.title
                ),
                options,
            },
        ) {
            break;
        }
    }
    Ok(checks)
}

pub(super) fn prepare_checks(
    state: &AppState,
    component: &StoryCandidate,
    beats: &[StoryBeat],
    index: &StoryResolutionIndex,
    component_records: &[RecordData],
    component_code: &[StoryCodeAnchor],
) -> anyhow::Result<Vec<PreparedStoryCheck>> {
    let mut checks = Vec::new();
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
    let shown_evidence_iris = beats
        .iter()
        .flat_map(|beat| beat.evidence.iter())
        .map(|evidence| evidence.iri.clone())
        .collect::<BTreeSet<_>>();
    let record_facts = records_for_components(state, &index.components)?
        .into_iter()
        .chain(component_records.iter().cloned())
        .map(|record| CheckOptionFact {
            matches_target: target_record_ids.contains(&record.evidence.iri),
            shown_in_story: shown_evidence_iris.contains(&record.evidence.iri),
            kind: record.evidence.kind.clone(),
            id: record.evidence.iri,
            label: record.evidence.title,
        });
    if let Some((correct_id, options)) =
        unambiguous_check_options(&displayed_record_ids, record_facts)
    {
        prepare_check(
            &mut checks,
            CheckSpec {
                kind: CheckKind::Concerns,
                counterpart_iri: &component.iri,
                correct_option_id: &correct_id,
                correct_kind: None,
                subject_entity: None,
                question: format!("Which accepted record is linked to {}?", component.label),
                options,
            },
        );
    }
    if checks.len() < 2 {
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
        let shown_code_iris = beats
            .iter()
            .flat_map(|beat| beat.code_anchors.iter())
            .filter_map(|anchor| anchor.entity_iri.clone())
            .collect::<BTreeSet<_>>();
        let code_facts = index
            .code_entities
            .iter()
            .chain(component_code.iter())
            .filter_map(|anchor| {
                anchor.entity_iri.as_ref().map(|entity| CheckOptionFact {
                    matches_target: target_code_ids.contains(entity),
                    shown_in_story: shown_code_iris.contains(entity),
                    kind: "CodeEntity".to_string(),
                    id: entity.clone(),
                    label: anchor.label.clone(),
                })
            })
            .filter(|fact| fact.matches_target || reads_as_code_identifier(&fact.label));
        if let Some((correct_id, options)) =
            unambiguous_check_options(&displayed_code_ids, code_facts)
        {
            prepare_check(
                &mut checks,
                CheckSpec {
                    kind: CheckKind::Realizes,
                    counterpart_iri: &component.iri,
                    correct_option_id: &correct_id,
                    correct_kind: None,
                    subject_entity: None,
                    question: format!("Which code entity realizes {}?", component.label),
                    options,
                },
            );
        }
    }
    // Keep the API bounded even if future generators add more check kinds.
    checks.truncate(2);
    Ok(checks)
}

#[derive(Debug, Clone)]
pub(super) struct CheckOptionFact {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) matches_target: bool,
    /// Typed kind, so a distractor can be drawn from the answer's OWN kind
    /// instead of letting the kind give the answer away.
    pub(super) kind: String,
    /// Whether this candidate appears in the Story the reader just read.
    pub(super) shown_in_story: bool,
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
        label: correct.label.clone(),
    }];

    // RANK the candidates; do not take whatever the map happened to yield.
    // Iterating `groups` directly means "prefer whatever label sorts first",
    // which let punctuation-leading labels win every draw.
    let mut candidates = groups
        .values()
        .filter(|group| group.iter().all(|fact| !fact.matches_target))
        .map(|group| &group[0])
        .collect::<Vec<_>>();
    candidates.sort_by_key(|fact| {
        (
            // A candidate the reader just read is the most demanding wrong
            // answer: choosing correctly means knowing which subject it
            // belonged to, not which title shares words with the question.
            !fact.shown_in_story,
            // Matching the answer's kind keeps the kind from leaking it.
            fact.kind != correct.kind,
            stable_rank(&fact.id),
        )
    });
    for candidate in candidates {
        options.push(StoryCheckOption {
            id: candidate.id.clone(),
            label: candidate.label.clone(),
        });
        if options.len() == MAX_CHECK_OPTIONS {
            break;
        }
    }
    (options.len() >= 2).then_some((correct.id, options))
}

/// Deterministic order for candidates of equal rank (FNV-1a over the IRI).
///
/// Deliberately not derived from the LABEL: label ordering is the defect this
/// replaces. Hashing the IRI keeps a Story's checks reproducible — which
/// Constraint `c1b8a8db` requires — without letting the text shape decide.
/// Presentation order is randomized separately, in `opaque_options`.
fn stable_rank(id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Whether a label reads as a code identifier.
///
/// Only distractors are filtered on this. A producer can mint entities whose
/// display label is not an identifier at all — quoted object keys such as
/// `'& h1'0` — and offering those as wrong answers makes a question absurd
/// rather than difficult.
pub(super) fn reads_as_code_identifier(label: &str) -> bool {
    // Deliberately narrow: it must BEGIN like a name and carry no quotes. A
    // stricter char-by-char rule would reject real labels that legitimately
    // contain spaces and punctuation, such as `HashMap<String, u32>`.
    label
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && !label.contains(['\'', '"'])
}

struct CheckSpec<'a> {
    kind: CheckKind,
    counterpart_iri: &'a str,
    correct_option_id: &'a str,
    correct_kind: Option<&'a str>,
    subject_entity: Option<(&'a str, &'a str)>,
    question: String,
    options: Vec<StoryCheckOption>,
}

pub(super) struct PreparedStoryCheck {
    question: String,
    options: Vec<StoryCheckOption>,
    grant: CheckGrant,
}

fn prepare_check(checks: &mut Vec<PreparedStoryCheck>, mut spec: CheckSpec<'_>) -> bool {
    if !nontrivial_options(&mut spec.options) {
        return false;
    }
    let (correct_option_token, options, option_entities) =
        opaque_options(spec.options, spec.correct_option_id, uuid::Uuid::new_v4);
    checks.push(PreparedStoryCheck {
        question: spec.question,
        options,
        grant: CheckGrant {
            kind: spec.kind,
            counterpart_iri: spec.counterpart_iri.to_string(),
            section_id: String::new(),
            correct_option_token,
            option_entities,
            correct_entity_iri: spec.correct_option_id.to_string(),
            correct_kind: spec.correct_kind.map(str::to_string),
            subject_entity: spec
                .subject_entity
                .map(|(iri, kind)| (iri.to_string(), kind.to_string())),
            expires_at: std::time::Instant::now() + CHECK_TTL,
        },
    });
    true
}

pub(super) fn bind_prepared_check_sections(
    checks: &mut Vec<PreparedStoryCheck>,
    narrative: &[StoryNarrativeSection],
) {
    checks.retain_mut(|check| {
        let Some(section) = narrative.iter().find(|section| {
            section.paragraphs.iter().any(|paragraph| {
                paragraph
                    .citation_iris
                    .contains(&check.grant.correct_entity_iri)
            })
        }) else {
            return false;
        };
        check.grant.section_id = section.id.clone();
        true
    });
}

pub(super) fn opaque_options(
    options: Vec<StoryCheckOption>,
    correct_option_id: &str,
    mut random_uuid: impl FnMut() -> uuid::Uuid,
) -> (String, Vec<StoryCheckOption>, BTreeMap<String, String>) {
    // Entity IDs become opaque tokens; independent UUID sort keys ensure the
    // correct answer's position reveals nothing about which token it received.
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

#[cfg(test)]
pub(super) fn register_check(state: &AppState, grant: CheckGrant) -> String {
    let mut registry = state
        .story_checks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    register_check_in_registry(&mut registry, grant)
}

pub(super) fn issue_prepared_checks(
    state: &AppState,
    prepared: Vec<PreparedStoryCheck>,
) -> Vec<StoryCheck> {
    let mut registry = state
        .story_checks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    prepared
        .into_iter()
        .map(|prepared| StoryCheck {
            id: register_check_in_registry(&mut registry, prepared.grant),
            question: prepared.question,
            options: prepared.options,
        })
        .collect()
}

fn register_check_in_registry(registry: &mut StoryCheckRegistry, grant: CheckGrant) -> String {
    let handle = uuid::Uuid::new_v4().to_string();
    let now = std::time::Instant::now();
    let expired = registry
        .grants
        .iter()
        .filter(|(_, grant)| grant.expires_at <= now)
        .map(|(handle, _)| handle.clone())
        .collect::<Vec<_>>();
    for handle in expired {
        registry.grants.remove(&handle);
        // Retain the bounded tombstone so grading can distinguish expiry or
        // eviction from a handle that never belonged to this process.
        remember_retired(registry, handle, RetiredCheckKind::Expired);
    }
    if registry.grants.len() >= MAX_CHECK_GRANTS {
        if let Some(oldest) = registry
            .grants
            .iter()
            .min_by_key(|(_, grant)| grant.expires_at)
            .map(|(handle, _)| handle.clone())
        {
            registry.grants.remove(&oldest);
            remember_retired(registry, oldest, RetiredCheckKind::Evicted);
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
    let counterpart_is_current = match grant.kind {
        // The question is about the record itself; there is no other endpoint.
        CheckKind::RecordKind => true,
        CheckKind::Concerns | CheckKind::Realizes => {
            component_iri_is_current(state, &grant.counterpart_iri)?
        }
        // Only EXISTENCE is required: a superseded record is retired by
        // definition, and demanding it be current would retire every such check
        // the moment it became answerable.
        CheckKind::Supersedes => record_data(state, &grant.counterpart_iri)?.is_some(),
        CheckKind::Weighs => record_data(state, &grant.counterpart_iri)?
            .is_some_and(|record| in_working_set(&record.evidence.status)),
    };
    if !counterpart_is_current {
        return Err(StoryCheckError::Stale.into());
    }
    let endpoint_is_current = |entity: &str| -> anyhow::Result<bool> {
        match grant.kind {
            CheckKind::Concerns
            | CheckKind::RecordKind
            | CheckKind::Supersedes
            | CheckKind::Weighs => Ok(record_data(state, entity)?
                .is_some_and(|record| in_working_set(&record.evidence.status))),
            CheckKind::Realizes => code_entity_is_current(state, entity),
        }
    };
    for entity in grant.option_entities.values() {
        if !endpoint_is_current(entity)? {
            return Err(StoryCheckError::Stale.into());
        }
    }
    let relationship_is_current = |entity: &str| match grant.kind {
        CheckKind::Concerns => record_concerns_component(state, entity, &grant.counterpart_iri),
        CheckKind::Realizes => code_realizes_component(state, entity, &grant.counterpart_iri),
        CheckKind::RecordKind => Ok(record_data(state, entity)?.is_some_and(|record| {
            grant.correct_kind.as_deref() == Some(record.evidence.kind.as_str())
        })),
        CheckKind::Supersedes => record_supersedes(state, entity, &grant.counterpart_iri),
        CheckKind::Weighs => record_weighs(state, entity, &grant.counterpart_iri),
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
    Ok(GradeResult {
        correct,
        feedback: if correct {
            "Correct. The selected relationship is present in current authoritative project knowledge."
                .to_string()
        } else {
            "Not quite. Revisit the cited evidence in that Story section.".to_string()
        },
        revisit_section_id: (!correct).then_some(grant.section_id),
        evidence_iris: vec![grant.correct_entity_iri],
    })
}
