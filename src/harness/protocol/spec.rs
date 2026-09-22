//! Explicit specification approval: immutable previews followed by one
//! revision-checked, durable graph transition.
use serde::{Deserialize, Serialize};

use super::CheckpointResponse;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecRecordDraft {
    pub kind: String,
    pub title: String,
    pub description: String,
    /// Repo-relative `path:line` or `path:start-end` references.
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPrepareRequest {
    pub operation_id: String,
    pub owner_id: String,
    pub path: String,
    pub source_sha256: String,
    pub knowledge_revision: String,
    pub drafts: Vec<SpecRecordDraft>,
    /// Repository paths the specification governs: directories end in `/`,
    /// a file path is exact, `.` is the whole project. Empty leaves the
    /// records unanchored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
}

/// Ask for the current approval of a specification path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecCurrentRequest {
    pub path: String,
}

/// The current approval of a specification path, with its active records
/// rebuilt as drafts: a source whose digest is unchanged can be prepared
/// again from these without extraction, and every entry then reuses its
/// record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecCurrentResponse {
    pub path: String,
    /// The accepted approval marker, when the path has one.
    pub approval_iri: Option<String>,
    /// The source digest that approval recorded.
    pub source_sha256: Option<String>,
    pub drafts: Vec<SpecRecordDraft>,
}

/// The SystemComponent an approval anchors its records to, minted or reused
/// at approval so the records govern every file under its paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecComponentPlan {
    pub iri: String,
    pub name: String,
    /// Minted by this approval rather than reused.
    pub new: bool,
    /// Every path the component covers after approval.
    pub covers: Vec<String>,
    /// The paths this approval adds to an existing component.
    pub added: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpecDisposition {
    New { iri: String },
    Reuse { iri: String },
    Supersede { iri: String, previous_iri: String },
}

impl SpecDisposition {
    pub fn active_iri(&self) -> &str {
        match self {
            Self::New { iri } | Self::Reuse { iri } | Self::Supersede { iri, .. } => iri,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPreviewEntry {
    pub draft: SpecRecordDraft,
    pub disposition: SpecDisposition,
    /// Frozen accepted record being reused or replaced. The approval surface
    /// shows this exact graph claim alongside the extracted draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<SpecExistingRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecExistingRecord {
    pub iri: String,
    pub kind: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecRetirementDisposition {
    Retract,
    RetainShared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecRetirement {
    pub iri: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub disposition: SpecRetirementDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPrepareResponse {
    pub operation_id: String,
    pub owner_id: String,
    pub path: String,
    pub source_sha256: String,
    pub knowledge_revision: String,
    pub entries: Vec<SpecPreviewEntry>,
    pub retirements: Vec<SpecRetirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<SpecComponentPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_approval_iri: Option<String>,
    /// The same source digest and active record set are already approved.
    #[serde(default)]
    pub already_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecApproveRequest {
    pub operation_id: String,
    pub owner_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecApprovedRecord {
    pub kind: String,
    pub title: String,
    pub iri: String,
    pub disposition: SpecDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecApproveResponse {
    pub operation_id: String,
    pub path: String,
    pub source_sha256: String,
    pub base_revision: String,
    pub result_revision: String,
    pub records: Vec<SpecApprovedRecord>,
    pub retirements: Vec<SpecRetirement>,
    pub approval_iri: String,
    pub checkpoint: CheckpointResponse,
}
