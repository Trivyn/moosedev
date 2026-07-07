//! On-demand SCIP producer runner for `moosedev index`.
//!
//! The persisted substrate is the raw producer artifact plus `meta.json`.
//! We deliberately validate the temporary SCIP file before promotion, then write
//! metadata last so a metadata file means "the substrate is complete enough to
//! load".

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
    /// Git commit captured before spawning the producer.
    pub commit: String,
    /// Wall-clock time spent in producer execution plus validation/promotion.
    pub duration: std::time::Duration,
    /// Number of documents in the accepted SCIP index.
    pub documents: usize,
    /// Number of occurrences in the accepted SCIP index.
    pub occurrences: usize,
    /// Number of definition occurrences in the accepted SCIP index.
    pub definitions: usize,
    /// Size of the promoted `index.scip` file in bytes.
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

    // rust-analyzer owns stdout/stderr while producing the index. This keeps the
    // CLI honest about producer warnings instead of trying to summarize them.
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

    // Parse the tmp file once before promotion. This catches unsupported encodings
    // or malformed ranges before `index.scip` can become the active substrate.
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

    // Only the SCIP file is promoted atomically. Metadata is written last and acts
    // as the completion marker for the pair.
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
