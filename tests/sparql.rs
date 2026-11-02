//! Raw SPARQL escape hatch: deterministic, read-only querying over the
//! queryable graphs, without the NLQ pipeline. The queries run on MOOSE's
//! bounded engine (moose-mcp). These tests pin what moosedev's tool promises
//! callers: the reply shape, the scope, the rejections and the notes.

use std::path::Path;

use chrono::Utc;
use moosedev::graph::{self, AppState, RecordInput};
use moosedev::sparql;
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use serde_json::Value;

fn bootstrap(name: &str) -> (std::path::PathBuf, AppState) {
    let dir = std::env::temp_dir().join(format!("moosedev-sparql-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");
    (dir, state)
}

fn record_decision(state: &AppState, title: &str) -> String {
    graph::record_instance(
        state,
        &RecordInput {
            class_iri: state.resolve_class("ArchitecturalDecision").unwrap(),
            class_local: "ArchitecturalDecision".to_string(),
            properties: vec![
                (moose::RDFS_LABEL.to_string(), title.to_string()),
                (state.capture.title.clone(), title.to_string()),
            ],
        },
        "test-agent",
        Utc::now(),
    )
    .expect("record decision")
}

#[tokio::test]
async fn sparql_selects_recorded_instances_and_serializes_ask() {
    let (dir, state) = bootstrap("select");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let iri = record_decision(&state, "Adopt SPARQL");

    let select = format!("SELECT ?s WHERE {{ ?s a <{class_iri}> }}");
    let reply = sparql::run_query(&state, &select)
        .await
        .expect("run SELECT");
    let json: Value = serde_json::from_str(&reply.text).expect("SPARQL JSON");
    assert!(
        json["results"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|binding| binding["s"]["value"] == iri),
        "SELECT should return the recorded instance: {json}"
    );
    assert_eq!(json["head"]["vars"][0], "s");
    assert!(json.get("truncated").is_none(), "{json}");
    assert!(reply.note.is_none(), "a non-empty result carries no note");

    let ask = format!("ASK {{ <{iri}> a <{class_iri}> }}");
    let reply = sparql::run_query(&state, &ask).await.expect("run ASK");
    let json: Value = serde_json::from_str(&reply.text).expect("SPARQL JSON");
    assert_eq!(json["boolean"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_serializes_construct_as_ntriples() {
    let (dir, state) = bootstrap("construct");
    let iri = record_decision(&state, "Construct me");

    let construct = format!("CONSTRUCT {{ <{iri}> ?p ?o }} WHERE {{ <{iri}> ?p ?o }}");
    let ntriples = sparql::run_query(&state, &construct)
        .await
        .expect("run CONSTRUCT")
        .text;
    assert!(
        ntriples.contains(&format!("<{iri}>")),
        "CONSTRUCT should serialize the subject as N-Triples:\n{ntriples}"
    );
    assert!(
        ntriples.contains(moose::RDF_TYPE),
        "CONSTRUCT should include the typed assertion:\n{ntriples}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_rejects_update_syntax() {
    let (dir, state) = bootstrap("update");
    let before = state.store.iter().count();

    sparql::run_query(
        &state,
        "INSERT DATA { <https://example.test/s> <https://example.test/p> <https://example.test/o> }",
    )
    .await
    .expect_err("SPARQL UPDATE must be rejected");
    assert_eq!(
        state.store.iter().count(),
        before,
        "rejected UPDATE must not change the store"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_rejects_from_and_says_how_to_name_a_graph() {
    let (dir, state) = bootstrap("from");
    let err = sparql::run_query(
        &state,
        &format!(
            "SELECT ?s FROM <{}> WHERE {{ ?s ?p ?o }}",
            graph::PROJECT_KG_GRAPH_IRI
        ),
    )
    .await
    .expect_err("FROM must be rejected");
    assert!(
        err.contains("GRAPH"),
        "the rejection names the alternative: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_sees_project_graphs_and_never_engine_state() {
    let (dir, state) = bootstrap("scope");
    let iri = record_decision(&state, "Visible");
    let hidden = NamedNode::new("urn:moose:session:test-hidden").unwrap();
    state
        .store
        .insert(&Quad::new(
            NamedNode::new("https://example.test/secret").unwrap(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Literal::new_simple_literal("SECRET"),
            GraphName::NamedNode(hidden.clone()),
        ))
        .unwrap();

    let graphs = sparql::run_query(&state, "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }")
        .await
        .expect("list graphs")
        .text;
    for visible in [
        graph::PROJECT_KG_GRAPH_IRI,
        "https://moosedev.dev/kg/ontology/software-architecture",
        "https://moosedev.dev/kg/ontology/software-architecture/shapes",
    ] {
        assert!(graphs.contains(visible), "{visible} is queryable: {graphs}");
    }
    assert!(!graphs.contains(hidden.as_str()), "{graphs}");
    assert!(!graphs.contains(moose::MOOSE_ONTOLOGY_GRAPH), "{graphs}");

    // A hidden graph reads as empty in the default graph and by name.
    let secret = sparql::run_query(&state, "ASK { ?s ?p \"SECRET\" }")
        .await
        .unwrap()
        .text;
    assert_eq!(
        serde_json::from_str::<Value>(&secret).unwrap()["boolean"],
        false
    );
    let by_name = sparql::run_query(
        &state,
        &format!("ASK {{ GRAPH <{}> {{ ?s ?p ?o }} }}", hidden.as_str()),
    )
    .await;
    assert!(
        by_name
            .map(|reply| reply.text.contains("false"))
            .unwrap_or(true),
        "a hidden graph never answers true"
    );

    // The default graph is the union of the queryable graphs.
    let ask = format!("ASK {{ <{iri}> ?p ?o }}");
    let visible = sparql::run_query(&state, &ask).await.unwrap().text;
    assert_eq!(
        serde_json::from_str::<Value>(&visible).unwrap()["boolean"],
        true
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_explains_an_empty_result_from_an_unknown_term() {
    let (dir, state) = bootstrap("note");
    record_decision(&state, "Noted");
    let class_iri = state.resolve_class("ArchitecturalDecision").unwrap();
    let namespace = class_iri.rsplit_once(['#', '/']).unwrap().0;
    let bogus = format!("{namespace}#hasStatusOfLifecycle");

    let reply = sparql::run_query(&state, &format!("SELECT ?s WHERE {{ ?s <{bogus}> ?o }}"))
        .await
        .expect("an empty result is not an error");
    let note = reply.note.expect("the unknown predicate is explained");
    assert!(note.contains(&bogus), "{note}");
    // The text stays parseable SPARQL JSON; the note travels separately.
    let json: Value = serde_json::from_str(&reply.text).expect("SPARQL JSON");
    assert!(json.get("note").is_none());

    // A correct query that matches nothing gets no note.
    let quiet = sparql::run_query(
        &state,
        &format!(
            "SELECT ?s WHERE {{ ?s <{}> \"no such title\" }}",
            state.capture.title
        ),
    )
    .await
    .unwrap();
    assert!(quiet.note.is_none(), "{:?}", quiet.note);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_parse_error_names_an_undeclared_prefix() {
    let (dir, state) = bootstrap("prefix");
    let err = sparql::run_query(&state, "SELECT ?s WHERE { ?s rdfs:label ?l }")
        .await
        .expect_err("undeclared prefix");
    assert!(err.contains("rdfs:"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sparql_marks_a_result_cut_at_the_row_cap() {
    let (dir, state) = bootstrap("cap");
    let graph = GraphName::NamedNode(NamedNode::new(graph::PROJECT_KG_GRAPH_IRI).unwrap());
    let predicate = NamedNode::new("https://example.test/p").unwrap();
    for i in 0..1_005 {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(format!("https://example.test/s{i}")).unwrap(),
                predicate.clone(),
                Literal::from(i),
                graph.clone(),
            ))
            .unwrap();
    }
    let reply = sparql::run_query(&state, "SELECT ?s WHERE { ?s <https://example.test/p> ?o }")
        .await
        .unwrap();
    let json: Value = serde_json::from_str(&reply.text).unwrap();
    assert_eq!(json["truncated"], true);
    assert_eq!(json["results"]["bindings"].as_array().unwrap().len(), 1_000);

    let _ = std::fs::remove_dir_all(&dir);
}
