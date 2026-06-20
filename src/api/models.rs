use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub project_graph: String,
    pub data_dir: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessagePayload {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequestPayload {
    #[serde(default)]
    pub session_id: Option<String>,
    pub messages: Vec<ChatMessagePayload>,
    #[serde(default)]
    pub include_structured: bool,
    #[serde(default = "default_true")]
    pub include_session_map: bool,
    #[serde(default = "default_true")]
    pub include_metrics: bool,
    #[serde(default = "default_llm_assist_level")]
    pub llm_assist_level: u8,
    #[serde(default)]
    pub clarification_reply: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

fn default_llm_assist_level() -> u8 {
    1
}

#[derive(Serialize)]
pub struct ChatSessionListResponse {
    pub sessions: Vec<ChatSessionSummaryPayload>,
    pub count: usize,
}

#[derive(Serialize)]
pub struct ChatSessionSummaryPayload {
    pub session_id: String,
    pub turn_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_user_message: Option<String>,
}

#[derive(Serialize)]
pub struct ChatSessionDetailResponse {
    pub session_id: String,
    pub turn_count: u32,
    pub messages: Vec<ChatMessagePayload>,
    pub focus_stack: serde_json::Value,
    pub session_subgraph: QueryResponse,
}

#[derive(Deserialize)]
pub struct SparqlQueryRequest {
    pub query: String,
}

#[derive(Serialize, Clone)]
pub struct QueryResponse {
    pub query_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<QueryHead>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<QueryResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boolean: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triples: Option<Vec<TriplePayload>>,
}

#[derive(Serialize, Clone)]
pub struct QueryHead {
    pub vars: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct QueryResults {
    pub bindings: Vec<QueryBinding>,
}

#[derive(Serialize, Clone)]
pub struct QueryBinding {
    #[serde(flatten)]
    pub bindings: HashMap<String, QueryValue>,
}

#[derive(Serialize, Clone)]
pub struct QueryValue {
    #[serde(rename = "type")]
    pub value_type: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

impl QueryValue {
    pub fn uri(value: impl Into<String>) -> Self {
        Self {
            value_type: "uri".to_string(),
            value: value.into(),
            datatype: None,
            lang: None,
        }
    }

    pub fn bnode(value: impl Into<String>) -> Self {
        Self {
            value_type: "bnode".to_string(),
            value: value.into(),
            datatype: None,
            lang: None,
        }
    }

    pub fn literal(
        value: impl Into<String>,
        datatype: impl Into<String>,
        lang: Option<String>,
    ) -> Self {
        Self {
            value_type: "literal".to_string(),
            value: value.into(),
            datatype: Some(datatype.into()),
            lang,
        }
    }

    pub fn unknown(value: impl Into<String>) -> Self {
        Self {
            value_type: "unknown".to_string(),
            value: value.into(),
            datatype: None,
            lang: None,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct TriplePayload {
    pub subject: QueryValue,
    pub predicate: QueryValue,
    pub object: QueryValue,
}
