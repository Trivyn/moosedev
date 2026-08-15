//! Cross-module Story domain regression tests.

use super::*;
use chrono::Utc;
use oxigraph::model::{GraphName, NamedNode, Quad};

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

fn link_successor(state: &AppState, old: &str, new: &str) {
    link_edge(state, old, "isSupersededBy", new);
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
        schema_version: STORY_SCHEMA_VERSION,
        subject: Some(StoryRecipeSubject::Entity {
            iri: "https://example.test/components/graph".to_string(),
        }),
        subject_component_iri: None,
        goal: "Understand the graph store".to_string(),
        audience: "reboarding".to_string(),
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
    assert_eq!(recipes[0].beats.len(), 1);
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
fn superseded_recipe_anchor_follows_chain_to_current_successor() {
    let state = story_state("supersession-chain");
    let old = record_with_status(&state, "Old", "superseded");
    let middle = record_with_status(&state, "Middle", "superseded");
    let current = record_with_status(&state, "Current", "accepted");
    link_successor(&state, &old, &middle);
    link_successor(&state, &old, "https://example.test/000-dead-branch");
    link_successor(&state, &middle, &current);

    match resolve_recipe_record(&state, &old).unwrap() {
        AnchorResolution::Superseded { replacements } => {
            assert_eq!(replacements[0].iri, current)
        }
        other => panic!("unexpected resolution: {other:?}"),
    }
    let competing = record_with_status(&state, "Competing current", "accepted");
    link_successor(&state, &old, &competing);
    match resolve_recipe_record(&state, &old).unwrap() {
        AnchorResolution::Superseded { replacements } => {
            assert_eq!(replacements.len(), 2);
            assert_eq!(
                replacements
                    .iter()
                    .map(|replacement| replacement.iri.as_str())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([current.as_str(), competing.as_str()])
            );
        }
        other => panic!("unexpected resolution: {other:?}"),
    }

    let legacy_old = record_with_status(&state, "Legacy old", "superseded");
    let legacy_current = record_with_status(&state, "Legacy current", "accepted");
    let status = NamedNode::new(&state.capture.status).unwrap();
    let legacy_node = NamedNode::new(&legacy_current).unwrap();
    let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI).unwrap();
    let status_quads = state
        .store
        .quads_for_pattern(
            Some(legacy_node.as_ref().into()),
            Some(status.as_ref()),
            None,
            Some(GraphNameRef::NamedNode(graph.as_ref())),
        )
        .flatten()
        .collect::<Vec<_>>();
    for quad in &status_quads {
        state.store.remove(quad).unwrap();
    }
    link_successor(&state, &legacy_old, &legacy_current);
    match resolve_recipe_record(&state, &legacy_old).unwrap() {
        AnchorResolution::Superseded { replacements } => {
            let replacement = &replacements[0];
            assert_eq!(replacement.iri, legacy_current);
            assert_eq!(replacement.status, "unknown");
        }
        other => panic!("unexpected legacy resolution: {other:?}"),
    }

    let orphan = record_with_status(&state, "Orphan", "superseded");
    assert!(matches!(
        resolve_recipe_record(&state, &orphan).unwrap(),
        AnchorResolution::Superseded { replacements } if replacements.is_empty()
    ));
    let recipe = StoryRecipe {
        id: "orphan".to_string(),
        title: "Orphan".to_string(),
        schema_version: STORY_SCHEMA_VERSION,
        subject: Some(StoryRecipeSubject::Entity {
            iri: "https://example.test/component".to_string(),
        }),
        subject_component_iri: None,
        goal: "See the gap".to_string(),
        audience: "reboarding".to_string(),
        beats: vec![StoryBeatRecipe {
            id: "governance".to_string(),
            title: "Governance".to_string(),
            intent: StoryIntent::Governance,
            record_iris: vec![orphan],
            code_symbols: vec![],
            curator_note: None,
        }],
        status: StoryStatus::Draft,
        curator: "tester".to_string(),
        updated_at: None,
    };
    let code = all_code_by_symbol(&state).unwrap();
    let (_, gaps) = recipe_beats(
        &state,
        &recipe,
        &StoryCandidate {
            iri: "https://example.test/component".to_string(),
            kind: "SystemComponent".to_string(),
            label: "Component".to_string(),
            description: None,
        },
        &code,
    )
    .unwrap();
    assert!(gaps[0]
        .detail
        .contains("no current successor can be resolved"));
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
    let generated_boundary = generated
        .iter()
        .find(|beat| beat.intent == StoryIntent::Boundary)
        .unwrap();
    let draft = StoryRecipe {
        id: "boundary-round-trip".to_string(),
        title: "Boundary round trip".to_string(),
        schema_version: STORY_SCHEMA_VERSION,
        subject: Some(StoryRecipeSubject::Entity {
            iri: component_iri.to_string(),
        }),
        subject_component_iri: None,
        goal: "Preserve the boundary".to_string(),
        audience: "reboarding".to_string(),
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
    let saved_boundary = saved
        .beats
        .iter()
        .find(|beat| beat.intent == StoryIntent::Boundary)
        .unwrap();
    assert!(saved_boundary.record_iris.is_empty());

    let code = all_code_by_symbol(&state).unwrap();
    let (reloaded, gaps) = recipe_beats(&state, &saved, &component, &code).unwrap();
    let reloaded_boundary = reloaded
        .iter()
        .find(|beat| beat.intent == StoryIntent::Boundary)
        .unwrap();
    assert_eq!(reloaded_boundary.evidence, generated_boundary.evidence);
    assert_eq!(reloaded_boundary.narrative, generated_boundary.narrative);
    assert!(!gaps
        .iter()
        .any(|gap| gap.beat_intent == Some(StoryIntent::Boundary)));
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
        false,
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
        false,
    )
    .unwrap();
    assert!(matches!(topic.subject, StorySubject::Topic { .. }));
    assert!(topic
        .beats
        .iter()
        .flat_map(|beat| &beat.evidence)
        .any(|evidence| evidence.iri == record_iri));

    let curated_iri = record_with_status(&state, "Explicitly curated anchor", "accepted");
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
        false,
    )
    .unwrap();
    assert!(curated_run.beats[0]
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
    let component = StoryCandidate {
        iri: target.to_string(),
        kind: "SystemComponent".to_string(),
        label: "Target".to_string(),
        description: None,
    };
    let index = StoryResolutionIndex::build(&state).unwrap();
    let symbolic = generate_symbolic_with_index(&state, &index, &component, None, true).unwrap();
    assert_eq!(symbolic.checks.len(), 1);
    let grants_after_symbolic = state.story_checks.lock().unwrap().grants.len();

    let presentation =
        generate_symbolic_with_index(&state, &index, &component, None, false).unwrap();
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
    let sparse = generate_symbolic_with_index(
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
        true,
    )
    .unwrap();
    assert!(sparse.checks.is_empty());
    assert!(sparse.gaps.iter().any(|gap| gap.id == "checks-unavailable"));
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
    let run = generate_symbolic_with_index(
        &state,
        &index,
        &StoryCandidate {
            iri: target.to_string(),
            kind: "SystemComponent".to_string(),
            label: "Retired component".to_string(),
            description: None,
        },
        None,
        true,
    )
    .unwrap();

    assert!(run.gaps.iter().any(|gap| gap.id == "subject-drift"));
    assert!(run.gaps.iter().any(|gap| gap.id == "checks-unavailable"));
    assert!(run.checks.is_empty());
    assert!(state.story_checks.lock().unwrap().grants.is_empty());
}

#[test]
fn curated_code_is_subject_grounded_and_check_uses_displayed_anchor() {
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
    let anchor = |symbol: &str, label: &str, iri: &str| StoryCodeAnchor {
        symbol: symbol.to_string(),
        label: label.to_string(),
        entity_iri: Some(iri.to_string()),
        path: None,
        line: None,
    };
    let anchor_a = anchor("symbol-a", "Code A", code_a);
    let anchor_b = anchor("symbol-b", "Code B", code_b);
    let code_by_symbol = BTreeMap::from([
        (anchor_a.symbol.clone(), anchor_a.clone()),
        (anchor_b.symbol.clone(), anchor_b.clone()),
    ]);
    let curated = StoryRecipe {
        id: "curated-code".to_string(),
        title: "Curated code".to_string(),
        schema_version: STORY_SCHEMA_VERSION,
        subject: Some(StoryRecipeSubject::Entity {
            iri: component_a.to_string(),
        }),
        subject_component_iri: None,
        goal: "Ground code".to_string(),
        audience: "reboarding".to_string(),
        beats: vec![StoryBeatRecipe {
            id: "core-code".to_string(),
            title: "Core code".to_string(),
            intent: StoryIntent::CoreCode,
            record_iris: vec![],
            code_symbols: vec![anchor_b.symbol.clone()],
            curator_note: None,
        }],
        status: StoryStatus::Draft,
        curator: "tester".to_string(),
        updated_at: None,
    };
    let (beats, gaps) = recipe_beats(
        &state,
        &curated,
        &StoryCandidate {
            iri: component_a.to_string(),
            kind: "SystemComponent".to_string(),
            label: "Component A".to_string(),
            description: None,
        },
        &code_by_symbol,
    )
    .unwrap();
    assert!(beats[0].code_anchors.is_empty());
    assert!(gaps
        .iter()
        .any(|gap| gap.title == "Story code does not realize this subject"));

    let displayed = vec![make_beat(
        "core-code",
        "Core code",
        StoryIntent::CoreCode,
        vec![],
        vec![anchor_b],
        None,
        None,
    )];
    let index = StoryResolutionIndex {
        components: vec![],
        components_by_iri: BTreeMap::new(),
        known_components_by_iri: BTreeMap::new(),
        code_entities: code_by_symbol.values().cloned().collect(),
        code_by_symbol,
    };
    let (checks, viable) = build_checks(
        &state,
        &StoryCandidate {
            iri: component_b.to_string(),
            kind: "SystemComponent".to_string(),
            label: "Component B".to_string(),
            description: None,
        },
        &displayed,
        &index,
        &[],
        &[anchor("symbol-b", "Code B", code_b)],
        true,
    )
    .unwrap();
    assert_eq!(viable, 1);
    assert_eq!(checks.len(), 1);
    let labels = checks[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(labels, BTreeSet::from(["Code A", "Code B"]));
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
fn published_recipes_require_three_to_five_beats() {
    let error = validate_recipe(&recipe("short", StoryStatus::Published, 2), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("3 to 5"));
    let mut valid = recipe("valid", StoryStatus::Published, 5);
    for beat in &mut valid.beats {
        beat.record_iris
            .push("https://example.test/record".to_string());
    }
    assert!(validate_recipe(&valid, true).is_ok());

    let mut duplicated = valid.clone();
    duplicated.beats[1].intent = StoryIntent::Purpose;
    assert!(validate_recipe(&duplicated, true)
        .unwrap_err()
        .to_string()
        .contains("intent only once"));

    let mut out_of_order = valid;
    out_of_order.beats.swap(0, 1);
    assert!(validate_recipe(&out_of_order, true)
        .unwrap_err()
        .to_string()
        .contains("must follow"));
}

#[test]
fn beat_reference_limit_accepts_six_and_rejects_seven_or_duplicates() {
    let six = (0..6)
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>();
    assert!(validate_refs("record IRI", &six).is_ok());

    let seven = (0..7)
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>();
    assert!(validate_refs("record IRI", &seven)
        .unwrap_err()
        .to_string()
        .contains("at most 6"));

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
            beat_id: "purpose".to_string(),
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
            beat_id: "purpose".to_string(),
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
        beat_id: "purpose".to_string(),
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
            beat_id: "purpose".to_string(),
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
        beat_id: "purpose".to_string(),
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

fn symbolic_run_for_narration() -> StoryRun {
    StoryRun {
        recipe_id: None,
        trust_state: StoryTrustState::Generated,
        narration_mode: NarrationMode::Symbolic,
        narration_outcome: NarrationOutcome::NotRequested,
        title: "A Story".to_string(),
        subject: StorySubject::Entity {
            iri: "component".to_string(),
            kind: "SystemComponent".to_string(),
            label: "Component".to_string(),
        },
        goal: "Understand it".to_string(),
        overview: "Overview".to_string(),
        beats: vec![
            StoryBeat {
                id: "purpose".to_string(),
                title: "Purpose".to_string(),
                intent: StoryIntent::Purpose,
                narrative: "Symbolic purpose".to_string(),
                evidence: vec![StoryEvidence {
                    iri: "record".to_string(),
                    title: "Requirement".to_string(),
                    kind: "Requirement".to_string(),
                    status: "accepted".to_string(),
                }],
                code_anchors: vec![],
                curator_note: Some("Keep this note".to_string()),
                gap: None,
            },
            StoryBeat {
                id: "code".to_string(),
                title: "Code".to_string(),
                intent: StoryIntent::CoreCode,
                narrative: "Symbolic code".to_string(),
                evidence: vec![],
                code_anchors: vec![StoryCodeAnchor {
                    symbol: "symbol".to_string(),
                    label: "function".to_string(),
                    entity_iri: Some("entity".to_string()),
                    path: Some("src/lib.rs".to_string()),
                    line: None,
                }],
                curator_note: None,
                gap: None,
            },
        ],
        gaps: vec![],
        checks: vec![],
    }
}

#[test]
fn valid_narration_changes_only_grounded_prose_and_mode() {
    let symbolic = symbolic_run_for_narration();
    let raw = r#"{"beats":[{"beat_id":"purpose","text":"Narrated purpose","citation_ids":["e0"]},{"beat_id":"code","text":"Narrated code","citation_ids":["c0"]}]}"#;
    let narrated = apply_narration_response(symbolic.clone(), raw).unwrap();
    let mut expected = symbolic;
    expected.narration_mode = NarrationMode::Llm;
    expected.narration_outcome = NarrationOutcome::Succeeded;
    expected.beats[0].narrative = "Narrated purpose".to_string();
    expected.beats[1].narrative = "Narrated code".to_string();
    assert_eq!(narrated, expected);
}

#[test]
fn narration_repairs_common_local_model_json_syntax_before_strict_validation() {
    let symbolic = symbolic_run_for_narration();
    let raw = r#"```json
{"beats":[{"beat_id":"purpose","text":"Narrated purpose","citation_ids":["e0"]},{"beat_id":"code","text":"Narrated code","citation_ids":["c0"]}]}
```"#;
    let narrated = apply_narration_response(symbolic.clone(), raw).unwrap();
    assert_eq!(narrated.narration_mode, NarrationMode::Llm);
    assert_eq!(narrated.beats[0].narrative, "Narrated purpose");
    assert_eq!(narrated.beats[1].narrative, "Narrated code");

    let prefixed = format!("Here you go:\n{raw}");
    assert!(apply_narration_response(symbolic.clone(), &prefixed).is_some());
    let json_like = r#"{'beats':[{'beat_id':'purpose','text':'Narrated purpose','citation_ids':['e0'],},{'beat_id':'code','text':'Narrated code','citation_ids':['c0'],}],}"#;
    assert!(apply_narration_response(symbolic.clone(), json_like).is_some());

    let unknown_field = r#"{'beats':[{'beat_id':'purpose','text':'Narrated purpose','citation_ids':['e0'],'claim':'invented'},{'beat_id':'code','text':'Narrated code','citation_ids':['c0']}] }"#;
    assert!(apply_narration_response(symbolic.clone(), unknown_field).is_none());
    let cross_beat_citation = r#"{'beats':[{'beat_id':'purpose','text':'Narrated purpose','citation_ids':['c0']},{'beat_id':'code','text':'Narrated code','citation_ids':['c0']}] }"#;
    assert!(apply_narration_response(symbolic.clone(), cross_beat_citation).is_none());
    let multiple = format!("{raw}\n{raw}");
    assert!(apply_narration_response(symbolic, &multiple).is_none());
}

#[test]
fn narration_prompt_bounds_individual_fields_and_total_input() {
    let state = story_state("narration-prompt-bounds");
    let mut run = symbolic_run_for_narration();
    run.beats[0].evidence[0].title = "x".repeat(MAX_LLM_FIELD_BYTES * 4);
    run.beats[0].narrative = "Owns src/boundary/ through coversPath".to_string();
    run.beats[0].curator_note = Some("private curator guidance".to_string());
    let prompt = build_narration_prompt(&state, &run, &[&run.beats[0]]).unwrap();
    assert!(prompt.len() <= MAX_LLM_PROMPT_BYTES);
    assert!(!prompt.contains(&"x".repeat(MAX_LLM_FIELD_BYTES + 1)));
    assert!(prompt.contains("Owns src/boundary/ through coversPath"));
    assert!(prompt.contains("two to four short, connected sentences"));
    assert!(prompt.contains(r#""title":"A Story""#));
    assert!(prompt.contains(r#""subject_label":"Component""#));
    assert!(prompt.contains(r#""goal":"Understand it""#));
    assert!(prompt.contains(r#""status":"accepted""#));
    assert!(!prompt.contains("private curator guidance"));

    let huge = "y".repeat(MAX_LLM_FIELD_BYTES * 4);
    let template = StoryBeat {
        id: "beat".to_string(),
        title: "Beat".to_string(),
        intent: StoryIntent::Governance,
        narrative: "Symbolic".to_string(),
        evidence: (0..MAX_ANCHORS_PER_BEAT)
            .map(|index| StoryEvidence {
                iri: format!("https://example.test/record/{index}"),
                title: huge.clone(),
                kind: huge.clone(),
                status: "accepted".to_string(),
            })
            .collect(),
        code_anchors: (0..MAX_ANCHORS_PER_BEAT)
            .map(|index| StoryCodeAnchor {
                symbol: format!("symbol-{index}"),
                label: huge.clone(),
                entity_iri: Some(format!("https://example.test/code/{index}")),
                path: Some(huge.clone()),
                line: None,
            })
            .collect(),
        curator_note: Some("not sent to the sensor".to_string()),
        gap: None,
    };
    let beats = (0..MAX_BEATS)
        .map(|index| {
            let mut beat = template.clone();
            beat.id = format!("beat-{index}");
            beat
        })
        .collect::<Vec<_>>();
    let mut oversized = symbolic_run_for_narration();
    oversized.beats = beats;
    let eligible = oversized.beats.iter().collect::<Vec<_>>();
    assert!(build_narration_prompt(&state, &oversized, &eligible).is_none());
}

#[test]
fn narration_rejects_drifted_or_noncurrent_evidence() {
    let mut run = symbolic_run_for_narration();
    assert!(narration_evidence_is_current(&run));

    run.beats[0].evidence[0].status = "superseded".to_string();
    assert!(!narration_evidence_is_current(&run));

    run.beats[0].evidence[0].status = "accepted".to_string();
    run.gaps.push(StoryGap {
        id: "subject-drift".to_string(),
        title: "Story subject is unresolved".to_string(),
        detail: "The subject is no longer current.".to_string(),
        beat_intent: None,
    });
    assert!(!narration_evidence_is_current(&run));
}

#[test]
fn invalid_narration_preserves_the_identical_symbolic_run() {
    let symbolic = symbolic_run_for_narration();
    let invalid = [
        "not json",
        r#"{"unknown":true,"beats":[]}"#,
        r#"{"beats":[{"beat_id":"purpose","text":"x","citation_ids":["e0","e0"]},{"beat_id":"code","text":"y","citation_ids":["c0"]}]}"#,
        r#"{"beats":[{"beat_id":"purpose","text":"x","citation_ids":["c0"]},{"beat_id":"code","text":"y","citation_ids":["c0"]}]}"#,
        r#"{"beats":[{"beat_id":"code","text":"y","citation_ids":["c0"]},{"beat_id":"purpose","text":"x","citation_ids":["e0"]}]}"#,
    ];
    for raw in invalid {
        let actual =
            apply_narration_response(symbolic.clone(), raw).unwrap_or_else(|| symbolic.clone());
        assert_eq!(actual, symbolic, "response should fall back: {raw}");
    }
}
