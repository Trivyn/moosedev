use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::Utc;

use super::meta::{SubstrateMeta, CURRENT_SCHEMA_VERSION};
use super::scip::{ingest, producer_info, read_index};
use super::{index_path, index_tmp_path, substrate_dir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub commit: String,
    pub duration: std::time::Duration,
    pub documents: usize,
    pub occurrences: usize,
    pub definitions: usize,
    pub index_bytes: u64,
}

pub fn run_index(repo_root: &Path, data_dir: &Path) -> Result<IndexReport> {
    let substrate_dir = substrate_dir(data_dir);
    fs::create_dir_all(&substrate_dir).with_context(|| {
        format!(
            "failed to create substrate directory {}",
            substrate_dir.display()
        )
    })?;

    let commit = SubstrateMeta::current_head(repo_root);
    let producer =
        std::env::var("MOOSEDEV_SCIP_PRODUCER").unwrap_or_else(|_| "rust-analyzer".to_string());
    let tmp_path = index_tmp_path(data_dir);
    let final_path = index_path(data_dir);

    let started = Instant::now();
    let status = Command::new(&producer)
        .arg("scip")
        .arg(repo_root)
        .arg("--output")
        .arg(&tmp_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let status = match status {
        Ok(status) => status,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!(
                "SCIP producer `{}` not found; install rust-analyzer with SCIP support or set MOOSEDEV_SCIP_PRODUCER",
                producer
            )
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to run SCIP producer `{producer}`"))
        }
    };

    if !status.success() {
        bail!("SCIP producer `{producer}` exited with status {status}");
    }

    let index = read_index(&tmp_path).with_context(|| {
        format!(
            "SCIP producer wrote invalid index {}; run `moosedev index` again",
            tmp_path.display()
        )
    })?;
    let ingested = ingest(&index).context("SCIP producer output failed substrate validation")?;
    let (producer_name, producer_version) = producer_info(&index);
    let index_bytes = fs::metadata(&tmp_path)
        .with_context(|| format!("failed to stat temporary SCIP index {}", tmp_path.display()))?
        .len();

    fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "failed to promote SCIP index {} to {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;

    let meta = SubstrateMeta {
        schema_version: CURRENT_SCHEMA_VERSION,
        indexed_commit: commit.clone(),
        indexed_at: Utc::now(),
        producer: producer_name,
        producer_version,
        mode: "scip".to_string(),
        documents: ingested.documents,
        occurrences: ingested.occurrences,
    };
    meta.save(data_dir)
        .context("failed to write substrate metadata after SCIP index promotion")?;

    Ok(IndexReport {
        commit,
        duration: started.elapsed(),
        documents: ingested.documents,
        occurrences: ingested.occurrences,
        definitions: ingested.definitions,
        index_bytes,
    })
}
