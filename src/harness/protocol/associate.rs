//! Deterministic post-edit associations: the substrate index the daemon
//! derived from, the bindings it proposes, and the scopes it skipped.
use super::{ChangedFile, HarnessSourceRange, IntentScopeBasis};
use serde::{Deserialize, Serialize};

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
pub struct IntentCandidateUnresolved {
    pub file: String,
    pub reason: String,
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
