use chrono::{TimeZone, Utc};
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::lessons::{generate_lesson_set, LessonGenerationOptions, LESSONS_INDEX_FILENAME};
use oxigraph::model::{GraphName, NamedNode, Quad};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-lessons-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record_item(state: &AppState, class_local: &str, title: &str, timestamp: &str) -> String {
    let class_iri = state.resolve_class(class_local).unwrap();
    graph::record_instance(
        state,
        &RecordInput {
            class_iri,
            class_local: class_local.to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
                (
                    state.capture.description.clone(),
                    format!("{class_local} body for {title}"),
                ),
                (state.capture.timestamp.clone(), timestamp.to_string()),
            ],
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 7, 6, 12, 0, 0).unwrap(),
    )
    .expect("record item")
}

fn insert_incomplete_lesson(state: &AppState, iri: &str) {
    let class = state.resolve_class("Lesson").unwrap();
    let quad = Quad::new(
        NamedNode::new(iri).unwrap(),
        NamedNode::new(moose::RDF_TYPE).unwrap(),
        NamedNode::new(class).unwrap(),
        GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    state
        .store
        .insert(quad.as_ref())
        .expect("insert incomplete lesson");
}

#[test]
fn lesson_set_renders_learned_from_sources() {
    let dir = temp_dir("render");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let lesson = record_item(
        &state,
        "Lesson",
        "Keep handlers thin",
        "2026-07-01T00:00:00Z",
    );
    let adr = record_item(
        &state,
        "ArchitecturalDecision",
        "Lesson Source ADR",
        "2026-07-02T00:00:00Z",
    );
    graph::relate(&state, &lesson, "learnedFrom", &adr).expect("relate lesson to decision");

    let set =
        generate_lesson_set(&state, LessonGenerationOptions::default()).expect("generate lessons");

    assert_eq!(set.graph_lessons, 1);
    assert_eq!(set.lesson_files, 1);
    assert_eq!(set.index_filename, LESSONS_INDEX_FILENAME);
    assert_eq!(set.lessons[0].num, "0001");
    assert_eq!(set.lessons[0].iri, lesson);
    assert_eq!(set.lessons[0].filename, "0001-keep-handlers-thin.md");
    assert_eq!(set.lessons[0].related_sources, 1);
    assert!(set.lessons[0]
        .markdown
        .contains("Lesson body for Keep handlers thin"));
    assert!(set.lessons[0].markdown.contains(&adr));
    assert!(set
        .index_markdown
        .contains("[Keep handlers thin](0001-keep-handlers-thin.md)"));
    assert!(set.warnings.unlinked_lessons.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lesson_sources_count_both_link_directions_without_duplicates() {
    let dir = temp_dir("directions");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let inverse_lesson = record_item(
        &state,
        "Lesson",
        "Linked via yieldsLesson",
        "2026-07-01T00:00:00Z",
    );
    let both_lesson = record_item(
        &state,
        "Lesson",
        "Linked in both directions",
        "2026-07-02T00:00:00Z",
    );
    let adr = record_item(
        &state,
        "ArchitecturalDecision",
        "Yielding ADR",
        "2026-07-03T00:00:00Z",
    );
    graph::relate(&state, &adr, "yieldsLesson", &inverse_lesson)
        .expect("relate decision to lesson");
    graph::relate(&state, &adr, "yieldsLesson", &both_lesson).expect("relate decision to lesson");
    graph::relate(&state, &both_lesson, "learnedFrom", &adr).expect("relate lesson to decision");

    let set =
        generate_lesson_set(&state, LessonGenerationOptions::default()).expect("generate lessons");
    let by_iri = |iri: &str| {
        set.lessons
            .iter()
            .find(|lesson| lesson.iri == iri)
            .expect("lesson in set")
    };

    assert_eq!(by_iri(&inverse_lesson).related_sources, 1);
    assert!(by_iri(&inverse_lesson).markdown.contains(&adr));
    assert_eq!(
        by_iri(&both_lesson).related_sources,
        1,
        "a pair linked in both directions is one source"
    );
    assert!(set.warnings.unlinked_lessons.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lesson_set_reports_duplicate_slugs_and_unlinked_lessons() {
    let dir = temp_dir("warnings");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_item(&state, "Lesson", "Repeated", "2026-07-01T00:00:00Z");
    record_item(&state, "Lesson", "Repeated", "2026-07-02T00:00:00Z");

    let set =
        generate_lesson_set(&state, LessonGenerationOptions::default()).expect("generate lessons");

    assert_eq!(set.lessons[0].filename, "0001-repeated.md");
    assert_eq!(set.lessons[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.unlinked_lessons, vec!["0001", "0002"]);
    assert!(set.warnings.missing_description.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lesson_set_renders_incomplete_records_instead_of_dropping_them() {
    let dir = temp_dir("incomplete");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    insert_incomplete_lesson(&state, "https://moosedev.dev/kg/Lesson/incomplete");

    let set =
        generate_lesson_set(&state, LessonGenerationOptions::default()).expect("generate lessons");

    assert_eq!(set.graph_lessons, 1);
    assert_eq!(
        set.lesson_files, 1,
        "typed lessons must not be silently dropped"
    );
    assert_eq!(set.lessons[0].title, "");
    assert_eq!(set.lessons[0].status, "not recorded");
    assert_eq!(set.lessons[0].filename, "0001-lesson.md");
    assert!(set
        .warnings
        .missing_description
        .contains(&"0001".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}
