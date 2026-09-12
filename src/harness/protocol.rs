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
    /// None identifies older daemons that cannot supply typed capture choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_targets: Option<CaptureTargets>,
    /// Supported durable capture protocol versions. Absent on legacy daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_contracts: Option<Vec<u32>>,
    /// Supported deterministic intent-discovery contracts. Absent on legacy daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_contracts: Option<Vec<u32>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteClaim {
    pub literals: Vec<CandidateLiteral>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurposeCandidate {
    pub handle: String,
    pub iri: String,
    pub kind: String,
    pub title: String,
    pub claim: CompleteClaim,
    pub lifecycle: String,
    pub assertion_digest: String,
    pub relations: Vec<CandidateRelation>,
    pub legal_predicates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurposeRetrieval {
    Page,
    ExhaustedEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurposeCandidateRequest {
    pub objective: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurposeCandidatePage {
    pub revision: String,
    pub candidates: Vec<PurposeCandidate>,
    pub retrieval: PurposeRetrieval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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
pub struct PostEditCandidate {
    pub id: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_range: Option<HarnessSourceRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_range: Option<HarnessSourceRange>,
    pub source_digest: String,
    pub scope_basis: IntentScopeBasis,
    pub candidate_digest: String,
    pub existing_record_iris: Vec<String>,
    pub record_choices: Vec<PurposeCandidate>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentCandidateUnresolved {
    pub file: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentCandidateRequest {
    pub files: Vec<ChangedFile>,
    pub refresh_policy: IntentRefreshPolicy,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentCandidatePage {
    pub knowledge_revision: String,
    pub index: IntentIndexSnapshot,
    pub scope_digest: String,
    pub candidates: Vec<PostEditCandidate>,
    pub unresolved: Vec<IntentCandidateUnresolved>,
    /// Historical definition evidence for deleted files. These are audit
    /// scopes, never candidates for a new CodeEntity link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<DeletedDefinitionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedDefinitionScope {
    pub file: String,
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub definition_range: HarnessSourceRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_range: Option<HarnessSourceRange>,
    pub before_digest: String,
    pub scope_digest: String,
}

/// Bounded choices from the same knowledge snapshot as context and dossiers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureTargets {
    pub components: Vec<CaptureTarget>,
    pub records: Vec<CaptureTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub iri: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    /// Globally unique stable operation ID, reused verbatim after interruption.
    pub operation_id: String,
    pub proposals: Vec<KnowledgeProposal>,
}

/// Versioned capture request for harness tasks that need durable ownership.
/// The legacy capture route deliberately has no implicit owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureV2Request {
    pub operation_id: String,
    pub owner_id: String,
    pub proposals: Vec<KnowledgeProposal>,
    /// Durable semantic dispositions authorizing otherwise colliding proposals.
    #[serde(default)]
    pub reconciliation_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureCollision {
    pub proposal_index: usize,
    pub candidate_iris: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CaptureV2Response {
    Captured { capture: CaptureResponse },
    ReconciliationRequired { collisions: Vec<CaptureCollision> },
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureDisposition {
    ReuseUnchanged,
    ReviseProposal,
    DistinctKnowledge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileCaptureRequest {
    pub operation_id: String,
    pub owner_id: String,
    pub proposal: KnowledgeProposal,
    pub candidate_iri: String,
    pub candidate_digest: String,
    pub candidate_revision: String,
    pub disposition: CaptureDisposition,
    #[serde(default)]
    pub replacement_proposal: Option<KnowledgeProposal>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcileCaptureResponse {
    pub operation_id: String,
    pub disposition: CaptureDisposition,
    pub candidate_iri: String,
    pub requires_human_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_capture_operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileReviewRequest {
    pub operation_id: String,
    pub accept: bool,
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
