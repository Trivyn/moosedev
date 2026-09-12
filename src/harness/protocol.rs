//! Shared wire types. Only the daemon interprets project knowledge and policy.
use crate::policy::PolicyDecision;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {
    pub topic: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub file: String,
    pub dossier: String,
    pub policy: PolicyDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub project_root: String,
    /// Content digest of current accepted project knowledge.
    pub revision: String,
    pub context: String,
    pub files: Vec<FileContext>,
    /// Supported durable capture protocol versions.
    #[serde(default)]
    pub capture_contracts: Vec<u32>,
    /// Supported deterministic intent-discovery contracts.
    #[serde(default)]
    pub intent_contracts: Vec<u32>,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HarnessSourcePosition {
    /// Zero-based source line.
    pub line: u32,
    /// Zero-based UTF-8 byte column, never a character or UTF-16 offset.
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HarnessSourceRange {
    /// Inclusive start position.
    pub start: HarnessSourcePosition,
    /// Exclusive end position. Insertions may have equal start and end.
    pub end: HarnessSourcePosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    pub file: String,
    #[serde(default)]
    pub before_digest: Option<String>,
    #[serde(default)]
    pub after_digest: Option<String>,
    pub changed_ranges: Vec<HarnessSourceRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRefreshPolicy {
    None,
    SupportedFrozen,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentIndexStatus {
    Current,
    Stale,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRefreshAction {
    NotRequested,
    Refreshed,
    Unsupported,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentIndexSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    pub status: IntentIndexStatus,
    pub refresh_action: IntentRefreshAction,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentScopeBasis {
    ChangedDefinition,
    EnclosingDefinition,
    ConservativeFile,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentCandidateUnresolved {
    pub file: String,
    pub reason: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub conforms: bool,
    pub durable: bool,
    pub revision: String,
    pub pending: Vec<String>,
}

/// Deterministic post-edit associations derived by the daemon from the changed
/// definition scopes and the runner's governing records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssociateRequest {
    pub files: Vec<ChangedFile>,
    /// Plan file -> governing record IRIs (the obligations derived at approval).
    pub governing: std::collections::BTreeMap<String, Vec<String>>,
    pub refresh_policy: IntentRefreshPolicy,
    pub knowledge_revision: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedBasis {
    /// The record governs the file through the approved plan's obligations.
    Obligation,
    /// The record is directly linked to a sibling definition changed in the file.
    FileDossier,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedBinding {
    pub file: String,
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub definition_range: HarnessSourceRange,
    pub scope_basis: IntentScopeBasis,
    pub source_digest: String,
    pub record_iri: String,
    pub record_kind: String,
    pub assertion_digest: String,
    pub predicate: String,
    pub basis: DerivedBasis,
    pub candidate_digest: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    TestPath,
    Parameter,
    TypeMember,
    Local,
    /// A kept definition that encloses a narrower kept definition of the same change.
    Enclosing,
    NoLegalPredicate,
    AlreadyLinked,
    NotAccepted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedScope {
    pub file: String,
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub reason: SkipReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_iri: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssociatePage {
    pub knowledge_revision: String,
    pub index: IntentIndexSnapshot,
    pub scope_digest: String,
    pub bindings: Vec<DerivedBinding>,
    pub skipped: Vec<SkippedScope>,
    /// Changed files with neither obligations nor sibling dossier records.
    pub ungoverned: Vec<String>,
    pub unresolved: Vec<IntentCandidateUnresolved>,
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
