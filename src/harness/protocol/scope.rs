//! Source geometry shared by the runner's diff and the daemon's scope
//! derivation: positions, ranges, changed files and the scope basis.
use serde::{Deserialize, Serialize};

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
    /// The same hunks as `changed_ranges`, pairwise, in the pre-change
    /// source's coordinates. Empty from producers before capture contract 3.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_ranges: Vec<HarnessSourceRange>,
    /// The change exceeded the hunk bound, so one prefix/suffix range stands
    /// in for its hunks on each side.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ranges_coalesced: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentScopeBasis {
    ChangedDefinition,
    EnclosingDefinition,
    ConservativeFile,
}

/// An edit to ground against the code index: the proposed full text of one
/// file and its changed ranges in that text's coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundRequest {
    pub file: String,
    pub after: String,
    #[serde(default)]
    pub ranges: Vec<HarnessSourceRange>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ranges_coalesced: bool,
}

/// A plan to ground against the code index: the approved plan's summary and the
/// model's replan reason as one text, and the plan's own files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanGroundRequest {
    pub text: String,
    #[serde(default)]
    pub files: Vec<String>,
}

/// An attribute of a function parameter the edit compares with string literals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundKey {
    pub attribute: String,
    pub literals: Vec<String>,
}

/// An indexed definition outside the edited file whose name matches a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundDefinition {
    pub key: String,
    pub name: String,
    pub file: String,
    pub symbol: String,
    /// `declaration` or `type_member`.
    pub role: String,
    /// The definition's first lines as proven indexed source; empty when the
    /// file cannot be proven to match the index.
    #[serde(default)]
    pub preview: String,
}

/// A compared literal that does not appear among a definition's string values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundMismatch {
    pub key: String,
    pub literal: String,
    pub file: String,
    pub definition: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundResponse {
    #[serde(default)]
    pub keys: Vec<GroundKey>,
    #[serde(default)]
    pub definitions: Vec<GroundDefinition>,
    #[serde(default)]
    pub mismatches: Vec<GroundMismatch>,
}
