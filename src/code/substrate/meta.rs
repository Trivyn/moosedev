//! Metadata sidecar for the derived substrate index.
//!
//! `meta.json` is intentionally small and human-readable. Its presence is the
//! completion marker for an index build: `producer::run_index` writes the SCIP
//! file first, validates it, promotes it into place, and saves metadata last.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::meta_path;

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerRun {
    /// Static registry name and on-disk index infix.
    pub name: String,
    /// Producer name from SCIP metadata, for diagnostics only.
    pub producer: String,
    /// Producer version from SCIP metadata, for diagnostics only.
    pub producer_version: String,
    /// Resolution mode represented by this producer output.
    pub mode: String,
    /// Number of SCIP documents accepted during validation.
    pub documents: usize,
    /// Number of SCIP occurrences accepted during validation.
    pub occurrences: usize,
    /// Optional prefix applied to document paths while loading.
    pub path_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstrateMeta {
    /// Version of this metadata schema, not the SCIP schema.
    pub schema_version: u32,
    /// Git commit that was indexed. Compared to current HEAD at load time.
    pub indexed_commit: String,
    /// Wall-clock time the producer output was accepted.
    pub indexed_at: DateTime<Utc>,
    /// Successfully completed producer runs included in this substrate.
    pub producers: Vec<ProducerRun>,
}

impl SubstrateMeta {
    pub fn single(
        name: impl Into<String>,
        indexed_commit: impl Into<String>,
        indexed_at: DateTime<Utc>,
        documents: usize,
        occurrences: usize,
    ) -> Self {
        let name = name.into();
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            indexed_commit: indexed_commit.into(),
            indexed_at,
            producers: vec![ProducerRun {
                producer: name.clone(),
                name,
                producer_version: "unknown".to_string(),
                mode: "scip".to_string(),
                documents,
                occurrences,
                path_prefix: None,
            }],
        }
    }

    pub fn documents(&self) -> usize {
        self.producers.iter().map(|run| run.documents).sum()
    }

    pub fn occurrences(&self) -> usize {
        self.producers.iter().map(|run| run.occurrences).sum()
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = meta_path(data_dir);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "substrate metadata missing at {}; run `moosedev index`",
                path.display()
            )
        })?;
        let schema_version = serde_json::from_str::<SchemaHeader>(&text)
            .ok()
            .map(|header| header.schema_version);
        if schema_version != Some(CURRENT_SCHEMA_VERSION) {
            bail!(
                "unsupported substrate metadata schema at {}; run `moosedev index`",
                path.display()
            );
        }
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
        let meta = SubstrateMeta::single(
            "rust-analyzer",
            "abc123",
            DateTime::parse_from_rfc3339("2026-07-07T01:02:03Z")
                .unwrap()
                .with_timezone(&Utc),
            2,
            3,
        );

        meta.save(&data_dir).unwrap();
        assert_eq!(SubstrateMeta::load(&data_dir).unwrap(), meta);
        assert_eq!(meta.documents(), 2);
        assert_eq!(meta.occurrences(), 3);

        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn v1_metadata_requires_reindexing() {
        let data_dir = unique_temp_dir("meta-v1");
        let path = meta_path(&data_dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            r#"{"schema_version":1,"indexed_commit":"abc","indexed_at":"2026-07-07T01:02:03Z","producer":"rust-analyzer","producer_version":"1","mode":"scip","documents":1,"occurrences":1}"#,
        )
        .unwrap();

        let error = SubstrateMeta::load(&data_dir).unwrap_err().to_string();
        assert!(error.contains("run `moosedev index`"));

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
