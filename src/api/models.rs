use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::adrs::{AdrSummary, AdrWarnings};
use crate::constraints::{ConstraintSummary, ConstraintWarnings};
use crate::lessons::{LessonSummary, LessonWarnings};
use crate::requirements::{RequirementSummary, RequirementWarnings};

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
    pub project_name: String,
    pub project_root: String,
    pub llm_configured: bool,
    pub llm_assist_level: String,
}

#[derive(Serialize)]
pub struct AdrListResponse {
    pub generated_at: String,
    pub graph_decisions: usize,
    pub adr_files: usize,
    pub index_filename: String,
    pub warnings: AdrWarnings,
    pub adrs: Vec<AdrSummary>,
}

#[derive(Serialize)]
pub struct AdrDetailResponse {
    pub summary: AdrSummary,
    pub markdown: String,
}

#[derive(Serialize)]
pub struct RecordDetailResponse {
    pub iri: String,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub timestamp: Option<String>,
    pub author: Option<String>,
    pub outgoing: Vec<RecordOutgoingEdge>,
    pub incoming: Vec<RecordIncomingEdge>,
}

#[derive(Serialize)]
pub struct RecordOutgoingEdge {
    pub predicate: String,
    pub target_iri: String,
    pub target_label: String,
    pub target_kind: String,
}

#[derive(Serialize)]
pub struct RecordIncomingEdge {
    pub predicate: String,
    pub source_iri: String,
    pub source_label: String,
    pub source_kind: String,
}

#[derive(Serialize)]
pub struct ConstraintListResponse {
    pub generated_at: String,
    pub graph_constraints: usize,
    pub constraint_files: usize,
    pub index_filename: String,
    pub warnings: ConstraintWarnings,
    pub constraints: Vec<ConstraintSummary>,
}

#[derive(Serialize)]
pub struct ConstraintDetailResponse {
    pub summary: ConstraintSummary,
    pub markdown: String,
}

#[derive(Serialize)]
pub struct WhyCoverageResponse {
    pub components: Vec<ComponentCoverageDto>,
    /// Public-surface definitions whose path maps to no component.
    pub unmapped: usize,
}

#[derive(Serialize)]
pub struct ComponentCoverageDto {
    pub iri: Option<String>,
    pub name: String,
    pub numerator: usize,
    pub denominator: usize,
    /// Documented fraction, or null when the component owns no public surface.
    pub coverage: Option<f64>,
    pub undocumented: Vec<String>,
}

#[derive(Serialize)]
pub struct ProposalListResponse {
    pub proposals: Vec<ProposalDto>,
}

#[derive(Serialize)]
pub struct ProposalDto {
    /// Minted UUID (last IRI segment) — the id in accept/reject routes.
    pub id: String,
    pub iri: String,
    pub label: String,
    pub subject_iri: String,
    pub predicate: String,
    pub target_symbol: String,
    pub target_path: String,
    pub evidence: Option<String>,
    pub status: String,
}

#[derive(Serialize)]
pub struct ProposalActionResponse {
    pub id: String,
    pub status: String,
    /// Set on accept: the code entity the materialized link points at.
    pub entity_iri: Option<String>,
    pub entity_name: Option<String>,
}

#[derive(Serialize)]
pub struct RequirementListResponse {
    pub generated_at: String,
    pub graph_requirements: usize,
    pub requirement_files: usize,
    pub index_filename: String,
    pub warnings: RequirementWarnings,
    pub requirements: Vec<RequirementSummary>,
}

#[derive(Serialize)]
pub struct RequirementDetailResponse {
    pub summary: RequirementSummary,
    pub markdown: String,
}

#[derive(Serialize)]
pub struct LessonListResponse {
    pub generated_at: String,
    pub graph_lessons: usize,
    pub lesson_files: usize,
    pub index_filename: String,
    pub warnings: LessonWarnings,
    pub lessons: Vec<LessonSummary>,
}

#[derive(Serialize)]
pub struct LessonDetailResponse {
    pub summary: LessonSummary,
    pub markdown: String,
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
