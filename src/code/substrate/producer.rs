//! On-demand SCIP producer runner for `moosedev index`.
//!
//! The persisted substrate is the raw producer artifact plus `meta.json`.
//! We deliberately validate the temporary SCIP file before promotion, then write
//! metadata last so a metadata file means "the substrate is complete enough to
//! load".

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::Utc;

use super::meta::{ProducerRun, SubstrateMeta, CURRENT_SCHEMA_VERSION};
use super::scip::{ingest, producer_info, read_index};
use super::{
    index_log_path, index_path, meta_path, producer_index_path, producer_index_tmp_path,
    substrate_dir,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerTarget {
    pub project_dir: PathBuf,
    pub path_prefix: Option<String>,
}

pub struct ProducerSpec {
    pub name: &'static str,
    /// Detect one project target. Multi-project producers are a future registry seam.
    pub detect: fn(&Path) -> Option<ProducerTarget>,
    pub command: fn(&ProducerTarget, &Path) -> Command,
}

static PRODUCERS: [ProducerSpec; 2] = [
    ProducerSpec {
        name: "rust-analyzer",
        detect: detect_rust,
        command: rust_analyzer_command,
    },
    ProducerSpec {
        name: "scip-typescript",
        detect: detect_typescript,
        command: scip_typescript_command,
    },
];

pub fn registry() -> &'static [ProducerSpec] {
    &PRODUCERS
}

fn detect_rust(repo_root: &Path) -> Option<ProducerTarget> {
    repo_root
        .join("Cargo.toml")
        .is_file()
        .then(|| ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        })
}

fn rust_analyzer_command(target: &ProducerTarget, output_tmp: &Path) -> Command {
    let binary =
        std::env::var("MOOSEDEV_SCIP_PRODUCER").unwrap_or_else(|_| "rust-analyzer".to_string());
    let mut command = Command::new(binary);
    command
        .arg("scip")
        .arg(&target.project_dir)
        .arg("--output")
        .arg(output_tmp);
    command
}

fn detect_typescript(repo_root: &Path) -> Option<ProducerTarget> {
    if is_typescript_project(repo_root) {
        return Some(ProducerTarget {
            project_dir: repo_root.to_path_buf(),
            path_prefix: None,
        });
    }

    let mut directories = fs::read_dir(repo_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name != "node_modules" && !name.starts_with('.')
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| entry.file_name());

    directories.into_iter().find_map(|entry| {
        let project_dir = entry.path();
        is_typescript_project(&project_dir).then(|| ProducerTarget {
            project_dir,
            path_prefix: Some(format!("{}/", entry.file_name().to_string_lossy())),
        })
    })
}

fn is_typescript_project(path: &Path) -> bool {
    path.join("tsconfig.json").is_file() && path.join("package.json").is_file()
}

fn scip_typescript_command(target: &ProducerTarget, output_tmp: &Path) -> Command {
    let mut command = match std::env::var_os("MOOSEDEV_SCIP_TYPESCRIPT") {
        Some(binary) => {
            let mut command = Command::new(binary);
            command.arg("index");
            command
        }
        None => {
            let mut command = Command::new("npx");
            command.args(["--yes", "@sourcegraph/scip-typescript", "index"]);
            command
        }
    };
    command
        .arg("--output")
        .arg(output_tmp)
        .current_dir(&target.project_dir);
    command
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
    let _ = fs::remove_file(index_path(data_dir));
    remove_if_present(&meta_path(data_dir))?;

    let mut runs = Vec::new();
    let mut reports = Vec::new();
    let mut warnings = Vec::new();
    for (spec, target) in detected {
        write_log_header(data_dir, spec.name)?;
        match run_producer(spec, &target, data_dir) {
            Ok((run, report)) => {
                runs.push(run);
                reports.push(report);
            }
            Err(error) => warnings.push(exclude_failed_producer(spec, data_dir, &error)?),
        }
    }

    if reports.is_empty() {
        bail!(
            "all detected SCIP producers failed: {}",
            warnings.join("; ")
        );
    }

    let meta = SubstrateMeta {
        schema_version: CURRENT_SCHEMA_VERSION,
        indexed_commit: commit.clone(),
        indexed_at: Utc::now(),
        producers: runs,
    };
    meta.save(data_dir)
        .context("failed to write substrate metadata after SCIP index promotion")?;

    Ok(IndexReport {
        commit,
        duration: started.elapsed(),
        documents: meta.documents(),
        occurrences: meta.occurrences(),
        definitions: reports.iter().map(|report| report.definitions).sum(),
        index_bytes: reports.iter().map(|report| report.index_bytes).sum(),
        producers: reports,
        warnings,
    })
}

fn run_producer(
    spec: &ProducerSpec,
    target: &ProducerTarget,
    data_dir: &Path,
) -> Result<(ProducerRun, ProducerReport)> {
    let tmp_path = producer_index_tmp_path(data_dir, spec.name);
    let absolute_tmp_path = std::path::absolute(&tmp_path).with_context(|| {
        format!(
            "failed to absolutize temporary SCIP index {}",
            tmp_path.display()
        )
    })?;
    let final_path = producer_index_path(data_dir, spec.name);
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

fn exclude_failed_producer(
    spec: &ProducerSpec,
    data_dir: &Path,
    error: &anyhow::Error,
) -> Result<String> {
    remove_if_present(&producer_index_tmp_path(data_dir, spec.name))?;
    remove_if_present(&producer_index_path(data_dir, spec.name))?;
    let warning = format!(
        "producer `{}` failed and was excluded: {error:#}",
        spec.name
    );
    tracing::warn!(producer = spec.name, error = %error, "SCIP producer failed and was excluded");
    write_log_warning(data_dir, &warning)?;
    Ok(warning)
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
    use scip::types::{Document, Index, Occurrence, PositionEncoding};

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
            },
            ProducerSpec {
                name: "second",
                detect: detect_prefixed,
                command: copy_second,
            },
        ];

        let report = run_index_with(&specs, &repo_root, &data_dir).unwrap();
        assert_eq!(report.producers.len(), 2);
        assert!(producer_index_path(&data_dir, "first").is_file());
        assert!(producer_index_path(&data_dir, "second").is_file());
        let meta = SubstrateMeta::load(&data_dir).unwrap();
        assert_eq!(meta.producers.len(), 2);
        assert_eq!(meta.producers[1].path_prefix.as_deref(), Some("ui/"));
        let log = fs::read_to_string(index_log_path(&data_dir)).unwrap();
        assert!(log.contains("=== first ==="));
        assert!(log.contains("=== second ==="));

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(data_dir);
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
            },
            ProducerSpec {
                name: "failed",
                detect: detect_all,
                command: fail_command,
            },
        ];

        let report = run_index_with(&specs, &repo_root, &data_dir).unwrap();
        assert_eq!(report.producers.len(), 1);
        assert_eq!(report.warnings.len(), 1);
        assert!(!failed_path.exists());
        assert_eq!(SubstrateMeta::load(&data_dir).unwrap().producers.len(), 1);
        let log = fs::read_to_string(index_log_path(&data_dir)).unwrap();
        assert!(log.contains("WARNING: producer `failed` failed"));

        let _ = fs::remove_dir_all(repo_root);
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
