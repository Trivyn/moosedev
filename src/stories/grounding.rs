//! Graph-facing evidence, code-anchor, lifecycle, and relationship helpers.

use super::model::*;
use super::*;

pub(super) fn topic_records(state: &AppState, query: &str) -> anyhow::Result<Vec<RecordData>> {
    let mut records = relevant_context_snapshot(state, Some(query), 18, false)?
        .into_iter()
        .filter_map(|item| record_data(state, &item.iri).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    canonicalize_records(&mut records);
    Ok(records)
}

pub(super) fn entity_records(
    state: &AppState,
    entity: &StoryCandidate,
) -> anyhow::Result<Vec<RecordData>> {
    let mut records = Vec::new();
    if let Some(record) = record_data(state, &entity.iri)? {
        if in_working_set(&record.evidence.status) {
            records.push(record);
        }
    }
    for neighbor in project_neighbors(state, &entity.iri)? {
        if let Some(record) = record_data(state, &neighbor)? {
            if in_working_set(&record.evidence.status) {
                records.push(record);
            }
        }
        if component_iri_is_current(state, &neighbor)? {
            records.extend(component_records(state, &neighbor)?);
        }
    }
    canonicalize_records(&mut records);
    Ok(records)
}

pub(super) fn canonicalize_records(records: &mut Vec<RecordData>) {
    sort_dedupe_records(records);
    records.truncate(18);
}

pub(super) fn sort_dedupe_records(records: &mut Vec<RecordData>) {
    records.sort_by(|left, right| {
        record_kind_rank(&left.evidence.kind)
            .cmp(&record_kind_rank(&right.evidence.kind))
            .then_with(|| left.evidence.title.cmp(&right.evidence.title))
            .then_with(|| left.evidence.iri.cmp(&right.evidence.iri))
    });
    records.dedup_by(|left, right| left.evidence.iri == right.evidence.iri);
}

fn project_neighbors(state: &AppState, iri: &str) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let node = NamedNodeRef::new(iri)?;
    let mut neighbors = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        Some(node.into()),
        None,
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(object) = quad?.object {
            neighbors.insert(object.as_str().to_string());
        }
    }
    for quad in state.store.quads_for_pattern(
        None,
        None,
        Some(node.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let NamedOrBlankNode::NamedNode(subject) = quad?.subject {
            neighbors.insert(subject.as_str().to_string());
        }
    }
    neighbors.remove(iri);
    Ok(neighbors)
}

pub(super) fn entity_code(
    state: &AppState,
    code_entities: &[StoryCodeAnchor],
    entity_iri: &str,
) -> anyhow::Result<Vec<StoryCodeAnchor>> {
    let mut anchors = code_entities
        .iter()
        .filter(|anchor| anchor.entity_iri.as_deref() == Some(entity_iri))
        .cloned()
        .collect::<Vec<_>>();
    for neighbor in project_neighbors(state, entity_iri)? {
        anchors.extend(
            code_entities
                .iter()
                .filter(|anchor| anchor.entity_iri.as_deref() == Some(neighbor.as_str()))
                .cloned(),
        );
        if component_iri_is_current(state, &neighbor)? {
            anchors.extend(component_code(state, &neighbor)?);
        }
    }
    Ok(dedupe_code_anchors(anchors))
}

pub(super) fn code_for_records(
    state: &AppState,
    code_entities: &[StoryCodeAnchor],
    records: &[RecordData],
) -> anyhow::Result<Vec<StoryCodeAnchor>> {
    let mut anchors = Vec::new();
    for record in records.iter().take(12) {
        anchors.extend(entity_code(state, code_entities, &record.evidence.iri)?);
    }
    Ok(dedupe_code_anchors(anchors))
}

pub(super) fn story_entity_is_current(
    state: &AppState,
    iri: &str,
    kind: &str,
) -> anyhow::Result<bool> {
    match kind {
        "SystemComponent" => component_iri_is_current(state, iri),
        "CodeEntity" => code_entity_is_current(state, iri),
        "Entity" => Ok(false),
        _ => {
            Ok(record_data(state, iri)?
                .is_some_and(|record| in_working_set(&record.evidence.status)))
        }
    }
}

pub(super) fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn recipe_beats(
    state: &AppState,
    recipe: &StoryRecipe,
    component: &StoryCandidate,
    code_by_symbol: &BTreeMap<String, StoryCodeAnchor>,
) -> anyhow::Result<(Vec<StoryBeat>, Vec<StoryGap>)> {
    let subject_iri = component.iri.as_str();
    let mut beats = Vec::new();
    let mut gaps = Vec::new();
    for spec in &recipe.beats {
        let mut evidence = Vec::new();
        for iri in &spec.record_iris {
            match resolve_recipe_record(state, iri)? {
                AnchorResolution::Current(item) => {
                    if record_concerns_component(state, &item.iri, subject_iri)? {
                        evidence.push(item);
                    } else {
                        gaps.push(StoryGap {
                            id: format!("subject-record-{}-{}", spec.id, gaps.len()),
                            title: "Story record does not concern this subject".to_string(),
                            detail: format!(
                                "Record {iri} is not linked to effective Story subject {subject_iri}."
                            ),
                            beat_intent: Some(spec.intent.clone()),
                        });
                    }
                }
                AnchorResolution::Superseded { replacements } => {
                    let detail = match replacements.as_slice() {
                        [successor] => format!(
                            "{iri} is retired; current successor {} is shown.",
                            successor.iri
                        ),
                        [] => {
                            format!("{iri} is retired and no current successor can be resolved.")
                        }
                        successors => format!(
                            "{iri} is retired and has multiple current successors ({}); curator selection is required.",
                            successors
                                .iter()
                                .map(|successor| successor.iri.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    gaps.push(StoryGap {
                        id: format!("drift-{}-{}", spec.id, gaps.len()),
                        title: "Story anchor was superseded".to_string(),
                        detail,
                        beat_intent: Some(spec.intent.clone()),
                    });
                    if let [item] = replacements.as_slice() {
                        if record_concerns_component(state, &item.iri, subject_iri)? {
                            evidence.push(item.clone());
                        } else {
                            gaps.push(StoryGap {
                                id: format!("subject-record-{}-{}", spec.id, gaps.len()),
                                title: "Story successor does not concern this subject".to_string(),
                                detail: format!(
                                    "Successor {} is not linked to effective Story subject {subject_iri}.",
                                    item.iri
                                ),
                                beat_intent: Some(spec.intent.clone()),
                            });
                        }
                    }
                }
                AnchorResolution::Missing => gaps.push(StoryGap {
                    id: format!("missing-{}-{}", spec.id, gaps.len()),
                    title: "Story anchor is missing".to_string(),
                    detail: format!("Record {iri} cannot be resolved."),
                    beat_intent: Some(spec.intent.clone()),
                }),
                AnchorResolution::Ineligible(status) => gaps.push(StoryGap {
                    id: format!("inactive-{}-{}", spec.id, gaps.len()),
                    title: "Story anchor is not current".to_string(),
                    detail: format!("Record {iri} has lifecycle status {status}."),
                    beat_intent: Some(spec.intent.clone()),
                }),
            }
        }
        let mut anchors = Vec::new();
        for symbol in &spec.code_symbols {
            if let Some(anchor) = code_by_symbol.get(symbol) {
                let grounded = match anchor.entity_iri.as_deref() {
                    Some(entity) => code_realizes_component(state, entity, subject_iri)?,
                    None => false,
                };
                if grounded {
                    anchors.push(anchor.clone());
                } else {
                    gaps.push(StoryGap {
                        id: format!("subject-code-{}-{}", spec.id, gaps.len()),
                        title: "Story code does not realize this subject".to_string(),
                        detail: format!(
                            "Symbol {symbol} is not linked to effective Story subject {subject_iri}."
                        ),
                        beat_intent: Some(spec.intent.clone()),
                    });
                }
            } else {
                gaps.push(StoryGap {
                    id: format!("code-{}-{}", spec.id, gaps.len()),
                    title: "Code anchor is unresolved".to_string(),
                    detail: format!("Symbol {symbol} is not present in the current code graph."),
                    beat_intent: Some(spec.intent.clone()),
                });
            }
        }
        if spec.intent == StoryIntent::Boundary && is_system_component(state, subject_iri)? {
            evidence.insert(
                0,
                StoryEvidence {
                    iri: subject_iri.to_string(),
                    title: component.label.clone(),
                    kind: "SystemComponent".to_string(),
                    status: first_literal(&state.store, subject_iri, &state.capture.status)
                        .unwrap_or_else(|| "unknown".to_string()),
                },
            );
        }
        let beat = make_beat(
            &spec.id,
            &spec.title,
            spec.intent.clone(),
            evidence,
            anchors,
            (spec.intent == StoryIntent::Boundary)
                .then(|| component.description.clone())
                .flatten(),
            spec.curator_note.clone(),
        );
        beats.push(beat);
    }
    Ok((beats, gaps))
}

#[derive(Debug)]
pub(super) enum AnchorResolution {
    Current(StoryEvidence),
    Superseded { replacements: Vec<StoryEvidence> },
    Missing,
    Ineligible(String),
}

pub(super) fn resolve_recipe_record(
    state: &AppState,
    iri: &str,
) -> anyhow::Result<AnchorResolution> {
    let Ok(node) = NamedNode::new(iri) else {
        return Ok(AnchorResolution::Missing);
    };
    if crate::graph::capture::require_information_record(state, &node).is_err() {
        return Ok(AnchorResolution::Missing);
    }
    let Some(record) = record_data(state, iri)? else {
        return Ok(AnchorResolution::Missing);
    };
    if in_working_set(&record.evidence.status) {
        return Ok(AnchorResolution::Current(record.evidence));
    }
    if record.evidence.status.eq_ignore_ascii_case("superseded") {
        let successors = accepted_successors(state, iri)?;
        return Ok(AnchorResolution::Superseded {
            replacements: successors,
        });
    }
    Ok(AnchorResolution::Ineligible(record.evidence.status))
}

pub(super) fn component_records(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<Vec<RecordData>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let object = NamedNodeRef::new(component_iri)?;
    let concerns = state.resolve_object_property("concerns")?;
    let predicate = NamedNodeRef::new(&concerns)?;
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(record) = record_data(state, subject.as_str())? {
            if in_working_set(&record.evidence.status) {
                out.push(record);
            }
        }
    }
    let inverse_iri = state.resolve_object_property("isConcernedBy")?;
    let inverse = NamedNodeRef::new(&inverse_iri)?;
    let component = NamedNodeRef::new(component_iri)?;
    for quad in state.store.quads_for_pattern(
        Some(component.into()),
        Some(inverse),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(record_node) = quad?.object {
            if let Some(record) = record_data(state, record_node.as_str())? {
                if in_working_set(&record.evidence.status) {
                    out.push(record);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.evidence
            .kind
            .cmp(&b.evidence.kind)
            .then(a.evidence.title.cmp(&b.evidence.title))
            .then(a.evidence.iri.cmp(&b.evidence.iri))
    });
    out.dedup_by(|a, b| a.evidence.iri == b.evidence.iri);
    Ok(out)
}

pub(super) fn pending_component_record_count(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<usize> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let component = NamedNodeRef::new(component_iri)?;
    let concerns_iri = state.resolve_object_property("concerns")?;
    let concerns = NamedNodeRef::new(&concerns_iri)?;
    let inverse_iri = state.resolve_object_property("isConcernedBy")?;
    let inverse = NamedNodeRef::new(&inverse_iri)?;
    let mut records = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(concerns),
        Some(component.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let NamedOrBlankNode::NamedNode(record) = quad?.subject {
            records.insert(record.as_str().to_string());
        }
    }
    for quad in state.store.quads_for_pattern(
        Some(component.into()),
        Some(inverse),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(record) = quad?.object {
            records.insert(record.as_str().to_string());
        }
    }
    Ok(records
        .into_iter()
        .filter(|iri| {
            let Ok(node) = NamedNode::new(iri) else {
                return false;
            };
            crate::graph::capture::require_information_record(state, &node).is_ok()
                && first_literal(&state.store, iri, &state.capture.status)
                    .as_deref()
                    .is_some_and(|status| status.eq_ignore_ascii_case("proposed"))
        })
        .count())
}

#[derive(Debug, Clone)]
pub(super) struct RecordData {
    pub(super) evidence: StoryEvidence,
    pub(super) description: Option<String>,
}

pub(super) fn record_data(state: &AppState, iri: &str) -> anyhow::Result<Option<RecordData>> {
    let node = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    if crate::graph::capture::require_information_record(state, &node).is_err() {
        return Ok(None);
    }
    let kinds = asserted_project_types(state, &node)
        .into_iter()
        .map(|kind| local_name(&kind).to_string())
        .filter(|kind| kind != "NamedIndividual")
        .collect::<Vec<_>>();
    let kind = choose_record_kind(kinds);
    let Some(kind) = kind else {
        return Ok(None);
    };
    let title = first_literal(&state.store, iri, moose::RDFS_LABEL)
        .or_else(|| first_literal(&state.store, iri, &state.capture.title))
        .unwrap_or_else(|| local_name(iri).to_string());
    let status = first_literal(&state.store, iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    let description = first_literal(&state.store, iri, &state.capture.description);
    Ok(Some(RecordData {
        evidence: StoryEvidence {
            iri: iri.to_string(),
            title,
            kind,
            status,
        },
        description,
    }))
}

pub(super) fn choose_record_kind(mut kinds: Vec<String>) -> Option<String> {
    kinds.sort_by(|left, right| {
        record_kind_rank(left)
            .cmp(&record_kind_rank(right))
            .then(left.cmp(right))
    });
    kinds.dedup();
    kinds.into_iter().next()
}

pub(super) fn record_kind_rank(kind: &str) -> usize {
    [
        "Requirement",
        "ArchitecturalDecision",
        "Constraint",
        "Pattern",
        "AntiPattern",
        "Lesson",
        "Consequence",
        "Rationale",
    ]
    .iter()
    .position(|candidate| *candidate == kind)
    .unwrap_or(match kind {
        "InformationRecord" => usize::MAX - 1,
        "ProjectEntity" => usize::MAX,
        _ => usize::MAX - 2,
    })
}

fn successor_iris(state: &AppState, iri: &str) -> anyhow::Result<Vec<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(iri)?;
    let predicate_iri = state.resolve_object_property("isSupersededBy")?;
    let predicate = NamedNodeRef::new(&predicate_iri)?;
    let mut out = Vec::new();
    for quad in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(node) = quad?.object {
            out.push(node.as_str().to_string());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn accepted_successors(state: &AppState, retired_iri: &str) -> anyhow::Result<Vec<StoryEvidence>> {
    let mut visited = BTreeSet::from([retired_iri.to_string()]);
    let mut frontier = VecDeque::new();
    for successor in successor_iris(state, retired_iri)? {
        if visited.insert(successor.clone()) {
            frontier.push_back(successor);
        }
    }
    let mut accepted = BTreeMap::new();
    for _ in 0..256 {
        let Some(next) = frontier.pop_front() else {
            break;
        };
        if let Some(record) = record_data(state, &next)? {
            if in_working_set(&record.evidence.status) {
                accepted.insert(record.evidence.iri.clone(), record.evidence);
                continue;
            }
        }
        for successor in successor_iris(state, &next)? {
            if visited.insert(successor.clone()) {
                frontier.push_back(successor);
            }
        }
    }
    Ok(accepted.into_values().collect())
}

pub(super) fn component_code(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<Vec<StoryCodeAnchor>> {
    let terms = CodeTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    let object = NamedNodeRef::new(component_iri)?;
    let mut anchors = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(anchor) = code_anchor(state, &terms, subject.as_str())? {
            anchors.push(anchor);
        }
    }
    Ok(dedupe_code_anchors(anchors))
}

#[cfg(test)]
pub(super) fn all_code_by_symbol(
    state: &AppState,
) -> anyhow::Result<BTreeMap<String, StoryCodeAnchor>> {
    Ok(all_code(state)?.0)
}

pub(super) fn all_code(
    state: &AppState,
) -> anyhow::Result<(BTreeMap<String, StoryCodeAnchor>, Vec<StoryCodeAnchor>)> {
    let terms = CodeTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    let mut by_symbol = BTreeMap::new();
    let mut entities = Vec::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        if let Some(anchor) = code_anchor(state, &terms, subject.as_str())? {
            entities.push(anchor.clone());
            insert_code_anchor(&mut by_symbol, anchor);
        }
    }
    entities.sort_by(code_anchor_order);
    entities.dedup_by(|left, right| left.entity_iri == right.entity_iri);
    Ok((by_symbol, entities))
}

pub(super) fn insert_code_anchor(
    anchors: &mut BTreeMap<String, StoryCodeAnchor>,
    candidate: StoryCodeAnchor,
) {
    anchors
        .entry(candidate.symbol.clone())
        .and_modify(|current| {
            if code_anchor_order(&candidate, current).is_lt() {
                *current = candidate.clone();
            }
        })
        .or_insert(candidate);
}

fn code_anchor_order(left: &StoryCodeAnchor, right: &StoryCodeAnchor) -> std::cmp::Ordering {
    left.symbol
        .cmp(&right.symbol)
        .then(left.entity_iri.cmp(&right.entity_iri))
        .then(left.label.cmp(&right.label))
        .then(left.path.cmp(&right.path))
}

pub(super) fn dedupe_code_anchors(mut anchors: Vec<StoryCodeAnchor>) -> Vec<StoryCodeAnchor> {
    anchors.sort_by(code_anchor_order);
    anchors.dedup_by(|left, right| left.symbol == right.symbol);
    anchors
}

fn code_anchor(
    state: &AppState,
    terms: &CodeTerms,
    iri: &str,
) -> anyhow::Result<Option<StoryCodeAnchor>> {
    if !is_instance_of(state, iri, &terms.code_entity_class)?
        || first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_some_and(|status| !in_working_set(status))
    {
        return Ok(None);
    }
    let Some(symbol) = first_literal(&state.store, iri, &terms.has_substrate_symbol) else {
        return Ok(None);
    };
    Ok(Some(StoryCodeAnchor {
        label: first_literal(&state.store, iri, &terms.has_code_name)
            .or_else(|| first_literal(&state.store, iri, moose::RDFS_LABEL))
            .unwrap_or_else(|| symbol.clone()),
        symbol,
        entity_iri: Some(iri.to_string()),
        path: first_literal(&state.store, iri, &terms.defined_in_path),
        line: None,
    }))
}

fn is_instance_of(state: &AppState, iri: &str, class_iri: &str) -> anyhow::Result<bool> {
    let node = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(false),
    };
    Ok(asserted_project_types(state, &node)
        .into_iter()
        .any(|kind| crate::graph::util::is_subclass_of(&state.store, &kind, class_iri)))
}

fn is_system_component(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    let class = state.resolve_class("SystemComponent")?;
    is_instance_of(state, iri, &class)
}

pub(super) fn component_iri_is_current(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    Ok(is_system_component(state, iri)?
        && first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_none_or(in_working_set))
}

pub(super) fn code_entity_is_current(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    let terms = CodeTerms::resolve(state)?;
    Ok(code_anchor(state, &terms, iri)?.is_some())
}

pub(super) fn dedupe_gaps(gaps: &mut Vec<StoryGap>) {
    let mut seen = BTreeSet::new();
    gaps.retain(|gap| seen.insert((gap.title.clone(), gap.detail.clone())));
}

pub(super) fn record_concerns_component(
    state: &AppState,
    record: &str,
    component: &str,
) -> anyhow::Result<bool> {
    if !component_iri_is_current(state, component)? {
        return Ok(false);
    }
    let Some(record) = record_data(state, record)? else {
        return Ok(false);
    };
    if !in_working_set(&record.evidence.status) {
        return Ok(false);
    }
    Ok(
        edge_exists(state, &record.evidence.iri, "concerns", component)?
            || edge_exists(state, component, "isConcernedBy", &record.evidence.iri)?,
    )
}

pub(super) fn code_realizes_component(
    state: &AppState,
    entity: &str,
    component: &str,
) -> anyhow::Result<bool> {
    if !component_iri_is_current(state, component)? || !code_entity_is_current(state, entity)? {
        return Ok(false);
    }
    edge_exists(state, entity, "realizes", component)
}

fn edge_exists(
    state: &AppState,
    subject: &str,
    predicate: &str,
    object: &str,
) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject)?;
    let predicate_iri = state.resolve_object_property(predicate)?;
    let predicate = NamedNodeRef::new(&predicate_iri)?;
    let object = NamedNodeRef::new(object)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            Some(object.into()),
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}
