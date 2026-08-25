use super::super::grounding::story_section_kind;
use super::super::model::{
    NarrationFailureReason, StoryCodeAnchor, StoryEvidenceDetail, StoryLiteralProperty,
    StoryNarrationCoverage, StoryRelationDirection, StoryRun, StorySectionKind, StorySubject,
    StoryTimelineEvent,
};
use crate::graph::AppState;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Caps prompt cost and latency even when the configured model has a larger context window.
const MAX_NARRATION_PROMPT_TOKENS: usize = 32_768;
/// Uses a conservative byte heuristic so prompt selection errs toward staying under budget.
const ESTIMATED_BYTES_PER_TOKEN: usize = 3;
/// Keeps the provenance schema and the model's all-sources citation obligation tractable.
const MAX_SOURCE_GROUPS: usize = 12;
/// Retains a compact early/middle/latest chronological spine before incidental evidence.
const MAX_CHRONOLOGY_MILESTONES: usize = 10;
/// Leaves room in each chronology sample for endpoints and even coverage after transitions.
const MAX_SUPERSESSION_MILESTONES: usize = 4;
/// Bump when prompt or response semantics change so stale cached prose cannot cross contracts.
const NARRATION_CONTRACT_VERSION: u8 = 1;

/// Reserve most of the model context for its response and provider overhead;
/// the 32k ceiling also keeps latency and local-model degradation bounded.
pub fn narration_prompt_token_budget(context_window_tokens: usize) -> usize {
    (context_window_tokens / 4).min(MAX_NARRATION_PROMPT_TOKENS)
}

#[derive(Clone, Serialize)]
struct NarrationSource {
    source_id: String,
    section_id: String,
    evidence_iris: Vec<String>,
    evidence: Vec<StoryEvidenceDetail>,
    timeline: Vec<StoryTimelineEvent>,
    code_anchors: Vec<StoryCodeAnchor>,
}

#[derive(Serialize)]
struct NarrationPayload<'a> {
    story: NarrationStory<'a>,
    outline: Vec<NarrationOutline<'a>>,
    sources: &'a [NarrationSource],
}

#[derive(Serialize)]
struct NarrationStory<'a> {
    title: &'a str,
    subject: &'a StorySubject,
    goal: &'a str,
}

#[derive(Serialize)]
struct NarrationOutline<'a> {
    section_id: &'a str,
    kind: &'a StorySectionKind,
    title: &'a str,
}

pub(super) struct NarrationPacket {
    pub(super) prompt: String,
    pub(super) schema: serde_json::Value,
    pub(super) citations_by_source: BTreeMap<String, Vec<String>>,
    pub(super) sections_by_source: BTreeMap<String, String>,
    pub(super) coverage: StoryNarrationCoverage,
    fingerprint: String,
}

impl NarrationPacket {
    pub(super) fn cache_key(&self, state: &AppState) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            NARRATION_CONTRACT_VERSION,
            state.project_write_generation(),
            state.model,
            narration_prompt_token_budget(state.llm_context_window_tokens),
            self.fingerprint,
        )
    }
}

pub(super) fn build_narration_packet(
    state: &AppState,
    run: &StoryRun,
    context_window_tokens: usize,
) -> Result<NarrationPacket, NarrationFailureReason> {
    let evidence = compact_narration_evidence(state, run);
    if evidence.is_empty() {
        return Err(NarrationFailureReason::PacketTooLarge);
    }
    let by_iri = evidence
        .iter()
        .map(|item| (item.iri.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let priority = evidence_priority(run, &by_iri);
    let budget_bytes = narration_prompt_token_budget(context_window_tokens)
        .saturating_mul(ESTIMATED_BYTES_PER_TOKEN);
    let mut selected = Vec::<StoryEvidenceDetail>::new();
    for iri in priority {
        let Some(item) = by_iri.get(iri.as_str()) else {
            continue;
        };
        if serde_json::to_vec(item).map_or(true, |bytes| bytes.len() > budget_bytes) {
            continue;
        }
        selected.push((*item).clone());
        let candidate = packet_parts(run, &selected)?;
        if candidate.total_bytes > budget_bytes {
            selected.pop();
        }
    }
    if selected.is_empty() {
        return Err(NarrationFailureReason::PacketTooLarge);
    }
    let parts = packet_parts(run, &selected)?;
    if parts.total_bytes > budget_bytes {
        return Err(NarrationFailureReason::PacketTooLarge);
    }
    if run.narrative.iter().any(|section| {
        !parts
            .sections_by_source
            .values()
            .any(|value| value == &section.id)
    }) {
        return Err(NarrationFailureReason::PacketTooLarge);
    }
    let fingerprint = hex_sha256(parts.prompt.as_bytes());
    Ok(NarrationPacket {
        prompt: parts.prompt,
        schema: parts.schema,
        citations_by_source: parts.citations_by_source,
        sections_by_source: parts.sections_by_source,
        coverage: StoryNarrationCoverage {
            eligible_entities: evidence.len(),
            included_entities: selected.len(),
            source_groups: parts.source_count,
            truncated: selected.len() < evidence.len(),
        },
        fingerprint,
    })
}

struct PacketParts {
    prompt: String,
    schema: serde_json::Value,
    citations_by_source: BTreeMap<String, Vec<String>>,
    sections_by_source: BTreeMap<String, String>,
    source_count: usize,
    total_bytes: usize,
}

fn packet_parts(
    run: &StoryRun,
    selected: &[StoryEvidenceDetail],
) -> Result<PacketParts, NarrationFailureReason> {
    let sources = group_sources(run, selected);
    let citations_by_source = sources
        .iter()
        .map(|source| (source.source_id.clone(), source.evidence_iris.clone()))
        .collect::<BTreeMap<_, _>>();
    let sections_by_source = sources
        .iter()
        .map(|source| (source.source_id.clone(), source.section_id.clone()))
        .collect::<BTreeMap<_, _>>();
    let payload = NarrationPayload {
        story: NarrationStory {
            title: &run.title,
            subject: &run.subject,
            goal: &run.goal,
        },
        outline: run
            .narrative
            .iter()
            .map(|section| NarrationOutline {
                section_id: &section.id,
                kind: &section.kind,
                title: &section.title,
            })
            .collect(),
        sources: &sources,
    };
    let section_ids = run
        .narrative
        .iter()
        .map(|section| section.id.clone())
        .collect::<Vec<_>>();
    let prompt = format!(
        "Write one cohesive project account for a maintainer new to this codebase. Use ONLY the supplied grounded sources. Connect causes, decisions, consequences, implementation, and change over time; explain project-specific terms in plain language. Preserve lifecycle meaning. Return only the requested JSON paragraphs. Each source belongs to its section_id: cite it only in that section. Put source IDs only in the source_ids metadata; never write [s1], [s2], or other source markers inside text. Use every source ID at least once, use no other source IDs, and return these section IDs in this exact order: {}. Grounded narration packet: {}",
        section_ids.join(", "),
        serde_json::to_string(&payload).map_err(|_| NarrationFailureReason::PacketTooLarge)?,
    );
    let schema = narration_response_schema(&section_ids, &sources);
    let total_bytes = prompt
        .len()
        .saturating_add(serde_json::to_vec(&schema).map_or(usize::MAX, |bytes| bytes.len()));
    Ok(PacketParts {
        prompt,
        schema,
        citations_by_source,
        sections_by_source,
        source_count: sources.len(),
        total_bytes,
    })
}

fn group_sources(run: &StoryRun, selected: &[StoryEvidenceDetail]) -> Vec<NarrationSource> {
    let selected_iris = selected
        .iter()
        .map(|item| item.iri.as_str())
        .collect::<BTreeSet<_>>();
    let mut by_section = BTreeMap::<String, Vec<StoryEvidenceDetail>>::new();
    for item in selected {
        by_section
            .entry(section_for_evidence(run, item))
            .or_default()
            .push(item.clone());
    }
    let mut groups = run
        .narrative
        .iter()
        .filter_map(|section| {
            by_section
                .remove(&section.id)
                // Every rendered section needs a source to satisfy the strict
                // response schema; reuse grounded evidence rather than inventing it.
                .or_else(|| selected.first().cloned().map(|item| vec![item]))
                .map(|items| (section.id.clone(), items))
        })
        .collect::<Vec<_>>();
    groups.extend(by_section);
    debug_assert!(groups.len() <= MAX_SOURCE_GROUPS);
    groups
        .into_iter()
        .enumerate()
        .map(|(index, (section_id, evidence))| {
            let evidence_iris = evidence
                .iter()
                .map(|item| item.iri.clone())
                .collect::<Vec<_>>();
            let group_iris = evidence_iris.iter().cloned().collect::<BTreeSet<_>>();
            NarrationSource {
                source_id: format!("s{}", index + 1),
                section_id,
                evidence_iris,
                evidence,
                timeline: run
                    .timeline
                    .iter()
                    .filter(|event| group_iris.contains(&event.evidence_iri))
                    .cloned()
                    .collect(),
                code_anchors: narratable_code_anchors(run)
                    .into_iter()
                    .filter(|anchor| {
                        anchor.entity_iri.as_deref().is_some_and(|iri| {
                            selected_iris.contains(iri) && group_iris.contains(iri)
                        })
                    })
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

fn evidence_priority(run: &StoryRun, by_iri: &BTreeMap<&str, &StoryEvidenceDetail>) -> Vec<String> {
    let mut ordered = Vec::<String>::new();
    let mut seen = BTreeSet::<String>::new();
    let mut push = |iri: &str| {
        if by_iri.contains_key(iri) && seen.insert(iri.to_string()) {
            ordered.push(iri.to_string());
        }
    };
    if let StorySubject::Entity { iri, .. } = &run.subject {
        push(iri);
    }
    // Chronology is a coverage dimension, not whatever remains after current
    // governance records. Preserve old, middle, new, and supersession events.
    for iri in chronology_spine(run, by_iri) {
        push(&iri);
    }
    // Reserve one semantically appropriate source for every rendered section.
    // Prefer evidence whose own text names the subject; incidental feature
    // records may share a broad component edge without explaining the subject.
    for section in &run.narrative {
        let candidate = by_iri
            .values()
            .copied()
            .filter(|item| section_for_evidence(run, item) == section.id)
            .find(|item| evidence_names_subject(run, item))
            .or_else(|| {
                by_iri
                    .values()
                    .copied()
                    .find(|item| section_for_evidence(run, item) == section.id)
            });
        if let Some(item) = candidate {
            push(&item.iri);
        }
    }
    for beat in &run.beats {
        for item in &beat.evidence {
            push(&item.iri);
        }
    }
    if let StorySubject::Entity { iri, .. } = &run.subject {
        for item in &run.evidence {
            if directly_relates_to_subject(item, iri) {
                push(&item.iri);
            }
        }
    }
    for anchor in &run.code_anchors {
        if let Some(iri) = &anchor.entity_iri {
            push(iri);
        }
    }
    for item in &run.evidence {
        if !item.status.eq_ignore_ascii_case("proposed")
            && matches!(
                item.kind.as_str(),
                "ArchitecturalDecision" | "Requirement" | "Constraint"
            )
        {
            push(&item.iri);
        }
    }
    for section in &run.narrative {
        for paragraph in &section.paragraphs {
            for iri in &paragraph.citation_iris {
                push(iri);
            }
        }
    }
    for event in &run.timeline {
        push(&event.evidence_iri);
    }
    for item in &run.evidence {
        push(&item.iri);
    }
    ordered
}

fn directly_relates_to_subject(item: &StoryEvidenceDetail, subject_iri: &str) -> bool {
    item.iri == subject_iri
        || item
            .relations
            .iter()
            .any(|relation| relation.target_iri == subject_iri)
}

fn chronology_spine(run: &StoryRun, by_iri: &BTreeMap<&str, &StoryEvidenceDetail>) -> Vec<String> {
    let mut all = Vec::<String>::new();
    let mut direct = Vec::<String>::new();
    let mut seen = BTreeSet::<String>::new();
    let subject_iri = match &run.subject {
        StorySubject::Entity { iri, .. } => Some(iri.as_str()),
        StorySubject::Topic { .. } => None,
    };
    for event in &run.timeline {
        let iri = event.evidence_iri.as_str();
        let Some(item) = by_iri.get(iri) else {
            continue;
        };
        if seen.insert(iri.to_string()) {
            all.push(iri.to_string());
            if subject_iri.is_some_and(|subject| directly_relates_to_subject(item, subject)) {
                direct.push(iri.to_string());
            }
        }
    }
    let mut focused = all
        .iter()
        .filter(|iri| {
            by_iri
                .get(iri.as_str())
                .is_some_and(|item| evidence_names_subject(run, item))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    // Once a subject-named record is found, retain its complete lifecycle chain
    // even when predecessor titles used older terminology.
    loop {
        let before = focused.len();
        for event in &run.timeline {
            let linked = event
                .predecessor_iris
                .iter()
                .chain(&event.successor_iris)
                .any(|iri| focused.contains(iri));
            if focused.contains(&event.evidence_iri) || linked {
                focused.insert(event.evidence_iri.clone());
                focused.extend(event.predecessor_iris.iter().cloned());
                focused.extend(event.successor_iris.iter().cloned());
            }
        }
        if focused.len() == before {
            break;
        }
    }
    let focused = all
        .iter()
        .filter(|iri| focused.contains(*iri))
        .cloned()
        .collect::<Vec<_>>();
    let mut selected = Vec::with_capacity(MAX_CHRONOLOGY_MILESTONES);
    let mut seen = BTreeSet::new();
    append_chronology_sample(run, &focused, &mut selected, &mut seen);
    append_chronology_sample(run, &direct, &mut selected, &mut seen);
    append_chronology_sample(run, &all, &mut selected, &mut seen);
    selected.truncate(MAX_CHRONOLOGY_MILESTONES);
    selected
}

fn append_chronology_sample(
    run: &StoryRun,
    candidates: &[String],
    selected: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) {
    if candidates.is_empty() || selected.len() >= MAX_CHRONOLOGY_MILESTONES {
        return;
    }
    let remaining = MAX_CHRONOLOGY_MILESTONES - selected.len();
    let transition_indexes = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, iri)| {
            run.timeline
                .iter()
                .find(|event| event.evidence_iri == *iri)
                .filter(|event| {
                    event.relation.is_some()
                        || !event.predecessor_iris.is_empty()
                        || !event.successor_iris.is_empty()
                })
                .map(|_| index)
        })
        .collect::<Vec<_>>();
    let mut seen_indexes = BTreeSet::new();
    let mut indexes = Vec::with_capacity(remaining);
    let mut select_index = |index: usize| {
        if indexes.len() < remaining && seen_indexes.insert(index) {
            indexes.push(index);
        }
    };
    select_index(0);
    select_index(candidates.len() - 1);
    for index in evenly_spaced_indexes(candidates.len(), remaining.min(6)) {
        select_index(index);
    }
    for transition_position in evenly_spaced_indexes(
        transition_indexes.len(),
        remaining
            .min(MAX_SUPERSESSION_MILESTONES)
            .min(transition_indexes.len()),
    ) {
        select_index(transition_indexes[transition_position]);
    }
    for index in evenly_spaced_indexes(candidates.len(), remaining) {
        select_index(index);
    }
    for index in indexes {
        let iri = candidates[index].clone();
        if seen.insert(iri.clone()) {
            selected.push(iri);
        }
    }
}

fn evenly_spaced_indexes(len: usize, count: usize) -> Vec<usize> {
    match (len, count.min(len)) {
        (_, 0) => vec![],
        (_, 1) => vec![0],
        (len, count) => (0..count)
            .map(|index| index * (len - 1) / (count - 1))
            .collect(),
    }
}

fn section_for_evidence(run: &StoryRun, item: &StoryEvidenceDetail) -> String {
    let subject_iri = match &run.subject {
        StorySubject::Entity { iri, .. } => Some(iri.as_str()),
        StorySubject::Topic { .. } => None,
    };
    let kind = story_section_kind(item, subject_iri);
    run.narrative
        .iter()
        .find(|section| section.kind == kind)
        .or_else(|| run.narrative.first())
        .map(|section| section.id.clone())
        .unwrap_or_else(|| "orientation".to_string())
}

fn evidence_names_subject(run: &StoryRun, item: &StoryEvidenceDetail) -> bool {
    let label = match &run.subject {
        StorySubject::Entity { label, .. } | StorySubject::Topic { label, .. } => label,
    };
    let tokens = label
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }
    let haystack = format!(
        "{} {}",
        item.title.to_ascii_lowercase(),
        item.description
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
    );
    tokens.iter().all(|token| haystack.contains(token))
}

fn narration_response_schema(
    section_ids: &[String],
    sources: &[NarrationSource],
) -> serde_json::Value {
    let variants = section_ids
        .iter()
        .map(|section_id| {
            let source_ids = sources
                .iter()
                .filter(|source| source.section_id == *section_id)
                .map(|source| source.source_id.clone())
                .collect::<Vec<_>>();
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["section_id", "text", "source_ids"],
                "properties": {
                    "section_id": {"const": section_id},
                    "text": {"type": "string", "minLength": 1},
                    "source_ids": {
                        "type": "array",
                        "minItems": 1,
                        "items": {"type": "string", "enum": source_ids}
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["paragraphs"],
        "properties": {
            "paragraphs": {
                "type": "array",
                "minItems": section_ids.len(),
                "items": {"oneOf": variants}
            }
        }
    })
}

pub(in crate::stories) fn narration_evidence_is_eligible(run: &StoryRun) -> bool {
    !run.gaps.iter().any(|gap| gap.id == "subject-drift") && !narratable_evidence(run).is_empty()
}

fn narratable_evidence(run: &StoryRun) -> Vec<StoryEvidenceDetail> {
    run.evidence
        .iter()
        .filter(|evidence| {
            !evidence.suppressed && !evidence.status.eq_ignore_ascii_case("proposed")
        })
        .cloned()
        .collect()
}

fn compact_narration_evidence(state: &AppState, run: &StoryRun) -> Vec<StoryEvidenceDetail> {
    let mut evidence = narratable_evidence(run);
    let allowed = evidence
        .iter()
        .map(|item| item.iri.clone())
        .collect::<BTreeSet<_>>();
    for item in &mut evidence {
        item.relations.retain(|relation| {
            relation.direction == StoryRelationDirection::Outgoing
                && allowed.contains(&relation.target_iri)
        });
        let mut properties = std::mem::take(&mut item.properties);
        properties.retain(|property| !is_redundant_evidence_property(state, item, property));
        item.properties = properties;
    }
    let mut known = evidence
        .iter()
        .map(|item| item.iri.clone())
        .collect::<BTreeSet<_>>();
    for anchor in narratable_code_anchors(run) {
        let Some(iri) = anchor.entity_iri.as_ref() else {
            continue;
        };
        if known.insert(iri.clone()) {
            // A bounded dossier may omit a current CodeEntity even though its
            // deterministic anchor remains public. Represent that public fact
            // as a private packet source so narration does not silently lose it.
            evidence.push(StoryEvidenceDetail {
                iri: iri.clone(),
                title: anchor.label.clone(),
                kind: "CodeEntity".to_string(),
                status: "accepted".to_string(),
                suppressed: false,
                description: None,
                timestamp: None,
                author: None,
                properties: vec![],
                relations: vec![],
            });
        }
    }
    evidence
}

fn is_redundant_evidence_property(
    state: &AppState,
    item: &StoryEvidenceDetail,
    property: &StoryLiteralProperty,
) -> bool {
    let predicate = property.predicate.as_str();
    let value = property.value.as_str();
    ((predicate == moose::RDFS_LABEL || predicate == state.capture.title) && value == item.title)
        || (predicate == state.capture.status && value == item.status)
        || (predicate == state.capture.description && item.description.as_deref() == Some(value))
        || (predicate == state.capture.timestamp && item.timestamp.as_deref() == Some(value))
        || (predicate == state.capture.author && item.author.as_deref() == Some(value))
}

fn narratable_code_anchors(run: &StoryRun) -> Vec<&StoryCodeAnchor> {
    let excluded = run
        .evidence
        .iter()
        .filter(|item| item.suppressed || item.status.eq_ignore_ascii_case("proposed"))
        .map(|item| item.iri.as_str())
        .collect::<BTreeSet<_>>();
    run.code_anchors
        .iter()
        .filter(|anchor| {
            anchor
                .entity_iri
                .as_deref()
                .is_none_or(|iri| !excluded.contains(iri))
        })
        .collect()
}

pub(super) fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
pub(in crate::stories) struct NarrationPacketTest {
    pub prompt: String,
    pub coverage: StoryNarrationCoverage,
    pub citations_by_source: BTreeMap<String, Vec<String>>,
    pub sections_by_source: BTreeMap<String, String>,
}

#[cfg(test)]
pub(in crate::stories) fn build_narration_packet_for_test(
    state: &AppState,
    run: &StoryRun,
    context_window_tokens: usize,
) -> Result<NarrationPacketTest, NarrationFailureReason> {
    let packet = build_narration_packet(state, run, context_window_tokens)?;
    Ok(NarrationPacketTest {
        prompt: packet.prompt,
        coverage: packet.coverage,
        citations_by_source: packet.citations_by_source,
        sections_by_source: packet.sections_by_source,
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn evidence(status: &str) -> StoryEvidenceDetail {
        StoryEvidenceDetail {
            iri: "https://example.test/record".to_string(),
            title: "Record".to_string(),
            kind: "ArchitecturalDecision".to_string(),
            status: status.to_string(),
            suppressed: false,
            description: None,
            timestamp: None,
            author: None,
            properties: vec![],
            relations: vec![],
        }
    }

    #[test]
    fn section_classification_uses_working_set_lifecycle_policy() {
        assert_eq!(
            story_section_kind(&evidence("unknown"), None),
            StorySectionKind::CurrentState
        );
        assert_eq!(
            story_section_kind(&evidence("proposed"), None),
            StorySectionKind::Evolution
        );
    }
}
