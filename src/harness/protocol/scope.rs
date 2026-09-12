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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentScopeBasis {
    ChangedDefinition,
    EnclosingDefinition,
    ConservativeFile,
}
