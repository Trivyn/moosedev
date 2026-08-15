//! Read-only Story subject discovery, resolution, summaries, and drift checks.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::model::{GraphNameRef, NamedNodeRef, NamedOrBlankNode};

use crate::graph::{
    first_literal, in_working_set, load_components, resolve_component_query, AppState,
    ComponentEntry, PROJECT_KG_GRAPH_IRI,
};

use super::grounding::{
    all_code, record_data, story_recipe_priority_iris, story_subject_closure_iris_with_priority,
    topic_records, truncate_utf8, RecordData,
};
use super::model::{
    validate_topic, ResolveOutcome, StoryCandidate, StoryCodeAnchor, StoryRecipe,
    StoryRecipeSubject, StorySummary,
};
use super::repository::StorySubjectInvalid;

/// One request-local snapshot reused across a Story library render. This avoids
/// rescanning every code entity and component for every recipe.
pub struct StoryResolutionIndex {
    pub(super) components: Vec<ComponentEntry>,
    pub(super) components_by_iri: BTreeMap<String, ComponentEntry>,
    pub(super) known_components_by_iri: BTreeMap<String, ComponentEntry>,
    pub(super) code_by_symbol: BTreeMap<String, StoryCodeAnchor>,
    pub(super) code_entities: Vec<StoryCodeAnchor>,
}

impl StoryResolutionIndex {
    pub fn build(state: &AppState) -> anyhow::Result<Self> {
        let all_components = load_components(state)?;
        let known_components_by_iri = components_by_iri(&all_components);
        let components = all_components
            .into_iter()
            .filter(|component| component_is_current(state, component))
            .collect::<Vec<_>>();
        let components_by_iri = components.iter().filter_map(component_key_value).collect();
        let (code_by_symbol, code_entities) = all_code(state)?;
        Ok(Self {
            components,
            components_by_iri,
            known_components_by_iri,
            code_by_symbol,
            code_entities,
        })
    }

    pub fn resolve_component(&self, selector: &str) -> anyhow::Result<ResolveOutcome> {
        resolve_component_from(&self.components, selector)
    }

    pub fn resolve_entity(&self, state: &AppState, iri: &str) -> anyhow::Result<StoryCandidate> {
        if let Some(component) = self.components_by_iri.get(iri) {
            return Ok(candidate(component));
        }
        if let Some(record) = record_data(state, iri)? {
            if in_working_set(&record.evidence.status) {
                return Ok(record_candidate(record));
            }
        }
        if let Some(anchor) = self
            .code_entities
            .iter()
            .find(|anchor| anchor.entity_iri.as_deref() == Some(iri))
        {
            return Ok(code_candidate(anchor));
        }
        Err(anyhow::Error::new(StorySubjectInvalid(format!(
            "no current Story subject matches entity IRI {iri:?}"
        ))))
    }

    pub(super) fn recipe_entity(
        &self,
        state: &AppState,
        iri: &str,
    ) -> anyhow::Result<StoryCandidate> {
        if let Some(component) = self.known_components_by_iri.get(iri) {
            return Ok(candidate(component));
        }
        if let Some(record) = record_data(state, iri)? {
            return Ok(record_candidate(record));
        }
        if let Some(anchor) = self
            .code_entities
            .iter()
            .find(|anchor| anchor.entity_iri.as_deref() == Some(iri))
        {
            return Ok(code_candidate(anchor));
        }
        Ok(StoryCandidate {
            iri: iri.to_string(),
            kind: "Entity".to_string(),
            label: iri.to_string(),
            description: None,
        })
    }
}

fn record_candidate(record: RecordData) -> StoryCandidate {
    StoryCandidate {
        iri: record.evidence.iri,
        kind: record.evidence.kind,
        label: record.evidence.title,
        description: record.description,
    }
}

fn code_candidate(anchor: &StoryCodeAnchor) -> StoryCandidate {
    StoryCandidate {
        iri: anchor.entity_iri.clone().unwrap_or_default(),
        kind: "CodeEntity".to_string(),
        label: anchor.label.clone(),
        description: anchor.path.clone(),
    }
}

pub fn story_subjects(
    state: &AppState,
    query: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<StoryCandidate>> {
    let query = query.map(str::trim).filter(|query| !query.is_empty());
    let mut subjects = load_components(state)?
        .into_iter()
        .filter(|component| component_is_current(state, component))
        .map(|component| candidate(&component))
        .collect::<Vec<_>>();
    subjects.extend(current_record_subjects(state)?);
    subjects.extend(
        all_code(state)?
            .1
            .into_iter()
            .filter(|anchor| anchor.entity_iri.is_some())
            .map(|anchor| code_candidate(&anchor)),
    );
    subjects.sort_by(story_subject_order);
    subjects.dedup_by(|left, right| left.iri == right.iri);

    if let Some(query) = query {
        let query = query.to_lowercase();
        subjects.retain(|subject| {
            subject.label.to_lowercase().contains(&query)
                || subject.kind.to_lowercase().contains(&query)
                || subject
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_lowercase().contains(&query))
        });
        subjects.truncate(limit.clamp(1, 300));
    }
    Ok(subjects)
}

fn current_record_subjects(state: &AppState) -> anyhow::Result<Vec<StoryCandidate>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let mut iris = BTreeSet::new();
    for quad in state.store.quads_for_pattern(
        None,
        Some(NamedNodeRef::new_unchecked(moose::RDF_TYPE)),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let quad = quad?;
        if let NamedOrBlankNode::NamedNode(subject) = quad.subject {
            iris.insert(subject.as_str().to_string());
        }
    }
    let mut records = Vec::new();
    for iri in iris {
        let Some(record) = record_data(state, &iri)? else {
            continue;
        };
        if in_working_set(&record.evidence.status) {
            let mut candidate = record_candidate(record);
            candidate.description = candidate
                .description
                .map(|description| truncate_utf8(&description, 180).to_string());
            records.push(candidate);
        }
    }
    Ok(records)
}

fn story_subject_order(left: &StoryCandidate, right: &StoryCandidate) -> std::cmp::Ordering {
    story_subject_kind_rank(&left.kind)
        .cmp(&story_subject_kind_rank(&right.kind))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
        .then_with(|| left.iri.cmp(&right.iri))
}

fn story_subject_kind_rank(kind: &str) -> usize {
    match kind {
        "SystemComponent" => 0,
        "Requirement" => 1,
        "ArchitecturalDecision" => 2,
        "Constraint" => 3,
        "Pattern" | "AntiPattern" => 4,
        "Lesson" => 5,
        "Consequence" | "Rationale" => 6,
        "CodeEntity" => 8,
        _ => 7,
    }
}

fn component_key_value(component: &ComponentEntry) -> Option<(String, ComponentEntry)> {
    component
        .iri
        .as_ref()
        .map(|iri| (iri.clone(), component.clone()))
}

fn components_by_iri(components: &[ComponentEntry]) -> BTreeMap<String, ComponentEntry> {
    components.iter().filter_map(component_key_value).collect()
}

fn component_is_current(state: &AppState, component: &ComponentEntry) -> bool {
    component.iri.as_deref().is_some_and(|iri| {
        first_literal(&state.store, iri, &state.capture.status)
            .as_deref()
            .is_none_or(in_working_set)
    })
}

/// Resolve display metadata and drift from one request-local graph snapshot.
pub fn enrich_summary(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
    summary: &mut StorySummary,
) -> anyhow::Result<()> {
    match recipe.resolved_subject()? {
        StoryRecipeSubject::Entity { iri } => {
            let subject = index.recipe_entity(state, iri)?;
            summary.subject_label = subject.label;
            summary.subject_kind = subject.kind;
        }
        StoryRecipeSubject::Topic { query } => {
            summary.subject_label = query.clone();
            summary.subject_kind = "Topic".to_string();
        }
    }
    summary.drifted = recipe_has_drift_with_index(state, index, recipe)?;
    Ok(())
}

pub fn recipe_has_drift(state: &AppState, recipe: &StoryRecipe) -> anyhow::Result<bool> {
    let index = StoryResolutionIndex::build(state)?;
    recipe_has_drift_with_index(state, &index, recipe)
}

fn recipe_has_drift_with_index(
    state: &AppState,
    index: &StoryResolutionIndex,
    recipe: &StoryRecipe,
) -> anyhow::Result<bool> {
    let subject = recipe.resolved_subject()?;
    match subject {
        StoryRecipeSubject::Entity { iri } if index.components_by_iri.contains_key(iri) => {}
        StoryRecipeSubject::Entity { iri } => {
            if index.resolve_entity(state, iri).is_err() {
                return Ok(true);
            }
        }
        StoryRecipeSubject::Topic { query } => {
            if validate_topic(query).is_err() || topic_records(state, query)?.is_empty() {
                return Ok(true);
            }
        }
    }
    let priority = story_recipe_priority_iris(state, recipe)?;
    let closure = story_subject_closure_iris_with_priority(state, subject, &priority)?;
    for iri in recipe
        .focus
        .include_record_iris
        .iter()
        .chain(&recipe.focus.exclude_record_iris)
    {
        if !closure.contains(iri) || record_data(state, iri)?.is_none() {
            return Ok(true);
        }
    }
    for iri in &recipe.focus.include_record_iris {
        if record_data(state, iri)?
            .is_some_and(|record| record.evidence.status.eq_ignore_ascii_case("proposed"))
        {
            return Ok(true);
        }
    }
    for symbol in recipe
        .focus
        .include_code_symbols
        .iter()
        .chain(&recipe.focus.exclude_code_symbols)
    {
        let Some(entity) = index
            .code_by_symbol
            .get(symbol)
            .and_then(|anchor| anchor.entity_iri.as_ref())
        else {
            return Ok(true);
        };
        if !closure.contains(entity) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_component_from(
    components: &[ComponentEntry],
    selector: &str,
) -> anyhow::Result<ResolveOutcome> {
    let selector = selector.trim();
    if selector.is_empty() {
        anyhow::bail!("a component IRI or prompt is required");
    }
    let matches = resolve_component_query(components, selector);
    if matches.is_empty() {
        anyhow::bail!("no SystemComponent matches {selector:?}");
    }
    if matches.len() == 1 {
        Ok(ResolveOutcome::Resolved(candidate(matches[0])))
    } else {
        Ok(ResolveOutcome::Ambiguous(
            matches.into_iter().map(candidate).collect(),
        ))
    }
}

fn candidate(component: &ComponentEntry) -> StoryCandidate {
    StoryCandidate {
        iri: component.iri.clone().unwrap_or_default(),
        kind: "SystemComponent".to_string(),
        label: component.name.clone(),
        description: (!component.covers_paths.is_empty()).then(|| {
            format!(
                "Owns {}",
                component
                    .covers_paths
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
    }
}
