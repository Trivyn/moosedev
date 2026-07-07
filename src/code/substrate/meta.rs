//! Metadata sidecar for the derived substrate index.
//!
//! `meta.json` is intentionally small and human-readable. Its presence is the
//! completion marker for an index build: `producer::run_index` writes the SCIP
//! file first, validates it, promotes it into place, and saves metadata last.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::meta_path;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstrateMeta {
    /// Version of this metadata schema, not the SCIP schema.
    pub schema_version: u32,
    /// Git commit that was indexed. Compared to current HEAD at load time.
    pub indexed_commit: String,
    /// Wall-clock time the producer output was accepted.
    pub indexed_at: DateTime<Utc>,
    /// Producer name from SCIP metadata, for diagnostics only.
    pub producer: String,
    /// Producer version from SCIP metadata, for diagnostics only.
    pub producer_version: String,
    /// Resolution mode represented by this substrate, currently `"scip"`.
    pub mode: String,
    /// Number of SCIP documents accepted during validation.
    pub documents: usize,
    /// Number of SCIP occurrences accepted during validation.
    pub occurrences: usize,
}

impl SubstrateMeta {
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = meta_path(data_dir);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "substrate metadata missing at {}; run `moosedev index`",
                path.display()
            )
        })?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "failed to parse substrate metadata at {}; run `moosedev index`",
                path.display()
            )
        })
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = meta_path(data_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create substrate directory {}", parent.display())
            })?;
        }
        let text =
            serde_json::to_string_pretty(self).context("failed to serialize substrate metadata")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write substrate metadata {}", path.display()))
    }

    pub fn current_head(repo_root: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("rev-parse")
            .arg("HEAD")
            .output();
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            // Non-git directories can still load a substrate for diagnostics; they
            // simply report as stale against this sentinel.
            _ => "unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_round_trip() {
        let data_dir = unique_temp_dir("meta-round-trip");
        let meta = SubstrateMeta {
            schema_version: CURRENT_SCHEMA_VERSION,
            indexed_commit: "abc123".to_string(),
            indexed_at: DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
                .unwrap()
                .with_timezone(&Utc),
            producer: "rust-analyzer".to_string(),
            producer_version: "1.2.3".to_string(),
            mode: "scip".to_string(),
            documents: 2,
            occurrences: 3,
        };

        meta.save(&data_dir).unwrap();
        assert_eq!(SubstrateMeta::load(&data_dir).unwrap(), meta);

        let _ = fs::remove_dir_all(data_dir);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moosedev-substrate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
