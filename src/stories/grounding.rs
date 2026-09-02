//! Graph-facing evidence, code-anchor, lifecycle, and relationship helpers.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNode, Term};

use crate::graph::{
    asserted_project_types, first_literal, in_working_set, local_name, relevant_context_snapshot,
    AppState, CodeTerms, PROJECT_KG_GRAPH_IRI,
};

use super::model::{
    StoryBeat, StoryCandidate, StoryCodeAnchor, StoryCoverage, StoryEvidence, StoryEvidenceDetail,
    StoryEvidenceRelation, StoryGap, StoryLiteralProperty, StoryNarrativeSection, StoryParagraph,
    StoryRecipe, StoryRecipeSubject, StoryRelationDirection, StorySectionKind, StorySubject,
    StoryTimelineEvent,
};

/// Bounds the public dossier and its downstream per-entity projections.
pub(super) const MAX_STORY_ENTITIES: usize = 512;
/// Looks past the output cap so deep curator-selected evidence can reserve a
/// slot without turning an unrelated inclusion into a new traversal root.
const MAX_STORY_DISCOVERY_ENTITIES: usize = MAX_STORY_ENTITIES * 8;
/// Complements the entity cap for unusually large records; either bound makes
/// the resulting Story explicitly report incomplete coverage.
const MAX_STORY_DOSSIER_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn topic_records(state: &AppState, query: &str) -> anyhow::Result<Vec<RecordData>> {
    let mut records = relevant_context_snapshot(state, Some(query), 64, false)?
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
    records.truncate(MAX_STORY_ENTITIES);
}

pub(super) fn sort_dedupe_records(records: &mut Vec<RecordData>) {
    records.sort_by(|left, right| {
        entity_kind_rank(&left.evidence.kind)
            .cmp(&entity_kind_rank(&right.evidence.kind))
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

pub(super) struct StoryDocument {
    pub brief: StoryParagraph,
    pub narrative: Vec<StoryNarrativeSection>,
    pub timeline: Vec<StoryTimelineEvent>,
    pub evidence: Vec<StoryEvidenceDetail>,
    pub code_anchors: Vec<StoryCodeAnchor>,
    pub coverage: StoryCoverage,
}

pub(super) fn build_story_document(
    state: &AppState,
    subject: &StorySubject,
    beats: &[StoryBeat],
    recipe: Option<&StoryRecipe>,
) -> anyhow::Result<StoryDocument> {
    let excluded: BTreeSet<String> = recipe
        .map(|recipe| recipe.focus.exclude_record_iris.iter().cloned().collect())
        .unwrap_or_default();
    let excluded_code = code_entity_iris_for_symbols(
        state,
        recipe
            .map(|recipe| recipe.focus.exclude_code_symbols.as_slice())
            .unwrap_or_default(),
    )?;
    let mut priority: BTreeSet<String> = recipe
        .map(|recipe| recipe.focus.include_record_iris.iter().cloned().collect())
        .unwrap_or_default();
    priority.extend(code_entity_iris_for_symbols(
        state,
        recipe
            .map(|recipe| recipe.focus.include_code_symbols.as_slice())
            .unwrap_or_default(),
    )?);
    let (mut evidence, truncated, dossier_bytes) =
        collect_story_closure(state, subject, &priority)?;
    for item in &mut evidence {
        item.suppressed = excluded.contains(&item.iri) || excluded_code.contains(&item.iri);
    }
    let allowed = evidence
        .iter()
        .map(|item| item.iri.clone())
        .collect::<BTreeSet<_>>();
    for item in &mut evidence {
        item.relations
            .retain(|relation| allowed.contains(&relation.target_iri));
    }
    let label = match subject {
        StorySubject::Entity { label, .. } | StorySubject::Topic { label, .. } => label,
    };
    let subject_iri = match subject {
        StorySubject::Entity { iri, .. } => Some(iri.as_str()),
        StorySubject::Topic { .. } => None,
    };
    let narrative = symbolic_sections_from_dossier(
        label,
        subject_iri,
        &evidence,
        recipe.map(|recipe| recipe.focus.emphasis.as_slice()),
    );
    let cited = narrative
        .iter()
        .flat_map(|section| &section.paragraphs)
        .flat_map(|paragraph| &paragraph.citation_iris)
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    let brief = StoryParagraph {
        text: format!(
            "This account brings together the recorded decisions, constraints, lessons, code, and lifecycle history connected to {label}."
        ),
        citation_iris: cited,
    };
    let timeline = build_timeline(&evidence);
    let mut code_anchors = beats
        .iter()
        .flat_map(|beat| beat.code_anchors.iter().cloned())
        .collect::<Vec<_>>();
    code_anchors = dedupe_code_anchors(code_anchors);
    resolve_anchor_lines(state, &mut code_anchors);
    let current_count = evidence
        .iter()
        .filter(|item| in_working_set(&item.status))
        .count();
    let proposed_count = evidence
        .iter()
        .filter(|item| item.status.eq_ignore_ascii_case("proposed"))
        .count();
    let coverage = StoryCoverage {
        entity_count: evidence.len(),
        dossier_bytes,
        current_count,
        historical_count: evidence
            .len()
            .saturating_sub(current_count)
            .saturating_sub(proposed_count),
        proposed_count,
        code_anchor_count: code_anchors.len(),
        subject_families: evidence
            .iter()
            .map(|item| item.kind.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        outline_sections: narrative
            .iter()
            .map(|section| section.kind.clone())
            .collect(),
        truncated,
    };
    Ok(StoryDocument {
        brief,
        narrative,
        timeline,
        evidence,
        code_anchors,
        coverage,
    })
}

pub(super) fn story_subject_closure_iris_with_priority(
    state: &AppState,
    subject: &StoryRecipeSubject,
    priority: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let subject = match subject {
        StoryRecipeSubject::Entity { iri } => StorySubject::Entity {
            iri: iri.clone(),
            kind: "Entity".to_string(),
            label: iri.clone(),
        },
        StoryRecipeSubject::Topic { query } => StorySubject::Topic {
            query: query.clone(),
            label: query.clone(),
        },
    };
    Ok(collect_story_closure(state, &subject, priority)?
        .0
        .into_iter()
        .map(|item| item.iri)
        .collect())
}

pub(super) fn story_recipe_priority_iris(
    state: &AppState,
    recipe: &StoryRecipe,
) -> anyhow::Result<BTreeSet<String>> {
    let mut priority = recipe
        .focus
        .include_record_iris
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    priority.extend(code_entity_iris_for_symbols(
        state,
        &recipe.focus.include_code_symbols,
    )?);
    Ok(priority)
}

fn collect_story_closure(
    state: &AppState,
    subject: &StorySubject,
    priority: &BTreeSet<String>,
) -> anyhow::Result<(Vec<StoryEvidenceDetail>, bool, usize)> {
    let roots = match subject {
        StorySubject::Entity { iri, .. } => BTreeSet::from([iri.clone()]),
        StorySubject::Topic { query, .. } => topic_records(state, query)?
            .into_iter()
            .map(|record| record.evidence.iri)
            .collect(),
    };
    let mut queued = roots.clone();
    let mut frontier = BTreeMap::<usize, BTreeSet<String>>::new();
    frontier.insert(0, roots.clone());
    let mut root_candidates = Vec::new();
    let mut priority_candidates = Vec::new();
    let mut normal_candidates = Vec::new();
    let mut cached_normal_bytes = 0usize;
    let mut truncated = false;
    while let Some(depth) = frontier.keys().next().copied() {
        let iri = {
            let bucket = frontier.get_mut(&depth).expect("frontier depth exists");
            bucket.pop_first()
        };
        if frontier.get(&depth).is_some_and(BTreeSet::is_empty) {
            frontier.remove(&depth);
        }
        let Some(iri) = iri else { continue };
        let Some(mut detail) = story_entity_detail(state, &iri)? else {
            continue;
        };
        detail.relations.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then(left.target_label.cmp(&right.target_label))
                .then(left.target_iri.cmp(&right.target_iri))
        });
        detail.relations.dedup();
        let prospective = serde_json::to_vec(&detail)?.len();
        let mut next = detail
            .relations
            .iter()
            .filter(|relation| expands_story_closure(&detail.kind, depth, relation))
            .map(|relation| relation.target_iri.clone())
            .collect::<Vec<_>>();
        next.sort_by(|left, right| {
            priority
                .contains(right)
                .cmp(&priority.contains(left))
                .then(left.cmp(right))
        });
        next.dedup();
        for target in next {
            if queued.contains(&target) {
                continue;
            }
            if queued.len() == MAX_STORY_DISCOVERY_ENTITIES {
                truncated = true;
                continue;
            }
            queued.insert(target.clone());
            frontier
                .entry(depth.saturating_add(1))
                .or_default()
                .insert(target);
        }

        if prospective > MAX_STORY_DOSSIER_BYTES {
            truncated = true;
            continue;
        }
        if roots.contains(&iri) {
            root_candidates.push((detail, prospective));
        } else if priority.contains(&iri) {
            priority_candidates.push((detail, prospective));
        } else if normal_candidates.len() < MAX_STORY_ENTITIES
            && cached_normal_bytes.saturating_add(prospective) <= MAX_STORY_DOSSIER_BYTES
        {
            cached_normal_bytes = cached_normal_bytes.saturating_add(prospective);
            normal_candidates.push((detail, prospective));
        } else {
            truncated = true;
        }
    }

    // Subject roots establish context. Naturally reached explicit includes
    // then reserve capacity before ordinary breadth-first evidence, so a deep
    // curated item cannot be starved by a broad first hop.
    let mut evidence = Vec::new();
    let mut serialized_bytes = 0usize;
    for (detail, bytes) in root_candidates
        .into_iter()
        .chain(priority_candidates)
        .chain(normal_candidates)
    {
        if evidence.len() == MAX_STORY_ENTITIES
            || serialized_bytes.saturating_add(bytes) > MAX_STORY_DOSSIER_BYTES
        {
            truncated = true;
            continue;
        }
        serialized_bytes = serialized_bytes.saturating_add(bytes);
        evidence.push(detail);
    }
    evidence.sort_by(|left, right| {
        entity_kind_rank(&left.kind)
            .cmp(&entity_kind_rank(&right.kind))
            .then(left.title.cmp(&right.title))
            .then(left.iri.cmp(&right.iri))
    });
    let allowed = evidence
        .iter()
        .map(|item| item.iri.clone())
        .collect::<BTreeSet<_>>();
    for item in &mut evidence {
        item.relations
            .retain(|relation| allowed.contains(&relation.target_iri));
    }
    Ok((evidence, truncated, serialized_bytes))
}

fn expands_story_closure(
    source_kind: &str,
    depth: usize,
    relation: &StoryEvidenceRelation,
) -> bool {
    let lifecycle = matches!(
        relation.label.as_str(),
        "supersedes" | "isSupersededBy" | "hasRationale" | "isRationaleFor"
    ) || relation.target_kind == "Rationale";
    if lifecycle {
        return true;
    }
    if depth == 0 {
        return true;
    }
    // Three typed hops cover the local knowledge cluster; stopping component
    // hubs after the root prevents a connected project graph from going global.
    if depth >= 3 || (depth > 0 && source_kind == "SystemComponent") {
        return false;
    }
    matches!(
        relation.label.as_str(),
        "concerns"
            | "isConcernedBy"
            | "isMotivatedBy"
            | "motivates"
            | "weighs"
            | "isWeighedBy"
            | "resultsIn"
            | "resultsFrom"
            | "constrains"
            | "isConstrainedBy"
            | "violates"
            | "isViolatedBy"
            | "learnedFrom"
            | "yieldsLesson"
            | "realizes"
            | "isRealizedBy"
            | "satisfies"
            | "isSatisfiedBy"
            | "embodies"
            | "isEmbodiedBy"
            | "supersedes"
            | "isSupersededBy"
            | "hasRationale"
            | "isRationaleFor"
    )
}

fn symbolic_sections_from_dossier(
    subject_label: &str,
    subject_iri: Option<&str>,
    evidence: &[StoryEvidenceDetail],
    emphasis: Option<&[StorySectionKind]>,
) -> Vec<StoryNarrativeSection> {
    let defaults = [
        StorySectionKind::Orientation,
        StorySectionKind::Evolution,
        StorySectionKind::CurrentState,
        StorySectionKind::Implementation,
        StorySectionKind::Implications,
    ];
    let mut order = emphasis.unwrap_or_default().to_vec();
    for kind in defaults {
        if !order.contains(&kind) {
            order.push(kind);
        }
    }
    let mut sections = Vec::new();
    for kind in order {
        let matching = evidence
            .iter()
            .filter(|item| !item.suppressed && !item.status.eq_ignore_ascii_case("proposed"))
            .filter(|item| story_section_kind(item, subject_iri) == kind)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }
        let statements = matching
            .iter()
            .map(|item| {
                let body = item
                    .description
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&item.title);
                if in_working_set(&item.status) {
                    body.trim().to_string()
                } else {
                    format!(
                        "Historically, {} was recorded as {}: {}",
                        item.title,
                        item.status,
                        body.trim()
                    )
                }
            })
            .collect::<Vec<_>>();
        let paragraphs = vec![StoryParagraph {
            text: format!("For {subject_label}, {}", statements.join(" ")),
            citation_iris: matching.iter().map(|item| item.iri.clone()).collect(),
        }];
        let title = match kind {
            StorySectionKind::Orientation => "Orientation",
            StorySectionKind::Evolution => "Evolution",
            StorySectionKind::CurrentState => "Current state",
            StorySectionKind::Implementation => "Implementation",
            StorySectionKind::Implications => "Implications",
        };
        sections.push(StoryNarrativeSection {
            id: kind.id().to_string(),
            kind,
            title: title.to_string(),
            paragraphs,
        });
    }
    sections
}

pub(super) fn story_section_kind(
    item: &StoryEvidenceDetail,
    subject_iri: Option<&str>,
) -> StorySectionKind {
    if subject_iri == Some(item.iri.as_str()) {
        return StorySectionKind::Orientation;
    }
    if !in_working_set(&item.status) {
        return StorySectionKind::Evolution;
    }
    if item
        .relations
        .iter()
        .any(|relation| matches!(relation.label.as_str(), "supersedes" | "isSupersededBy"))
    {
        return StorySectionKind::Evolution;
    }
    match item.kind.as_str() {
        "Requirement" | "Rationale" | "Alternative" => StorySectionKind::Orientation,
        "ArchitecturalDecision" | "Constraint" | "Pattern" => StorySectionKind::CurrentState,
        "CodeEntity" | "SystemComponent" => StorySectionKind::Implementation,
        "Lesson" | "AntiPattern" | "Consequence" => StorySectionKind::Implications,
        _ => StorySectionKind::Orientation,
    }
}

fn story_entity_detail(state: &AppState, iri: &str) -> anyhow::Result<Option<StoryEvidenceDetail>> {
    let Some((title, kind)) = story_entity_identity(state, iri)? else {
        return Ok(None);
    };
    let status = first_literal(&state.store, iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(iri)?;
    let mut relations = Vec::new();
    let mut properties = Vec::new();
    for quad in state.store.quads_for_pattern(
        Some(subject.into()),
        None,
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        match quad.object {
            Term::NamedNode(target) => {
                if let Some((target_label, target_kind)) =
                    story_entity_identity(state, target.as_str())?
                {
                    relations.push(StoryEvidenceRelation {
                        predicate: quad.predicate.as_str().to_string(),
                        label: local_name(quad.predicate.as_str()).to_string(),
                        direction: StoryRelationDirection::Outgoing,
                        target_iri: target.as_str().to_string(),
                        target_label,
                        target_kind,
                    });
                }
            }
            Term::Literal(literal) => properties.push(StoryLiteralProperty {
                predicate: quad.predicate.as_str().to_string(),
                label: local_name(quad.predicate.as_str()).to_string(),
                value: literal.value().to_string(),
            }),
            _ => {}
        }
    }
    for quad in state.store.quads_for_pattern(
        None,
        None,
        Some(subject.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        if let NamedOrBlankNode::NamedNode(target) = quad.subject {
            if let Some((target_label, target_kind)) =
                story_entity_identity(state, target.as_str())?
            {
                relations.push(StoryEvidenceRelation {
                    predicate: quad.predicate.as_str().to_string(),
                    label: local_name(quad.predicate.as_str()).to_string(),
                    direction: StoryRelationDirection::Incoming,
                    target_iri: target.as_str().to_string(),
                    target_label,
                    target_kind,
                });
            }
        }
    }
    properties.sort();
    properties.dedup();
    Ok(Some(StoryEvidenceDetail {
        iri: iri.to_string(),
        title,
        kind,
        status,
        suppressed: false,
        description: first_literal(&state.store, iri, &state.capture.description),
        timestamp: first_literal(&state.store, iri, &state.capture.timestamp),
        author: first_literal(&state.store, iri, &state.capture.author),
        properties,
        relations,
    }))
}

fn story_entity_identity(state: &AppState, iri: &str) -> anyhow::Result<Option<(String, String)>> {
    let node = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    let mut kinds = asserted_project_types(state, &node)
        .into_iter()
        .map(|kind| local_name(&kind).to_string())
        .filter(|kind| kind != "NamedIndividual")
        .collect::<Vec<_>>();
    kinds.sort_by(|left, right| {
        entity_kind_rank(left)
            .cmp(&entity_kind_rank(right))
            .then(left.cmp(right))
    });
    kinds.dedup();
    let kind = kinds.into_iter().next();
    Ok(kind.map(|kind| {
        let label = first_literal(&state.store, iri, moose::RDFS_LABEL)
            .or_else(|| first_literal(&state.store, iri, &state.capture.title))
            .unwrap_or_else(|| local_name(iri).to_string());
        (label, kind)
    }))
}

pub(super) fn build_timeline(evidence: &[StoryEvidenceDetail]) -> Vec<StoryTimelineEvent> {
    let mut events = evidence
        .iter()
        .filter(|item| timeline_evidence_is_eligible(item))
        .map(|item| {
            let mut predecessor_iris = Vec::new();
            let mut successor_iris = Vec::new();
            let mut rationale_iris = Vec::new();
            for relation in &item.relations {
                match (relation.direction.clone(), relation.label.as_str()) {
                    (StoryRelationDirection::Outgoing, "supersedes")
                    | (StoryRelationDirection::Incoming, "isSupersededBy") => {
                        predecessor_iris.push(relation.target_iri.clone())
                    }
                    (StoryRelationDirection::Outgoing, "isSupersededBy")
                    | (StoryRelationDirection::Incoming, "supersedes") => {
                        successor_iris.push(relation.target_iri.clone())
                    }
                    _ if relation.target_kind == "Rationale" => {
                        rationale_iris.push(relation.target_iri.clone())
                    }
                    _ => {}
                }
            }
            predecessor_iris.sort();
            predecessor_iris.dedup();
            successor_iris.sort();
            successor_iris.dedup();
            rationale_iris.sort();
            rationale_iris.dedup();
            StoryTimelineEvent {
                id: format!("event-{}", item.iri),
                title: item.title.clone(),
                kind: item.kind.clone(),
                status: item.status.clone(),
                timestamp: item.timestamp.clone(),
                evidence_iri: item.iri.clone(),
                relation: if !successor_iris.is_empty() {
                    Some("is_superseded_by".to_string())
                } else if !predecessor_iris.is_empty() {
                    Some("supersedes".to_string())
                } else {
                    None
                },
                predecessor_iris,
                successor_iris,
                rationale_iris,
            }
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        timeline_instant(left)
            .is_none()
            .cmp(&timeline_instant(right).is_none())
            .then(timeline_instant(left).cmp(&timeline_instant(right)))
            .then(left.title.cmp(&right.title))
            .then(left.evidence_iri.cmp(&right.evidence_iri))
    });
    events
}

fn timeline_evidence_is_eligible(item: &StoryEvidenceDetail) -> bool {
    if item.status.eq_ignore_ascii_case("proposed") {
        return false;
    }
    !in_working_set(&item.status)
        || item
            .relations
            .iter()
            .any(|relation| matches!(relation.label.as_str(), "supersedes" | "isSupersededBy"))
}

fn timeline_instant(event: &StoryTimelineEvent) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(event.timestamp.as_deref()?)
        .ok()?
        .timestamp_nanos_opt()
}

/// Whether a neighboring SystemComponent contributes its ENTIRE code listing.
///
/// A record often has no code of its own, so the component it concerns is its
/// only grounding — those subjects expand. A CodeEntity subject already names
/// its own position precisely: the component's other members are siblings, not
/// anchors of the subject, and expanding them buries the subject under its
/// whole layer (`propose_link` drew 512 anchors across 26 files).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ComponentExpansion {
    Full,
    SubjectOnly,
}

pub(super) fn entity_code(
    state: &AppState,
    code_entities: &[StoryCodeAnchor],
    entity_iri: &str,
    expansion: ComponentExpansion,
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
        if expansion == ComponentExpansion::Full && component_iri_is_current(state, &neighbor)? {
            anchors.extend(component_code(state, &neighbor)?);
        }
    }
    Ok(dedupe_code_anchors(anchors))
}

fn code_entity_iris_for_symbols(
    state: &AppState,
    symbols: &[String],
) -> anyhow::Result<BTreeSet<String>> {
    if symbols.is_empty() {
        return Ok(BTreeSet::new());
    }
    let wanted = symbols.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let terms = CodeTerms::resolve(state)?;
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let predicate = NamedNodeRef::new(&terms.has_substrate_symbol)?;
    let mut iris = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        let (NamedOrBlankNode::NamedNode(subject), Term::Literal(symbol)) =
            (quad.subject, quad.object)
        else {
            continue;
        };
        if wanted.contains(symbol.value()) {
            iris.insert(subject.as_str().to_string());
        }
    }
    Ok(iris)
}

pub(super) fn code_for_records(
    state: &AppState,
    code_entities: &[StoryCodeAnchor],
    records: &[RecordData],
) -> anyhow::Result<Vec<StoryCodeAnchor>> {
    let mut anchors = Vec::new();
    for record in records.iter().take(12) {
        anchors.extend(entity_code(
            state,
            code_entities,
            &record.evidence.iri,
            ComponentExpansion::Full,
        )?);
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

pub(super) fn component_records(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<Vec<RecordData>> {
    let mut out = Vec::new();
    for record_iri in component_record_iris(state, component_iri)? {
        if let Some(record) = record_data(state, &record_iri)? {
            if in_working_set(&record.evidence.status) {
                out.push(record);
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
    Ok(out)
}

/// Return records linked to a component through either ontology direction.
/// Keeping the pair together prevents Story surfaces from drifting on inverse edges.
fn component_record_iris(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let component = NamedNodeRef::new(component_iri)?;
    let concerns = state.resolve_object_property("concerns")?;
    let predicate = NamedNodeRef::new(&concerns)?;
    let mut records = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(component.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let NamedOrBlankNode::NamedNode(record) = quad?.subject {
            records.insert(record.as_str().to_string());
        }
    }
    let inverse_iri = state.resolve_object_property("isConcernedBy")?;
    let inverse = NamedNodeRef::new(&inverse_iri)?;
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
    Ok(records)
}

pub(super) fn pending_component_record_count(
    state: &AppState,
    component_iri: &str,
) -> anyhow::Result<usize> {
    Ok(component_record_iris(state, component_iri)?
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
        entity_kind_rank(left)
            .cmp(&entity_kind_rank(right))
            .then(left.cmp(right))
    });
    kinds.dedup();
    kinds.into_iter().next()
}

pub(super) fn entity_kind_rank(kind: &str) -> usize {
    match kind {
        "Requirement" => 0,
        "ArchitecturalDecision" => 1,
        "Constraint" => 2,
        "Pattern" => 3,
        "AntiPattern" => 4,
        "Lesson" => 5,
        "Consequence" => 6,
        "Rationale" => 7,
        "Alternative" => 8,
        "SystemComponent" => 9,
        "CodeEntity" => 10,
        "InformationRecord" => 98,
        "ProjectEntity" => 99,
        _ => 50,
    }
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

/// Give the anchors a Story actually renders their current definition line.
///
/// Resolved here rather than in [`all_code`] because that catalog spans every
/// minted entity, while this list is bounded by the beats. Lines come from the
/// substrate's definition lookup only — a miss stays a miss rather than
/// becoming a guess found by searching the file for the symbol's name.
///
/// A line is a claim about the file as it is NOW, so it is published only when
/// the defining file is provably the one this generation indexed. Without that
/// proof the index's line numbers may have drifted, and a Story would send a
/// reader to the wrong place with no way to tell.
fn resolve_anchor_lines(state: &AppState, anchors: &mut [StoryCodeAnchor]) {
    let Some(substrate) = state.substrate() else {
        return;
    };
    // Anchors cluster into a handful of files, so the baseline verdict is
    // memoized per file rather than re-stat'ed per anchor.
    let mut trusted_files: BTreeMap<String, bool> = BTreeMap::new();
    for anchor in anchors {
        let located = substrate
            .definition_location(&anchor.symbol)
            .filter(|found| {
                *trusted_files
                    .entry(found.entry.file.clone())
                    .or_insert_with(|| substrate.indexed_source_len(&found.entry.file).is_some())
            });
        // Take the path from the SAME FileDefinition as the line. The anchor's
        // path came from the graph's defined_in_path, which a file move
        // outdates until the next mint — pairing that stale path with a fresh
        // line would read as `old/path.rs:<line in new/path.rs>` and send the
        // reader somewhere the definition is not.
        if let Some(found) = located {
            anchor.line = Some(found.range.start.line.saturating_add(1));
            anchor.path = Some(found.entry.file);
        } else {
            anchor.line = None;
        }
    }
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
    Ok(component_record_iris(state, component)?.contains(&record.evidence.iri))
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

/// Whether `successor` still supersedes `superseded`.
///
/// The superseded record is deliberately NOT required to be in the working set —
/// being retired is the very fact the question is about — but the successor must
/// still be current, or the Story would teach a replacement that itself went away.
pub(super) fn record_supersedes(
    state: &AppState,
    successor: &str,
    superseded: &str,
) -> anyhow::Result<bool> {
    let Some(record) = record_data(state, successor)? else {
        return Ok(false);
    };
    if !in_working_set(&record.evidence.status) {
        return Ok(false);
    }
    edge_exists(state, successor, "supersedes", superseded)
}

/// Whether `decision` still weighs `alternative`. The edge runs from the
/// decision, so the option and the counterpart swap places here.
pub(super) fn record_weighs(
    state: &AppState,
    alternative: &str,
    decision: &str,
) -> anyhow::Result<bool> {
    edge_exists(state, decision, "weighs", alternative)
}

/// Targets of `subject -predicate-> ?`, in a deterministic order.
pub(super) fn edge_targets(
    state: &AppState,
    subject: &str,
    predicate: &str,
) -> anyhow::Result<Vec<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject)?;
    let predicate_iri = state.resolve_object_property(predicate)?;
    let predicate = NamedNodeRef::new(&predicate_iri)?;
    let mut targets = state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
        .filter_map(|quad| match quad.object {
            oxigraph::model::Term::NamedNode(node) => Some(node.into_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    Ok(targets)
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
