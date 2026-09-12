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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub conforms: bool,
    pub durable: bool,
    pub revision: String,
    pub pending: Vec<String>,
}
