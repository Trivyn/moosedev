//! Git-derived churn + authorship sidecar (observation stratum, AD `8dd7da0c`).
//!
//! Per-file commit counts, recency, and authorship concentration over a fixed
//! window, extracted from `git log` in ONE subprocess invocation and cached as
//! `churn.json` beside the SCIP index. This is a **derived cache**, not
//! knowledge: git is the append-only commit-anchored ledger it derives from,
//! so any clone reproduces it by running `moosedev index`. It never enters the
//! project graph — only the classifier's evidence strings and the dossier's
//! observations digest read it.
//!
//! Documented limitations: renames are not followed (per-path attribution
//! resets at a rename) and shallow clones under-count. `anchored_commit` makes
//! every extraction auditable against the history it saw.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{substrate_dir, CHURN_FILE_NAME};

pub const CHURN_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_WINDOW_MONTHS: u32 = 24;

pub fn churn_path(data_dir: &Path) -> PathBuf {
    substrate_dir(data_dir).join(CHURN_FILE_NAME)
}

/// Churn + authorship for one repo-relative file within the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileChurn {
    /// Commits touching this path inside the window.
    pub commits: u32,
    /// Committer date (RFC3339) of the newest commit touching this path.
    pub last_commit: String,
    /// Distinct author emails that touched this path.
    pub distinct_authors: u32,
    /// Share of commits by the most frequent author (0.0–1.0) — the
    /// authorship-concentration / bus-factor signal.
    pub top_author_share: f32,
}

/// The whole sidecar: per-file churn anchored to the indexed commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChurnIndex {
    /// Version of this sidecar's schema (independent of `meta.json`).
    pub schema_version: u32,
    /// History window the metrics cover, in months back from extraction.
    pub window_months: u32,
    /// HEAD at extraction time — matches `SubstrateMeta::indexed_commit`.
    pub anchored_commit: String,
    /// Repo-relative path → churn metrics. Only paths with ≥1 commit appear.
    pub files: BTreeMap<String, FileChurn>,
}

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

impl ChurnIndex {
    /// Extract churn for the whole repo in one `git log` pass.
    pub fn extract(repo_root: &Path, window_months: u32) -> Result<Self> {
        let since = format!("--since={window_months}.months");
        // \x01 marks a commit header line; author email + committer date are
        // tab-separated; --name-only lists the touched paths after each header.
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args([
                "log",
                &since,
                "--no-renames",
                "--format=%x01%ae%x09%cI",
                "--name-only",
            ])
            .output()
            .context("failed to spawn git for churn extraction")?;
        if !output.status.success() {
            anyhow::bail!(
                "git log failed for churn extraction: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let anchored_commit = super::SubstrateMeta::current_head(repo_root);
        Ok(Self::parse(&text, anchored_commit, window_months))
    }

    /// Parse `git log --format=%x01%ae%x09%cI --name-only` output.
    fn parse(text: &str, anchored_commit: String, window_months: u32) -> Self {
        // Per file: (commit count, newest committer date, author → count).
        let mut acc: BTreeMap<String, (u32, String, BTreeMap<String, u32>)> = BTreeMap::new();
        let mut current: Option<(String, String)> = None; // (author, date)

        for line in text.lines() {
            if let Some(header) = line.strip_prefix('\u{1}') {
                let mut parts = header.splitn(2, '\t');
                let author = parts.next().unwrap_or_default().to_string();
                let date = parts.next().unwrap_or_default().to_string();
                current = Some((author, date));
                continue;
            }
            let path = line.trim();
            if path.is_empty() {
                continue;
            }
            let Some((author, date)) = &current else {
                continue;
            };
            let entry = acc
                .entry(path.to_string())
                .or_insert_with(|| (0, date.clone(), BTreeMap::new()));
            entry.0 += 1;
            // `git log` is newest-first: the first date seen per path wins.
            *entry.2.entry(author.clone()).or_insert(0) += 1;
        }

        let files = acc
            .into_iter()
            .map(|(path, (commits, last_commit, authors))| {
                let top = authors.values().copied().max().unwrap_or(0);
                (
                    path,
                    FileChurn {
                        commits,
                        last_commit,
                        distinct_authors: authors.len() as u32,
                        top_author_share: if commits > 0 {
                            top as f32 / commits as f32
                        } else {
                            0.0
                        },
                    },
                )
            })
            .collect();

        Self {
            schema_version: CHURN_SCHEMA_VERSION,
            window_months,
            anchored_commit,
            files,
        }
    }

    /// Load the sidecar. Missing file or unknown schema → `Ok(None)`: churn is
    /// optional evidence, never a load failure (the substrate must keep
    /// working on installs indexed before this sidecar existed).
    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        Self::load_from(&churn_path(data_dir))
    }

    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let header = serde_json::from_str::<SchemaHeader>(&text).ok();
        if header.map(|h| h.schema_version) != Some(CHURN_SCHEMA_VERSION) {
            return Ok(None);
        }
        match serde_json::from_str(&text) {
            Ok(index) => Ok(Some(index)),
            Err(_) => Ok(None),
        }
    }

    /// Write the sidecar (pretty JSON, mirrors `SubstrateMeta::save`).
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        self.save_to(&churn_path(data_dir))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create substrate directory {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("serialize churn index")?;
        fs::write(path, text).with_context(|| format!("write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\u{1}alice@example.com\t2026-07-01T10:00:00+00:00\nsrc/a.rs\nsrc/b.rs\n\n\u{1}bob@example.com\t2026-06-01T10:00:00+00:00\nsrc/a.rs\n\n\u{1}alice@example.com\t2026-05-01T10:00:00+00:00\nsrc/a.rs\n";

    #[test]
    fn parse_counts_recency_and_concentration() {
        let index = ChurnIndex::parse(LOG, "abc123".to_string(), 24);
        assert_eq!(index.anchored_commit, "abc123");

        let a = &index.files["src/a.rs"];
        assert_eq!(a.commits, 3);
        assert_eq!(a.last_commit, "2026-07-01T10:00:00+00:00", "newest wins");
        assert_eq!(a.distinct_authors, 2);
        assert!((a.top_author_share - 2.0 / 3.0).abs() < 1e-6, "alice 2/3");

        let b = &index.files["src/b.rs"];
        assert_eq!(b.commits, 1);
        assert_eq!(b.distinct_authors, 1);
        assert_eq!(b.top_author_share, 1.0);
    }

    #[test]
    fn round_trip_and_schema_gate() {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-churn-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let index = ChurnIndex::parse(LOG, "abc".to_string(), 24);
        index.save(&dir).unwrap();
        assert_eq!(ChurnIndex::load(&dir).unwrap(), Some(index));

        // Unknown schema → None, never an error.
        fs::write(churn_path(&dir), r#"{"schema_version":99}"#).unwrap();
        assert_eq!(ChurnIndex::load(&dir).unwrap(), None);

        // Missing file → None.
        fs::remove_file(churn_path(&dir)).unwrap();
        assert_eq!(ChurnIndex::load(&dir).unwrap(), None);

        let _ = fs::remove_dir_all(&dir);
    }
}
