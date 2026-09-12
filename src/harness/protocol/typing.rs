//! Typing one prose note into reconciled proposals: the request context,
//! typing mode, proposal origins and dispositions.
use super::{KnowledgeProposal, ReconcileThresholds};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub command: String,
    pub success: bool,
    /// The check ran after at least one applied edit.
    pub after_edit: bool,
}

/// Turn one prose note plus the task's plan, diff and check history into
/// typed, reconciled proposals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureTypeRequest {
    pub owner_id: String,
    pub operation_id: String,
    pub note: String,
    /// Evidence references the runner journaled for the note.
    pub note_evidence: Vec<String>,
    pub plan_summary: String,
    pub changed_files: Vec<String>,
    pub check_history: Vec<CheckOutcome>,
    pub knowledge_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypingMode {
    SymbolicOnly,
    Sensor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOrigin {
    SymbolicDecision,
    SymbolicLesson,
    LlmSensor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypedDisposition {
    /// Durable receipt only: no record, no review.
    Restates {
        candidate_iri: String,
        score: f64,
        confidence: f64,
        receipt_operation_id: String,
    },
    /// Proposal plus a confidence-annotated `refines` edge at capture.
    Refines {
        candidate_iri: String,
        score: f64,
        containment: f64,
        confidence: f64,
        receipt_operation_id: String,
    },
    Distinct {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nearest_iri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        score: Option<f64>,
        receipt_operation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedProposal {
    pub proposal: KnowledgeProposal,
    pub origin: ProposalOrigin,
    pub disposition: TypedDisposition,
    /// `symbolic`, or `llm_sensor` when a band tiebreak was resolved by the sensor.
    pub resolved_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureTypeResponse {
    pub revision: String,
    pub typing_mode: TypingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typing_note: Option<String>,
    pub thresholds: ReconcileThresholds,
    pub proposals: Vec<TypedProposal>,
}
