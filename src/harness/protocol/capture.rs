//! Knowledge capture: proposals, owned capture operations, collisions,
//! captured records and their human review.
use serde::{Deserialize, Serialize};

/// The knowledge record classes the harness captures and reconciles.
pub const RECORD_KINDS: [&str; 6] = [
    "ArchitecturalDecision",
    "Requirement",
    "Constraint",
    "Lesson",
    "Pattern",
    "AntiPattern",
];

pub fn is_record_kind(kind: &str) -> bool {
    RECORD_KINDS.contains(&kind)
}

/// A current knowledge record offered to the link path by IRI, label and kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub iri: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProposal {
    pub kind: String,
    pub title: String,
    pub description: String,
    /// Literal contemporaneous evidence references supplied by the runner.
    pub evidence: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub requirement: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub retracts: Option<String>,
    /// Relations the daemon derived for this proposal from a durable
    /// reconciliation receipt (today only `refines`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconciled: Vec<ReconciledRelation>,
}

impl KnowledgeProposal {
    /// Governing knowledge blocks further work until a human reviews it.
    pub fn is_governing(&self) -> bool {
        matches!(self.kind.as_str(), "Requirement" | "Constraint")
            || self.supersedes.is_some()
            || self.retracts.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciledRelation {
    pub predicate: String,
    pub target_iri: String,
    pub confidence: f64,
    pub receipt_operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    /// Globally unique stable operation ID, reused verbatim after interruption.
    pub operation_id: String,
    pub proposals: Vec<KnowledgeProposal>,
}

impl CaptureRequest {
    pub fn has_governing(&self) -> bool {
        self.proposals.iter().any(KnowledgeProposal::is_governing)
    }
}

/// Owned capture request: every harness capture names the task that owns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureV2Request {
    pub operation_id: String,
    pub owner_id: String,
    pub proposals: Vec<KnowledgeProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureCollision {
    pub proposal_index: usize,
    pub candidate_iris: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CaptureV2Response {
    Captured {
        capture: CaptureResponse,
    },
    /// A proposal title already names current or pending knowledge; the
    /// caller retypes under a qualified title rather than duplicating.
    Collision {
        collisions: Vec<CaptureCollision>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedProposal {
    pub iri: String,
    pub title: String,
    pub kind: String,
    pub links: Vec<String>,
    #[serde(default)]
    pub unanchored: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub proposals: Vec<CapturedProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub operation_id: String,
    pub accept: bool,
}
