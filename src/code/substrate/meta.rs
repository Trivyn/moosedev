//! Metadata sidecar for the derived substrate index.
//!
//! `meta.json` is intentionally small and human-readable. Its presence is the
//! completion marker for an index build: `producer::run_index` writes and syncs
//! an immutable generation first, then atomically replaces this manifest.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{generation_dir, meta_path, substrate_dir, META_FILE_NAME};

pub const CURRENT_SCHEMA_VERSION: u32 = 3;
pub const LEGACY_SCHEMA_VERSION: u32 = 2;

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
    /// Wall-clock time immediately before the producer commands started.
    /// When present, unchanged source files with older mtimes are trustworthy
    /// baselines for this generation's indexed positions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_started_at: Option<DateTime<Utc>>,
    /// Immutable artifact generation selected by this manifest. Schema v2
    /// metadata omits this field and uses the legacy fixed substrate paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Successfully completed producer runs included in this substrate.
    pub producers: Vec<ProducerRun>,
}

/// Stable manifest identity used to avoid retrying the same broken generation
/// on every editor request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateIdentity {
    pub schema_version: u32,
    pub indexed_commit: String,
    pub indexed_at: DateTime<Utc>,
    pub indexed_started_at: Option<DateTime<Utc>>,
    pub generation: Option<String>,
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
            // This constructor is retained for legacy and synthetic test data,
            // which writes producer indexes directly under `substrate/`.
            schema_version: LEGACY_SCHEMA_VERSION,
            indexed_commit: indexed_commit.into(),
            indexed_at,
            indexed_started_at: None,
            generation: None,
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

    pub fn identity(&self) -> SubstrateIdentity {
        SubstrateIdentity {
            schema_version: self.schema_version,
            indexed_commit: self.indexed_commit.clone(),
            indexed_at: self.indexed_at,
            indexed_started_at: self.indexed_started_at,
            generation: self.generation.clone(),
        }
    }

    pub fn artifact_root(&self, data_dir: &Path) -> Result<PathBuf> {
        match self.schema_version {
            LEGACY_SCHEMA_VERSION if self.generation.is_none() => Ok(substrate_dir(data_dir)),
            CURRENT_SCHEMA_VERSION => {
                let generation = self.generation.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("substrate schema v3 metadata is missing a generation")
                })?;
                validate_generation(generation)?;
                Ok(generation_dir(data_dir, generation))
            }
            _ => bail!("unsupported substrate metadata schema"),
        }
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
        if !matches!(
            schema_version,
            Some(CURRENT_SCHEMA_VERSION) | Some(LEGACY_SCHEMA_VERSION)
        ) {
            bail!(
                "unsupported substrate metadata schema at {}; run `moosedev index`",
                path.display()
            );
        }
        let meta: Self = serde_json::from_str(&text).with_context(|| {
            format!(
                "failed to parse substrate metadata at {}; run `moosedev index`",
                path.display()
            )
        })?;
        meta.artifact_root(data_dir).with_context(|| {
            format!(
                "invalid substrate metadata at {}; run `moosedev index`",
                path.display()
            )
        })?;
        Ok(meta)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.artifact_root(data_dir)
            .context("refusing to save invalid substrate metadata")?;
        let path = meta_path(data_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create substrate directory {}", parent.display())
            })?;
        }
        let text =
            serde_json::to_string_pretty(self).context("failed to serialize substrate metadata")?;
        let temp_path =
            path.with_file_name(format!("{META_FILE_NAME}.tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| {
                    format!(
                        "failed to create temporary substrate metadata {}",
                        temp_path.display()
                    )
                })?;
            file.write_all(text.as_bytes()).with_context(|| {
                format!(
                    "failed to write temporary substrate metadata {}",
                    temp_path.display()
                )
            })?;
            file.sync_all().with_context(|| {
                format!(
                    "failed to sync temporary substrate metadata {}",
                    temp_path.display()
                )
            })?;
            fs::rename(&temp_path, &path).with_context(|| {
                format!("failed to publish substrate metadata {}", path.display())
            })?;
            if let Some(parent) = path.parent() {
                sync_directory(parent).with_context(|| {
                    format!(
                        "failed to durably publish substrate metadata {}",
                        path.display()
                    )
                })?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
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

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("failed to open {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

fn validate_generation(generation: &str) -> Result<()> {
    let uuid = generation
        .strip_prefix("gen-")
        .ok_or_else(|| anyhow::anyhow!("invalid substrate generation `{generation}`"))?;
    uuid::Uuid::parse_str(uuid)
        .map(|_| ())
        .with_context(|| format!("invalid substrate generation `{generation}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_metadata_round_trip() {
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

    #[test]
    fn v3_metadata_selects_an_immutable_generation() {
        let data_dir = unique_temp_dir("meta-v3");
        let generation = format!("gen-{}", uuid::Uuid::new_v4());
        let meta = SubstrateMeta {
            schema_version: CURRENT_SCHEMA_VERSION,
            indexed_commit: "abc123".to_string(),
            indexed_at: Utc::now(),
            indexed_started_at: Some(Utc::now()),
            generation: Some(generation.clone()),
            producers: Vec::new(),
        };

        meta.save(&data_dir).unwrap();
        let loaded = SubstrateMeta::load(&data_dir).unwrap();
        assert_eq!(loaded, meta);
        assert_eq!(
            loaded.artifact_root(&data_dir).unwrap(),
            generation_dir(&data_dir, &generation)
        );
        let substrate = substrate_dir(&data_dir);
        assert!(fs::read_dir(substrate).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("meta.json.tmp-")));

        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn malformed_v3_generation_is_rejected() {
        let data_dir = unique_temp_dir("meta-invalid-v3");
        let path = meta_path(&data_dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            r#"{"schema_version":3,"indexed_commit":"abc","indexed_at":"2026-07-07T01:02:03Z","generation":"../escape","producers":[]}"#,
        )
        .unwrap();

        let error = SubstrateMeta::load(&data_dir).unwrap_err().to_string();
        assert!(error.contains("run `moosedev index`"), "{error}");

        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn syncing_a_missing_directory_is_an_error() {
        let data_dir = unique_temp_dir("missing-sync-directory");
        let error = sync_directory(&data_dir.join("missing"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to open"), "{error}");
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
