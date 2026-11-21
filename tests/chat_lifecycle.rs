//! Web chat reads a lifecycle-filtered view of the project store (#28).
//!
//! Proposed and rejected records belong to the ratification inbox. A chat turn
//! must not see them or any edge touching them, while its own session, overlay
//! and trace writes still persist in the live store and read back mid-turn.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use moose::entity_index::EntityIndexCache;
use moose::store_capability::{GraphReader, GraphWriter, Quad};
use moose::traits::LlmClient;
use moose::types::{ChatMessage, ChatRequest, EngineError, LlmAssistLevel, LlmParams, Role};
use moosedev::graph::authoritative::{authoritative_view, LifecycleFilteredStore};
use moosedev::graph::{self, AppState, RecordInput, PROJECT_KG_GRAPH_IRI};
use oxigraph::model::{GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef};
use oxigraph::store::Store;

const SESSION_GRAPH: &str = "https://moosedev.dev/kg/test-session";

struct Fixture {
    state: AppState,
    dir: std::path::PathBuf,
    component: String,
    proposed: String,
    rejected: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An accepted component that a proposed and a rejected decision each concern.
fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-chat-lifecycle-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ontology_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
    let state = AppState::bootstrap(&dir, &ontology_dir).expect("bootstrap app state");

    let record = |kind: &str, title: &str, status: &str| {
        graph::record_instance(
            &state,
            &RecordInput {
                class_iri: state.resolve_class(kind).unwrap(),
                class_local: kind.to_string(),
                properties: vec![
                    (moose::RDFS_LABEL.to_string(), title.to_string()),
                    (state.capture.title.clone(), title.to_string()),
                    (state.capture.status.clone(), status.to_string()),
                ],
            },
            "test-agent",
            Utc::now(),
        )
        .unwrap()
    };
    let component = record("SystemComponent", "Lifecycle chat target", "accepted");
    let proposed = record(
        "ArchitecturalDecision",
        "PROPOSED-SECRET-CHAT-TITLE",
        "proposed",
    );
    let rejected = record(
        "ArchitecturalDecision",
        "REJECTED-SECRET-CHAT-TITLE",
        "rejected",
    );
    graph::relate(&state, &proposed, "concerns", &component).unwrap();
    graph::relate(&state, &rejected, "concerns", &component).unwrap();
    Fixture {
        state,
        dir,
        component,
        proposed,
        rejected,
    }
}

fn mentions(store: &Store, iri: &str) -> usize {
    let node = NamedNodeRef::new(iri).unwrap();
    let as_subject = store
        .quads_for_pattern(Some(node.into()), None, None, None)
        .count();
    let as_object = store
        .quads_for_pattern(None, None, Some(node.into()), None)
        .count();
    as_subject + as_object
}

fn graph_len(store: &Store, graph: &str) -> usize {
    store
        .quads_for_pattern(
            None,
            None,
            None,
            Some(GraphNameRef::NamedNode(NamedNodeRef::new(graph).unwrap())),
        )
        .count()
}

fn session_quad(n: usize) -> Quad {
    Quad::new(
        NamedNode::new(format!("https://moosedev.dev/kg/test-turn/{n}")).unwrap(),
        NamedNode::new(moose::RDFS_LABEL).unwrap(),
        Literal::new_simple_literal(format!("turn {n}")),
        GraphName::NamedNode(NamedNode::new(SESSION_GRAPH).unwrap()),
    )
}

#[test]
fn view_hides_proposed_and_rejected_records_and_their_edges() {
    let f = fixture("view");
    assert!(mentions(&f.state.store, &f.proposed) > 0);
    assert!(mentions(&f.state.store, &f.rejected) > 0);
    // A non-project graph also mentions a hidden record (as provenance does).
    f.state
        .store
        .insert(&Quad::new(
            NamedNode::new(&f.proposed).unwrap(),
            NamedNode::new(moose::RDFS_LABEL).unwrap(),
            Literal::new_simple_literal("noted elsewhere"),
            GraphName::NamedNode(NamedNode::new(SESSION_GRAPH).unwrap()),
        ))
        .unwrap();

    let view = authoritative_view(&f.state, None).unwrap();

    assert_eq!(
        mentions(&view, &f.proposed),
        0,
        "no quad touches a proposed record"
    );
    assert_eq!(
        mentions(&view, &f.rejected),
        0,
        "no quad touches a rejected record"
    );
    assert!(mentions(&view, &f.component) > 0, "accepted records stay");
    for graph in moosedev::graph::authoritative::ONTOLOGY_GRAPH_IRIS {
        assert_eq!(
            graph_len(&view, graph),
            graph_len(&f.state.store, graph),
            "ontology graph {graph} is copied whole"
        );
    }
    // Only the hidden nodes' quads are gone from the project graph.
    let hidden_quads = |store: &Store| {
        let graph = Some(GraphNameRef::NamedNode(
            NamedNodeRef::new(PROJECT_KG_GRAPH_IRI).unwrap(),
        ));
        store
            .quads_for_pattern(None, None, None, graph)
            .flatten()
            .filter(|q| {
                [&f.proposed, &f.rejected].iter().any(|iri| {
                    q.subject.to_string() == format!("<{iri}>")
                        || q.object.to_string() == format!("<{iri}>")
                })
            })
            .count()
    };
    assert_eq!(
        graph_len(&view, PROJECT_KG_GRAPH_IRI),
        graph_len(&f.state.store, PROJECT_KG_GRAPH_IRI) - hidden_quads(&f.state.store)
    );
}

#[test]
fn writes_reach_the_view_and_the_live_store() {
    let f = fixture("write-through");
    let store = LifecycleFilteredStore::for_chat_turn(&f.state).unwrap();
    let live = &f.state.store;

    store
        .insert_quads(&[session_quad(1), session_quad(2)])
        .unwrap();
    let read_back = GraphReader::quads_for_pattern(
        &store,
        None,
        None,
        None,
        Some(GraphNameRef::NamedNode(
            NamedNodeRef::new(SESSION_GRAPH).unwrap(),
        )),
    )
    .count();
    assert_eq!(read_back, 2, "the turn reads its own writes");
    assert_eq!(graph_len(live, SESSION_GRAPH), 2, "the writes persist");
    let handle = store.reader_handle();
    assert_eq!(
        handle
            .quads_for_pattern(None, None, None, None)
            .flatten()
            .filter(|q| q == &session_quad(1))
            .count(),
        1,
        "a blocking-thread handle reads the same view"
    );

    store.remove_quads(&[session_quad(1)]).unwrap();
    assert!(!live.contains(&session_quad(1)).unwrap());
    assert_eq!(graph_len(live, SESSION_GRAPH), 1);

    store
        .clear_graph(NamedNodeRef::new(SESSION_GRAPH).unwrap())
        .unwrap();
    assert_eq!(graph_len(live, SESSION_GRAPH), 0);
    assert_eq!(
        GraphReader::quads_for_pattern(&store, None, None, None, None)
            .flatten()
            .filter(|q| q.graph_name.to_string() == format!("<{SESSION_GRAPH}>"))
            .count(),
        0
    );
    // Hidden records are untouched in the live store.
    assert!(mentions(live, &f.proposed) > 0);
}

/// A live writer that refuses every call, to prove a failed write leaves the
/// view as it was (the GraphWriter all-or-nothing clause).
struct RefusingWriter;

impl GraphWriter for RefusingWriter {
    fn insert_quads(&self, _: &[Quad]) -> Result<(), EngineError> {
        Err(EngineError::InternalError("refused".into()))
    }
    fn remove_quads(&self, _: &[Quad]) -> Result<(), EngineError> {
        Err(EngineError::InternalError("refused".into()))
    }
    fn clear_graph(&self, _: NamedNodeRef<'_>) -> Result<(), EngineError> {
        Err(EngineError::InternalError("refused".into()))
    }
}

#[test]
fn a_failed_live_write_leaves_the_view_unchanged() {
    let f = fixture("rollback");
    let view = authoritative_view(&f.state, None).unwrap();
    view.insert(&session_quad(1)).unwrap();
    let snapshot = |store: &Store| {
        let mut quads: Vec<String> = store
            .quads_for_pattern(None, None, None, None)
            .flatten()
            .map(|q| q.to_string())
            .collect();
        quads.sort();
        quads
    };
    let before = snapshot(&view);
    let store = LifecycleFilteredStore::new(view.clone(), RefusingWriter);

    // Inserts one new quad and one already present: only the new one may go.
    assert!(store
        .insert_quads(&[session_quad(1), session_quad(2)])
        .is_err());
    assert_eq!(snapshot(&view), before, "failed insert rolled back");

    assert!(store.remove_quads(&[session_quad(1)]).is_err());
    assert_eq!(snapshot(&view), before, "failed remove rolled back");

    assert!(store
        .clear_graph(NamedNodeRef::new(SESSION_GRAPH).unwrap())
        .is_err());
    assert_eq!(snapshot(&view), before, "failed clear rolled back");
}

struct RecordingLlm {
    prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmClient for RecordingLlm {
    async fn chat_completion(
        &self,
        _model: &str,
        prompt: &str,
        _params: Option<&LlmParams>,
    ) -> Result<String, EngineError> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        if prompt.contains("Respond with ONLY a JSON object")
            && prompt.contains("\"intent\"")
            && prompt.contains("\"object\"")
        {
            return Ok(
                r#"{"intent":"list","object":"architectural decisions","modifiers":[]}"#
                    .to_string(),
            );
        }
        Ok(r#"{"summary":"synthetic answer","relevant_iris":[],"confidence":"high","unanswered_aspects":[]}"#.to_string())
    }
}

/// Run one chat turn over `store` and return everything that left the graph:
/// the LLM prompts and the serialized response.
async fn chat_turn(state: &AppState, store: &dyn moose::store_capability::GraphStore) -> String {
    let llm = RecordingLlm {
        prompts: Mutex::new(Vec::new()),
    };
    let request = ChatRequest {
        session_id: None,
        messages: vec![ChatMessage {
            role: Role::User,
            content: "List the architectural decisions".to_string(),
        }],
        graphs: vec![PROJECT_KG_GRAPH_IRI.to_string()],
        ontology_groups: Vec::new(),
        model: "fake-model".to_string(),
        temperature: None,
        max_tokens: None,
        stream: false,
        include_structured: true,
        include_session_map: true,
        include_metrics: false,
        llm_assist_level: LlmAssistLevel::Sensor,
        clarification_reply: None,
        reference_time: None,
        include_queries: false,
        turn_budget_ms: None,
    };
    let response = moose::chat::chat_pipeline(
        store,
        &llm,
        &state.ontology_resolver,
        &state.engine_config,
        state.session_db.as_deref().expect("chat sessions enabled"),
        Arc::new(EntityIndexCache::new(64)),
        request,
    )
    .await
    .expect("chat turn runs");
    let prompts = llm.prompts.lock().unwrap().join("\n");
    format!("{prompts}\n{}", serde_json::to_string(&response).unwrap())
}

#[tokio::test]
async fn chat_turn_never_sees_proposed_or_rejected_records() {
    let mut f = fixture("turn");
    f.state.engine_config.llm_assist_level = LlmAssistLevel::Sensor;
    f.state.enable_chat_sessions().await.unwrap();

    // Control: the same turn over the live store does reach the hidden
    // records, so the assertion below can fail.
    let leaked = chat_turn(&f.state, &f.state.store).await;
    assert!(
        leaked.contains("PROPOSED-SECRET-CHAT-TITLE") || leaked.contains(&f.proposed),
        "control: a turn over the live store reaches the proposed record"
    );

    let store = LifecycleFilteredStore::for_chat_turn(&f.state).unwrap();
    let seen = chat_turn(&f.state, &store).await;
    for secret in [
        "PROPOSED-SECRET-CHAT-TITLE",
        "REJECTED-SECRET-CHAT-TITLE",
        f.proposed.as_str(),
        f.rejected.as_str(),
    ] {
        assert!(!seen.contains(secret), "{secret} reached the chat turn");
    }
}
