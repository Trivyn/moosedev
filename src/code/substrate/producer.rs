//! On-demand SCIP producer runner for `moosedev index`.
//!
//! Producer output is assembled in an immutable generation. We deliberately
//! validate and sync every artifact before atomically publishing `meta.json`, so
//! readers see either the previous complete generation or the next one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};

use super::meta::{
    sync_directory, HistoricalDefinitionProof, HistoricalFileProof, ProducerRun, SubstrateMeta,
    CURRENT_SCHEMA_VERSION,
};
use super::scip::{ingest, producer_info, read_index};
use super::{
    generation_dir, generations_dir, index_lock_path, index_log_path, index_path, index_tmp_path,
    producer_index_path, producer_index_path_in, producer_index_tmp_path,
    producer_index_tmp_path_in, substrate_dir, CHURN_FILE_NAME,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerTarget {
    pub project_dir: PathBuf,
    pub path_prefix: Option<String>,
}

#[derive(Clone, Copy)]
pub struct ProducerSpec {
    pub name: &'static str,
    /// Detect one project target. Multi-project producers are a future registry seam.
    pub detect: fn(&Path) -> Option<ProducerTarget>,
    pub command: fn(&ProducerTarget, &Path) -> Command,
    /// File extensions whose saves can shift this producer's indexed positions.
    pub extensions: &'static [&'static str],
}

/// The producer registry, defined per language in `super::lang`.
pub fn registry() -> &'static [ProducerSpec] {
    super::lang::producer_registry()
}

/// Extensions covered by the producers that detect this repository right now.
/// Empty when no producer detects it (non-code repos, synthetic test roots).
pub fn detected_save_extensions(repo_root: &Path) -> Vec<&'static str> {
    registry()
        .iter()
        .filter(|producer| (producer.detect)(repo_root).is_some())
        .flat_map(|producer| producer.extensions.iter().copied())
        .collect()
}

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
    /// Total size of all promoted producer index files in bytes.
    pub index_bytes: u64,
    /// Per-producer accepted output, in registry order.
    pub producers: Vec<ProducerReport>,
    /// Files covered by the churn sidecar, when extraction succeeded.
    pub churn_files: Option<usize>,
    /// Producer failures excluded from the completed substrate.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerReport {
    pub name: String,
    pub documents: usize,
    pub occurrences: usize,
    pub definitions: usize,
    pub index_bytes: u64,
}

pub fn run_index(repo_root: &Path, data_dir: &Path) -> Result<IndexReport> {
    run_index_with(registry(), repo_root, data_dir)
}

#[doc(hidden)]
pub fn run_index_with(
    producers: &[ProducerSpec],
    repo_root: &Path,
    data_dir: &Path,
) -> Result<IndexReport> {
    let substrate_dir = substrate_dir(data_dir);
    fs::create_dir_all(&substrate_dir).with_context(|| {
        format!(
            "failed to create substrate directory {}",
            substrate_dir.display()
        )
    })?;

    // Every writer participates in one stable advisory lock. The file itself
    // is never replaced, so concurrent invocations cannot split across inodes.
    let _index_lock = acquire_index_lock(data_dir)?;
    let started = start_index(data_dir)?;
    let commit = SubstrateMeta::current_head(repo_root);
    let detected = producers
        .iter()
        .filter_map(|producer| (producer.detect)(repo_root).map(|target| (producer, target)))
        .collect::<Vec<_>>();
    if detected.is_empty() {
        bail!(
            "no SCIP producer detected for repository {}",
            repo_root.display()
        );
    }
    let generation = format!("gen-{}", uuid::Uuid::new_v4());
    let artifact_root = generation_dir(data_dir, &generation);
    fs::create_dir_all(&artifact_root).with_context(|| {
        format!(
            "failed to create candidate substrate generation {}",
            artifact_root.display()
        )
    })?;
    let producer_names = detected
        .iter()
        .map(|(spec, _)| spec.name)
        .collect::<Vec<_>>();
    // This timestamp brackets every producer read. A source file whose mtime
    // is still strictly older after publication did not change during this
    // build and can safely serve as the generation's LSP mapping baseline.
    let indexed_started_at = Utc::now();
    let prior_substrate = super::Substrate::load(data_dir, repo_root).ok();

    let mut runs = Vec::new();
    let mut reports = Vec::new();
    let mut warnings = Vec::new();
    for (spec, target) in detected {
        write_log_header(data_dir, spec.name)?;
        match run_producer(spec, &target, data_dir, &artifact_root) {
            Ok((run, report)) => {
                runs.push(run);
                reports.push(report);
            }
            Err(error) => warnings.push(exclude_failed_producer(
                spec,
                data_dir,
                &artifact_root,
                &error,
            )?),
        }
    }

    if reports.is_empty() {
        let _ = fs::remove_dir_all(&artifact_root);
        bail!(
            "all detected SCIP producers failed: {}",
            warnings.join("; ")
        );
    }

    // Observation sidecar (AD 8dd7da0c): best-effort — a non-git dir or a
    // missing git binary degrades to "no churn", never a failed index. Saved
    // BEFORE meta.json: metadata is the completion marker the daemon's lazy
    // reload watches, so everything the reload should pick up must already
    // be on disk when it lands.
    let churn_files =
        match super::ChurnIndex::extract(repo_root, super::churn::DEFAULT_WINDOW_MONTHS).and_then(
            |index| {
                let count = index.files.len();
                index.save_to(&artifact_root.join(CHURN_FILE_NAME))?;
                Ok(count)
            },
        ) {
            Ok(count) => Some(count),
            Err(error) => {
                let _ = std::fs::remove_file(artifact_root.join(CHURN_FILE_NAME));
                warnings.push(format!("churn sidecar skipped: {error}"));
                None
            }
        };

    let source_digests =
        generation_source_digests(repo_root, &artifact_root, &runs, indexed_started_at);
    let historical_files = generation_historical_files(prior_substrate.as_ref(), &source_digests);
    let meta = SubstrateMeta {
        schema_version: CURRENT_SCHEMA_VERSION,
        indexed_commit: commit.clone(),
        indexed_at: Utc::now(),
        indexed_started_at: Some(indexed_started_at),
        generation: Some(generation.clone()),
        producers: runs,
        source_digests,
        historical_files,
    };
    sync_generation(&artifact_root)?;
    meta.save(data_dir)
        .context("failed to write substrate metadata after SCIP index promotion")?;
    warnings.extend(cleanup_after_publish(
        data_dir,
        &generation,
        &producer_names,
    ));

    Ok(IndexReport {
        commit,
        duration: started.elapsed(),
        documents: meta.documents(),
        occurrences: meta.occurrences(),
        definitions: reports.iter().map(|report| report.definitions).sum(),
        index_bytes: reports.iter().map(|report| report.index_bytes).sum(),
        producers: reports,
        churn_files,
        warnings,
    })
}

fn generation_historical_files(
    prior: Option<&super::Substrate>,
    current: &BTreeMap<String, String>,
) -> BTreeMap<String, HistoricalFileProof> {
    let Some(prior) = prior else {
        return BTreeMap::new();
    };
    let mut history = prior.meta().historical_files.clone();
    for current_path in current.keys() {
        history.remove(current_path);
    }
    for (path, source_digest) in &prior.meta().source_digests {
        if current.contains_key(path) {
            continue;
        }
        let definitions = prior
            .definition_scopes_in_file(path)
            .into_iter()
            .map(|scope| HistoricalDefinitionProof {
                symbol: scope.definition.entry.normalized_symbol,
                name: scope.definition.entry.display_name,
                definition_range: range_array(scope.definition.range),
                enclosing_range: scope.enclosing_range.map(range_array),
            })
            .collect();
        history.insert(
            path.clone(),
            HistoricalFileProof {
                source_digest: source_digest.clone(),
                index_generation: prior.meta().generation.clone(),
                definitions,
            },
        );
    }
    history
}

fn range_array(range: super::SourceRange) -> [u32; 4] {
    [
        range.start.line,
        range.start.col,
        range.end.line,
        range.end.col,
    ]
}

fn run_producer(
    spec: &ProducerSpec,
    target: &ProducerTarget,
    data_dir: &Path,
    artifact_root: &Path,
) -> Result<(ProducerRun, ProducerReport)> {
    let tmp_path = producer_index_tmp_path_in(artifact_root, spec.name);
    let absolute_tmp_path = std::path::absolute(&tmp_path).with_context(|| {
        format!(
            "failed to absolutize temporary SCIP index {}",
            tmp_path.display()
        )
    })?;
    let final_path = producer_index_path_in(artifact_root, spec.name);
    remove_if_present(&tmp_path)?;

    let mut command = (spec.command)(target, &absolute_tmp_path);
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .stdin(Stdio::inherit())
        .stdout(producer_output(data_dir)?)
        .stderr(producer_output(data_dir)?)
        .status();
    let status = match status {
        Ok(status) => status,
        Err(err) if err.kind() == ErrorKind::NotFound => bail!(
            "SCIP producer `{program}` not found; install it or configure its binary override"
        ),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to run SCIP producer `{program}`"))
        }
    };
    if !status.success() {
        return Err(producer_failure(&program, status, data_dir));
    }

    let index = read_index(&tmp_path).with_context(|| {
        format!(
            "SCIP producer wrote invalid index {}; run `moosedev index` again",
            tmp_path.display()
        )
    })?;
    let ingested = ingest(&index).context("SCIP producer output failed substrate validation")?;
    let (producer, producer_version) = producer_info(&index);
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

    Ok((
        ProducerRun {
            name: spec.name.to_string(),
            producer,
            producer_version,
            mode: "scip".to_string(),
            documents: ingested.documents,
            occurrences: ingested.occurrences,
            path_prefix: target.path_prefix.clone(),
        },
        ProducerReport {
            name: spec.name.to_string(),
            documents: ingested.documents,
            occurrences: ingested.occurrences,
            definitions: ingested.definitions,
            index_bytes,
        },
    ))
}

/// Hash only source files whose stable metadata proves they predate the whole
/// producer run. Omitted entries remain an honest lack of durable source proof.
fn generation_source_digests(
    repo_root: &Path,
    artifact_root: &Path,
    runs: &[ProducerRun],
    indexed_started_at: chrono::DateTime<Utc>,
) -> BTreeMap<String, String> {
    let Some(canonical_root) = repo_root.canonicalize().ok() else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for run in runs {
        let Ok(index) = read_index(&producer_index_path_in(artifact_root, &run.name)) else {
            continue;
        };
        for document in index.documents {
            let relative = match run.path_prefix.as_deref() {
                Some(prefix) => format!("{prefix}{}", document.relative_path),
                None => document.relative_path,
            };
            if !Path::new(&relative)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                continue;
            }
            let Ok(path) = canonical_root.join(&relative).canonicalize() else {
                continue;
            };
            if !path.starts_with(&canonical_root) {
                continue;
            }
            let (Ok(before), Ok(mut file)) = (fs::metadata(&path), fs::File::open(&path)) else {
                continue;
            };
            let Ok(modified) = before.modified() else {
                continue;
            };
            if chrono::DateTime::<Utc>::from(modified) >= indexed_started_at {
                continue;
            }
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut bytes = 0u64;
            let mut failed = false;
            loop {
                match std::io::Read::read(&mut file, &mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        hasher.update(&buffer[..n]);
                        bytes = bytes.saturating_add(n as u64)
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            let Ok(after) = fs::metadata(&path) else {
                continue;
            };
            if failed
                || bytes != before.len()
                || after.len() != before.len()
                || after.modified().ok() != Some(modified)
            {
                continue;
            }
            out.insert(relative, format!("{:x}", hasher.finalize()));
        }
    }
    out
}

fn exclude_failed_producer(
    spec: &ProducerSpec,
    data_dir: &Path,
    artifact_root: &Path,
    error: &anyhow::Error,
) -> Result<String> {
    remove_if_present(&producer_index_tmp_path_in(artifact_root, spec.name))?;
    remove_if_present(&producer_index_path_in(artifact_root, spec.name))?;
    let warning = format!(
        "producer `{}` failed and was excluded: {error:#}",
        spec.name
    );
    tracing::warn!(producer = spec.name, error = %error, "SCIP producer failed and was excluded");
    write_log_warning(data_dir, &warning)?;
    Ok(warning)
}

fn open_index_lock(data_dir: &Path) -> Result<fs::File> {
    let path = index_lock_path(data_dir);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open index lock {}", path.display()))
}

fn acquire_index_lock(data_dir: &Path) -> Result<fs::File> {
    let path = index_lock_path(data_dir);
    let file = open_index_lock(data_dir)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            tracing::info!(path = %path.display(), "another index build is running; waiting for it to finish");
            file.lock()
                .with_context(|| format!("failed to wait on index lock {}", path.display()))?;
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("failed to lock {}", path.display()));
        }
    }
    Ok(file)
}

fn sync_generation(artifact_root: &Path) -> Result<()> {
    for entry in fs::read_dir(artifact_root)
        .with_context(|| format!("failed to read generation {}", artifact_root.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to inspect generation {}", artifact_root.display()))?;
        if entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?
            .is_file()
        {
            fs::File::open(entry.path())
                .with_context(|| format!("failed to open {} for sync", entry.path().display()))?
                .sync_all()
                .with_context(|| format!("failed to sync {}", entry.path().display()))?;
        }
    }
    fs::File::open(artifact_root)
        .with_context(|| format!("failed to open {} for sync", artifact_root.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync generation {}", artifact_root.display()))?;
    // The new generation's directory entry lives in the parent (generations/);
    // without syncing it a crash can preserve meta.json while losing the
    // directory it references.
    if let Some(parent) = artifact_root.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn cleanup_after_publish(
    data_dir: &Path,
    active_generation: &str,
    producer_names: &[&str],
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Ok(entries) = fs::read_dir(generations_dir(data_dir)) {
        for entry in entries.flatten() {
            if entry.file_name() == active_generation {
                continue;
            }
            if let Err(error) = fs::remove_dir_all(entry.path()) {
                warnings.push(format!(
                    "failed to remove stale substrate generation {}: {error}",
                    entry.path().display()
                ));
            }
        }
    }

    let mut names = producer_names.iter().copied().collect::<BTreeSet<_>>();
    names.extend(registry().iter().map(|producer| producer.name));
    let mut legacy_paths = vec![
        index_path(data_dir),
        index_tmp_path(data_dir),
        super::churn::churn_path(data_dir),
    ];
    for name in names {
        legacy_paths.push(producer_index_path(data_dir, name));
        legacy_paths.push(producer_index_tmp_path(data_dir, name));
    }
    for path in legacy_paths {
        if let Err(error) = remove_if_present(&path) {
            warnings.push(format!(
                "failed to remove legacy substrate artifact {}: {error:#}",
                path.display()
            ));
        }
    }
    for warning in &warnings {
        tracing::warn!(warning, "substrate cleanup was incomplete");
    }
    warnings
}

/// Summarize the producer log without re-emitting noisy upstream diagnostics.
pub fn diagnostic_summary(data_dir: &Path) -> String {
    if index_verbose() {
        return "producer diagnostics: verbose output enabled".to_string();
    }
    let count = count_noteworthy_lines(&index_log_path(data_dir));
    if count == 0 {
        "producer diagnostics: none".to_string()
    } else {
        format!(
            "producer diagnostics: {count} noteworthy line(s) — see {}",
            index_log_path(data_dir).display()
        )
    }
}

/// Count the lines retained in the per-run producer log for tests and callers
/// that need structured access to the concise diagnostics summary.
pub fn noteworthy_diagnostics(data_dir: &Path) -> Option<usize> {
    (!index_verbose()).then(|| count_noteworthy_lines(&index_log_path(data_dir)))
}

fn index_verbose() -> bool {
    std::env::var_os("MOOSEDEV_INDEX_VERBOSE")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

fn prepare_log(data_dir: &Path) -> Result<()> {
    let path = index_log_path(data_dir);
    fs::File::create(&path)
        .with_context(|| format!("failed to create producer log {}", path.display()))?;
    Ok(())
}

fn write_log_header(data_dir: &Path, producer: &str) -> Result<()> {
    append_log_line(data_dir, &format!("=== {producer} ==="))
}

fn write_log_warning(data_dir: &Path, warning: &str) -> Result<()> {
    append_log_line(data_dir, &format!("WARNING: {warning}"))
}

fn append_log_line(data_dir: &Path, line: &str) -> Result<()> {
    let path = index_log_path(data_dir);
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open producer log {}", path.display()))?;
    writeln!(log, "{line}")
        .with_context(|| format!("failed to write producer log {}", path.display()))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn start_index(data_dir: &Path) -> Result<Instant> {
    prepare_log(data_dir)?;
    Ok(Instant::now())
}

fn producer_output(data_dir: &Path) -> Result<Stdio> {
    if index_verbose() {
        return Ok(Stdio::inherit());
    }
    let path = index_log_path(data_dir);
    let log = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open producer log {}", path.display()))?;
    Ok(Stdio::from(log))
}

fn producer_failure(
    producer: &str,
    status: std::process::ExitStatus,
    data_dir: &Path,
) -> anyhow::Error {
    if index_verbose() {
        return anyhow::anyhow!(
            "SCIP producer `{producer}` exited with status {status}; check terminal output"
        );
    }
    anyhow::anyhow!(
        "SCIP producer `{producer}` exited with status {status}; check {}",
        index_log_path(data_dir).display()
    )
}

fn count_noteworthy_lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| {
                    line.contains("ERROR")
                        || line.contains("WARNING")
                        || line.contains("Duplicate symbol")
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use protobuf::{EnumOrUnknown, Message};
    use scip::types::{Document, Index, Occurrence, PositionEncoding, SymbolInformation};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rust_registry_detection_requires_cargo_manifest() {
        let repo_root = unique_temp_dir("registry-detect");
        let rust = &registry()[0];
        assert!((rust.detect)(&repo_root).is_none());
        fs::write(
            repo_root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        assert_eq!(
            (rust.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.clone(),
                path_prefix: None,
            })
        );
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn detected_save_extensions_follow_producer_detection() {
        let repo_root = unique_temp_dir("save-extensions");
        assert!(detected_save_extensions(&repo_root).is_empty());
        fs::write(
            repo_root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        let extensions = detected_save_extensions(&repo_root);
        assert!(extensions.contains(&"rs"));
        assert!(!extensions.contains(&"ts"));
        assert!(!extensions.contains(&"py"));

        write_python_manifest(&repo_root, "pyproject.toml");
        let extensions = detected_save_extensions(&repo_root);
        assert!(extensions.contains(&"py"));
        assert!(extensions.contains(&"pyi"));
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn typescript_detection_finds_root_or_sorted_first_level_project() {
        let repo_root = unique_temp_dir("typescript-detect");
        let typescript = &registry()[1];
        assert!((typescript.detect)(&repo_root).is_none());

        write_typescript_manifests(&repo_root.join("z-project"));
        write_typescript_manifests(&repo_root.join("a-project"));
        assert_eq!(
            (typescript.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.join("a-project"),
                path_prefix: Some("a-project/".to_string()),
            })
        );

        write_typescript_manifests(&repo_root);
        assert_eq!(
            (typescript.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.clone(),
                path_prefix: None,
            })
        );
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn typescript_detection_requires_both_manifests_and_skips_excluded_directories() {
        let repo_root = unique_temp_dir("typescript-exclusions");
        let typescript = &registry()[1];
        fs::create_dir_all(repo_root.join("only-tsconfig")).unwrap();
        fs::write(repo_root.join("only-tsconfig/tsconfig.json"), "{}").unwrap();
        fs::create_dir_all(repo_root.join("only-package")).unwrap();
        fs::write(repo_root.join("only-package/package.json"), "{}").unwrap();
        write_typescript_manifests(&repo_root.join("node_modules"));
        write_typescript_manifests(&repo_root.join(".hidden"));

        assert!((typescript.detect)(&repo_root).is_none());
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn python_detection_finds_root_or_sorted_first_level_project() {
        let repo_root = unique_temp_dir("python-detect");
        let python = &registry()[2];
        assert!((python.detect)(&repo_root).is_none());

        write_python_manifest(&repo_root.join("z-project"), "pyproject.toml");
        write_python_manifest(&repo_root.join("a-project"), "setup.py");
        assert_eq!(
            (python.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.join("a-project"),
                path_prefix: Some("a-project/".to_string()),
            })
        );

        write_python_manifest(&repo_root, "setup.cfg");
        assert_eq!(
            (python.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.clone(),
                path_prefix: None,
            })
        );
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn python_detection_accepts_root_requirements_but_not_subdir_requirements() {
        let repo_root = unique_temp_dir("python-requirements");
        let python = &registry()[2];
        write_python_manifest(&repo_root.join("bench"), "requirements.txt");
        write_python_manifest(&repo_root.join("node_modules"), "pyproject.toml");
        write_python_manifest(&repo_root.join(".hidden"), "pyproject.toml");
        assert!((python.detect)(&repo_root).is_none());

        write_python_manifest(&repo_root, "requirements.txt");
        assert_eq!(
            (python.detect)(&repo_root),
            Some(ProducerTarget {
                project_dir: repo_root.clone(),
                path_prefix: None,
            })
        );
        let _ = fs::remove_dir_all(repo_root);
    }

    #[test]
    fn rust_command_honors_binary_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("MOOSEDEV_SCIP_PRODUCER");
        std::env::set_var("MOOSEDEV_SCIP_PRODUCER", "fake-scip-producer");
        let target = ProducerTarget {
            project_dir: PathBuf::from("repo"),
            path_prefix: None,
        };
        let command = (registry()[0].command)(&target, Path::new("out.scip"));
        assert_eq!(command.get_program(), "fake-scip-producer");
        match previous {
            Some(value) => std::env::set_var("MOOSEDEV_SCIP_PRODUCER", value),
            None => std::env::remove_var("MOOSEDEV_SCIP_PRODUCER"),
        }
    }

    #[test]
    fn typescript_command_override_replaces_npx_and_sets_project_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("MOOSEDEV_SCIP_TYPESCRIPT");
        std::env::set_var("MOOSEDEV_SCIP_TYPESCRIPT", "fake-scip-typescript");
        let target = ProducerTarget {
            project_dir: PathBuf::from("ui"),
            path_prefix: Some("ui/".to_string()),
        };

        let command = (registry()[1].command)(&target, Path::new("/tmp/out.scip"));
        assert_eq!(command.get_program(), "fake-scip-typescript");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["index", "--output", "/tmp/out.scip"]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("ui")));

        match previous {
            Some(value) => std::env::set_var("MOOSEDEV_SCIP_TYPESCRIPT", value),
            None => std::env::remove_var("MOOSEDEV_SCIP_TYPESCRIPT"),
        }
    }

    #[test]
    fn python_command_override_replaces_npx_and_sets_project_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("MOOSEDEV_SCIP_PYTHON");
        std::env::set_var("MOOSEDEV_SCIP_PYTHON", "fake-scip-python");
        let target = ProducerTarget {
            project_dir: PathBuf::from("api"),
            path_prefix: Some("api/".to_string()),
        };

        let command = (registry()[2].command)(&target, Path::new("/tmp/out.scip"));
        assert_eq!(command.get_program(), "fake-scip-python");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["index", "--output", "/tmp/out.scip"]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("api")));

        std::env::remove_var("MOOSEDEV_SCIP_PYTHON");
        let command = (registry()[2].command)(&target, Path::new("/tmp/out.scip"));
        assert_eq!(command.get_program(), "npx");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--yes",
                "@sourcegraph/scip-python",
                "index",
                "--output",
                "/tmp/out.scip"
            ]
        );

        if let Some(value) = previous {
            std::env::set_var("MOOSEDEV_SCIP_PYTHON", value);
        }
    }

    #[test]
    fn two_producers_are_promoted_and_recorded() {
        let repo_root = unique_temp_dir("two-producer-repo");
        let data_dir = unique_temp_dir("two-producer-data");
        write_prepared(&repo_root.join("first.scip"), "src/first.rs", "first");
        write_prepared(&repo_root.join("second.scip"), "src/second.rs", "second");
        let specs = [
            ProducerSpec {
                name: "first",
                detect: detect_all,
                command: copy_first,
                extensions: &["rs"],
            },
            ProducerSpec {
                name: "second",
                detect: detect_prefixed,
                command: copy_second,
                extensions: &["rs"],
            },
        ];

        let report = run_index_with(&specs, &repo_root, &data_dir).unwrap();
        assert_eq!(report.producers.len(), 2);
        let meta = SubstrateMeta::load(&data_dir).unwrap();
        let artifact_root = meta.artifact_root(&data_dir).unwrap();
        assert!(producer_index_path_in(&artifact_root, "first").is_file());
        assert!(producer_index_path_in(&artifact_root, "second").is_file());
        assert_eq!(meta.producers.len(), 2);
        assert_eq!(meta.producers[1].path_prefix.as_deref(), Some("ui/"));
        let log = fs::read_to_string(index_log_path(&data_dir)).unwrap();
        assert!(log.contains("=== first ==="));
        assert!(log.contains("=== second ==="));

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn generation_metadata_authenticates_indexed_source_for_restart_recovery() {
        let repo_root = unique_temp_dir("source-proof-repo");
        let artifact_root = unique_temp_dir("source-proof-artifact");
        fs::create_dir_all(repo_root.join("src")).unwrap();
        let source = b"fn retained() {}\n";
        fs::write(repo_root.join("src/lib.rs"), source).unwrap();
        std::fs::File::open(repo_root.join("src/lib.rs"))
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2)),
            )
            .unwrap();
        write_prepared(
            &producer_index_path_in(&artifact_root, "test"),
            "src/lib.rs",
            "retained",
        );
        let runs = vec![ProducerRun {
            name: "test".into(),
            producer: "test".into(),
            producer_version: "1".into(),
            mode: "scip".into(),
            documents: 1,
            occurrences: 1,
            path_prefix: None,
        }];
        let proofs = generation_source_digests(&repo_root, &artifact_root, &runs, Utc::now());
        assert_eq!(
            proofs.get("src/lib.rs"),
            Some(&format!("{:x}", Sha256::digest(source)))
        );
        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(artifact_root);
    }

    #[test]
    fn next_generation_retains_deleted_definition_proof() {
        let artifact_root = unique_temp_dir("historical-scope-artifact");
        let path = producer_index_path_in(&artifact_root, "test");
        let symbol = "rust-analyzer cargo sample 1 src/deleted/retained().";
        write_prepared(&path, "src/deleted.rs", symbol);
        let mut index = read_index(&path).unwrap();
        let mut info = SymbolInformation::new();
        info.symbol = symbol.into();
        info.display_name = "retained".into();
        index.documents[0].symbols.push(info);
        let mut meta = SubstrateMeta::single("test", "old", Utc::now(), 1, 1);
        meta.generation = Some("old-generation".into());
        meta.source_digests
            .insert("src/deleted.rs".into(), "abc123".into());
        let prior = crate::code::substrate::Substrate::from_index(index, meta, false).unwrap();
        let history = generation_historical_files(Some(&prior), &BTreeMap::new());
        let proof = history.get("src/deleted.rs").unwrap();
        assert_eq!(proof.source_digest, "abc123");
        assert_eq!(proof.index_generation.as_deref(), Some("old-generation"));
        assert_eq!(proof.definitions.len(), 1);
        let _ = fs::remove_dir_all(artifact_root);
    }

    #[test]
    fn failed_producer_is_removed_and_successful_one_completes() {
        let repo_root = unique_temp_dir("partial-producer-repo");
        let data_dir = unique_temp_dir("partial-producer-data");
        write_prepared(&repo_root.join("first.scip"), "src/first.rs", "first");
        let failed_path = producer_index_path(&data_dir, "failed");
        fs::create_dir_all(failed_path.parent().unwrap()).unwrap();
        fs::write(&failed_path, b"stale").unwrap();
        let specs = [
            ProducerSpec {
                name: "first",
                detect: detect_all,
                command: copy_first,
                extensions: &["rs"],
            },
            ProducerSpec {
                name: "failed",
                detect: detect_all,
                command: fail_command,
                extensions: &["rs"],
            },
        ];

        let report = run_index_with(&specs, &repo_root, &data_dir).unwrap();
        assert_eq!(report.producers.len(), 1);
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.contains("producer `failed` failed"))
                .count(),
            1
        );
        // Non-git repo: churn extraction degrades to a warning, never a failure.
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("churn sidecar skipped")));
        assert_eq!(report.churn_files, None);
        assert!(!failed_path.exists());
        assert_eq!(SubstrateMeta::load(&data_dir).unwrap().producers.len(), 1);
        let log = fs::read_to_string(index_log_path(&data_dir)).unwrap();
        assert!(log.contains("WARNING: producer `failed` failed"));
        let artifact_root = SubstrateMeta::load(&data_dir)
            .unwrap()
            .artifact_root(&data_dir)
            .unwrap();
        assert!(!producer_index_path_in(&artifact_root, "failed").exists());

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn all_failed_build_preserves_the_published_substrate() {
        let repo_root = unique_temp_dir("all-failed-repo");
        let data_dir = unique_temp_dir("all-failed-data");
        let published_path = producer_index_path(&data_dir, "published");
        fs::create_dir_all(published_path.parent().unwrap()).unwrap();
        write_prepared(&published_path, "src/published.rs", "published");
        let published = SubstrateMeta::single("published", "old", Utc::now(), 1, 1);
        published.save(&data_dir).unwrap();
        let specs = [ProducerSpec {
            name: "failed",
            detect: detect_all,
            command: fail_command,
            extensions: &["rs"],
        }];

        let error = run_index_with(&specs, &repo_root, &data_dir).unwrap_err();
        assert!(error
            .to_string()
            .contains("all detected SCIP producers failed"));
        assert_eq!(SubstrateMeta::load(&data_dir).unwrap(), published);
        assert!(published_path.is_file());

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn successful_publish_removes_orphan_generations() {
        let repo_root = unique_temp_dir("orphan-repo");
        let data_dir = unique_temp_dir("orphan-data");
        write_prepared(&repo_root.join("first.scip"), "src/first.rs", "first");
        let orphan = generation_dir(&data_dir, "gen-00000000-0000-4000-8000-000000000000");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("partial"), b"partial").unwrap();
        let specs = [ProducerSpec {
            name: "first",
            detect: detect_all,
            command: copy_first,
            extensions: &["rs"],
        }];

        run_index_with(&specs, &repo_root, &data_dir).unwrap();
        let meta = SubstrateMeta::load(&data_dir).unwrap();
        assert!(meta.artifact_root(&data_dir).unwrap().is_dir());
        assert!(!orphan.exists());

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn index_lock_serializes_builders() {
        let data_dir = unique_temp_dir("index-lock");
        fs::create_dir_all(substrate_dir(&data_dir)).unwrap();
        let first = acquire_index_lock(&data_dir).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let other_data_dir = data_dir.clone();
        let waiter = std::thread::spawn(move || {
            let lock = acquire_index_lock(&other_data_dir).unwrap();
            send.send(()).unwrap();
            drop(lock);
        });

        assert!(receive
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        drop(first);
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        waiter.join().unwrap();

        let _ = fs::remove_dir_all(data_dir);
    }

    fn detect_all(repo_root: &Path) -> Option<ProducerTarget> {
        Some(ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        })
    }

    fn detect_prefixed(repo_root: &Path) -> Option<ProducerTarget> {
        Some(ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: Some("ui/".to_string()),
        })
    }

    fn copy_first(target: &ProducerTarget, output: &Path) -> Command {
        let mut command = Command::new("cp");
        command
            .arg(target.project_dir.join("first.scip"))
            .arg(output);
        command
    }

    fn copy_second(target: &ProducerTarget, output: &Path) -> Command {
        let mut command = Command::new("cp");
        command
            .arg(target.project_dir.join("second.scip"))
            .arg(output);
        command
    }

    fn fail_command(_: &ProducerTarget, _: &Path) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 1"]);
        command
    }

    fn write_typescript_manifests(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("tsconfig.json"), "{}").unwrap();
        fs::write(path.join("package.json"), "{}").unwrap();
    }

    fn write_python_manifest(path: &Path, marker: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join(marker), "").unwrap();
    }

    fn write_prepared(path: &Path, relative_path: &str, symbol: &str) {
        let mut index = Index::new();
        let mut document = Document::new();
        document.relative_path = relative_path.to_string();
        document.position_encoding =
            EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol.to_string();
        occurrence.range = vec![0, 0, 1];
        occurrence.symbol_roles = 1;
        document.occurrences.push(occurrence);
        index.documents.push(document);
        fs::write(path, index.write_to_bytes().unwrap()).unwrap();
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-substrate-producer-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
