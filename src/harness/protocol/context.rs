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
    /// Only the topic's accepted records: no inventory or file dossiers. With
    /// `max_bytes`, claims may use a reduced delivery tier. Serves the model's
    /// `search` action.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub evidence_only: bool,
    /// Maximum bytes the daemon may spend on an evidence-only model-facing
    /// `context`. `None` preserves the legacy unbounded response; ordinary
    /// file-context requests use their established dossier claim bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub file: String,
    pub dossier: String,
    pub policy: PolicyDecision,
}

/// One governing rule of the requested files: an accepted Constraint or
/// Requirement linked to their code or reached by the linked-evidence walk.
/// Past the claim budget a rule is named with an empty claim; it is never
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoverningRule {
    pub iri: String,
    pub label: String,
    /// The record kind, as the Project rules block tags it. Tasks journaled
    /// before Requirements became rules carry Constraints only, so a missing
    /// value replays as one.
    #[serde(default = "constraint_kind")]
    pub kind: String,
    /// The claim body as the shared claim renderer prints it; empty past the budget.
    pub claim: String,
    /// The `via:` line naming what reached the rule.
    pub via: String,
}

fn constraint_kind() -> String {
    "Constraint".to_string()
}

/// A typed graph record selected for one harness context response. This is the
/// human-facing representation; `ContextResponse::context` remains the exact
/// model-facing prompt text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRecord {
    pub iri: String,
    pub kind: String,
    pub title: String,
    /// Complete claim text when this retrieval supplied it. Inventory-only
    /// entries deliberately leave this empty rather than implying a claim.
    pub claim: String,
    /// Deterministic descriptions of how this retrieval selected the record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

/// How much of one selected record reached the model-facing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRecordDeliveryTier {
    FullClaim,
    FirstSentence,
    TitleOnly,
    Omitted,
}

impl ContextRecordDeliveryTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullClaim => "full_claim",
            Self::FirstSentence => "first_sentence",
            Self::TitleOnly => "title_only",
            Self::Omitted => "omitted",
        }
    }
}

/// The auditable delivery outcome for one record considered by retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRecordDelivery {
    pub iri: String,
    pub kind: String,
    pub tier: ContextRecordDeliveryTier,
    /// Deterministic explanation of why this tier was selected.
    pub reason: String,
}

/// Receipt for the exact record-aware context assembled under a byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDeliveryReceipt {
    /// Budget requested by the caller; `None` means the legacy unbounded path.
    pub max_bytes: Option<usize>,
    /// Bytes in the response's model-facing `context` string.
    pub context_bytes: usize,
    /// Every record considered for delivery, in deterministic selection order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ContextRecordDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub project_root: String,
    /// Content digest of current accepted project knowledge.
    pub revision: String,
    pub context: String,
    pub files: Vec<FileContext>,
    /// Typed records for the Knowledge view. This is additive metadata and is
    /// never substituted for the model-facing `context` string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ContextRecord>,
    /// IRIs of non-omitted records present in model-facing `context`, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_iris: Vec<String>,
    /// Record-aware delivery accounting for audit and durable task journals.
    /// Absent on responses from daemons predating bounded context delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_receipt: Option<ContextDeliveryReceipt>,
    /// Supported durable capture protocol versions.
    #[serde(default)]
    pub capture_contracts: Vec<u32>,
    /// Supported deterministic intent-discovery contracts.
    #[serde(default)]
    pub intent_contracts: Vec<u32>,
    /// The governing rules of the requested files, direct rules first and
    /// Constraints ahead of Requirements. The runner renders them as Project
    /// rules; linked evidence points there.
    #[serde(
        default,
        alias = "governing_constraints",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub governing_rules: Vec<GoverningRule>,
    /// Every specification with a current approval marker, and whether its
    /// file still matches the approved digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_specs: Vec<ApprovedSpecStatus>,
}

/// An approved specification's standing against its file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedSpecStatus {
    pub path: String,
    /// The file changed, or is gone, since the approval that governs it.
    pub stale: bool,
    /// Accepted Requirements and Constraints the approval owns.
    pub record_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub conforms: bool,
    pub durable: bool,
    pub revision: String,
    pub pending: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_budget_is_additive_but_unknown_request_fields_still_fail() {
        let legacy: ContextRequest = serde_json::from_value(serde_json::json!({
            "topic": "legacy client"
        }))
        .unwrap();
        assert_eq!(legacy.max_bytes, None);
        assert!(legacy.files.is_empty());
        assert!(!legacy.evidence_only);

        let bounded: ContextRequest = serde_json::from_value(serde_json::json!({
            "topic": "bounded search",
            "evidence_only": true,
            "max_bytes": 4096
        }))
        .unwrap();
        assert_eq!(bounded.max_bytes, Some(4096));

        assert!(serde_json::from_value::<ContextRequest>(serde_json::json!({
            "topic": "typo",
            "max_byte": 4096
        }))
        .is_err());
    }
}
