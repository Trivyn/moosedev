//! Context and checkpoint exchange: what the daemon tells the runner about
//! the files it reads and the durability of accepted knowledge.
use crate::policy::PolicyDecision;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {
    pub topic: String,
    #[serde(default)]
    pub files: Vec<String>,
    /// Only the topic's accepted records with their complete claims: no
    /// inventory and no file dossiers. Serves the model's `search` action.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub evidence_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub file: String,
    pub dossier: String,
    pub policy: PolicyDecision,
}

/// One governing rule of the requested files: an accepted Constraint linked to
/// their code or reached by the linked-evidence walk. Past the claim limit a
/// rule is named with an empty claim; it is never dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoverningConstraint {
    pub iri: String,
    pub label: String,
    /// The claim body as the shared claim renderer prints it; empty past the limit.
    pub claim: String,
    /// The `via:` line naming what reached the rule.
    pub via: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub project_root: String,
    /// Content digest of current accepted project knowledge.
    pub revision: String,
    pub context: String,
    pub files: Vec<FileContext>,
    /// IRIs of the records an evidence-only request returned, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_iris: Vec<String>,
    /// Supported durable capture protocol versions.
    #[serde(default)]
    pub capture_contracts: Vec<u32>,
    /// Supported deterministic intent-discovery contracts.
    #[serde(default)]
    pub intent_contracts: Vec<u32>,
    /// The governing rules of the requested files, direct rules first. The
    /// runner renders them as Project rules; linked evidence points there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub governing_constraints: Vec<GoverningConstraint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub conforms: bool,
    pub durable: bool,
    pub revision: String,
    pub pending: Vec<String>,
}
