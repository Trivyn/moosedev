use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use moose::chat::session_db::SessionDb;
use moose::types::{ChatMessage, ChatRequest, LlmAssistLevel, Role};
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

use crate::api::error::ApiError;
use crate::api::models::{
    ChatMessagePayload, ChatRequestPayload, ChatSessionDetailResponse, ChatSessionListResponse,
    ChatSessionSummaryPayload, QueryBinding, QueryHead, QueryResponse, QueryResults, QueryValue,
};
use crate::graph::AppState;

const RDFS_LABEL: &str = moose::RDFS_LABEL;
const RDF_TYPE: &str = moose::RDF_TYPE;

/// Run one real MOOSE chat turn over the project knowledge graph.
///
/// This is intentionally not a thin wrapper around MOOSEDev's MCP `query`
/// tool. The human UI should exercise MOOSE's session machinery: focus stack,
/// transcript persistence, pending clarifications, and per-session graph
/// materialization.
pub async fn chat(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ChatRequestPayload>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if payload.messages.is_empty() {
        return Err(ApiError::bad_request("messages must not be empty"));
    }
    if !state.llm_configured {
        return Err(ApiError::unavailable(
            "MOOSE chat requires an explicit LLM provider; set MOOSEDEV_LLM_BASE_URL to enable chat",
        ));
    }
    let session_db = chat_session_db(&state)?;
    let clarification_reply = match payload.clarification_reply {
        Some(value) => Some(serde_json::from_value(value).map_err(|e| {
            ApiError::bad_request(format!("invalid clarification_reply payload: {e}"))
        })?),
        None => None,
    };

    // Web chat is scoped to the project KG for v1. There is no graph selector:
    // MOOSEDev's product surface is project memory, not a general Trivyn graph
    // workbench.
    let request = ChatRequest {
        session_id: payload.session_id,
        messages: payload.messages.into_iter().map(to_moose_message).collect(),
        graphs: vec![crate::graph::PROJECT_KG_GRAPH_IRI.to_string()],
        ontology_groups: Vec::new(),
        model: state.model.clone(),
        temperature: None,
        max_tokens: None,
        stream: false,
        include_structured: payload.include_structured,
        include_session_map: payload.include_session_map,
        include_metrics: payload.include_metrics,
        llm_assist_level: assist_level(payload.llm_assist_level),
        clarification_reply,
    };

    let llm = state.llm.with_fresh_usage();
    let response = moose::chat::chat_pipeline(
        &state.store,
        &llm,
        &state.ontology_resolver,
        &state.engine_config,
        session_db,
        state.entity_index.clone(),
        request,
    )
    .await
    .map_err(|e| ApiError::internal(format!("chat failed: {e}")))?;

    let session_id = response.moose.as_ref().map(|ext| ext.session_id.clone());
    let mut value = serde_json::to_value(response)
        .map_err(|e| ApiError::internal(format!("serialize chat response: {e}")))?;
    if let Some(session_id) = session_id {
        // `MooseExtension` is owned by the engine and does not have a host
        // extension map. Inject the UI-only `session_subgraph` after serializing
        // rather than forking or widening the engine type.
        let subgraph = session_subgraph(&state, &session_id);
        if let Some(moose) = value
            .get_mut("moose")
            .and_then(serde_json::Value::as_object_mut)
        {
            moose.insert(
                "session_subgraph".to_string(),
                serde_json::to_value(subgraph)
                    .map_err(|e| ApiError::internal(format!("serialize session subgraph: {e}")))?,
            );
        }
    }

    Ok(Json(value))
}

pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ChatSessionListResponse>, ApiError> {
    let session_db = chat_session_db(&state)?;
    let sessions = moose::chat::list_sessions(session_db)
        .await
        .map_err(|e| ApiError::internal(format!("list sessions: {e}")))?;
    let sessions: Vec<ChatSessionSummaryPayload> = sessions
        .into_iter()
        .map(|s| ChatSessionSummaryPayload {
            session_id: s.session_id,
            turn_count: s.turn_count,
            created_at: s.created_at,
            updated_at: s.updated_at,
            last_user_message: s.last_user_message,
        })
        .collect();
    Ok(Json(ChatSessionListResponse {
        count: sessions.len(),
        sessions,
    }))
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ChatSessionDetailResponse>, ApiError> {
    let session_db = chat_session_db(&state)?;
    let Some((ctx, messages)) = moose::chat::get_session(session_db, &session_id)
        .await
        .map_err(|e| ApiError::internal(format!("get session: {e}")))?
    else {
        return Err(ApiError::not_found("chat session not found"));
    };
    let messages = messages.into_iter().map(from_moose_message).collect();
    let focus_stack = serde_json::to_value(&ctx.focus_stack)
        .map_err(|e| ApiError::internal(format!("serialize focus stack: {e}")))?;
    Ok(Json(ChatSessionDetailResponse {
        session_id,
        turn_count: ctx.turn_count,
        messages,
        focus_stack,
        session_subgraph: session_subgraph(&state, &ctx.session_id.to_string()),
    }))
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_db = chat_session_db(&state)?;
    let deleted = moose::chat::delete_session(session_db, &session_id, &state.store)
        .await
        .map_err(|e| ApiError::internal(format!("delete session: {e}")))?;
    if !deleted {
        return Err(ApiError::not_found("chat session not found"));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

fn chat_session_db(state: &AppState) -> Result<&SessionDb, ApiError> {
    state
        .session_db
        .as_deref()
        .ok_or_else(|| ApiError::unavailable("MOOSE chat sessions are not enabled"))
}

fn to_moose_message(message: ChatMessagePayload) -> ChatMessage {
    let role = match message.role.trim().to_ascii_lowercase().as_str() {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        _ => Role::User,
    };
    ChatMessage {
        role,
        content: message.content,
    }
}

fn from_moose_message(message: ChatMessage) -> ChatMessagePayload {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    ChatMessagePayload {
        role: role.to_string(),
        content: message.content,
    }
}

fn assist_level(value: u8) -> LlmAssistLevel {
    match value {
        0 => LlmAssistLevel::PureSymbolic,
        2 => LlmAssistLevel::RelaxedExtraction,
        3 => LlmAssistLevel::AssistedPlanning,
        4 => LlmAssistLevel::AssistedValidation,
        5 => LlmAssistLevel::FallbackExecutor,
        _ => LlmAssistLevel::Standard,
    }
}

/// Return the materialized per-session named graph in the same SELECT-like shape
/// the UI graph converter already understands. MOOSE owns this graph; MOOSEDev
/// only exposes it for visualization.
fn session_subgraph(state: &AppState, session_id: &str) -> QueryResponse {
    let graph_iri = format!("urn:moose:session:{session_id}");
    let Ok(graph) = NamedNodeRef::new(&graph_iri) else {
        return empty_query_response();
    };
    let mut bindings = Vec::new();
    let mut seen = HashSet::new();
    let mut uri_terms = HashSet::new();
    for quad in state
        .store
        .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(graph)))
        .flatten()
    {
        let subject = named_or_blank_value(&quad.subject);
        let predicate = named_value(quad.predicate.as_str());
        let object = term_value(&quad.object);
        collect_uri(&mut uri_terms, &subject);
        collect_uri(&mut uri_terms, &predicate);
        collect_uri(&mut uri_terms, &object);
        push_binding(&mut bindings, &mut seen, subject, predicate, object);
    }
    enrich_session_terms(state, &mut bindings, &mut seen, &uri_terms);
    QueryResponse {
        query_type: "SELECT".to_string(),
        head: Some(QueryHead {
            vars: vec![
                "subject".to_string(),
                "predicate".to_string(),
                "object".to_string(),
            ],
        }),
        results: Some(QueryResults { bindings }),
        boolean: None,
        triples: None,
    }
}

fn enrich_session_terms(
    state: &AppState,
    bindings: &mut Vec<QueryBinding>,
    seen: &mut HashSet<String>,
    uri_terms: &HashSet<String>,
) {
    for iri in uri_terms {
        let Ok(subject) = NamedNodeRef::new(iri) else {
            continue;
        };
        for predicate_iri in session_display_predicates(state) {
            let Ok(predicate) = NamedNodeRef::new(predicate_iri) else {
                continue;
            };
            for quad in state
                .store
                .quads_for_pattern(Some(subject.into()), Some(predicate), None, None)
                .flatten()
            {
                push_binding(
                    bindings,
                    seen,
                    named_value(iri),
                    named_value(predicate_iri),
                    term_value(&quad.object),
                );
            }
        }
    }
}

fn session_display_predicates(state: &AppState) -> [&str; 7] {
    [
        RDFS_LABEL,
        RDF_TYPE,
        state.capture.title.as_str(),
        state.capture.description.as_str(),
        state.capture.status.as_str(),
        state.capture.author.as_str(),
        state.capture.timestamp.as_str(),
    ]
}

fn collect_uri(out: &mut HashSet<String>, value: &QueryValue) {
    if value.value_type == "uri" {
        out.insert(value.value.clone());
    }
}

fn push_binding(
    bindings: &mut Vec<QueryBinding>,
    seen: &mut HashSet<String>,
    subject: QueryValue,
    predicate: QueryValue,
    object: QueryValue,
) {
    let key = binding_key(&subject, &predicate, &object);
    if !seen.insert(key) {
        return;
    }
    let mut row = HashMap::new();
    row.insert("subject".to_string(), subject);
    row.insert("predicate".to_string(), predicate);
    row.insert("object".to_string(), object);
    bindings.push(QueryBinding { bindings: row });
}

fn binding_key(subject: &QueryValue, predicate: &QueryValue, object: &QueryValue) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        subject.value_type,
        subject.value,
        predicate.value,
        object.value_type,
        object.value,
        object.datatype.as_deref().unwrap_or(""),
        object.lang.as_deref().unwrap_or("")
    )
}

fn empty_query_response() -> QueryResponse {
    QueryResponse {
        query_type: "SELECT".to_string(),
        head: Some(QueryHead {
            vars: vec!["subject".into(), "predicate".into(), "object".into()],
        }),
        results: Some(QueryResults {
            bindings: Vec::new(),
        }),
        boolean: None,
        triples: None,
    }
}

fn named_or_blank_value(value: &oxigraph::model::NamedOrBlankNode) -> QueryValue {
    match value {
        oxigraph::model::NamedOrBlankNode::NamedNode(node) => named_value(node.as_str()),
        oxigraph::model::NamedOrBlankNode::BlankNode(node) => QueryValue::bnode(node.as_str()),
    }
}

fn named_value(value: &str) -> QueryValue {
    QueryValue::uri(value)
}

fn term_value(value: &Term) -> QueryValue {
    match value {
        Term::NamedNode(node) => named_value(node.as_str()),
        Term::BlankNode(node) => QueryValue::bnode(node.as_str()),
        Term::Literal(lit) => QueryValue::literal(
            lit.value(),
            lit.datatype().as_str(),
            lit.language().map(str::to_string),
        ),
        #[allow(unreachable_patterns)]
        _ => QueryValue::unknown(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use oxigraph::model::{GraphName, Literal, NamedNode, Quad};

    use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

    const SESSION_ID: &str = "session-enrichment-test";
    const SESSION_GRAPH: &str = "urn:moose:session:session-enrichment-test";
    const MOOSE_NS: &str = "https://trivyn.io/ontologies/moose#";

    fn ontology_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-chat-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn insert_uri(state: &AppState, subject: &str, predicate: &str, object: &str, graph: &str) {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(subject).unwrap(),
                NamedNode::new(predicate).unwrap(),
                NamedNode::new(object).unwrap(),
                GraphName::NamedNode(NamedNode::new(graph).unwrap()),
            ))
            .expect("insert uri quad");
    }

    fn insert_literal(state: &AppState, subject: &str, predicate: &str, value: &str, graph: &str) {
        state
            .store
            .insert(&Quad::new(
                NamedNode::new(subject).unwrap(),
                NamedNode::new(predicate).unwrap(),
                Literal::new_simple_literal(value),
                GraphName::NamedNode(NamedNode::new(graph).unwrap()),
            ))
            .expect("insert literal quad");
    }

    fn response_has_literal(
        response: &QueryResponse,
        subject: &str,
        predicate: &str,
        value: &str,
    ) -> bool {
        response
            .results
            .as_ref()
            .expect("results")
            .bindings
            .iter()
            .any(|binding| {
                let row = &binding.bindings;
                row.get("subject").map(|v| v.value.as_str()) == Some(subject)
                    && row.get("predicate").map(|v| v.value.as_str()) == Some(predicate)
                    && row.get("object").map(|v| v.value.as_str()) == Some(value)
            })
    }

    #[test]
    fn session_subgraph_enriches_referenced_project_record_display_fields() {
        let dir = temp_dir("project-labels");
        let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
        let record = "https://moosedev.dev/kg/ArchitecturalDecision/session-ui-label-test";
        let answer = format!("{SESSION_GRAPH}/answer/1");

        insert_uri(
            &state,
            &answer,
            &format!("{MOOSE_NS}resultEntity"),
            record,
            SESSION_GRAPH,
        );
        insert_literal(
            &state,
            record,
            RDFS_LABEL,
            "Readable project label",
            PROJECT_KG_GRAPH_IRI,
        );
        insert_literal(
            &state,
            record,
            &state.capture.description,
            "Readable project description",
            PROJECT_KG_GRAPH_IRI,
        );
        insert_literal(
            &state,
            record,
            &state.capture.status,
            "accepted",
            PROJECT_KG_GRAPH_IRI,
        );
        insert_literal(
            &state,
            "https://moosedev.dev/kg/ArchitecturalDecision/unrelated",
            RDFS_LABEL,
            "Unrelated label",
            PROJECT_KG_GRAPH_IRI,
        );

        let response = session_subgraph(&state, SESSION_ID);

        assert!(response_has_literal(
            &response,
            record,
            RDFS_LABEL,
            "Readable project label"
        ));
        assert!(response_has_literal(
            &response,
            record,
            &state.capture.description,
            "Readable project description"
        ));
        assert!(response_has_literal(
            &response,
            record,
            &state.capture.status,
            "accepted"
        ));
        assert!(
            !response_has_literal(
                &response,
                "https://moosedev.dev/kg/ArchitecturalDecision/unrelated",
                RDFS_LABEL,
                "Unrelated label"
            ),
            "enrichment should not expand to records absent from the session graph"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_subgraph_enriches_referenced_moose_ontology_labels() {
        let dir = temp_dir("moose-labels");
        let state = AppState::bootstrap(&dir, &ontology_dir()).expect("bootstrap app state");
        let run = format!("{SESSION_GRAPH}/execution/1/stage-run/0");
        let stage_run_class = format!("{MOOSE_NS}StageRun");

        insert_uri(&state, &run, RDF_TYPE, &stage_run_class, SESSION_GRAPH);

        let response = session_subgraph(&state, SESSION_ID);

        assert!(response_has_literal(
            &response,
            &stage_run_class,
            RDFS_LABEL,
            "Stage Run"
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
