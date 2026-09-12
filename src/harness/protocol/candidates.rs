//! Reconciliation candidates and the frozen scoring thresholds every
//! receipt records.
use super::KnowledgeProposal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCandidateRequest {
    pub owner_id: String,
    pub proposal: KnowledgeProposal,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateLiteral {
    pub predicate: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateRelation {
    pub predicate: String,
    pub target_iri: String,
    /// True when the candidate is the object rather than the subject.
    pub incoming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateOrigin {
    pub operation_id: String,
    pub owner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegalRelationDirection {
    Forward,
    Inverse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegalRelationChoice {
    pub predicate: String,
    pub direction: LegalRelationDirection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureCandidate {
    pub iri: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub assertion_digest: String,
    pub literals: Vec<CandidateLiteral>,
    pub relations: Vec<CandidateRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CandidateOrigin>,
    pub owned_by_requester: bool,
    pub exact_title: bool,
    pub legal_relations: Vec<LegalRelationChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureCandidatePage {
    /// Digest of all accepted and proposed project assertions in this snapshot.
    pub revision: String,
    pub proposal_digest: String,
    pub candidates: Vec<CaptureCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Frozen default reconciliation thresholds, overridable through environment
/// only and recorded in every receipt.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReconcileThresholds {
    pub restates: f64,
    pub refines: f64,
    pub refines_containment: f64,
    pub tiebreak_band: f64,
}

impl Default for ReconcileThresholds {
    fn default() -> Self {
        Self {
            restates: 0.80,
            refines: 0.55,
            refines_containment: 0.60,
            tiebreak_band: 0.08,
        }
    }
}
