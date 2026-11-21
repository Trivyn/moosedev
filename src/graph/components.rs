//! Project `SystemComponent` path coverage and lifecycle helpers.
//!
//! Components declare the repository paths they own with `coversPath` literals
//! in the project KG. This module is the shared read/match layer for tools that
//! need to turn a source path into the most specific owning component, and the
//! write layer that retires, merges and renames components (issue #69).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term,
};
use oxigraph::store::Store;

use super::capture::{
    asserted_project_types, capture_instance_quads, require_information_record, CaptureStamp,
};
use super::context::first_literal;
use super::lifecycle::{declared_inverses, in_working_set, is_retired, LIFECYCLE_PREDICATES};
use super::state::AppState;
use super::util::{datatype_property_iri, is_subclass_of, local_name, mint_instance_iri};
use super::PROJECT_KG_GRAPH_IRI;

/// A SystemComponent from the project KG together with the repo paths it
/// covers (`coversPath` literals; trailing '/' = directory prefix, otherwise
/// an exact file path).
#[derive(Debug, Clone)]
pub struct ComponentEntry {
    /// None only for planned-but-not-yet-minted components (seeding flows
    /// like backfill_concerns); `load_components` always sets it.
    pub iri: Option<String>,
    pub name: String,
    pub covers_paths: BTreeSet<String>,
}

/// Result of declaring new path coverage for a `SystemComponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareOutcome {
    pub component_iri: String,
    pub component_name: String,
    pub added: Vec<String>,
    pub already_covered: Vec<String>,
}

/// Result of removing path coverage from a `SystemComponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub component_iri: String,
    pub component_name: String,
    pub removed: Vec<String>,
    pub not_declared: Vec<String>,
}

/// Whether a component with this lifecycle status belongs to the current
/// component set: everything except `rejected`, `superseded` and `deprecated`.
/// No status (legacy) and `proposed` stay current, so a proposed component keeps
/// governing its paths as it always has.
///
/// The Stories code deliberately applies the stricter
/// [`in_working_set`](super::lifecycle::in_working_set) to component subjects,
/// which also excludes `proposed`: a Story is an authoritative narrative, while
/// path coverage is structure the codebase already has.
pub fn component_is_current(status: Option<&str>) -> bool {
    !status.is_some_and(|status| is_retired(status) || status.eq_ignore_ascii_case("rejected"))
}

/// The Stories rule for a component: [`in_working_set`] (so `proposed` is
/// out too) over the shared [`component_status`], so a retiring literal wins
/// there exactly as it does in [`load_components`].
pub(crate) fn component_in_working_set(state: &AppState, iri: &str) -> anyhow::Result<bool> {
    Ok(component_status(state, iri)?
        .as_deref()
        .is_none_or(in_working_set))
}

/// Load the current `SystemComponent`s (see [`component_is_current`]) and
/// their `coversPath` values from the project graph. A retired component's
/// paths stay on it as history and govern nothing.
///
/// Ontology terms are resolved by local name from the loaded architecture
/// vocabulary here, so callers do not need to know the current ontology
/// namespace or full term IRIs.
pub fn load_components(state: &AppState) -> anyhow::Result<Vec<ComponentEntry>> {
    Ok(load_component_records(state)?
        .into_iter()
        .filter(|component| component_is_current(component.status.as_deref()))
        .map(|component| component.entry)
        .collect())
}

/// Every minted `SystemComponent`, retired ones included, for readers that must
/// still recognize a component after it left the current set (a Story recipe
/// that names it, a lifecycle tool that explains why it is gone).
pub fn load_all_components(state: &AppState) -> anyhow::Result<Vec<ComponentEntry>> {
    Ok(load_component_records(state)?
        .into_iter()
        .map(|component| component.entry)
        .collect())
}

/// A loaded component with its lifecycle status (see [`component_status`]).
#[derive(Debug, Clone)]
struct StatusedComponent {
    entry: ComponentEntry,
    status: Option<String>,
}

impl StatusedComponent {
    fn iri(&self) -> &str {
        self.entry
            .iri
            .as_deref()
            .expect("load_component_records sets component IRI")
    }

    fn is_current(&self) -> bool {
        component_is_current(self.status.as_deref())
    }
}

fn load_component_records(state: &AppState) -> anyhow::Result<Vec<StatusedComponent>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let system_component = state.resolve_class("SystemComponent")?;
    let class = NamedNodeRef::new(&system_component)?;
    let covers_path = datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let component_name = datatype_property_iri(&state.arch_vocab, "hasComponentName").ok();
    let mut out = Vec::new();

    // SystemComponents are identified by their rdf:type in the project graph.
    // The ontology resolver above supplies the full class IRI at runtime.
    for q in state.store.quads_for_pattern(
        None,
        Some(rdf_type),
        Some(class.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let NamedOrBlankNode::NamedNode(subject) = q.subject else {
            continue;
        };
        let iri = subject.as_str().to_string();
        let name = component_display_name(state, &iri, component_name.as_deref());
        let covers_paths = literal_values(&state.store, &iri, &covers_path)?;
        let status = component_status(state, &iri)?;
        out.push(StatusedComponent {
            entry: ComponentEntry {
                iri: Some(iri),
                name,
                covers_paths,
            },
            status,
        });
    }

    // Deterministic ordering keeps downstream mint plans and dry-run reports
    // stable regardless of store iteration order.
    out.sort_by(|a, b| {
        a.entry
            .name
            .cmp(&b.entry.name)
            .then(a.entry.iri.cmp(&b.entry.iri))
    });
    Ok(out)
}

/// rdfs:label is canonical for display/search; hasComponentName is where a
/// component's own name lives, and capture.title is kept as a compatibility
/// fallback for older or partially seeded records. A component seeded without
/// rdfs:label is still known by its own name rather than its IRI.
fn component_display_name(state: &AppState, iri: &str, component_name: Option<&str>) -> String {
    first_literal(&state.store, iri, moose::RDFS_LABEL)
        .or_else(|| {
            component_name.and_then(|predicate| first_literal(&state.store, iri, predicate))
        })
        .or_else(|| first_literal(&state.store, iri, &state.capture.title))
        .unwrap_or_else(|| iri.to_string())
}

/// A component's lifecycle status, as every reader must see it. A store merged
/// from two diverged copies can briefly carry two status literals; a retiring
/// one wins, so a component retired on either side never silently comes back.
pub(crate) fn component_status(state: &AppState, iri: &str) -> anyhow::Result<Option<String>> {
    let statuses = literal_values(&state.store, iri, &state.capture.status)?;
    Ok(statuses
        .iter()
        .find(|status| !component_is_current(Some(status)))
        .or_else(|| statuses.iter().next())
        .cloned())
}

/// Resolve a human component query against a caller-selected component set.
///
/// Exact IRI and case-insensitive label matches win. Otherwise query words are
/// scored against labels and all top-scoring matches are returned so callers
/// can surface ambiguity instead of guessing. Keeping this alongside component
/// loading prevents UI projections from inventing divergent matching rules.
pub fn resolve_component_query<'a>(
    components: &'a [ComponentEntry],
    query: &str,
) -> Vec<&'a ComponentEntry> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    if let Some(component) = components
        .iter()
        .find(|entry| entry.iri.as_deref() == Some(query))
    {
        return vec![component];
    }
    // IRI-shaped input is an identity lookup, never fuzzy prose. A typo in an
    // IRI must not silently select a similarly named component.
    if NamedNode::new(query).is_ok() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let exact = components
        .iter()
        .filter(|entry| entry.name.to_lowercase() == needle)
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        return exact;
    }
    let words = needle
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .collect::<Vec<_>>();
    let mut scored = components
        .iter()
        .filter_map(|component| {
            let name = component.name.to_lowercase();
            let score = words.iter().filter(|word| name.contains(**word)).count()
                + usize::from(name.contains(&needle));
            (score > 0).then_some((score, component))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then(left.name.cmp(&right.name))
            .then(left.iri.cmp(&right.iri))
    });
    let Some(best_score) = scored.first().map(|entry| entry.0) else {
        return Vec::new();
    };
    scored
        .into_iter()
        .filter(|(score, _)| *score == best_score)
        .map(|(_, component)| component)
        .collect()
}

/// The `coversPath` value that covers every repository path: a component
/// whose rules are project-wide, such as one a top-level spec governs.
pub const COVERS_WHOLE_PROJECT: &str = ".";

/// Return the most specific component that covers `path`.
///
/// `coversPath` values ending in `/` are directory prefixes. Values without a
/// trailing slash are exact file paths. [`COVERS_WHOLE_PROJECT`] covers
/// everything and loses to any other match. If several entries match, the
/// longest matching `coversPath` wins.
pub fn best_component_for_path<'a>(
    path: &str,
    components: &'a [ComponentEntry],
) -> Option<&'a ComponentEntry> {
    components_for_path(path, components).into_iter().next()
}

/// Every component that covers `path`, most specific first: a file inside a
/// crate that has its own spec is governed by that crate's rules and by the
/// rules of every component enclosing it, such as a whole-project spec's.
/// Specificity is the length of the matching `coversPath` (the whole-project
/// marker is 0); ties keep catalog order.
pub fn components_for_path<'a>(
    path: &str,
    components: &'a [ComponentEntry],
) -> Vec<&'a ComponentEntry> {
    let mut matched: Vec<(&ComponentEntry, usize)> = Vec::new();
    for component in components {
        let specificity = component
            .covers_paths
            .iter()
            .filter_map(|covers_path| {
                if covers_path == COVERS_WHOLE_PROJECT {
                    Some(0)
                } else if covers_path.ends_with('/') {
                    path.starts_with(covers_path.as_str())
                        .then_some(covers_path.len())
                } else {
                    (path == covers_path).then_some(covers_path.len())
                }
            })
            .max();
        if let Some(specificity) = specificity {
            matched.push((component, specificity));
        }
    }
    matched.sort_by_key(|(_, specificity)| std::cmp::Reverse(*specificity));
    matched
        .into_iter()
        .map(|(component, _)| component)
        .collect()
}

/// Declare repository path coverage for a minted `SystemComponent`.
///
/// `paths` are repo-relative. Values ending in `/` cover a directory prefix;
/// values without a trailing slash cover one exact file path. Re-declaring an
/// existing value is a successful no-op.
pub fn declare_component_paths(
    state: &AppState,
    component: &str,
    paths: &[String],
) -> anyhow::Result<DeclareOutcome> {
    let requested = validate_paths(paths)?;
    // Held from resolution through commit: a component retired or merged in
    // between must not receive (or lose) coverage.
    let _guard = state.lock_proposal_writes()?;
    let (resolved, subject) = resolve_system_component(state, component)?;

    let covers_path = datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let existing = literal_values(&state.store, &resolved.iri, &covers_path)?;
    let mut already_covered = Vec::new();
    let mut added = Vec::new();
    for path in requested {
        if existing.contains(&path) {
            already_covered.push(path);
        } else {
            added.push(path);
        }
    }
    already_covered.sort();
    added.sort();

    if !added.is_empty() {
        let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI)?;
        let predicate = NamedNode::new(&covers_path)?;
        let mut txn = state
            .store
            .start_transaction()
            .map_err(|e| anyhow::anyhow!("declare component paths transaction: {e}"))?;
        for path in &added {
            let quad = Quad::new(
                subject.clone(),
                predicate.clone(),
                Literal::new_simple_literal(path),
                GraphName::NamedNode(graph.clone()),
            );
            txn.insert(quad.as_ref());
        }
        txn.commit()
            .map_err(|e| anyhow::anyhow!("declare component paths commit: {e}"))?;
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    }

    Ok(DeclareOutcome {
        component_iri: resolved.iri,
        component_name: resolved.name,
        added,
        already_covered,
    })
}

/// Remove repository path coverage from a minted `SystemComponent`.
///
/// The inverse of [`declare_component_paths`], for coverage that no longer
/// applies — most often a component whose files moved, whose old paths would
/// otherwise be reported as matching no file on every validation run (#57).
/// `paths` must match declared values exactly (`src/code/` and `src/code` are
/// different declarations). Removing an undeclared value is a successful no-op,
/// reported as such.
pub fn remove_component_paths(
    state: &AppState,
    component: &str,
    paths: &[String],
) -> anyhow::Result<RemoveOutcome> {
    let requested = validate_paths(paths)?;
    let _guard = state.lock_proposal_writes()?;
    let (resolved, subject) = resolve_system_component(state, component)?;

    let covers_path = datatype_property_iri(&state.arch_vocab, "coversPath")?;
    let existing = literal_values(&state.store, &resolved.iri, &covers_path)?;
    let (removed, not_declared): (Vec<String>, Vec<String>) = requested
        .into_iter()
        .partition(|path| existing.contains(path));

    if !removed.is_empty() {
        let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI)?;
        let predicate = NamedNode::new(&covers_path)?;
        let mut txn = state
            .store
            .start_transaction()
            .map_err(|e| anyhow::anyhow!("remove component paths transaction: {e}"))?;
        for path in &removed {
            let quad = Quad::new(
                subject.clone(),
                predicate.clone(),
                Literal::new_simple_literal(path),
                GraphName::NamedNode(graph.clone()),
            );
            txn.remove(quad.as_ref());
        }
        txn.commit()
            .map_err(|e| anyhow::anyhow!("remove component paths commit: {e}"))?;
        state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    }

    Ok(RemoveOutcome {
        component_iri: resolved.iri,
        component_name: resolved.name,
        removed,
        not_declared,
    })
}

/// Resolve `component` (IRI or exact name) to a current minted component,
/// refusing a retired one and a target whose asserted project types do not
/// include `SystemComponent`.
fn resolve_system_component(
    state: &AppState,
    component: &str,
) -> anyhow::Result<(ResolvedComponent, NamedNode)> {
    let all = load_component_records(state)?;
    let components = all
        .iter()
        .filter(|entry| entry.is_current())
        .map(|entry| entry.entry.clone())
        .collect::<Vec<_>>();
    // A retired component is not a coverage target. Say what became of it
    // instead of calling it unknown: by IRI (which `resolve_component` would
    // otherwise accept) or by a name no current component carries.
    let resolved = match resolve_component(state, &components, component) {
        Ok(resolved) => resolved,
        Err(error) => {
            match find_components(all.iter().filter(|e| !e.is_current()), component).as_slice() {
                [entry] => anyhow::bail!(
                    "{}; it no longer covers paths",
                    describe_retired(state, &all, entry)
                ),
                _ => return Err(error),
            }
        }
    };
    if let Some(entry) = all
        .iter()
        .find(|entry| !entry.is_current() && entry.iri() == resolved.iri)
    {
        anyhow::bail!(
            "{}; it no longer covers paths",
            describe_retired(state, &all, entry)
        );
    }
    let subject = NamedNode::new(&resolved.iri)
        .map_err(|e| anyhow::anyhow!("invalid component IRI {}: {e}", resolved.iri))?;
    let system_component = state.resolve_class("SystemComponent")?;
    let asserted = asserted_project_types(state, &subject);
    if !asserted.iter().any(|class| class == &system_component) {
        anyhow::bail!(
            "target {} is not a SystemComponent; asserted project types: {}",
            resolved.iri,
            if asserted.is_empty() {
                "(none)".to_string()
            } else {
                asserted.join(", ")
            }
        );
    }
    Ok((resolved, subject))
}

/// Collect literal objects for one predicate on one subject in the project KG.
///
/// Non-literal objects are ignored because `coversPath` is a datatype property
/// and only literal values participate in path matching.
fn literal_values(
    store: &Store,
    subject_iri: &str,
    predicate_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(subject_iri)?;
    let predicate = NamedNodeRef::new(predicate_iri)?;
    let mut out = BTreeSet::new();
    for q in store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let Term::Literal(literal) = q.object {
            out.insert(literal.value().to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct ResolvedComponent {
    iri: String,
    name: String,
}

fn resolve_component(
    state: &AppState,
    components: &[ComponentEntry],
    component: &str,
) -> anyhow::Result<ResolvedComponent> {
    if let Ok(node) = NamedNode::new(component) {
        let iri = node.as_str().to_string();
        if has_project_subject_quads(state, &node)? {
            let name = components
                .iter()
                .find(|entry| entry.iri.as_deref() == Some(iri.as_str()))
                .map(|entry| entry.name.clone())
                .or_else(|| first_literal(&state.store, &iri, moose::RDFS_LABEL))
                .or_else(|| first_literal(&state.store, &iri, &state.capture.title))
                .unwrap_or_else(|| iri.clone());
            return Ok(ResolvedComponent { iri, name });
        }
    }

    let matches = components
        .iter()
        .filter(|entry| entry.name == component)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [entry] => Ok(ResolvedComponent {
            iri: entry
                .iri
                .clone()
                .expect("load_components sets component IRI"),
            name: entry.name.clone(),
        }),
        [] => {
            let names = components
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "unknown SystemComponent {component:?}; known components from load_components: {}",
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            )
        }
        many => {
            let iris = many
                .iter()
                .filter_map(|entry| entry.iri.as_deref())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "ambiguous SystemComponent label {component:?}; matching IRIs: {}",
                iris.join(", ")
            )
        }
    }
}

/// Components matching `query` by exact IRI, else by case-insensitive name.
fn find_components<'a>(
    components: impl Iterator<Item = &'a StatusedComponent>,
    query: &str,
) -> Vec<&'a StatusedComponent> {
    let query = query.trim();
    let components = components.collect::<Vec<_>>();
    let by_iri = components
        .iter()
        .copied()
        .filter(|entry| entry.iri() == query)
        .collect::<Vec<_>>();
    if !by_iri.is_empty() {
        return by_iri;
    }
    components
        .into_iter()
        .filter(|entry| entry.entry.name.eq_ignore_ascii_case(query))
        .collect()
}

/// The component that replaced `iri`: the stored `isSupersededBy`, else the
/// subject of a `supersedes` edge naming it.
fn successor_of(state: &AppState, iri: &str) -> anyhow::Result<Option<String>> {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let node = NamedNodeRef::new(iri)?;
    let successor = state.resolve_object_property("isSupersededBy")?;
    for quad in state.store.quads_for_pattern(
        Some(node.into()),
        Some(NamedNodeRef::new(&successor)?),
        None,
        Some(graph),
    ) {
        if let Term::NamedNode(successor) = quad?.object {
            return Ok(Some(successor.into_string()));
        }
    }
    let supersedes = state.resolve_object_property("supersedes")?;
    for quad in state.store.quads_for_pattern(
        None,
        Some(NamedNodeRef::new(&supersedes)?),
        Some(node.into()),
        Some(graph),
    ) {
        if let NamedOrBlankNode::NamedNode(successor) = quad?.subject {
            return Ok(Some(successor.into_string()));
        }
    }
    Ok(None)
}

/// One sentence saying what became of a retired component, naming its
/// successor when it was merged.
fn describe_retired(
    state: &AppState,
    all: &[StatusedComponent],
    entry: &StatusedComponent,
) -> String {
    let status = entry.status.as_deref().unwrap_or("retired");
    let successor = successor_of(state, entry.iri()).ok().flatten().map(|iri| {
        let name = all
            .iter()
            .find(|candidate| candidate.iri() == iri)
            .map(|candidate| candidate.entry.name.clone())
            .unwrap_or_else(|| component_display_name(state, &iri, None));
        (iri, name)
    });
    match successor {
        Some((iri, name)) => format!(
            "SystemComponent {:?} ({}) is {status}: it was merged into {name:?} ({iri})",
            entry.entry.name,
            entry.iri()
        ),
        None => format!(
            "SystemComponent {:?} ({}) is {status}",
            entry.entry.name,
            entry.iri()
        ),
    }
}

/// Whether `iri` is a `SystemComponent` that has left the current set.
pub(crate) fn is_retired_component(state: &AppState, iri: &str) -> bool {
    let Ok(node) = NamedNode::new(iri) else {
        return false;
    };
    let Ok(component) = state.resolve_class("SystemComponent") else {
        return false;
    };
    asserted_project_types(state, &node)
        .iter()
        .any(|class| is_subclass_of(&state.store, class, &component))
        && component_status(state, iri)
            .ok()
            .flatten()
            .is_some_and(|status| !component_is_current(Some(&status)))
}

/// One end of an edge about to be written: a node already in the graph, or one
/// being captured with the class and lifecycle status it is written with.
pub(crate) enum EdgeEnd<'a> {
    Existing(&'a NamedNode),
    New {
        class_iri: &'a str,
        status: Option<&'a str>,
    },
}

/// Refuse a NEW `concerns`/`constrains`/`realizes` edge (either orientation)
/// whose component end is retired: knowledge and code recorded from now on
/// belong on the successor. Existing edges stay as history. Shared by
/// `relate`, inline capture relations and cluster capture.
pub(crate) fn guard_retired_component_endpoint(
    state: &AppState,
    predicate_iri: &str,
    subject: EdgeEnd<'_>,
    object: EdgeEnd<'_>,
) -> anyhow::Result<()> {
    for local in ["concerns", "constrains", "realizes"] {
        let forward = state.resolve_object_property(local)?;
        let component_end = if predicate_iri == forward {
            &object
        } else if declared_inverses(state, &forward)?
            .iter()
            .any(|inverse| inverse == predicate_iri)
        {
            &subject
        } else {
            continue;
        };
        match component_end {
            EdgeEnd::Existing(node) => {
                if is_retired_component(state, node.as_str()) {
                    let all = load_component_records(state)?;
                    if let Some(entry) = all.iter().find(|entry| entry.iri() == node.as_str()) {
                        anyhow::bail!(
                            "cannot add a new {:?} edge: {}; link the current component instead",
                            local_name(predicate_iri),
                            describe_retired(state, &all, entry)
                        );
                    }
                }
            }
            EdgeEnd::New { class_iri, status } => {
                // A component captured already retired would carry the very
                // links its status says it no longer takes.
                let component = state.resolve_class("SystemComponent")?;
                if is_subclass_of(&state.store, class_iri, &component)
                    && !component_is_current(*status)
                {
                    anyhow::bail!(
                        "cannot add a new {:?} edge: the SystemComponent being captured is {}; \
                         a retired component takes no new links",
                        local_name(predicate_iri),
                        status.unwrap_or_default()
                    );
                }
            }
        }
    }
    Ok(())
}

/// A record linked to a component, for lifecycle reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedRecord {
    pub iri: String,
    /// Class local name, e.g. `Constraint`.
    pub kind: String,
    pub title: String,
}

/// A `concerns` / `constrains` / `realizes` edge re-pointed from the retired
/// component to its successor, named by its forward predicate and the record
/// or code entity at its other end. Both stored orientations of one edge are
/// reported once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedEdge {
    pub predicate_local: String,
    pub subject_iri: String,
    /// The successor already carried this edge, so nothing was added for it.
    pub already_on_successor: bool,
}

/// An asserted edge incident to the retired component that a merge left in
/// place, for the caller to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentEdge {
    pub predicate_local: String,
    pub other_iri: String,
    /// `true` when the retired component is the subject.
    pub outgoing: bool,
}

/// What [`retire_component`] did, or on a dry run would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireOutcome {
    pub component_iri: String,
    pub component_name: String,
    /// `deprecated` (plain retire) or `superseded` (merge).
    pub status: String,
    /// `(iri, name)` of the component it was merged into.
    pub successor: Option<(String, String)>,
    /// The minted Rationale; `None` on a dry run, which mints nothing.
    pub rationale_iri: Option<String>,
    pub dry_run: bool,
    pub moved_edges: Vec<MovedEdge>,
    /// `coversPath` values moved to the successor.
    pub moved_paths: Vec<String>,
    /// `coversPath` values the successor already declared; dropped from the
    /// retired component without adding a duplicate.
    pub paths_already_on_successor: Vec<String>,
    /// Asserted edges a merge did not move (see [`IncidentEdge`]).
    pub not_moved_edges: Vec<IncidentEdge>,
    /// A plain retire's `coversPath` values, kept on the component as history.
    pub remaining_paths: Vec<String>,
    /// Records still linked to a plainly retired component.
    pub remaining_records: Vec<LinkedRecord>,
    /// Current Constraints linked to the component by `concerns`/`constrains`:
    /// on a merge their reach widens to the successor, on a plain retire they
    /// stop being delivered through it. Sorted by title.
    pub affected_constraints: Vec<LinkedRecord>,
}

/// Retire a `SystemComponent`, or merge it into another (issue #69).
///
/// Without `into` the component becomes `deprecated` and carries the
/// Rationale; its paths and links stay as history and it drops out of the
/// current set. With `into`, the successor `supersedes` it (the inverse is
/// stored explicitly, never left to the reasoner), carries the Rationale, and
/// receives its asserted `concerns`/`constrains`/`realizes` edges and its
/// paths; the retired component becomes `superseded`. Everything else incident
/// to it is reported, not moved. One transaction; `dry_run` runs every check
/// and returns the same report without writing or minting anything.
pub fn retire_component(
    state: &AppState,
    component: &str,
    rationale: &str,
    into: Option<&str>,
    dry_run: bool,
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<RetireOutcome> {
    let rationale = rationale.trim();
    anyhow::ensure!(
        !rationale.is_empty(),
        "retire_component needs a rationale: say why the component is retired or merged"
    );
    let _proposal_guard = state.lock_proposal_writes()?;
    let all = load_component_records(state)?;
    let retiring = resolve_retiring_component(state, &all, component)?;
    let successor = into
        .map(|query| resolve_merge_target(state, &all, query, retiring))
        .transpose()?;
    let a_node = NamedNode::new(retiring.iri())?;
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);

    // Hold the enrichment lock from the inferred-quad read through the stale
    // flag, so no enrichment computed from the old graph commits in between.
    let _enrich_guard = state.lock_enrichment();
    let inferred: HashSet<Quad> =
        crate::provenance::reasoner_inferred_data_quads(&state.store, PROJECT_KG_GRAPH_IRI)?
            .into_iter()
            .collect();
    let lifecycle = LIFECYCLE_PREDICATES
        .iter()
        .map(|local| state.resolve_object_property(local))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let MergePlan {
        mut removals,
        mut insertions,
        promoted,
        moved_edges,
        moved_paths,
        paths_already_on_successor,
    } = match successor {
        Some(successor) => plan_merge(state, retiring, successor, &inferred)?,
        None => MergePlan::default(),
    };
    let (not_moved_edges, remaining_paths, remaining_records) = match successor {
        Some(_) => (
            not_moved_edges(state, &a_node, &inferred, &removals, &lifecycle)?,
            Vec::new(),
            Vec::new(),
        ),
        None => (
            Vec::new(),
            retiring.entry.covers_paths.iter().cloned().collect(),
            linked_records(state, &a_node, |predicate| {
                !lifecycle.iter().any(|iri| iri == predicate)
            })?,
        ),
    };

    let status = if successor.is_some() {
        "superseded"
    } else {
        "deprecated"
    };
    let mut outcome = RetireOutcome {
        component_iri: retiring.iri().to_string(),
        component_name: retiring.entry.name.clone(),
        status: status.to_string(),
        successor: successor.map(|entry| (entry.iri().to_string(), entry.entry.name.clone())),
        rationale_iri: None,
        dry_run,
        moved_edges,
        moved_paths,
        paths_already_on_successor,
        not_moved_edges,
        remaining_paths,
        remaining_records,
        affected_constraints: affected_constraints(state, &a_node)?,
    };
    if dry_run {
        return Ok(outcome);
    }

    let rationale_iri = mint_instance_iri("Rationale");
    let rationale_title = match successor {
        Some(successor) => format!(
            "Rationale: merge {} into {}",
            retiring.entry.name, successor.entry.name
        ),
        None => format!("Rationale: retire {}", retiring.entry.name),
    };
    let timestamp = when.to_rfc3339();
    let stamp = CaptureStamp {
        capture: &state.capture,
        author,
        timestamp: &timestamp,
        status: "accepted",
    };
    let rationale_quads = capture_instance_quads(
        &state.store,
        &rationale_iri,
        &state.resolve_class("Rationale")?,
        &[
            (moose::RDFS_LABEL.to_string(), rationale_title.clone()),
            (state.capture.title.clone(), rationale_title),
            (state.capture.description.clone(), rationale.to_string()),
        ],
        &[],
        &stamp,
    )?;
    let rationale_node = NamedNode::new(&rationale_iri)?;
    let has_rationale = NamedNode::new(state.resolve_object_property("hasRationale")?)?;
    match successor {
        Some(successor) => {
            let b_node = NamedNode::new(successor.iri())?;
            insertions.push(Quad::new(
                b_node.clone(),
                NamedNode::new(state.resolve_object_property("supersedes")?)?,
                a_node.clone(),
                graph.clone(),
            ));
            // Integrity-critical inverse, persisted at mutation time so readers
            // that walk from the retired component find its successor without
            // waiting for enrichment.
            insertions.push(Quad::new(
                a_node.clone(),
                NamedNode::new(state.resolve_object_property("isSupersededBy")?)?,
                b_node.clone(),
                graph.clone(),
            ));
            insertions.push(Quad::new(
                b_node,
                has_rationale,
                rationale_node,
                graph.clone(),
            ));
        }
        None => insertions.push(Quad::new(
            a_node.clone(),
            has_rationale,
            rationale_node,
            graph.clone(),
        )),
    }
    let status_predicate = NamedNode::new(&state.capture.status)?;
    removals.extend(
        state
            .store
            .quads_for_pattern(
                Some(a_node.as_ref().into()),
                Some(status_predicate.as_ref()),
                None,
                Some(GraphNameRef::NamedNode(NamedNodeRef::new(
                    PROJECT_KG_GRAPH_IRI,
                )?)),
            )
            .collect::<Result<Vec<_>, _>>()?,
    );
    insertions.push(Quad::new(
        a_node,
        status_predicate,
        Literal::new_simple_literal(status),
        graph,
    ));
    // Drop the inference tags of the quads going away (a stale tag would mark a
    // later re-assertion as inferred) and of the ones being promoted.
    let mut targets = removals.clone();
    targets.extend(promoted);
    let provenance = crate::provenance::reasoner_inference_provenance_for(
        &state.store,
        PROJECT_KG_GRAPH_IRI,
        &targets,
    )?;

    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("retire component transaction: {e}"))?;
    for quad in removals.iter().chain(&provenance) {
        txn.remove(quad.as_ref());
    }
    txn.extend(rationale_quads.iter().map(Quad::as_ref));
    txn.extend(insertions.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("retire component commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);
    state.mark_inferred_stale();

    outcome.rationale_iri = Some(rationale_iri);
    Ok(outcome)
}

/// The quads a merge rewrites, and the part of the report they produce.
#[derive(Default)]
struct MergePlan {
    removals: Vec<Quad>,
    insertions: Vec<Quad>,
    /// Quads the successor already holds only as reasoner inferences. Asserting
    /// them means dropping their inference tags, or the next enrichment would
    /// clear them along with the evidence they were derived from.
    promoted: Vec<Quad>,
    moved_edges: Vec<MovedEdge>,
    moved_paths: Vec<String>,
    paths_already_on_successor: Vec<String>,
}

/// Re-point the retiring component's asserted `concerns`/`constrains`/
/// `realizes` edges, in both stored orientations, to the successor, and move
/// its paths. Reasoner-materialized quads are left for enrichment to rebuild.
fn plan_merge(
    state: &AppState,
    retiring: &StatusedComponent,
    successor: &StatusedComponent,
    inferred: &HashSet<Quad>,
) -> anyhow::Result<MergePlan> {
    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let graph_ref = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let a_node = NamedNode::new(retiring.iri())?;
    let b_node = NamedNode::new(successor.iri())?;
    let mut plan = MergePlan::default();
    let mut moved: BTreeMap<(String, String), bool> = BTreeMap::new();

    for local in ["concerns", "constrains", "realizes"] {
        let forward = state.resolve_object_property(local)?;
        let mut orientations = vec![(forward.clone(), false)];
        orientations.extend(
            declared_inverses(state, &forward)?
                .into_iter()
                .map(|inverse| (inverse, true)),
        );
        for (predicate_iri, component_is_subject) in orientations {
            let predicate = NamedNode::new(&predicate_iri)?;
            let (subject, object) = if component_is_subject {
                (Some(a_node.as_ref().into()), None)
            } else {
                (None, Some(a_node.as_ref().into()))
            };
            let quads = state
                .store
                .quads_for_pattern(subject, Some(predicate.as_ref()), object, Some(graph_ref))
                .collect::<Result<Vec<_>, _>>()?;
            for quad in quads {
                if inferred.contains(&quad) {
                    continue;
                }
                let other = match (component_is_subject, &quad.subject, &quad.object) {
                    (true, _, Term::NamedNode(other)) => other.clone(),
                    (false, NamedOrBlankNode::NamedNode(other), _) => other.clone(),
                    _ => continue,
                };
                // Re-pointing an edge between the two components would make the
                // successor relate to itself; it stays, reported as not moved.
                if other == b_node {
                    continue;
                }
                let repointed = if component_is_subject {
                    Quad::new(
                        b_node.clone(),
                        predicate.clone(),
                        other.clone(),
                        graph.clone(),
                    )
                } else {
                    Quad::new(
                        other.clone(),
                        predicate.clone(),
                        b_node.clone(),
                        graph.clone(),
                    )
                };
                let added = if !state.store.contains(&repointed)? {
                    plan.insertions.push(repointed);
                    true
                } else if inferred.contains(&repointed) {
                    plan.promoted.push(repointed);
                    true
                } else {
                    false
                };
                plan.removals.push(quad);
                *moved
                    .entry((local.to_string(), other.into_string()))
                    .or_insert(false) |= added;
            }
        }
    }
    plan.moved_edges = moved
        .into_iter()
        .map(|((predicate_local, subject_iri), added)| MovedEdge {
            predicate_local,
            subject_iri,
            already_on_successor: !added,
        })
        .collect();

    let covers_path = NamedNode::new(datatype_property_iri(&state.arch_vocab, "coversPath")?)?;
    for path in &retiring.entry.covers_paths {
        let literal = Literal::new_simple_literal(path);
        plan.removals.push(Quad::new(
            a_node.clone(),
            covers_path.clone(),
            literal.clone(),
            graph.clone(),
        ));
        if successor.entry.covers_paths.contains(path) {
            plan.paths_already_on_successor.push(path.clone());
        } else {
            plan.insertions.push(Quad::new(
                b_node.clone(),
                covers_path.clone(),
                literal,
                graph.clone(),
            ));
            plan.moved_paths.push(path.clone());
        }
    }
    Ok(plan)
}

/// Every asserted object-property edge incident to `node` that the merge leaves
/// in place: not a type, not lifecycle bookkeeping, not reasoner-materialized,
/// not among `removals`.
fn not_moved_edges(
    state: &AppState,
    node: &NamedNode,
    inferred: &HashSet<Quad>,
    removals: &[Quad],
    lifecycle: &[String],
) -> anyhow::Result<Vec<IncidentEdge>> {
    let removed: HashSet<&Quad> = removals.iter().collect();
    let mut edges = Vec::new();
    for quad in incident_quads(state, node)? {
        if inferred.contains(&quad)
            || removed.contains(&quad)
            || quad.predicate.as_str() == moose::RDF_TYPE
            || lifecycle.iter().any(|iri| iri == quad.predicate.as_str())
        {
            continue;
        }
        let (other, outgoing) = match (&quad.subject, &quad.object) {
            (NamedOrBlankNode::NamedNode(subject), Term::NamedNode(object)) if subject == node => {
                (object.as_str().to_string(), true)
            }
            (NamedOrBlankNode::NamedNode(subject), Term::NamedNode(_)) => {
                (subject.as_str().to_string(), false)
            }
            _ => continue,
        };
        edges.push(IncidentEdge {
            predicate_local: local_name(quad.predicate.as_str()).to_string(),
            other_iri: other,
            outgoing,
        });
    }
    edges.sort_by(|left, right| {
        (&left.predicate_local, &left.other_iri, left.outgoing).cmp(&(
            &right.predicate_local,
            &right.other_iri,
            right.outgoing,
        ))
    });
    edges.dedup();
    Ok(edges)
}

/// Current Constraints linked to `node` by `concerns`/`constrains` in either
/// orientation, sorted by title.
fn affected_constraints(state: &AppState, node: &NamedNode) -> anyhow::Result<Vec<LinkedRecord>> {
    let mut governing = Vec::new();
    for local in ["concerns", "constrains"] {
        let forward = state.resolve_object_property(local)?;
        governing.extend(declared_inverses(state, &forward)?);
        governing.push(forward);
    }
    let constraint = state.resolve_class("Constraint")?;
    Ok(linked_records(state, node, |predicate| {
        governing.iter().any(|iri| iri == predicate)
    })?
    .into_iter()
    .filter(|record| {
        asserted_project_types(state, &NamedNode::new_unchecked(record.iri.clone()))
            .iter()
            .any(|class| is_subclass_of(&state.store, class, &constraint))
            && first_literal(&state.store, &record.iri, &state.capture.status)
                .is_none_or(|status| in_working_set(&status))
    })
    .collect())
}

/// Resolve the component to retire over every component, so a retired one is
/// named as such rather than reported unknown.
fn resolve_retiring_component<'a>(
    state: &AppState,
    all: &'a [StatusedComponent],
    query: &str,
) -> anyhow::Result<&'a StatusedComponent> {
    let matches = find_components(all.iter(), query);
    let current = matches
        .iter()
        .copied()
        .filter(|entry| entry.is_current())
        .collect::<Vec<_>>();
    // A name shared by a retired component and a current one means the current
    // one: the retired one has nothing left to retire.
    let entry = match (current.as_slice(), matches.as_slice()) {
        ([entry], _) | ([], [entry]) => *entry,
        ([], []) => anyhow::bail!(
            "unknown SystemComponent {query:?}; current components: {}",
            current_names(all)
        ),
        _ => anyhow::bail!(
            "ambiguous SystemComponent {query:?}; matches: {}; pass the IRI instead",
            describe_matches(if current.is_empty() {
                &matches
            } else {
                &current
            })
        ),
    };
    if !entry.is_current() {
        anyhow::bail!(
            "{}; it is already retired",
            describe_retired(state, all, entry)
        );
    }
    Ok(entry)
}

/// Resolve a merge target over the current components; it must be accepted
/// (or status-less legacy) and must not be the component being retired.
fn resolve_merge_target<'a>(
    state: &AppState,
    all: &'a [StatusedComponent],
    query: &str,
    retiring: &StatusedComponent,
) -> anyhow::Result<&'a StatusedComponent> {
    let entry = resolve_current_component(state, all, query)?;
    if entry.iri() == retiring.iri() {
        anyhow::bail!(
            "cannot merge SystemComponent {:?} into itself",
            retiring.entry.name
        );
    }
    if let Some(status) = entry
        .status
        .as_deref()
        .filter(|status| !status.eq_ignore_ascii_case("accepted"))
    {
        anyhow::bail!(
            "cannot merge into SystemComponent {:?} ({}): it is {status}; only an accepted \
             component can absorb another",
            entry.entry.name,
            entry.iri()
        );
    }
    Ok(entry)
}

/// Resolve `query` to one current component, naming a retired match's fate and
/// listing every match of an ambiguous name.
fn resolve_current_component<'a>(
    state: &AppState,
    all: &'a [StatusedComponent],
    query: &str,
) -> anyhow::Result<&'a StatusedComponent> {
    let matches = find_components(all.iter().filter(|entry| entry.is_current()), query);
    match matches.as_slice() {
        [entry] => Ok(*entry),
        [] => {
            if let [retired] =
                find_components(all.iter().filter(|e| !e.is_current()), query).as_slice()
            {
                anyhow::bail!(
                    "{}; it is no longer a current component",
                    describe_retired(state, all, retired)
                );
            }
            anyhow::bail!(
                "unknown SystemComponent {query:?}; current components: {}",
                current_names(all)
            )
        }
        many => anyhow::bail!(
            "ambiguous SystemComponent {query:?}; matches: {}; pass the IRI instead",
            describe_matches(many)
        ),
    }
}

fn current_names(all: &[StatusedComponent]) -> String {
    let names = all
        .iter()
        .filter(|entry| entry.is_current())
        .map(|entry| entry.entry.name.as_str())
        .collect::<Vec<_>>();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

fn describe_matches(matches: &[&StatusedComponent]) -> String {
    matches
        .iter()
        .map(|entry| format!("{:?} ({})", entry.entry.name, entry.iri()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every project-graph quad with `node` as subject or object.
fn incident_quads(state: &AppState, node: &NamedNode) -> anyhow::Result<Vec<Quad>> {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let mut quads = state
        .store
        .quads_for_pattern(Some(node.as_ref().into()), None, None, Some(graph))
        .collect::<Result<Vec<_>, _>>()?;
    quads.extend(
        state
            .store
            .quads_for_pattern(None, None, Some(node.as_ref().into()), Some(graph))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(quads)
}

/// Records linked to `node` in either direction by a predicate `keep` admits,
/// asserted or inferred, deduplicated and sorted by title.
fn linked_records(
    state: &AppState,
    node: &NamedNode,
    keep: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<LinkedRecord>> {
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for quad in incident_quads(state, node)? {
        if !keep(quad.predicate.as_str()) {
            continue;
        }
        let other = match (&quad.subject, &quad.object) {
            (NamedOrBlankNode::NamedNode(subject), Term::NamedNode(object)) if subject == node => {
                object.clone()
            }
            (NamedOrBlankNode::NamedNode(subject), Term::NamedNode(_)) => subject.clone(),
            _ => continue,
        };
        if other == *node || !seen.insert(other.as_str().to_string()) {
            continue;
        }
        let Ok(class) = require_information_record(state, &other) else {
            continue;
        };
        let iri = other.into_string();
        let title = first_literal(&state.store, &iri, &state.capture.title)
            .or_else(|| first_literal(&state.store, &iri, moose::RDFS_LABEL))
            .unwrap_or_else(|| iri.clone());
        records.push(LinkedRecord {
            kind: local_name(&class).to_string(),
            iri,
            title,
        });
    }
    records.sort_by(|left, right| (&left.title, &left.iri).cmp(&(&right.title, &right.iri)));
    Ok(records)
}

/// What [`rename_component`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOutcome {
    pub component_iri: String,
    pub old_name: String,
    pub new_name: String,
    /// `false` when the component already had that name; nothing was written.
    pub changed: bool,
}

/// Rename a current `SystemComponent` in place, keeping its IRI and every link
/// on it, for a component whose scope widened. Replaces `rdfs:label`,
/// `hasComponentName` and (when present) the capture title in one transaction.
/// Refused when another current component already has the name.
pub fn rename_component(
    state: &AppState,
    component: &str,
    new_name: &str,
) -> anyhow::Result<RenameOutcome> {
    let new_name = new_name.trim();
    anyhow::ensure!(!new_name.is_empty(), "a component name must not be empty");
    let _guard = state.lock_proposal_writes()?;
    let all = load_component_records(state)?;
    let target = resolve_current_component(state, &all, component)?;
    let mut outcome = RenameOutcome {
        component_iri: target.iri().to_string(),
        old_name: target.entry.name.clone(),
        new_name: new_name.to_string(),
        changed: false,
    };
    if target.entry.name == new_name {
        return Ok(outcome);
    }
    if let Some(clash) = all.iter().find(|entry| {
        entry.is_current()
            && entry.iri() != target.iri()
            && entry.entry.name.eq_ignore_ascii_case(new_name)
    }) {
        anyhow::bail!(
            "cannot rename {:?} to {new_name:?}: current SystemComponent {} already has that name",
            target.entry.name,
            clash.iri()
        );
    }

    let graph = GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI)?);
    let graph_ref = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let subject = NamedNode::new(target.iri())?;
    let component_name = datatype_property_iri(&state.arch_vocab, "hasComponentName")?;
    let mut removals = Vec::new();
    let mut insertions = Vec::new();
    for (predicate_iri, always) in [
        (moose::RDFS_LABEL, true),
        (component_name.as_str(), true),
        (state.capture.title.as_str(), false),
    ] {
        let predicate = NamedNode::new(predicate_iri)?;
        let existing = state
            .store
            .quads_for_pattern(
                Some(subject.as_ref().into()),
                Some(predicate.as_ref()),
                None,
                Some(graph_ref),
            )
            .collect::<Result<Vec<_>, _>>()?;
        if always || !existing.is_empty() {
            insertions.push(Quad::new(
                subject.clone(),
                predicate,
                Literal::new_simple_literal(new_name),
                graph.clone(),
            ));
        }
        removals.extend(existing);
    }
    let mut txn = state
        .store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("rename component transaction: {e}"))?;
    for quad in &removals {
        txn.remove(quad.as_ref());
    }
    txn.extend(insertions.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("rename component commit: {e}"))?;
    state.entity_index.invalidate_graph(PROJECT_KG_GRAPH_IRI);

    outcome.changed = true;
    Ok(outcome)
}

fn has_project_subject_quads(state: &AppState, subject: &NamedNode) -> anyhow::Result<bool> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            None,
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .next()
        .transpose()?
        .is_some())
}

fn validate_paths(paths: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for raw in paths {
        let path = raw.trim();
        if path.is_empty() {
            anyhow::bail!("component coverage path must not be empty");
        }
        if path.starts_with('/') {
            anyhow::bail!("component coverage path {path:?} must be repo-relative");
        }
        if path.starts_with("./") {
            anyhow::bail!("component coverage path {path:?} must not start with ./");
        }
        if path.contains('\\') {
            anyhow::bail!("component coverage path {path:?} must use forward slashes");
        }
        if path.split('/').any(|segment| segment == "..") {
            anyhow::bail!("component coverage path {path:?} must not contain .. segments");
        }
        out.insert(path.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, paths: &[&str]) -> ComponentEntry {
        ComponentEntry {
            iri: Some(format!("urn:{name}")),
            name: name.to_string(),
            covers_paths: paths.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let components = vec![
            component("broad graph", &["src/"]),
            component("graph layer", &["src/graph/"]),
        ];
        let best = best_component_for_path("src/graph/capture.rs", &components).unwrap();
        assert_eq!(best.name, "graph layer");
    }

    #[test]
    fn exact_file_matches_only_exact_path() {
        let runtime = vec![component("runtime", &["src/runtime.rs"])];
        assert_eq!(
            best_component_for_path("src/runtime.rs", &runtime)
                .unwrap()
                .name,
            "runtime"
        );
        assert!(best_component_for_path("src/runtime.rs.bak", &runtime).is_none());
        assert!(best_component_for_path("src/runtime", &runtime).is_none());

        let graph = vec![component("graph", &["src/graph"])];
        assert!(best_component_for_path("src/graph/capture.rs", &graph).is_none());
    }

    #[test]
    fn miss_returns_none() {
        let components = vec![component("graph", &["src/graph/"])];
        assert!(best_component_for_path("../moose/src/core.rs", &components).is_none());
    }

    #[test]
    fn the_whole_project_marker_covers_everything_but_yields_to_any_other_match() {
        let components = vec![
            component("project", &[COVERS_WHOLE_PROJECT]),
            component("graph", &["src/graph/"]),
        ];
        assert_eq!(
            best_component_for_path("README.md", &components)
                .unwrap()
                .name,
            "project"
        );
        assert_eq!(
            best_component_for_path("src/graph/capture.rs", &components)
                .unwrap()
                .name,
            "graph"
        );
    }

    #[test]
    fn every_enclosing_component_covers_a_path_most_specific_first() {
        let components = vec![
            component("project", &[COVERS_WHOLE_PROJECT]),
            component("map", &["map/"]),
            component("parser", &["map/src/parse.rs"]),
            component("sim", &["sim/"]),
        ];
        let names = |path: &str| -> Vec<String> {
            components_for_path(path, &components)
                .into_iter()
                .map(|component| component.name.clone())
                .collect()
        };
        assert_eq!(names("map/src/parse.rs"), ["parser", "map", "project"]);
        assert_eq!(names("map/src/lib.rs"), ["map", "project"]);
        assert_eq!(names("Cargo.toml"), ["project"]);
        let local = vec![component("map", &["map/"])];
        assert!(components_for_path("sim/src/lib.rs", &local).is_empty());
    }

    #[test]
    fn component_query_prefers_exact_identity_and_surfaces_shared_prefix_ambiguity() {
        let components = vec![
            component("Graph API", &["src/api/"]),
            component("Graph Store", &["src/graph/"]),
        ];
        let exact = resolve_component_query(&components, "urn:Graph API");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].name, "Graph API");

        let label = resolve_component_query(&components, "graph store");
        assert_eq!(label.len(), 1);
        assert_eq!(label[0].name, "Graph Store");

        let ambiguous = resolve_component_query(&components, "graph");
        assert_eq!(ambiguous.len(), 2);
        assert_eq!(ambiguous[0].name, "Graph API");
        assert_eq!(ambiguous[1].name, "Graph Store");

        assert!(resolve_component_query(&components, "urn:Graph-Stor").is_empty());
    }
}
