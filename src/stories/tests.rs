//! Cross-module Story domain regression tests.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use chrono::Utc;
use oxigraph::model::{GraphName, NamedNode, Quad};

use crate::graph::{AppState, CodeTerms, PROJECT_KG_GRAPH_IRI};

struct StoryTestState {
    state: AppState,
    dir: PathBuf,
}

impl std::ops::Deref for StoryTestState {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl Drop for StoryTestState {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn story_state(name: &str) -> StoryTestState {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-story-state-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let state = AppState::bootstrap(
        &dir,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
    )
    .unwrap();
    StoryTestState { state, dir }
}

fn record_with_status(state: &AppState, title: &str, status: &str) -> String {
    let kind = "ArchitecturalDecision";
    crate::graph::record_instance(
        state,
        &crate::graph::RecordInput {
            class_iri: state.resolve_class(kind).unwrap(),
            class_local: kind.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (state.capture.status.clone(), status.to_string()),
            ],
        },
        "story-test",
        Utc::now(),
    )
    .unwrap()
}

fn link_edge(state: &AppState, subject: &str, predicate: &str, object: &str) {
    let quad = Quad::new(
        NamedNode::new(subject).unwrap(),
        NamedNode::new(state.resolve_object_property(predicate).unwrap()).unwrap(),
        NamedNode::new(object).unwrap(),
        GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    state.store.insert(&quad).unwrap();
}

fn type_entity(state: &AppState, iri: &str, class_iri: &str) {
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(iri).unwrap(),
            NamedNode::new(moose::RDF_TYPE).unwrap(),
            NamedNode::new(class_iri).unwrap(),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}

fn type_component(state: &AppState, iri: &str) {
    type_entity(state, iri, &state.resolve_class("SystemComponent").unwrap());
}

fn add_literal(state: &AppState, iri: &str, predicate: &str, value: &str) {
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(iri).unwrap(),
            NamedNode::new(predicate).unwrap(),
            oxigraph::model::Literal::new_simple_literal(value),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
}

fn type_raw_record(state: &AppState, iri: &str, title: &str) {
    type_entity(
        state,
        iri,
        &state.resolve_class("ArchitecturalDecision").unwrap(),
    );
    add_literal(state, iri, moose::RDFS_LABEL, title);
    add_literal(state, iri, &state.capture.status, "accepted");
}

fn type_code_entity(state: &AppState, iri: &str, symbol: &str, label: &str) {
    let terms = CodeTerms::resolve(state).unwrap();
    type_entity(state, iri, &terms.code_entity_class);
    for (predicate, value) in [
        (terms.has_substrate_symbol, symbol),
        (terms.has_code_name, label),
    ] {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(iri).unwrap(),
                NamedNode::new(predicate).unwrap(),
                oxigraph::model::Literal::new_simple_literal(value),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
    }
}

fn recipe(id: &str, status: StoryStatus, beats: usize) -> StoryRecipe {
    StoryRecipe {
        id: id.to_string(),
        title: "Graph store overview".to_string(),
        schema_version: if beats == 0 { STORY_SCHEMA_VERSION } else { 2 },
        subject: Some(StoryRecipeSubject::Entity {
            iri: "https://example.test/components/graph".to_string(),
        }),
        subject_component_iri: None,
        goal: "Understand the graph store".to_string(),
        audience: "reboarding".to_string(),
        focus: StoryFocus::default(),
        curator_context: None,
        beats: (0..beats)
            .map(|index| StoryBeatRecipe {
                id: format!("beat-{index}"),
                title: format!("Beat {index}"),
                intent: [
                    StoryIntent::Purpose,
                    StoryIntent::Boundary,
                    StoryIntent::CoreCode,
                    StoryIntent::Governance,
                    StoryIntent::Risk,
                ][index]
                    .clone(),
                record_iris: vec![],
                code_symbols: vec![],
                curator_note: None,
            })
            .collect(),
        status,
        curator: "maintainer".to_string(),
        updated_at: None,
    }
}

#[test]
fn repository_round_trips_and_lists_recipe() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-repository-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let saved = repository
        .save("graph-store", recipe("graph-store", StoryStatus::Draft, 1))
        .unwrap();
    assert!(saved.updated_at.is_some());
    assert_eq!(repository.get("graph-store").unwrap(), Some(saved));
    let recipes = repository.list_recipes().unwrap();
    assert_eq!(recipes.len(), 1);
    assert!(recipes[0].beats.is_empty());
    assert_eq!(
        recipes[0].focus.emphasis,
        vec![StorySectionKind::Orientation]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_quarantines_invalid_files_and_filename_id_mismatches() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-quarantine-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    repository
        .save("valid", recipe("valid", StoryStatus::Draft, 1))
        .unwrap();
    let stories = root.join("stories");
    std::fs::write(stories.join("broken.json"), b"{not-json").unwrap();
    let mismatched = serde_json::to_vec(&recipe("body-id", StoryStatus::Draft, 1)).unwrap();
    std::fs::write(stories.join("filename-id.json"), mismatched).unwrap();

    let loaded = repository.list_recipes().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, "valid");
    assert!(repository.get("filename-id").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_rejects_future_schema_versions_without_downgrading() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-future-schema-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let mut future = recipe("future", StoryStatus::Draft, 0);
    future.schema_version = STORY_SCHEMA_VERSION + 1;
    assert!(repository
        .save("future", future.clone())
        .unwrap_err()
        .to_string()
        .contains("unsupported Story schema version"));

    std::fs::create_dir_all(root.join("stories")).unwrap();
    std::fs::write(
        root.join("stories/future.json"),
        serde_json::to_vec_pretty(&future).unwrap(),
    )
    .unwrap();
    let error = repository.get("future").unwrap_err();
    assert!(error.downcast_ref::<StoryCorrupt>().is_some());
    assert!(repository.list_recipes().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn repository_rejects_symlinked_directories_and_recipe_files() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!(
        "moosedev-story-symlink-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let project = base.join("project");
    let outside = base.join("outside");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, project.join("stories")).unwrap();
    let error = StoryRepository::new(&project).list_recipes().unwrap_err();
    assert!(error.downcast_ref::<StoryInternal>().is_some());

    let project_two = base.join("project-two");
    std::fs::create_dir_all(project_two.join("stories")).unwrap();
    let external_recipe = outside.join("linked.json");
    std::fs::write(
        &external_recipe,
        serde_json::to_vec(&recipe("linked", StoryStatus::Draft, 1)).unwrap(),
    )
    .unwrap();
    symlink(&external_recipe, project_two.join("stories/linked.json")).unwrap();
    let error = StoryRepository::new(&project_two)
        .get("linked")
        .unwrap_err();
    assert!(error.downcast_ref::<StoryInternal>().is_some());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn stale_publish_conflicts_and_current_snapshot_publishes() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-publish-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let mut validated = recipe("publish", StoryStatus::Draft, 3);
    for beat in &mut validated.beats {
        beat.record_iris
            .push("https://example.test/record".to_string());
    }
    let saved = repository.save("publish", validated).unwrap();

    let mut concurrent_disk_version = saved.clone();
    concurrent_disk_version.title = "Unvalidated concurrent title".to_string();
    let concurrent = repository.save("publish", concurrent_disk_version).unwrap();

    let conflict = repository
        .publish_checked("publish", saved.updated_at.as_deref().unwrap(), |_| Ok(()))
        .unwrap_err();
    assert!(conflict.downcast_ref::<StoryConflict>().is_some());

    let published = repository
        .publish_checked("publish", concurrent.updated_at.as_deref().unwrap(), |_| {
            Ok(())
        })
        .unwrap();
    assert_eq!(published.title, "Unvalidated concurrent title");
    assert_eq!(published.status, StoryStatus::Published);
    assert_eq!(repository.get("publish").unwrap(), Some(published));
    assert!(std::fs::read_dir(root.join("stories"))
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_writers_use_cas_and_leave_no_shared_temp_artifacts() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-writers-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for title in ["Writer A", "Writer B"] {
        let repository = StoryRepository::new(&root);
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let mut candidate = recipe("race", StoryStatus::Draft, 1);
            candidate.title = title.to_string();
            barrier.wait();
            repository.save("race", candidate)
        }));
    }
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<StoryConflict>().is_some()))
            .count(),
        1
    );
    assert!(std::fs::read_dir(root.join("stories"))
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_revisions_are_unique_and_monotonic() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-revisions-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let first = repository
        .save("revision", recipe("revision", StoryStatus::Draft, 1))
        .unwrap();
    let mut update = first.clone();
    update.title = "Second revision".to_string();
    let second = repository.save("revision", update).unwrap();
    let first_revision = revision_value(first.updated_at.as_deref().unwrap()).unwrap();
    let second_revision = revision_value(second.updated_at.as_deref().unwrap()).unwrap();
    assert!(second_revision > first_revision);
    assert_ne!(first.updated_at, second.updated_at);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_allows_only_one_published_story_per_subject() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-unique-subject-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let mut first = recipe("first", StoryStatus::Published, 3);
    let mut second = recipe("second", StoryStatus::Published, 3);
    for candidate in [&mut first, &mut second] {
        for beat in &mut candidate.beats {
            beat.record_iris
                .push("https://example.test/record".to_string());
        }
    }
    repository.save("first", first).unwrap();
    let error = repository.save("second", second).unwrap_err();
    assert!(error.downcast_ref::<StoryConflict>().is_some());
    assert_eq!(repository.list_recipes().unwrap().len(), 1);

    // Legacy/manual duplicate files are detected at selection time rather
    // than whichever filename happens to be visited first winning.
    let mut duplicate = recipe("second", StoryStatus::Published, 3);
    for beat in &mut duplicate.beats {
        beat.record_iris
            .push("https://example.test/record".to_string());
    }
    duplicate.updated_at = Some(next_revision(None));
    std::fs::write(
        root.join("stories/second.json"),
        serde_json::to_vec_pretty(&duplicate).unwrap(),
    )
    .unwrap();
    assert!(repository
        .published_for_subject(duplicate.resolved_subject().unwrap())
        .unwrap_err()
        .to_string()
        .contains("multiple published Stories"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_publish_allows_exactly_one_story_per_subject() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-concurrent-subject-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let mut saved = Vec::new();
    for id in ["first", "second"] {
        let mut draft = recipe(id, StoryStatus::Draft, 3);
        for beat in &mut draft.beats {
            beat.record_iris
                .push("https://example.test/record".to_string());
        }
        saved.push(repository.save(id, draft).unwrap());
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let outcomes = saved
        .into_iter()
        .map(|saved| {
            let repository = StoryRepository::new(&root);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                repository
                    .publish_checked(&saved.id, saved.updated_at.as_deref().unwrap(), |_| Ok(()))
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<StoryConflict>().is_some()))
            .count(),
        1
    );
    assert_eq!(
        repository
            .list_recipes()
            .unwrap()
            .iter()
            .filter(|recipe| recipe.status == StoryStatus::Published)
            .count(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_save_conflicts_before_graph_dependent_validation() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-save-check-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let repository = StoryRepository::new(&root);
    let stale = repository
        .save("checked", recipe("checked", StoryStatus::Draft, 1))
        .unwrap();
    let mut current = stale.clone();
    current.title = "Current".to_string();
    repository.save("checked", current).unwrap();

    let validation_calls = std::sync::atomic::AtomicUsize::new(0);
    let error = repository
        .save_checked("checked", stale, |_| {
            validation_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();
    assert!(error.downcast_ref::<StoryConflict>().is_some());
    assert_eq!(validation_calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inverse_concerns_is_evidence_while_proposed_knowledge_is_only_a_gap_signal() {
    let state = story_state("inverse-concerns");
    let component = "https://example.test/component";
    let accepted = record_with_status(&state, "Accepted evidence", "accepted");
    let proposed = record_with_status(&state, "Proposed evidence", "proposed");
    let proposed_component = crate::graph::record_instance(
        &state,
        &crate::graph::RecordInput {
            class_iri: state.resolve_class("SystemComponent").unwrap(),
            class_local: "SystemComponent".to_string(),
            properties: vec![
                (
                    moose::RDFS_LABEL.to_string(),
                    "Proposed component".to_string(),
                ),
                (state.capture.status.clone(), "proposed".to_string()),
            ],
        },
        "story-test",
        Utc::now(),
    )
    .unwrap();
    let predicate =
        NamedNode::new(state.resolve_object_property("isConcernedBy").unwrap()).unwrap();
    for record in [&accepted, &proposed, &proposed_component] {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(component).unwrap(),
                predicate.clone(),
                NamedNode::new(record).unwrap(),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
    }

    let records = component_records(&state, component).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].evidence.iri, accepted);
    assert_eq!(
        pending_component_record_count(&state, component).unwrap(),
        1
    );
    let index = StoryResolutionIndex::build(&state).unwrap();
    assert!(!index.components_by_iri.contains_key(&proposed_component));
    assert_eq!(
        index
            .recipe_entity(&state, &proposed_component)
            .unwrap()
            .label,
        "Proposed component"
    );
}

#[test]
fn generated_boundary_round_trips_as_intrinsic_subject_evidence() {
    let state = story_state("boundary-round-trip");
    let component_iri = "https://example.test/component/boundary";
    type_component(&state, component_iri);
    let component = StoryCandidate {
        iri: component_iri.to_string(),
        kind: "SystemComponent".to_string(),
        label: "Boundary component".to_string(),
        description: Some("Owns src/boundary/".to_string()),
    };
    let (generated, _) = generated_beats(&component, "unknown", &[], &[]);
    let draft = StoryRecipe {
        id: "boundary-round-trip".to_string(),
        title: "Boundary round trip".to_string(),
        schema_version: 2,
        subject: Some(StoryRecipeSubject::Entity {
            iri: component_iri.to_string(),
        }),
        subject_component_iri: None,
        goal: "Preserve the boundary".to_string(),
        audience: "reboarding".to_string(),
        focus: StoryFocus::default(),
        curator_context: None,
        beats: generated
            .iter()
            .map(|beat| StoryBeatRecipe {
                id: beat.id.clone(),
                title: beat.title.clone(),
                intent: beat.intent.clone(),
                record_iris: beat
                    .evidence
                    .iter()
                    .map(|evidence| evidence.iri.clone())
                    .collect(),
                code_symbols: beat
                    .code_anchors
                    .iter()
                    .map(|anchor| anchor.symbol.clone())
                    .collect(),
                curator_note: None,
            })
            .collect(),
        status: StoryStatus::Draft,
        curator: "tester".to_string(),
        updated_at: None,
    };
    let repository = StoryRepository::new(&state.dir);
    let saved = repository.save("boundary-round-trip", draft).unwrap();
    assert!(saved.beats.is_empty());
    assert!(!saved
        .focus
        .include_record_iris
        .contains(&component_iri.to_string()));
    let index = StoryResolutionIndex::build(&state).unwrap();
    let reloaded = generate_component_story(&state, &index, &component, Some(&saved)).unwrap();
    assert!(reloaded
        .evidence
        .iter()
        .any(|item| item.iri == component_iri));
    assert!(!reloaded
        .gaps
        .iter()
        .any(|gap| gap.section_kind == Some(StorySectionKind::CurrentState)));
}

#[test]
fn record_and_topic_subjects_generate_from_current_project_knowledge() {
    let state = story_state("general-story-subjects");
    let record_iri = record_with_status(&state, "Plain language Story narration", "accepted");
    let code_iri = "https://example.test/code/story_subject";
    type_code_entity(&state, code_iri, "story subject symbol", "story_subject");
    let index = StoryResolutionIndex::build(&state).unwrap();

    let catalog = story_subjects(&state, None, 1).unwrap();
    assert!(catalog.iter().any(|subject| subject.iri == record_iri));
    assert!(catalog.iter().any(|subject| subject.iri == code_iri));
    let search = story_subjects(&state, Some("plain language"), 12).unwrap();
    assert!(search
        .iter()
        .any(|subject| { subject.iri == record_iri && subject.kind == "ArchitecturalDecision" }));

    let entity = generate_story_with_index(
        &state,
        &index,
        &StoryRecipeSubject::Entity {
            iri: record_iri.clone(),
        },
        None,
    )
    .unwrap();
    assert!(matches!(
        entity.subject,
        StorySubject::Entity { ref iri, ref kind, .. }
            if iri == &record_iri && kind == "ArchitecturalDecision"
    ));
    assert!(entity
        .beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .any(|evidence| evidence.iri == record_iri));

    let topic = generate_story_with_index(
        &state,
        &index,
        &StoryRecipeSubject::Topic {
            query: "plain language narration".to_string(),
        },
        None,
    )
    .unwrap();
    assert!(matches!(topic.subject, StorySubject::Topic { .. }));
    assert!(topic
        .beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .any(|evidence| evidence.iri == record_iri));

    let curated_iri = record_with_status(&state, "Explicitly curated anchor", "accepted");
    link_edge(&state, &record_iri, "hasRationale", &curated_iri);
    let curated = StoryRecipe {
        id: "curated-topic".to_string(),
        title: "Curated topic".to_string(),
        schema_version: STORY_SCHEMA_VERSION,
        subject: Some(StoryRecipeSubject::Topic {
            query: "plain language narration".to_string(),
        }),
        subject_component_iri: None,
        goal: "Keep the selected route".to_string(),
        audience: "reboarding".to_string(),
        focus: StoryFocus {
            include_record_iris: vec![curated_iri.clone()],
            ..StoryFocus::default()
        },
        curator_context: None,
        beats: vec![StoryBeatRecipe {
            id: "governance".to_string(),
            title: "Chosen decision".to_string(),
            intent: StoryIntent::Governance,
            record_iris: vec![curated_iri.clone()],
            code_symbols: vec![],
            curator_note: None,
        }],
        status: StoryStatus::Draft,
        curator: "maintainer".to_string(),
        updated_at: None,
    };
    let curated_run = generate_story_with_index(
        &state,
        &index,
        curated.resolved_subject().unwrap(),
        Some(&curated),
    )
    .unwrap();
    assert!(curated_run
        .evidence
        .iter()
        .any(|evidence| evidence.iri == curated_iri));
}

#[test]
fn generated_evidence_requires_canonical_record_and_code_types() {
    let state = story_state("canonical-evidence-types");
    let component = "https://example.test/component/typed";
    let impostor_record = "https://example.test/component/impostor";
    let untyped_code = "https://example.test/code/untyped";
    let typed_code = "https://example.test/code/typed";
    type_component(&state, component);
    type_component(&state, impostor_record);
    link_edge(&state, impostor_record, "concerns", component);

    let accepted_record = record_with_status(&state, "Accepted record", "accepted");
    link_edge(&state, &accepted_record, "concerns", component);

    let terms = CodeTerms::resolve(&state).unwrap();
    for (entity, symbol) in [
        (untyped_code, "untyped-symbol"),
        (typed_code, "typed-symbol"),
    ] {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(entity).unwrap(),
                NamedNode::new(&terms.has_substrate_symbol).unwrap(),
                oxigraph::model::Literal::new_simple_literal(symbol),
                GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
            ))
            .unwrap();
        link_edge(&state, entity, "realizes", component);
    }
    type_code_entity(&state, typed_code, "typed-symbol", "Typed code");

    let records = component_records(&state, component).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.evidence.iri.as_str())
            .collect::<Vec<_>>(),
        vec![accepted_record.as_str()]
    );
    let code = component_code(&state, component).unwrap();
    assert_eq!(code.len(), 1);
    assert_eq!(code[0].entity_iri.as_deref(), Some(typed_code));
    assert!(!all_code_by_symbol(&state)
        .unwrap()
        .contains_key("untyped-symbol"));
}

#[test]
fn canonical_section_policy_uses_subject_lifecycle_supersession_and_kind() {
    let detail = |iri: &str, kind: &str, status: &str| StoryEvidenceDetail {
        iri: iri.to_string(),
        title: iri.to_string(),
        kind: kind.to_string(),
        status: status.to_string(),
        suppressed: false,
        description: None,
        timestamp: None,
        author: None,
        properties: vec![],
        relations: vec![],
    };
    let subject = "https://example.test/component/subject";
    assert_eq!(
        story_section_kind(
            &detail(subject, "SystemComponent", "deprecated"),
            Some(subject)
        ),
        StorySectionKind::Orientation
    );
    assert_eq!(
        story_section_kind(
            &detail(
                "https://example.test/component/dependency",
                "SystemComponent",
                "accepted"
            ),
            Some(subject)
        ),
        StorySectionKind::Implementation
    );
    assert_eq!(
        story_section_kind(
            &detail(
                "https://example.test/decision/historical",
                "ArchitecturalDecision",
                "superseded"
            ),
            Some(subject)
        ),
        StorySectionKind::Evolution
    );
    let mut successor = detail(
        "https://example.test/decision/successor",
        "ArchitecturalDecision",
        "accepted",
    );
    successor.relations.push(StoryEvidenceRelation {
        predicate: "https://example.test/supersedes".to_string(),
        label: "supersedes".to_string(),
        direction: StoryRelationDirection::Outgoing,
        target_iri: "https://example.test/decision/predecessor".to_string(),
        target_label: "Predecessor".to_string(),
        target_kind: "ArchitecturalDecision".to_string(),
    });
    assert_eq!(
        story_section_kind(&successor, Some(subject)),
        StorySectionKind::Evolution
    );
    assert_eq!(
        story_section_kind(
            &detail("https://example.test/pattern/current", "Pattern", "unknown"),
            Some(subject)
        ),
        StorySectionKind::CurrentState
    );
}

#[test]
fn rendered_citation_drives_narration_group_and_wrong_answer_revisit() {
    let state = story_state("rendered-check-section");
    let subject = "https://example.test/component/target";
    let other = "https://example.test/component/other";
    for (iri, label) in [(subject, "Target"), (other, "Other")] {
        type_component(&state, iri);
        add_literal(&state, iri, moose::RDFS_LABEL, label);
    }
    let correct = record_with_status(&state, "Current target decision", "accepted");
    let distractor = record_with_status(&state, "Other component decision", "accepted");
    link_edge(&state, &correct, "concerns", subject);
    link_edge(&state, &distractor, "concerns", other);

    let run = generate_consistent_story(
        &state,
        &StoryRecipeSubject::Entity {
            iri: subject.to_string(),
        },
        None,
        true,
    )
    .unwrap();
    let rendered_section = run
        .narrative
        .iter()
        .find(|section| {
            section
                .paragraphs
                .iter()
                .any(|paragraph| paragraph.citation_iris.contains(&correct))
        })
        .unwrap();
    assert_eq!(rendered_section.kind, StorySectionKind::CurrentState);

    let packet = build_narration_packet_for_test(&state, &run, 32_768).unwrap();
    let source = packet
        .citations_by_source
        .iter()
        .find_map(|(source, iris)| iris.contains(&correct).then_some(source))
        .unwrap();
    assert_eq!(
        packet.sections_by_source.get(source),
        Some(&rendered_section.id)
    );

    let check = run
        .checks
        .iter()
        .find(|check| {
            check
                .options
                .iter()
                .any(|option| option.label == "Current target decision")
        })
        .unwrap();
    let wrong = check
        .options
        .iter()
        .find(|option| option.label == "Other component decision")
        .unwrap();
    let result = grade_check(&state, &check.id, std::slice::from_ref(&wrong.id)).unwrap();
    assert!(!result.correct);
    assert_eq!(result.revisit_section_id, Some(rendered_section.id.clone()));
}

#[test]
fn presentation_only_generation_preserves_structure_without_issuing_grants() {
    let state = story_state("presentation-only-checks");
    let target = "https://example.test/component/target";
    let other = "https://example.test/component/other";
    type_component(&state, target);
    type_component(&state, other);
    let correct = record_with_status(&state, "Correct evidence", "accepted");
    let distractor = record_with_status(&state, "Other evidence", "accepted");
    link_edge(&state, &correct, "concerns", target);
    link_edge(&state, &distractor, "concerns", other);
    let subject = StoryRecipeSubject::Entity {
        iri: target.to_string(),
    };
    let symbolic = generate_consistent_story(&state, &subject, None, true).unwrap();
    assert_eq!(symbolic.checks.len(), 1);
    let grants_after_symbolic = state.story_checks.lock().unwrap().grants.len();

    let presentation = generate_consistent_story(&state, &subject, None, false).unwrap();
    assert!(presentation.checks.is_empty());
    assert_eq!(presentation.gaps, symbolic.gaps);
    assert_eq!(
        state.story_checks.lock().unwrap().grants.len(),
        grants_after_symbolic
    );

    let empty_component = StoryCandidate {
        iri: other.to_string(),
        kind: "SystemComponent".to_string(),
        label: "Other".to_string(),
        description: None,
    };
    let empty_index = StoryResolutionIndex::build(&state).unwrap();
    let sparse = generate_component_story(
        &state,
        &empty_index,
        &empty_component,
        Some(&StoryRecipe {
            id: "sparse".to_string(),
            title: "Sparse".to_string(),
            schema_version: STORY_SCHEMA_VERSION,
            subject: Some(StoryRecipeSubject::Entity {
                iri: other.to_string(),
            }),
            subject_component_iri: None,
            goal: "Show gaps".to_string(),
            audience: "reboarding".to_string(),
            focus: StoryFocus::default(),
            curator_context: None,
            beats: vec![StoryBeatRecipe {
                id: "boundary".to_string(),
                title: "Boundary".to_string(),
                intent: StoryIntent::Boundary,
                record_iris: vec![],
                code_symbols: vec![],
                curator_note: None,
            }],
            status: StoryStatus::Draft,
            curator: "tester".to_string(),
            updated_at: None,
        }),
    )
    .unwrap();
    assert!(sparse.checks.is_empty());
}

#[test]
fn drifted_subjects_remain_readable_but_never_issue_checks() {
    let state = story_state("drifted-subject-no-checks");
    let target = "https://example.test/component/retired";
    let other = "https://example.test/component/current";
    type_component(&state, target);
    type_component(&state, other);
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(target).unwrap(),
            NamedNode::new(&state.capture.status).unwrap(),
            oxigraph::model::Literal::new_simple_literal("deprecated"),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
    let correct = record_with_status(&state, "Retired subject evidence", "accepted");
    let distractor = record_with_status(&state, "Current subject evidence", "accepted");
    link_edge(&state, &correct, "concerns", target);
    link_edge(&state, &distractor, "concerns", other);

    let index = StoryResolutionIndex::build(&state).unwrap();
    let run = generate_component_story(
        &state,
        &index,
        &StoryCandidate {
            iri: target.to_string(),
            kind: "SystemComponent".to_string(),
            label: "Retired component".to_string(),
            description: None,
        },
        None,
    )
    .unwrap();

    assert!(run.gaps.iter().any(|gap| gap.id == "subject-drift"));
    assert!(run.gaps.iter().any(|gap| gap.id == "checks-unavailable"));
    assert!(run.checks.is_empty());
    assert!(state.story_checks.lock().unwrap().grants.is_empty());
}

#[test]
fn comprehension_check_uses_displayed_code_anchor() {
    let state = story_state("curated-code-grounding");
    let component_a = "https://example.test/component/a";
    let component_b = "https://example.test/component/b";
    let code_a = "https://example.test/code/a";
    let code_b = "https://example.test/code/b";
    type_component(&state, component_a);
    type_component(&state, component_b);
    type_code_entity(&state, code_a, "symbol-a", "Code A");
    type_code_entity(&state, code_b, "symbol-b", "Code B");
    link_edge(&state, code_a, "realizes", component_a);
    link_edge(&state, code_b, "realizes", component_b);
    let run = generate_consistent_story(
        &state,
        &StoryRecipeSubject::Entity {
            iri: component_b.to_string(),
        },
        None,
        true,
    )
    .unwrap();
    assert_eq!(run.checks.len(), 1);
    let labels = run.checks[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(labels, BTreeSet::from(["Code A", "Code B"]));
}

#[test]
fn stable_checks_never_treat_undisplayed_component_evidence_as_a_distractor() {
    let state = story_state("stable-check-complete-target-set");
    let component_iri = "https://example.test/component/target";
    let other_component_iri = "https://example.test/component/other";
    for (iri, label) in [
        (component_iri, "Target component"),
        (other_component_iri, "Other component"),
    ] {
        type_component(&state, iri);
        add_literal(&state, iri, moose::RDFS_LABEL, label);
    }
    let displayed = record_with_status(&state, "A displayed decision", "accepted");
    let undisplayed = record_with_status(&state, "B undisplayed valid decision", "accepted");
    let distractor = record_with_status(&state, "C actual distractor", "accepted");
    link_edge(&state, &displayed, "concerns", component_iri);
    link_edge(&state, &undisplayed, "concerns", component_iri);
    link_edge(&state, &distractor, "concerns", other_component_iri);

    let subject = StoryRecipeSubject::Entity {
        iri: component_iri.to_string(),
    };
    let index = StoryResolutionIndex::build(&state).unwrap();
    let mut run = generate_story_with_index(&state, &index, &subject, None).unwrap();
    for beat in &mut run.beats {
        beat.evidence.retain(|item| item.iri != undisplayed);
    }

    let prepared = prepare_checks_for_stable_story(&state, &index, &run).unwrap();
    let checks = issue_prepared_checks(&state, prepared);
    assert!(!checks.is_empty());
    assert!(checks
        .iter()
        .flat_map(|check| &check.options)
        .all(|option| option.label != "B undisplayed valid decision"));
}

#[test]
fn multiply_typed_kind_and_duplicate_symbol_selection_are_deterministic() {
    assert_eq!(
        choose_record_kind(vec![
            "InformationRecord".to_string(),
            "Constraint".to_string(),
            "ArchitecturalDecision".to_string(),
        ]),
        Some("ArchitecturalDecision".to_string())
    );
    assert_eq!(
        choose_record_kind(vec![
            "InformationRecord".to_string(),
            "Alternative".to_string(),
        ]),
        Some("Alternative".to_string())
    );

    let state = story_state("multiply-typed-story-kind");
    let record_iri = "https://example.test/record/multiply-typed";
    type_entity(
        &state,
        record_iri,
        &state.resolve_class("Alternative").unwrap(),
    );
    type_entity(
        &state,
        record_iri,
        &state.resolve_class("Consequence").unwrap(),
    );
    add_literal(&state, record_iri, moose::RDFS_LABEL, "Multiple types");
    add_literal(&state, record_iri, &state.capture.status, "accepted");
    assert_eq!(
        record_data(&state, record_iri)
            .unwrap()
            .unwrap()
            .evidence
            .kind,
        "Consequence"
    );
    let document = build_story_document(
        &state,
        &StorySubject::Entity {
            iri: record_iri.to_string(),
            kind: "Consequence".to_string(),
            label: "Multiple types".to_string(),
        },
        &[],
        None,
    )
    .unwrap();
    assert_eq!(document.evidence[0].kind, "Consequence");

    let anchor = |iri: &str, label: &str| StoryCodeAnchor {
        symbol: "same-symbol".to_string(),
        label: label.to_string(),
        entity_iri: Some(iri.to_string()),
        path: None,
        line: None,
    };
    let first = dedupe_code_anchors(vec![anchor("urn:z", "A label"), anchor("urn:a", "Z label")]);
    let reversed =
        dedupe_code_anchors(vec![anchor("urn:a", "Z label"), anchor("urn:z", "A label")]);
    assert_eq!(first, reversed);
    assert_eq!(first[0].entity_iri.as_deref(), Some("urn:a"));

    let mut forward_map = BTreeMap::new();
    insert_code_anchor(&mut forward_map, anchor("urn:z", "A label"));
    insert_code_anchor(&mut forward_map, anchor("urn:a", "Z label"));
    let mut reverse_map = BTreeMap::new();
    insert_code_anchor(&mut reverse_map, anchor("urn:a", "Z label"));
    insert_code_anchor(&mut reverse_map, anchor("urn:z", "A label"));
    assert_eq!(forward_map, reverse_map);
    assert_eq!(
        forward_map["same-symbol"].entity_iri.as_deref(),
        Some("urn:a")
    );
}

#[test]
fn v3_recipe_focus_is_disjoint_and_curator_context_is_bounded() {
    let mut valid = recipe("valid", StoryStatus::Published, 0);
    valid.focus.include_record_iris = vec!["https://example.test/record".to_string()];
    assert!(validate_recipe(&valid, true).is_ok());

    let mut overlapping = valid.clone();
    overlapping.focus.exclude_record_iris = overlapping.focus.include_record_iris.clone();
    assert!(validate_recipe(&overlapping, true)
        .unwrap_err()
        .to_string()
        .contains("both included and excluded"));

    valid.curator_context = Some("x".repeat(2_001));
    assert!(validate_recipe(&valid, true)
        .unwrap_err()
        .to_string()
        .contains("at most 2000"));
}

#[test]
fn focus_reference_limit_accepts_128_and_rejects_129_or_duplicates() {
    let allowed = (0..MAX_FOCUS_REFS)
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>();
    assert!(validate_refs("record IRI", &allowed).is_ok());

    let too_many = (0..=MAX_FOCUS_REFS)
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>();
    assert!(validate_refs("record IRI", &too_many)
        .unwrap_err()
        .to_string()
        .contains("at most 128"));

    assert!(
        validate_refs("record IRI", &["same".to_string(), "same".to_string()])
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn recipe_ids_cannot_escape_story_directory() {
    let error = validate_recipe(&recipe("../escape", StoryStatus::Draft, 1), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid Story id"));
}

#[test]
fn curator_note_stays_separate_from_narration_and_utf8_truncation_is_bounded() {
    let note = "A curator's bridge";
    let beat = make_beat(
        "boundary",
        "Boundary",
        StoryIntent::Boundary,
        vec![StoryEvidence {
            iri: "record".to_string(),
            title: "Component boundary".to_string(),
            kind: "Constraint".to_string(),
            status: "accepted".to_string(),
        }],
        vec![],
        None,
        Some(note.to_string()),
    );
    assert_eq!(beat.curator_note.as_deref(), Some(note));
    assert!(!beat.narrative.contains(note));

    let unicode = "🫎".repeat(100);
    let clipped = truncate_utf8(&unicode, 320);
    assert!(clipped.len() <= 320);
    assert!(clipped.is_char_boundary(clipped.len()));
    assert_eq!(clipped.chars().count(), 80);
}

#[test]
fn comprehension_checks_require_two_unique_options() {
    let mut one = vec![StoryCheckOption {
        id: "same".to_string(),
        label: "First".to_string(),
    }];
    assert!(!nontrivial_options(&mut one));

    let mut duplicated = vec![
        StoryCheckOption {
            id: "same".to_string(),
            label: "First".to_string(),
        },
        StoryCheckOption {
            id: "same".to_string(),
            label: "Duplicate".to_string(),
        },
    ];
    assert!(!nontrivial_options(&mut duplicated));
    assert_eq!(duplicated.len(), 1);

    let mut same_visible_label = vec![
        StoryCheckOption {
            id: "one".to_string(),
            label: "  Same   Answer ".to_string(),
        },
        StoryCheckOption {
            id: "two".to_string(),
            label: "same answer".to_string(),
        },
    ];
    assert!(!nontrivial_options(&mut same_visible_label));
    assert_eq!(same_visible_label.len(), 1);

    let mut duplicate_then_unique = vec![
        StoryCheckOption {
            id: "correct".to_string(),
            label: "Correct answer".to_string(),
        },
        StoryCheckOption {
            id: "duplicate".to_string(),
            label: " correct   ANSWER ".to_string(),
        },
        StoryCheckOption {
            id: "first-unique".to_string(),
            label: "First distractor".to_string(),
        },
        StoryCheckOption {
            id: "second-unique".to_string(),
            label: "Second distractor".to_string(),
        },
        StoryCheckOption {
            id: "third-unique".to_string(),
            label: "Third distractor".to_string(),
        },
    ];
    assert!(nontrivial_options(&mut duplicate_then_unique));
    assert_eq!(duplicate_then_unique.len(), MAX_CHECK_OPTIONS);
    assert_eq!(duplicate_then_unique[0].id, "correct");
    assert_eq!(duplicate_then_unique[1].id, "first-unique");
    assert_eq!(duplicate_then_unique[2].id, "second-unique");

    let mut two = vec![
        StoryCheckOption {
            id: "a".to_string(),
            label: "A".to_string(),
        },
        StoryCheckOption {
            id: "b".to_string(),
            label: "B".to_string(),
        },
    ];
    assert!(nontrivial_options(&mut two));
}

#[test]
fn check_options_reject_mixed_truth_labels_and_cap_after_deduplication() {
    let fact = |id: &str, label: &str, matches_target| CheckOptionFact {
        id: id.to_string(),
        label: label.to_string(),
        matches_target,
    };
    let displayed = vec!["correct".to_string()];
    let (_, options) = unambiguous_check_options(
        &displayed,
        vec![
            fact("correct", "Correct", true),
            fact("also-valid", "Shared answer", true),
            fact("looks-valid", " shared   ANSWER ", false),
            fact("duplicate-a", "Duplicate", false),
            fact("duplicate-b", " duplicate ", false),
            fact("first", "First distractor", false),
            fact("second", "Second distractor", false),
            fact("third", "Third distractor", false),
        ],
    )
    .expect("two unambiguous distractor labels remain");
    assert_eq!(
        options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        vec!["correct", "duplicate-a", "first"]
    );
    assert!(!options.iter().any(|option| option.label == "Shared answer"));

    let ambiguous_only = unambiguous_check_options(
        &displayed,
        vec![
            fact("correct", "Correct", true),
            fact("true-shared", "Shared", true),
            fact("false-shared", " shared ", false),
        ],
    );
    assert!(ambiguous_only.is_none());

    let mixed_correct_label = unambiguous_check_options(
        &displayed,
        vec![
            fact("correct", "Shared", true),
            fact("looks-correct", " shared ", false),
            fact("other", "Other", false),
        ],
    );
    assert!(mixed_correct_label.is_none());
}

#[test]
fn opaque_option_order_has_no_fixed_answer_position() {
    let source = || {
        vec![
            StoryCheckOption {
                id: "correct-entity".to_string(),
                label: "Correct".to_string(),
            },
            StoryCheckOption {
                id: "other-entity".to_string(),
                label: "Other".to_string(),
            },
        ]
    };
    let mut first_keys = [1_u128, 40, 2, 10].into_iter().map(uuid::Uuid::from_u128);
    let (correct_token, other_first, option_entities) =
        opaque_options(source(), "correct-entity", || first_keys.next().unwrap());
    assert_eq!(other_first[1].id, correct_token);
    assert_eq!(option_entities[&correct_token], "correct-entity");
    assert!(other_first
        .iter()
        .all(|option| { option.id != "correct-entity" && option.id != "other-entity" }));

    let mut second_keys = [1_u128, 10, 2, 40].into_iter().map(uuid::Uuid::from_u128);
    let (correct_token, correct_first, _) =
        opaque_options(source(), "correct-entity", || second_keys.next().unwrap());
    assert_eq!(correct_first[0].id, correct_token);
}

#[test]
fn check_grants_are_project_scoped_and_distinguish_error_states() {
    let state = story_state("check-registry-a");
    let other_state = story_state("check-registry-b");
    let allowed = "allowed-token".to_string();
    let handle = register_check(
        &state,
        CheckGrant {
            kind: CheckKind::Concerns,
            component_iri: "https://example.test/component".to_string(),
            section_id: "orientation".to_string(),
            correct_option_token: allowed.clone(),
            option_entities: BTreeMap::from([(
                allowed.clone(),
                "https://example.test/record".to_string(),
            )]),
            correct_entity_iri: "https://example.test/record".to_string(),
            correct_kind: None,
            subject_entity: None,
            expires_at: std::time::Instant::now() + CHECK_TTL,
        },
    );
    assert_eq!(
        grade_check(&other_state, &handle, std::slice::from_ref(&allowed))
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Unknown)
    );
    assert_eq!(
        grade_check(&state, &handle, &["foreign".to_string()])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::ForeignOption)
    );
    assert_eq!(
        grade_check(&state, &handle, &[allowed])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Stale)
    );

    let expired = register_check(
        &state,
        CheckGrant {
            kind: CheckKind::Concerns,
            component_iri: "https://example.test/component".to_string(),
            section_id: "orientation".to_string(),
            correct_option_token: "token".to_string(),
            option_entities: BTreeMap::from([(
                "token".to_string(),
                "https://example.test/record".to_string(),
            )]),
            correct_entity_iri: "https://example.test/record".to_string(),
            correct_kind: None,
            subject_entity: None,
            expires_at: std::time::Instant::now(),
        },
    );
    assert_eq!(
        grade_check(&state, &expired, &[])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Expired)
    );
}

#[test]
fn grading_rejects_retired_subjects_and_newly_valid_distractors() {
    let state = story_state("check-current-graph");
    let component = "https://example.test/component/check";
    type_component(&state, component);
    let correct = record_with_status(&state, "Correct", "accepted");
    let distractor = record_with_status(&state, "Distractor", "accepted");
    link_edge(&state, &correct, "concerns", component);

    let grant = || CheckGrant {
        kind: CheckKind::Concerns,
        component_iri: component.to_string(),
        section_id: "orientation".to_string(),
        correct_option_token: "correct-token".to_string(),
        option_entities: BTreeMap::from([
            ("correct-token".to_string(), correct.clone()),
            ("distractor-token".to_string(), distractor.clone()),
        ]),
        correct_entity_iri: correct.clone(),
        correct_kind: None,
        subject_entity: None,
        expires_at: std::time::Instant::now() + CHECK_TTL,
    };
    let distractor_handle = register_check(&state, grant());
    assert!(
        grade_check(&state, &distractor_handle, &["correct-token".to_string()])
            .unwrap()
            .correct
    );
    link_edge(&state, &distractor, "concerns", component);
    assert_eq!(
        grade_check(&state, &distractor_handle, &["correct-token".to_string()])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Stale)
    );

    let other_component = "https://example.test/component/retired";
    type_component(&state, other_component);
    let other_record = record_with_status(&state, "Other correct", "accepted");
    link_edge(&state, &other_record, "concerns", other_component);
    let retired_handle = register_check(
        &state,
        CheckGrant {
            kind: CheckKind::Concerns,
            component_iri: other_component.to_string(),
            section_id: "orientation".to_string(),
            correct_option_token: "only-token".to_string(),
            option_entities: BTreeMap::from([("only-token".to_string(), other_record.clone())]),
            correct_entity_iri: other_record,
            correct_kind: None,
            subject_entity: None,
            expires_at: std::time::Instant::now() + CHECK_TTL,
        },
    );
    state
        .store
        .insert(&Quad::new(
            NamedNode::new(other_component).unwrap(),
            NamedNode::new(&state.capture.status).unwrap(),
            oxigraph::model::Literal::new_simple_literal("deprecated"),
            GraphName::NamedNode(NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap()),
        ))
        .unwrap();
    assert_eq!(
        grade_check(&state, &retired_handle, &["only-token".to_string()])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Stale)
    );
}

#[test]
fn capacity_evicted_check_reports_unavailable() {
    let state = story_state("check-capacity");
    let grant = || CheckGrant {
        kind: CheckKind::Concerns,
        component_iri: "https://example.test/component".to_string(),
        section_id: "orientation".to_string(),
        correct_option_token: "token".to_string(),
        option_entities: BTreeMap::from([(
            "token".to_string(),
            "https://example.test/record".to_string(),
        )]),
        correct_entity_iri: "https://example.test/record".to_string(),
        correct_kind: None,
        subject_entity: None,
        expires_at: std::time::Instant::now() + CHECK_TTL,
    };
    let evicted = register_check(&state, grant());
    for _ in 0..MAX_CHECK_GRANTS {
        register_check(&state, grant());
    }
    assert_eq!(
        grade_check(&state, &evicted, &["token".to_string()])
            .unwrap_err()
            .downcast_ref::<StoryCheckError>(),
        Some(&StoryCheckError::Evicted)
    );
}

#[test]
fn typed_closure_is_depth_bounded_deterministic_and_honors_natural_exclusions() {
    let state = story_state("typed-story-closure");
    let subject = "https://example.test/component/root";
    let other_component = "https://example.test/component/other";
    type_component(&state, subject);
    type_component(&state, other_component);
    let direct = record_with_status(&state, "Direct decision", "accepted");
    let rationale = record_with_status(&state, "Decision rationale", "accepted");
    let unrelated = record_with_status(&state, "Other component decision", "accepted");
    link_edge(&state, &direct, "concerns", subject);
    link_edge(&state, &direct, "hasRationale", &rationale);
    link_edge(&state, &direct, "concerns", other_component);
    link_edge(&state, &unrelated, "concerns", other_component);

    let subject_selector = StoryRecipeSubject::Entity {
        iri: subject.to_string(),
    };
    let first =
        story_subject_closure_iris_with_priority(&state, &subject_selector, &BTreeSet::new())
            .unwrap();
    let second =
        story_subject_closure_iris_with_priority(&state, &subject_selector, &BTreeSet::new())
            .unwrap();
    assert_eq!(first, second);
    assert!(first.contains(subject));
    assert!(first.contains(&direct));
    assert!(first.contains(&rationale));
    assert!(!first.contains(&unrelated));

    let mut focused = recipe("closure", StoryStatus::Draft, 0);
    focused.subject = Some(subject_selector);
    focused.focus.exclude_record_iris = vec![rationale.clone()];
    focused.focus.emphasis = vec![StorySectionKind::CurrentState];
    let index = StoryResolutionIndex::build(&state).unwrap();
    let run = generate_story_with_index(
        &state,
        &index,
        focused.resolved_subject().unwrap(),
        Some(&focused),
    )
    .unwrap();
    assert!(run
        .evidence
        .iter()
        .any(|item| item.iri == rationale && item.suppressed));
    assert!(run
        .evidence
        .iter()
        .find(|item| item.iri == direct)
        .unwrap()
        .properties
        .iter()
        .any(|property| property.label == "hasLifecycleStatus"));
    assert!(run
        .narrative
        .iter()
        .flat_map(|section| &section.paragraphs)
        .all(|paragraph| !paragraph.citation_iris.contains(&rationale)));
    assert_eq!(run.narrative[0].kind, StorySectionKind::CurrentState);

    focused.focus.exclude_record_iris = vec![unrelated];
    assert!(recipe_has_drift(&state, &focused).unwrap());
}

#[test]
fn exact_subject_closure_includes_typed_direct_neighbors_for_other_predicates() {
    let state = story_state("typed-direct-neighbor");
    let subject = "https://example.test/component/root";
    let dependency = "https://example.test/role/dependency";
    type_component(&state, subject);
    type_entity(
        &state,
        dependency,
        "https://trivyn.io/ontologies/software/code#CodeRole",
    );
    link_edge(&state, subject, "playsRole", dependency);

    let closure = story_subject_closure_iris_with_priority(
        &state,
        &StoryRecipeSubject::Entity {
            iri: subject.to_string(),
        },
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(closure.contains(subject));
    assert!(closure.contains(dependency));
}

#[test]
fn curated_deep_include_reserves_capacity_without_becoming_a_root() {
    let state = story_state("closure-priority-pressure");
    let subject = "https://example.test/component/root";
    type_component(&state, subject);
    let priority = "https://example.test/record/priority";
    type_raw_record(&state, priority, "Curated deep rationale");

    for index in 0..(MAX_STORY_ENTITIES + 1) {
        let iri = format!("https://example.test/record/direct-{index:04}");
        type_raw_record(&state, &iri, &format!("Direct {index:04}"));
        link_edge(&state, &iri, "concerns", subject);
        if index == MAX_STORY_ENTITIES {
            link_edge(&state, &iri, "hasRationale", priority);
        }
    }

    let selected = story_subject_closure_iris_with_priority(
        &state,
        &StoryRecipeSubject::Entity {
            iri: subject.to_string(),
        },
        &BTreeSet::from([priority.to_string()]),
    )
    .unwrap();
    assert_eq!(selected.len(), MAX_STORY_ENTITIES);
    assert!(selected.contains(subject));
    assert!(selected.contains(priority));
}

#[test]
fn code_symbol_exclusion_only_suppresses_matching_code_entities() {
    let state = story_state("exact-code-exclusion");
    let subject = "https://example.test/component/root";
    type_component(&state, subject);
    let record = record_with_status(&state, "Decision with coincidental text", "accepted");
    add_literal(&state, &record, &state.capture.description, "shared-symbol");
    link_edge(&state, &record, "concerns", subject);
    let code = "https://example.test/code/shared";
    type_code_entity(&state, code, "shared-symbol", "Shared code");
    link_edge(&state, code, "realizes", subject);

    let mut focused = recipe("exact-code-exclusion", StoryStatus::Draft, 0);
    focused.subject = Some(StoryRecipeSubject::Entity {
        iri: subject.to_string(),
    });
    focused.focus.exclude_code_symbols = vec!["shared-symbol".to_string()];
    let index = StoryResolutionIndex::build(&state).unwrap();
    let run = generate_story_with_index(
        &state,
        &index,
        focused.resolved_subject().unwrap(),
        Some(&focused),
    )
    .unwrap();
    assert!(
        !run.evidence
            .iter()
            .find(|item| item.iri == record)
            .unwrap()
            .suppressed
    );
    assert!(
        run.evidence
            .iter()
            .find(|item| item.iri == code)
            .unwrap()
            .suppressed
    );
    assert!(run.coverage.dossier_bytes > 0);
    assert!(run
        .coverage
        .subject_families
        .contains(&"ArchitecturalDecision".to_string()));
    assert_eq!(
        run.coverage.outline_sections,
        run.narrative
            .iter()
            .map(|section| section.kind.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn timeline_orders_rfc3339_instants_and_puts_invalid_dates_last() {
    let detail = |iri: &str, timestamp: Option<&str>| StoryEvidenceDetail {
        iri: iri.to_string(),
        title: iri.to_string(),
        kind: "ArchitecturalDecision".to_string(),
        status: "accepted".to_string(),
        suppressed: false,
        description: None,
        timestamp: timestamp.map(str::to_string),
        author: None,
        properties: vec![],
        relations: vec![],
    };
    let timeline = build_timeline(&[
        detail("later-offset", Some("2026-08-01T02:00:00+02:00")),
        detail("earlier", Some("2026-07-31T23:30:00Z")),
        detail("invalid", Some("yesterday")),
        detail("undated", None),
    ]);
    assert_eq!(
        timeline
            .iter()
            .map(|event| event.evidence_iri.as_str())
            .collect::<Vec<_>>(),
        vec!["earlier", "later-offset", "invalid", "undated"]
    );
}

#[test]
fn legacy_recipe_reads_losslessly_and_writes_v3() {
    let root = std::env::temp_dir().join(format!(
        "moosedev-story-v3-migration-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(root.join("stories")).unwrap();
    let raw = serde_json::json!({
        "id": "legacy",
        "title": "Legacy story",
        "schema_version": 2,
        "subject": {"type":"entity", "iri":"https://example.test/component"},
        "goal": "Preserve curation",
        "audience": "reboarding",
        "beats": [{
            "id":"governance", "title":"Governance", "intent":"governance",
            "record_iris":["https://example.test/decision"], "code_symbols":[],
            "curator_note":"Explain the trade-off"
        }],
        "status":"draft", "curator":"maintainer", "updated_at": null
    });
    std::fs::write(
        root.join("stories/legacy.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
    let repository = StoryRepository::new(&root);
    let migrated = repository.get("legacy").unwrap().unwrap();
    assert_eq!(migrated.schema_version, 3);
    assert_eq!(
        migrated.focus.include_record_iris,
        vec!["https://example.test/decision"]
    );
    assert_eq!(
        migrated.curator_context.as_deref(),
        Some("Governance: Explain the trade-off")
    );
    let saved = repository.save("legacy", migrated).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("stories/legacy.json")).unwrap()).unwrap();
    assert_eq!(written["schema_version"], 3);
    assert!(written.get("beats").is_none());
    assert_eq!(
        saved.curator_context.as_deref(),
        Some("Governance: Explain the trade-off")
    );
    let mut long_raw = raw.clone();
    long_raw["id"] = serde_json::json!("legacy-long");
    long_raw["beats"][0]["curator_note"] = serde_json::json!("x".repeat(2_001));
    std::fs::write(
        root.join("stories/legacy-long.json"),
        serde_json::to_vec_pretty(&long_raw).unwrap(),
    )
    .unwrap();
    let long = repository.get("legacy-long").unwrap().unwrap();
    assert!(long.curator_context.as_ref().unwrap().chars().count() > 2_000);
    assert!(repository
        .save("legacy-long", long)
        .unwrap_err()
        .to_string()
        .contains("at most 2000"));
    std::fs::remove_dir_all(root).unwrap();
}

fn symbolic_run_for_narration() -> StoryRun {
    let evidence = vec![
        StoryEvidenceDetail {
            iri: "https://example.test/requirement".to_string(),
            title: "Readable stories".to_string(),
            kind: "Requirement".to_string(),
            status: "accepted".to_string(),
            suppressed: false,
            description: Some("Stories explain the project in plain language.".to_string()),
            timestamp: Some("2026-08-01T00:00:00Z".to_string()),
            author: Some("maintainer".to_string()),
            properties: vec![],
            relations: vec![],
        },
        StoryEvidenceDetail {
            iri: "https://example.test/decision".to_string(),
            title: "Ground narration".to_string(),
            kind: "ArchitecturalDecision".to_string(),
            status: "accepted".to_string(),
            suppressed: false,
            description: Some("Narration cites deterministic evidence.".to_string()),
            timestamp: Some("2026-08-02T00:00:00Z".to_string()),
            author: Some("maintainer".to_string()),
            properties: vec![],
            relations: vec![],
        },
    ];
    let narrative = vec![
        StoryNarrativeSection {
            id: "orientation".to_string(),
            kind: StorySectionKind::Orientation,
            title: "Orientation".to_string(),
            paragraphs: vec![StoryParagraph {
                text: "Symbolic orientation.".to_string(),
                citation_iris: vec![evidence[0].iri.clone()],
            }],
        },
        StoryNarrativeSection {
            id: "evolution".to_string(),
            kind: StorySectionKind::Evolution,
            title: "Evolution".to_string(),
            paragraphs: vec![StoryParagraph {
                text: "Symbolic evolution.".to_string(),
                citation_iris: vec![evidence[1].iri.clone()],
            }],
        },
    ];
    StoryRun {
        schema_version: STORY_SCHEMA_VERSION,
        recipe_id: None,
        trust_state: StoryTrustState::Generated,
        narration_mode: NarrationMode::Symbolic,
        narration_strategy: NarrationStrategy::Symbolic,
        narration_outcome: NarrationOutcome::NotRequested,
        narration_failure_reason: None,
        narration_coverage: None,
        title: "A Story".to_string(),
        subject: StorySubject::Entity {
            iri: "https://example.test/component".to_string(),
            kind: "SystemComponent".to_string(),
            label: "Component".to_string(),
        },
        goal: "Understand it".to_string(),
        curator_context: Some("Human guidance stays separate.".to_string()),
        brief: StoryParagraph {
            text: "Brief.".to_string(),
            citation_iris: vec![evidence[0].iri.clone()],
        },
        narrative,
        timeline: vec![],
        evidence,
        code_anchors: vec![],
        coverage: StoryCoverage {
            entity_count: 2,
            current_count: 2,
            ..StoryCoverage::default()
        },
        gaps: vec![],
        checks: vec![],
        beats: vec![],
    }
}

#[test]
fn valid_narration_replaces_only_article_paragraphs() {
    let state = story_state("valid-packet-narration");
    let run = symbolic_run_for_narration();
    let citations = build_narration_packet_for_test(&state, &run, 32_768)
        .unwrap()
        .citations_by_source;
    let raw = r#"{"paragraphs":[{"section_id":"orientation","text":"The project requires readable explanations.","source_ids":["s1"]},{"section_id":"evolution","text":"It later grounded narration in typed evidence.","source_ids":["s2"]}]}"#;
    let narrative = apply_packet_response_for_test(&run, raw, &citations).unwrap();
    assert_eq!(
        narrative[0].paragraphs[0].text,
        "The project requires readable explanations."
    );
    assert_eq!(
        narrative[1].paragraphs[0].text,
        "It later grounded narration in typed evidence."
    );
    assert_eq!(narrative[0].kind, StorySectionKind::Orientation);
}

#[test]
fn narration_accepts_provider_section_arrays_without_weakening_citations() {
    let run = symbolic_run_for_narration();
    let citations = BTreeMap::from([
        (
            "s1".to_string(),
            vec!["https://example.test/requirement".to_string()],
        ),
        (
            "s2".to_string(),
            vec!["https://example.test/decision".to_string()],
        ),
    ]);
    let raw = r#"[{"section_id":"orientation","title":"Orientation","paragraph":"The project requires readable explanations.","source_ids":["s1"]},{"section_id":"evolution","title":"Evolution","paragraph":"It later grounded narration in typed evidence.","source_ids":["s2"]}]"#;
    let narrative = apply_packet_response_for_test(&run, raw, &citations).unwrap();
    assert_eq!(narrative[0].paragraphs[0].citation_iris, citations["s1"]);
    assert_eq!(narrative[1].paragraphs[0].citation_iris, citations["s2"]);

    let plural = r#"[{"section_id":"orientation","title":"Orientation","paragraphs":["The project requires readable explanations."],"source_ids":["s1"]},{"section_id":"evolution","title":"Evolution","paragraphs":["It later grounded narration.","The evidence remained deterministic."],"source_ids":["s2"]}]"#;
    let plural_narrative = apply_packet_response_for_test(&run, plural, &citations).unwrap();
    assert_eq!(plural_narrative[1].paragraphs.len(), 2);

    let cross_section = r#"[{"section_id":"orientation","title":"Orientation","paragraph":"Wrong evidence.","source_ids":["s2"]},{"section_id":"evolution","title":"Evolution","paragraph":"Also wrong.","source_ids":["s1"]}]"#;
    assert_eq!(
        apply_packet_response_for_test(&run, cross_section, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
    let ambiguous = r#"[{"section_id":"orientation","title":"Orientation","paragraph":"One.","paragraphs":["Two."],"source_ids":["s1"]},{"section_id":"evolution","title":"Evolution","paragraph":"Evolution.","source_ids":["s2"]}]"#;
    assert_eq!(
        apply_packet_response_for_test(&run, ambiguous, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
}

#[test]
fn packet_source_ids_expand_to_complete_public_evidence_citations() {
    let run = symbolic_run_for_narration();
    let citations = BTreeMap::from([
        (
            "source-1".to_string(),
            vec!["https://example.test/requirement".to_string()],
        ),
        (
            "source-2".to_string(),
            vec!["https://example.test/decision".to_string()],
        ),
    ]);
    let raw = r#"{"paragraphs":[{"section_id":"orientation","text":"Readable explanations.","source_ids":["source-1"]},{"section_id":"evolution","text":"Grounded narration.","source_ids":["source-2"]}]}"#;
    let narrative = apply_packet_response_for_test(&run, raw, &citations).unwrap();
    assert_eq!(
        narrative[0].paragraphs[0].citation_iris,
        vec!["https://example.test/requirement"]
    );
    assert_eq!(
        narrative[1].paragraphs[0].citation_iris,
        vec!["https://example.test/decision"]
    );

    let missing_source = r#"{"paragraphs":[{"section_id":"orientation","text":"Readable explanations.","source_ids":["source-1"]},{"section_id":"evolution","text":"Grounded narration.","source_ids":["source-1"]}]}"#;
    assert_eq!(
        apply_packet_response_for_test(&run, missing_source, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
}

#[test]
fn narration_repairs_json_but_rejects_unknown_or_incomplete_citations() {
    let run = symbolic_run_for_narration();
    let citations = BTreeMap::from([
        (
            "s1".to_string(),
            vec!["https://example.test/requirement".to_string()],
        ),
        (
            "s2".to_string(),
            vec!["https://example.test/decision".to_string()],
        ),
    ]);
    let repaired = r#"{'paragraphs':[{'section_id':'orientation','text':'Readable explanations.','source_ids':['s1'],},{'section_id':'evolution','text':'Grounded narration.','source_ids':['s2']}],}"#;
    assert!(apply_packet_response_for_test(&run, repaired, &citations).is_ok());
    let fenced_with_trailing_quote = r#"```json
{"paragraphs":[{"section_id":"orientation","text":"Readable explanations.","source_ids":["s1"]},{"section_id":"evolution","text":"Grounded narration.","source_ids":["s2"]}]}
"
```"#;
    let fenced = apply_packet_response_for_test(&run, fenced_with_trailing_quote, &citations);
    assert!(fenced.is_ok(), "{fenced:?}");
    let missing = r#"{"paragraphs":[{"section_id":"orientation","text":"Only one.","source_ids":["s1"]},{"section_id":"evolution","text":"Still one.","source_ids":["s1"]}]}"#;
    assert_eq!(
        apply_packet_response_for_test(&run, missing, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
    let invented = r#"{"paragraphs":[{"section_id":"orientation","text":"Invented.","source_ids":["invented"]},{"section_id":"evolution","text":"Grounded.","source_ids":["s2"]}]}"#;
    assert_eq!(
        apply_packet_response_for_test(&run, invented, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
    let wrong_section = r#"{"paragraphs":[{"section_id":"orientation","text":"Wrong source section.","source_ids":["s2"]},{"section_id":"evolution","text":"Grounded.","source_ids":["s1"]}]}"#;
    assert_eq!(
        apply_packet_response_for_test(&run, wrong_section, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
    let leaked_marker = r#"{"paragraphs":[{"section_id":"orientation","text":"Readable explanations [s1].","source_ids":["s1"]},{"section_id":"evolution","text":"Grounded narration.","source_ids":["s2"]}]}"#;
    assert_eq!(
        apply_packet_response_for_test(&run, leaked_marker, &citations),
        Err(NarrationFailureReason::SchemaMismatch)
    );
}

#[test]
fn narration_packet_reserves_a_chronological_spine_under_budget_pressure() {
    let state = story_state("narration-chronology-spine");
    let mut run = symbolic_run_for_narration();
    let subject_iri = match &run.subject {
        StorySubject::Entity { iri, .. } => iri.clone(),
        StorySubject::Topic { .. } => unreachable!(),
    };
    for index in 0..20 {
        let iri = format!("https://example.test/milestone/{index:02}");
        run.evidence.push(StoryEvidenceDetail {
            iri: iri.clone(),
            title: format!("Milestone {index:02}"),
            kind: "ArchitecturalDecision".to_string(),
            status: "accepted".to_string(),
            suppressed: false,
            description: Some(format!("{} {index:02}", "chronology".repeat(250))),
            timestamp: Some(format!("2026-07-{:02}T00:00:00Z", index + 1)),
            author: None,
            properties: vec![],
            relations: vec![StoryEvidenceRelation {
                predicate: "https://example.test/concerns".to_string(),
                label: "concerns".to_string(),
                direction: StoryRelationDirection::Outgoing,
                target_iri: subject_iri.clone(),
                target_label: "Component".to_string(),
                target_kind: "SystemComponent".to_string(),
            }],
        });
        run.timeline.push(StoryTimelineEvent {
            id: format!("event-{index:02}"),
            title: format!("Milestone {index:02}"),
            kind: "ArchitecturalDecision".to_string(),
            status: "accepted".to_string(),
            timestamp: Some(format!("2026-07-{:02}T00:00:00Z", index + 1)),
            evidence_iri: iri,
            relation: None,
            predecessor_iris: vec![],
            successor_iris: vec![],
            rationale_iris: vec![],
        });
    }

    let packet = build_narration_packet_for_test(&state, &run, 32_768).unwrap();
    assert!(packet.coverage.truncated);
    assert!(packet.prompt.contains("Milestone 00"));
    assert!(packet.prompt.contains("Milestone 19"));
    assert!(packet.prompt.contains("Milestone 07"));
}

#[test]
fn narration_chronology_prefers_subject_history_over_incidental_component_features() {
    let state = story_state("narration-subject-history");
    let mut run = symbolic_run_for_narration();
    let (subject_iri, subject_label) = match &mut run.subject {
        StorySubject::Entity { iri, label, .. } => {
            *label = "HTTP API".to_string();
            (iri.clone(), label.clone())
        }
        StorySubject::Topic { .. } => unreachable!(),
    };
    for index in 0..20 {
        let iri = format!("https://example.test/incidental/{index:02}");
        run.evidence.push(StoryEvidenceDetail {
            iri: iri.clone(),
            title: format!("Incidental feature {index:02}"),
            kind: "ArchitecturalDecision".to_string(),
            status: "accepted".to_string(),
            suppressed: false,
            description: Some("feature detail ".repeat(150)),
            timestamp: Some(format!("2026-07-{:02}T00:00:00Z", index + 1)),
            author: None,
            properties: vec![],
            relations: vec![StoryEvidenceRelation {
                predicate: "https://example.test/concerns".to_string(),
                label: "concerns".to_string(),
                direction: StoryRelationDirection::Outgoing,
                target_iri: subject_iri.clone(),
                target_label: subject_label.clone(),
                target_kind: "SystemComponent".to_string(),
            }],
        });
        run.timeline.push(StoryTimelineEvent {
            id: format!("incidental-{index:02}"),
            title: format!("Incidental feature {index:02}"),
            kind: "ArchitecturalDecision".to_string(),
            status: "accepted".to_string(),
            timestamp: Some(format!("2026-07-{:02}T00:00:00Z", index + 1)),
            evidence_iri: iri,
            relation: None,
            predecessor_iris: vec![],
            successor_iris: vec![],
            rationale_iris: vec![],
        });
    }
    let history_iris = (0..4)
        .map(|index| format!("https://example.test/http-history/{index}"))
        .collect::<Vec<_>>();
    for (index, iri) in history_iris.iter().enumerate() {
        let title = format!("HTTP API discovery generation {index}");
        run.evidence.push(StoryEvidenceDetail {
            iri: iri.clone(),
            title: title.clone(),
            kind: "ArchitecturalDecision".to_string(),
            status: if index == 3 { "accepted" } else { "superseded" }.to_string(),
            suppressed: false,
            description: Some(format!("{title}. {}", "identity verification ".repeat(40))),
            timestamp: Some(format!("2026-06-{:02}T00:00:00Z", index + 1)),
            author: None,
            properties: vec![],
            relations: vec![StoryEvidenceRelation {
                predicate: "https://example.test/concerns".to_string(),
                label: "concerns".to_string(),
                direction: StoryRelationDirection::Outgoing,
                target_iri: subject_iri.clone(),
                target_label: subject_label.clone(),
                target_kind: "SystemComponent".to_string(),
            }],
        });
        run.timeline.push(StoryTimelineEvent {
            id: format!("history-{index}"),
            title,
            kind: "ArchitecturalDecision".to_string(),
            status: if index == 3 { "accepted" } else { "superseded" }.to_string(),
            timestamp: Some(format!("2026-06-{:02}T00:00:00Z", index + 1)),
            evidence_iri: iri.clone(),
            relation: Some("isSupersededBy".to_string()),
            predecessor_iris: index
                .checked_sub(1)
                .map(|previous| vec![history_iris[previous].clone()])
                .unwrap_or_default(),
            successor_iris: history_iris.get(index + 1).cloned().into_iter().collect(),
            rationale_iris: vec![],
        });
    }
    run.timeline
        .sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

    let packet = build_narration_packet_for_test(&state, &run, 32_768).unwrap();
    assert!(packet.coverage.truncated);
    for index in 0..4 {
        assert!(
            packet
                .prompt
                .contains(&format!("HTTP API discovery generation {index}")),
            "missing lifecycle generation {index}"
        );
    }
}

#[test]
fn narration_prompt_uses_complete_evidence_and_excludes_curator_context() {
    let state = story_state("narration-prompt-completeness");
    let mut run = symbolic_run_for_narration();
    let full = "x".repeat(4_000);
    run.evidence[0].description = Some(full.clone());
    run.evidence[0].properties = vec![
        StoryLiteralProperty {
            predicate: state.capture.description.clone(),
            label: "hasDescription".to_string(),
            value: full.clone(),
        },
        StoryLiteralProperty {
            predicate: state.capture.description.clone(),
            label: "hasDescription".to_string(),
            value: "A second canonical description remains evidence.".to_string(),
        },
        StoryLiteralProperty {
            predicate: "https://foreign.example/hasDescription".to_string(),
            label: "hasDescription".to_string(),
            value: full.clone(),
        },
        StoryLiteralProperty {
            predicate: "https://example.test/tradeoff".to_string(),
            label: "tradeoff".to_string(),
            value: "Keep the explicit tradeoff.".to_string(),
        },
    ];
    run.evidence[0].relations = vec![
        StoryEvidenceRelation {
            predicate: "https://example.test/motivates".to_string(),
            label: "motivates".to_string(),
            direction: StoryRelationDirection::Outgoing,
            target_iri: run.evidence[1].iri.clone(),
            target_label: run.evidence[1].title.clone(),
            target_kind: run.evidence[1].kind.clone(),
        },
        StoryEvidenceRelation {
            predicate: "https://example.test/motivates".to_string(),
            label: "motivates".to_string(),
            direction: StoryRelationDirection::Incoming,
            target_iri: run.evidence[1].iri.clone(),
            target_label: run.evidence[1].title.clone(),
            target_kind: run.evidence[1].kind.clone(),
        },
    ];
    let packet = build_narration_packet_for_test(&state, &run, 131_072).unwrap();
    let prompt = packet.prompt;
    let coverage = packet.coverage;
    assert_eq!(coverage.included_entities, 2);
    assert_eq!(coverage.source_groups, 2);
    assert_eq!(prompt.matches(&full).count(), 2);
    assert_eq!(prompt.matches(&state.capture.description).count(), 1);
    assert!(prompt.contains("A second canonical description remains evidence."));
    assert!(prompt.contains("https://foreign.example/hasDescription"));
    assert!(prompt.contains("Keep the explicit tradeoff."));
    assert_eq!(prompt.matches("https://example.test/motivates").count(), 1);
    assert!(!prompt.contains("Human guidance stays separate."));
    assert!(prompt.contains("Use every source ID"));
}

#[test]
fn narration_prompt_removes_relations_to_non_narratable_evidence() {
    let state = story_state("narration-prompt-suppression");
    let mut run = symbolic_run_for_narration();
    let suppressed_iri = run.evidence[1].iri.clone();
    run.evidence[1].suppressed = true;
    run.evidence[0].relations.push(StoryEvidenceRelation {
        predicate: "https://example.test/concerns".to_string(),
        label: "concerns".to_string(),
        direction: StoryRelationDirection::Outgoing,
        target_iri: suppressed_iri.clone(),
        target_label: "Suppressed target".to_string(),
        target_kind: "ArchitecturalDecision".to_string(),
    });
    let prompt = build_narration_packet_for_test(&state, &run, 32_768)
        .unwrap()
        .prompt;
    assert!(!prompt.contains(&suppressed_iri));
    assert!(!prompt.contains("Suppressed target"));
}

#[test]
fn larger_context_windows_admit_more_complete_evidence() {
    let state = story_state("narration-prompt-chunks");
    let mut run = symbolic_run_for_narration();
    run.evidence = (0..20)
        .map(|index| StoryEvidenceDetail {
            iri: format!("https://example.test/evidence/{index}"),
            title: format!("Evidence {index}"),
            kind: "Requirement".to_string(),
            status: "accepted".to_string(),
            suppressed: false,
            description: Some("y".repeat(3_000)),
            timestamp: None,
            author: None,
            properties: vec![],
            relations: vec![],
        })
        .collect();
    let small = build_narration_packet_for_test(&state, &run, 32_768)
        .unwrap()
        .coverage;
    let large = build_narration_packet_for_test(&state, &run, 131_072)
        .unwrap()
        .coverage;
    assert!(small.truncated);
    assert!(large.included_entities > small.included_entities);
    assert!(large.source_groups <= 12);
}

#[test]
fn narration_includes_current_grounded_code_anchors_only() {
    let state = story_state("narration-truncated-code-anchor");
    let mut run = symbolic_run_for_narration();
    run.code_anchors = vec![
        StoryCodeAnchor {
            symbol: "current::symbol".to_string(),
            label: "Current symbol".to_string(),
            entity_iri: Some(run.evidence[0].iri.clone()),
            path: Some("src/current.rs".to_string()),
            line: Some(7),
        },
        StoryCodeAnchor {
            symbol: "proposed::symbol".to_string(),
            label: "Proposed symbol".to_string(),
            entity_iri: Some(run.evidence[1].iri.clone()),
            path: Some("src/proposed.rs".to_string()),
            line: Some(9),
        },
        StoryCodeAnchor {
            symbol: "truncated::symbol".to_string(),
            label: "Current anchor outside the bounded dossier".to_string(),
            entity_iri: Some("https://example.test/truncated-code".to_string()),
            path: Some("src/truncated.rs".to_string()),
            line: Some(11),
        },
    ];
    run.evidence[1].status = "proposed".to_string();

    let prompt = build_narration_packet_for_test(&state, &run, 32_768)
        .unwrap()
        .prompt;
    assert!(prompt.contains("current::symbol"));
    assert!(prompt.contains("src/current.rs"));
    assert!(!prompt.contains("proposed::symbol"));
    assert!(!prompt.contains("src/proposed.rs"));
    assert!(prompt.contains("truncated::symbol"));
    assert!(prompt.contains("src/truncated.rs"));
}

#[test]
fn narration_prompt_budget_scales_but_never_exceeds_32k_tokens() {
    assert_eq!(narration_prompt_token_budget(32_768), 8_192);
    assert_eq!(narration_prompt_token_budget(131_072), 32_768);
    assert_eq!(narration_prompt_token_budget(1_000_000), 32_768);
}

#[test]
fn narration_excludes_proposed_but_allows_labeled_history() {
    let mut run = symbolic_run_for_narration();
    assert!(narration_evidence_is_eligible(&run));
    run.evidence[0].status = "proposed".to_string();
    assert!(narration_evidence_is_eligible(&run));
    run.evidence[1].status = "rejected".to_string();
    assert!(narration_evidence_is_eligible(&run));
    run.evidence[1].status = "proposed".to_string();
    assert!(!narration_evidence_is_eligible(&run));
    run.evidence[0].status = "accepted".to_string();
    run.gaps.push(StoryGap {
        id: "subject-drift".to_string(),
        title: "Story subject is unresolved".to_string(),
        detail: "The subject is no longer current.".to_string(),
        section_kind: None,
    });
    assert!(!narration_evidence_is_eligible(&run));
}
