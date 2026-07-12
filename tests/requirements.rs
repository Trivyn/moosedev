use chrono::{TimeZone, Utc};
use moosedev::graph::{self, AppState, RecordInput, SupersedeInput};
use moosedev::requirements::{
    generate_requirement_set, RequirementGenerationOptions, REQUIREMENTS_INDEX_FILENAME,
};
use oxigraph::model::{GraphName, GraphNameRef, NamedNode, NamedNodeRef, Quad, Term};

fn ontology_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-requirements-{name}-{}-{}",
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
        Utc.with_ymd_and_hms(2026, 6, 25, 12, 0, 0).unwrap(),
    )
    .expect("record item")
}

fn insert_incomplete_requirement(state: &AppState, iri: &str) {
    let class = state.resolve_class("Requirement").unwrap();
    let quad = Quad::new(
        NamedNode::new(iri).unwrap(),
        NamedNode::new(moose::RDF_TYPE).unwrap(),
        NamedNode::new(class).unwrap(),
        GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap()),
    );
    state
        .store
        .insert(quad.as_ref())
        .expect("insert incomplete requirement");
}

fn literals(state: &AppState, subject_iri: &str, predicate_iri: &str) -> Vec<String> {
    let subject = NamedNodeRef::new(subject_iri).unwrap();
    let predicate = NamedNodeRef::new(predicate_iri).unwrap();
    let graph_ref = NamedNodeRef::new(graph::PROJECT_KG_GRAPH_IRI).unwrap();
    let mut values: Vec<String> = state
        .store
        .quads_for_pattern(
            Some(subject.into()),
            Some(predicate),
            None,
            Some(GraphNameRef::NamedNode(graph_ref)),
        )
        .flatten()
        .filter_map(|q| match q.object {
            Term::Literal(lit) => Some(lit.value().to_string()),
            _ => None,
        })
        .collect();
    values.sort();
    values
}

#[test]
fn record_without_status_defaults_to_accepted() {
    let dir = temp_dir("default-accepted");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let req = record_item(
        &state,
        "Requirement",
        "Default accepted requirement",
        "2026-06-25T00:00:00Z",
    );

    assert_eq!(
        literals(&state, &req, &state.capture.status),
        vec!["accepted"],
        "capture without explicit status stores accepted"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requirement_set_renders_related_adrs_from_project_graph() {
    let dir = temp_dir("render");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let req = record_item(
        &state,
        "Requirement",
        "Trace decisions to needs",
        "2026-06-25T00:00:00Z",
    );
    let adr = record_item(
        &state,
        "ArchitecturalDecision",
        "ADR Requirement Link",
        "2026-06-26T00:00:00Z",
    );
    graph::relate(&state, &adr, "isMotivatedBy", &req).expect("relate decision to requirement");

    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())
        .expect("generate requirements");

    assert_eq!(set.graph_requirements, 1);
    assert_eq!(set.requirement_files, 1);
    assert_eq!(set.index_filename, REQUIREMENTS_INDEX_FILENAME);
    assert_eq!(set.requirements[0].num, "0001");
    assert_eq!(set.requirements[0].iri, req);
    assert_eq!(
        set.requirements[0].filename,
        "0001-trace-decisions-to-needs.md"
    );
    assert_eq!(set.requirements[0].related_adrs, 1);
    assert!(set.requirements[0].addressed);
    assert!(set.requirements[0]
        .markdown
        .contains("Requirement body for Trace decisions to needs"));
    assert!(set.requirements[0].markdown.contains("- Addressed: Yes"));
    assert!(set.requirements[0].markdown.contains(&adr));
    assert_eq!(set.summaries()[0].search_text, set.requirements[0].markdown);
    assert!(set
        .index_markdown
        .contains("[Trace decisions to needs](0001-trace-decisions-to-needs.md)"));
    assert!(set.warnings.unlinked_requirements.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requirement_addressed_requires_a_live_linked_adr() {
    let dir = temp_dir("addressed");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    let live_req = record_item(
        &state,
        "Requirement",
        "Live linked requirement",
        "2026-06-25T00:00:00Z",
    );
    let retired_req = record_item(
        &state,
        "Requirement",
        "Retired linked requirement",
        "2026-06-26T00:00:00Z",
    );
    let open_req = record_item(
        &state,
        "Requirement",
        "Open requirement",
        "2026-06-27T00:00:00Z",
    );
    let live_adr = record_item(
        &state,
        "ArchitecturalDecision",
        "Live addressing ADR",
        "2026-06-25T12:00:00Z",
    );
    let retired_adr = record_item(
        &state,
        "ArchitecturalDecision",
        "Retired addressing ADR",
        "2026-06-26T12:00:00Z",
    );

    graph::relate(&state, &live_adr, "isMotivatedBy", &live_req)
        .expect("relate live ADR to requirement");
    graph::relate(&state, &retired_adr, "isMotivatedBy", &retired_req)
        .expect("relate retired ADR to requirement");
    graph::supersede_decision(
        &state,
        &SupersedeInput {
            superseded_iri: retired_adr,
            new: RecordInput {
                class_iri: state.resolve_class("ArchitecturalDecision").unwrap(),
                class_local: "ArchitecturalDecision".to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), "Replacement ADR".to_string()),
                    (state.capture.title.clone(), "Replacement ADR".to_string()),
                    (
                        state.capture.timestamp.clone(),
                        "2026-06-28T00:00:00Z".to_string(),
                    ),
                ],
            },
            rationale: "Retire the original ADR link.".to_string(),
        },
        "test-agent",
        Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
    )
    .expect("supersede linked ADR");

    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())
        .expect("generate requirements");
    let by_iri = |iri: &str| {
        set.requirements
            .iter()
            .find(|req| req.iri == iri)
            .expect("requirement in set")
    };

    assert!(by_iri(&live_req).addressed);
    assert!(by_iri(&live_req).markdown.contains("- Addressed: Yes"));
    assert!(!by_iri(&retired_req).addressed);
    assert_eq!(by_iri(&retired_req).related_adrs, 1);
    assert!(by_iri(&retired_req).markdown.contains("- Addressed: No"));
    assert!(!by_iri(&open_req).addressed);
    assert_eq!(by_iri(&open_req).related_adrs, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requirement_set_reports_duplicate_slugs_and_unlinked_requirements() {
    let dir = temp_dir("warnings");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    record_item(&state, "Requirement", "Repeated", "2026-06-25T00:00:00Z");
    record_item(&state, "Requirement", "Repeated", "2026-06-26T00:00:00Z");

    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())
        .expect("generate requirements");

    assert_eq!(set.requirements[0].filename, "0001-repeated.md");
    assert_eq!(set.requirements[1].filename, "0002-repeated-2.md");
    assert_eq!(set.warnings.unlinked_requirements, vec!["0001", "0002"]);
    assert!(set.warnings.missing_description.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requirement_set_renders_incomplete_records_instead_of_dropping_them() {
    let dir = temp_dir("incomplete");
    let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
    insert_incomplete_requirement(&state, "https://moosedev.dev/kg/Requirement/incomplete");

    let set = generate_requirement_set(&state, RequirementGenerationOptions::default())
        .expect("generate requirements");

    assert_eq!(set.graph_requirements, 1);
    assert_eq!(
        set.requirement_files, 1,
        "typed requirements must not be silently dropped"
    );
    assert_eq!(set.requirements[0].title, "");
    assert_eq!(set.requirements[0].status, "not recorded");
    assert_eq!(set.requirements[0].filename, "0001-requirement.md");
    assert!(set
        .warnings
        .missing_description
        .contains(&"0001".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}
