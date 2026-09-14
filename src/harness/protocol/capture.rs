//! Knowledge capture: proposals, owned capture operations, collisions,
//! captured records and their human review.
use serde::{Deserialize, Serialize};

use super::scope::ChangedFile;

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
        matches!(self.kind.as_str(), "Requirement" | "Constraint") || self.changes_lifecycle()
    }

    /// Supersedes or retracts existing knowledge: its acceptance changes
    /// another record's lifecycle and is never attested as the task's own.
    pub fn changes_lifecycle(&self) -> bool {
        self.supersedes.is_some() || self.retracts.is_some()
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
    /// The task's changed files with hunk geometry: capture anchors knowledge
    /// to the definitions these hunks touch (capture contract 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<ChangedFile>,
    /// Existing records the note restated, with their receipts: capture links
    /// each to the definitions its files' hunks touch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restated: Vec<RestatedCandidate>,
}

/// A restated note's existing record, carried with the reconciliation receipt
/// that found it so the daemon can prove the restatement before linking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestatedCandidate {
    pub candidate_iri: String,
    pub receipt_operation_id: String,
    pub files: Vec<String>,
}

impl CaptureRequest {
    pub fn has_governing(&self) -> bool {
        self.proposals.iter().any(KnowledgeProposal::is_governing)
    }

    pub fn has_lifecycle_change(&self) -> bool {
        self.proposals
            .iter()
            .any(KnowledgeProposal::changes_lifecycle)
    }
}

/// Owned capture request: every harness capture names the task that owns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureV2Request {
    pub operation_id: String,
    pub owner_id: String,
    pub proposals: Vec<KnowledgeProposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<ChangedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restated: Vec<RestatedCandidate>,
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
    /// The code entities the queued links target, in link order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<CaptureAnchor>,
    /// Why a file's anchoring is weaker than its hunks: coalesced ranges, an
    /// unproven index, an ambiguous span or a capped anchor count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchor_notes: Vec<AnchorNote>,
}

/// One code anchor the daemon resolved for a captured proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAnchor {
    pub file: String,
    /// Version-normalized SCIP symbol the queued link targets.
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub basis: AnchorBasis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorBasis {
    /// A leaf definition a hunk of the file's change touches.
    Definition,
    /// The file's module: the fallback when no definition anchor resolved.
    Module,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorNote {
    pub file: String,
    pub note: AnchorNoteKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorNoteKind {
    /// The runner coalesced this file's hunks into one prefix/suffix range.
    RangesCoalesced,
    /// The loaded index proves neither the changed nor the original source.
    IndexUnproven,
    /// Leaves of different symbols share one span; neither is picked.
    AnchorAmbiguous,
    /// More definitions changed than a file or a proposal anchors.
    AnchorOverflow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub proposals: Vec<CapturedProposal>,
    /// Links queued from restated existing records, one entry per record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restated: Vec<RestatedLinks>,
}

/// The code links a capture queued for one restated existing record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestatedLinks {
    pub candidate_iri: String,
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<CaptureAnchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchor_notes: Vec<AnchorNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub operation_id: String,
    pub accept: bool,
}
