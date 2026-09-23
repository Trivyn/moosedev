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
    /// The records governing the plan's files, as the runner derived them at
    /// approval. Capture derives `isMotivatedBy`/`learnedFrom` from these, so
    /// the edge is drawn from the obligations the human actually approved
    /// against rather than recomputed against a graph that may have moved.
    /// `#[serde(default)]`: an older runner omits it and simply derives nothing.
    #[serde(default)]
    pub obligation_iris: Vec<String>,
    /// The digest of that obligation set at approval, carried so a derived
    /// edge can be tied back to the approval it came from.
    #[serde(default)]
    pub obligations_digest: String,
    /// Labels of the Constraints and Requirements that governed the task.
    /// A proposal whose claim names one is marked for the reviewer, since
    /// accepting it records a decision about a governing rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub governing_labels: Vec<String>,
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
    /// What the obligation-based derivation decided for this proposal, drawn or
    /// not. Journaled so a record without `isMotivatedBy` can be read as "the
    /// plan offered two and choosing was a judgement call" rather than "nothing
    /// was tried" — the two were indistinguishable before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<DerivedRelation>,
    /// Governing rules the proposal's own claim names (not its approved-plan
    /// paragraph). Shown at review: a record such as "defer the database
    /// constraint" is exactly what a later task could cite to excuse ignoring
    /// that rule, so the human should see which rules it speaks about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names_rules: Vec<String>,
}

/// One obligation-derived relation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedRelation {
    pub predicate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
    pub candidates_considered: usize,
    /// `asserted`, `none_legal`, or `ambiguous`.
    pub reason: String,
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
