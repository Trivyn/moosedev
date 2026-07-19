use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use moosedev::code::substrate::producer::noteworthy_diagnostics;
use moosedev::code::substrate::{
    generation_dir, index_log_path, producer_index_path, producer_index_path_in, run_index,
    ProducerRun, Substrate, SubstrateMeta, STALE_CHECK_TTL,
};
use moosedev::graph::AppState;
use protobuf::{EnumOrUnknown, Message, MessageField};
use scip::types::{
    symbol_information, Document, Index, Occurrence, PositionEncoding, Signature, SymbolInformation,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moosedev-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temporary directory");
    dir
}

fn git(repo_root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_repo(tag: &str) -> PathBuf {
    let repo_root = fresh_dir(tag);
    git(&repo_root, &["init"]);
    git(
        &repo_root,
        &["config", "user.email", "tests@moosedev.local"],
    );
    git(&repo_root, &["config", "user.name", "MOOSEDev tests"]);
    std::fs::write(repo_root.join("tracked.rs"), "pub fn tracked() {}\n")
        .expect("write tracked source");
    std::fs::write(
        repo_root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .expect("write Cargo manifest");
    git(&repo_root, &["add", "tracked.rs", "Cargo.toml"]);
    git(&repo_root, &["commit", "-m", "initial"]);
    repo_root
}

fn python_git_repo(tag: &str) -> PathBuf {
    let repo_root = fresh_dir(tag);
    git(&repo_root, &["init"]);
    git(
        &repo_root,
        &["config", "user.email", "tests@moosedev.local"],
    );
    git(&repo_root, &["config", "user.name", "MOOSEDev tests"]);
    std::fs::write(
        repo_root.join("pyproject.toml"),
        "[project]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write pyproject manifest");
    std::fs::write(
        repo_root.join("main.py"),
        "def greet(name):\n    return name\n",
    )
    .expect("write python source");
    git(&repo_root, &["add", "pyproject.toml", "main.py"]);
    git(&repo_root, &["commit", "-m", "initial"]);
    repo_root
}

fn commit(repo_root: &Path, contents: &str) -> String {
    std::fs::write(repo_root.join("tracked.rs"), contents).expect("update tracked source");
    git(repo_root, &["add", "tracked.rs"]);
    git(repo_root, &["commit", "-m", "update"]);
    SubstrateMeta::current_head(repo_root)
}

fn index_with_definitions(definitions: usize) -> Index {
    let mut index = Index::new();
    let mut document = Document::new();
    document.relative_path = "src/lib.rs".to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);

    for number in 0..definitions {
        let symbol = format!("rust-analyzer cargo reload 0.1.0 lib/item_{number}().");
        let mut info = SymbolInformation::new();
        info.symbol = symbol.clone();
        info.display_name = format!("item_{number}");
        info.kind = EnumOrUnknown::new(symbol_information::Kind::Function);
        let mut signature = Signature::new();
        signature.text = format!("pub fn item_{number}()");
        info.signature_documentation = MessageField::some(signature);
        document.symbols.push(info);

        let mut occurrence = Occurrence::new();
        occurrence.symbol = symbol;
        occurrence.range = vec![number as i32, 0, 6];
        occurrence.symbol_roles = 1;
        occurrence.enclosing_range = vec![number as i32, 0, 6];
        document.occurrences.push(occurrence);
    }
    index.documents.push(document);
    index
}

fn write_substrate(data_dir: &Path, index: &Index, commit: &str, indexed_at: DateTime<Utc>) {
    let path = producer_index_path(data_dir, "rust-analyzer");
    std::fs::create_dir_all(path.parent().expect("substrate parent"))
        .expect("create substrate directory");
    std::fs::write(&path, index.write_to_bytes().expect("serialize SCIP index"))
        .expect("write SCIP index");
    SubstrateMeta::single(
        "rust-analyzer",
        commit,
        indexed_at,
        index.documents.len(),
        index
            .documents
            .iter()
            .map(|document| document.occurrences.len())
            .sum(),
    )
    .save(data_dir)
    .expect("write substrate metadata");
}

fn write_generation(
    data_dir: &Path,
    index: &Index,
    commit: &str,
    indexed_at: DateTime<Utc>,
) -> SubstrateMeta {
    let generation = format!("gen-{}", uuid::Uuid::new_v4());
    let artifact_root = generation_dir(data_dir, &generation);
    let path = producer_index_path_in(&artifact_root, "rust-analyzer");
    std::fs::create_dir_all(&artifact_root).expect("create generation directory");
    std::fs::write(&path, index.write_to_bytes().expect("serialize SCIP index"))
        .expect("write generation SCIP index");
    SubstrateMeta {
        schema_version: moosedev::code::substrate::meta::CURRENT_SCHEMA_VERSION,
        indexed_commit: commit.to_string(),
        indexed_at,
        indexed_started_at: None,
        generation: Some(generation),
        producers: vec![ProducerRun {
            name: "rust-analyzer".to_string(),
            producer: "rust-analyzer".to_string(),
            producer_version: "test".to_string(),
            mode: "scip".to_string(),
            documents: index.documents.len(),
            occurrences: index
                .documents
                .iter()
                .map(|document| document.occurrences.len())
                .sum(),
            path_prefix: None,
        }],
    }
}

fn publish_generation(
    data_dir: &Path,
    index: &Index,
    commit: &str,
    indexed_at: DateTime<Utc>,
) -> SubstrateMeta {
    let meta = write_generation(data_dir, index, commit, indexed_at);
    meta.save(data_dir).expect("publish generation manifest");
    meta
}

#[test]
fn disk_backed_substrate_updates_staleness_after_head_changes() {
    let repo_root = git_repo("live-stale-repo");
    let data_dir = fresh_dir("live-stale-data");
    let head = SubstrateMeta::current_head(&repo_root);
    write_substrate(&data_dir, &index_with_definitions(1), &head, Utc::now());

    let substrate = Substrate::load(&data_dir, &repo_root).expect("load substrate");
    assert!(!substrate.is_stale());

    commit(&repo_root, "pub fn tracked() { let _ = 1; }\n");
    std::thread::sleep(STALE_CHECK_TTL + Duration::from_millis(50));
    assert!(substrate.is_stale());
}

#[test]
fn app_state_reloads_completed_substrates_and_keeps_last_good_copy() {
    let repo_root = git_repo("reload-repo");
    let data_dir = fresh_dir("reload-data");
    let head = SubstrateMeta::current_head(&repo_root);
    let first_indexed_at = Utc::now();
    publish_generation(
        &data_dir,
        &index_with_definitions(1),
        &head,
        first_indexed_at,
    );

    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    state.load_substrate(&repo_root);
    assert_eq!(
        state
            .substrate()
            .expect("initial substrate")
            .stats()
            .definitions,
        1
    );

    let second_indexed_at = first_indexed_at + ChronoDuration::seconds(1);
    publish_generation(
        &data_dir,
        &index_with_definitions(2),
        &head,
        second_indexed_at,
    );
    let reloaded = state.substrate().expect("reloaded substrate");
    assert_eq!(reloaded.stats().definitions, 2);

    let corrupt_meta = write_generation(
        &data_dir,
        &index_with_definitions(3),
        &head,
        second_indexed_at + ChronoDuration::seconds(1),
    );
    let corrupt_path = producer_index_path_in(
        &corrupt_meta.artifact_root(&data_dir).unwrap(),
        "rust-analyzer",
    );
    std::fs::write(&corrupt_path, b"not a SCIP index").expect("corrupt SCIP index");
    corrupt_meta
        .save(&data_dir)
        .expect("publish corrupt metadata");
    let retained = state.substrate().expect("retained substrate");
    assert!(Arc::ptr_eq(&reloaded, &retained));
    assert_eq!(retained.stats().definitions, 2);

    // Repairing immutable artifact bytes without changing the manifest is only
    // a test probe: the failed identity must remain memoized and not be retried.
    std::fs::write(
        &corrupt_path,
        index_with_definitions(3)
            .write_to_bytes()
            .expect("serialize repaired SCIP index"),
    )
    .expect("repair corrupt SCIP index");
    let still_retained = state.substrate().expect("memoized failed generation");
    assert!(Arc::ptr_eq(&reloaded, &still_retained));

    publish_generation(
        &data_dir,
        &index_with_definitions(4),
        &head,
        second_indexed_at + ChronoDuration::seconds(2),
    );
    assert_eq!(
        state
            .substrate()
            .expect("new manifest identity recovers")
            .stats()
            .definitions,
        4
    );
}

#[test]
fn app_state_recovers_from_absent_substrate_without_restart() {
    let repo_root = git_repo("cold-recovery-repo");
    let data_dir = fresh_dir("cold-recovery-data");
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    state.load_substrate(&repo_root);
    assert!(state.substrate().is_none());

    let head = SubstrateMeta::current_head(&repo_root);
    publish_generation(&data_dir, &index_with_definitions(2), &head, Utc::now());

    let recovered = state.substrate().expect("cold substrate recovery");
    assert_eq!(recovered.stats().definitions, 2);
    assert_eq!(recovered.repo_root(), Some(repo_root.as_path()));
}

#[test]
fn candidate_is_invisible_until_the_manifest_is_published() {
    let repo_root = git_repo("manifest-last-repo");
    let data_dir = fresh_dir("manifest-last-data");
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    state.load_substrate(&repo_root);
    let head = SubstrateMeta::current_head(&repo_root);
    let meta = write_generation(&data_dir, &index_with_definitions(1), &head, Utc::now());

    assert!(state.substrate().is_none(), "candidate must not be visible");
    meta.save(&data_dir).expect("publish manifest last");
    assert_eq!(
        state
            .substrate()
            .expect("published candidate")
            .stats()
            .definitions,
        1
    );
}

#[test]
fn synthetic_substrate_injection_skips_disk_reload() {
    let data_dir = fresh_dir("synthetic-substrate");
    let state = AppState::bootstrap(&data_dir, &ontology_dir()).expect("bootstrap app state");
    let substrate = Arc::new(
        Substrate::from_index(
            index_with_definitions(1),
            SubstrateMeta::single("rust-analyzer", "synthetic", Utc::now(), 1, 1),
            false,
        )
        .expect("build synthetic substrate"),
    );
    state.set_substrate(substrate.clone());

    let returned = state.substrate().expect("synthetic substrate");
    assert!(Arc::ptr_eq(&substrate, &returned));
}

struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[cfg(unix)]
#[test]
fn python_producer_runs_via_override_and_publishes() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let repo_root = python_git_repo("python-producer-repo");
    let data_dir = fresh_dir("python-producer-data");

    let symbol = "scip-python python fixture 0.1 main/greet().";
    let mut index = Index::new();
    let mut metadata = scip::types::Metadata::new();
    let mut tool_info = scip::types::ToolInfo::new();
    tool_info.name = "scip-python".to_string();
    tool_info.version = "0.6.6".to_string();
    metadata.tool_info = MessageField::some(tool_info);
    index.metadata = MessageField::some(metadata);
    let mut document = Document::new();
    document.relative_path = "main.py".to_string();
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    let mut info = SymbolInformation::new();
    info.symbol = symbol.to_string();
    info.documentation = vec!["```python\ndef greet(name):\n```".to_string()];
    document.symbols.push(info);
    let mut occurrence = Occurrence::new();
    occurrence.symbol = symbol.to_string();
    occurrence.range = vec![0, 4, 9];
    occurrence.symbol_roles = 1;
    document.occurrences.push(occurrence);
    index.documents.push(document);

    let prepared_index = data_dir.join("prepared.scip");
    std::fs::write(
        &prepared_index,
        index.write_to_bytes().expect("serialize prepared index"),
    )
    .expect("write prepared index");
    // scip-python invocation is `index --output <path>`, so the output is "$3"
    // (the rust-analyzer fake's is "$4": `scip <project> --output <path>`).
    let script = data_dir.join("fake-scip-python.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncp '{}' \"$3\"\n", prepared_index.display()),
    )
    .expect("write fake producer");
    let mut permissions = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("make fake producer executable");
    let _restore = EnvRestore::set("MOOSEDEV_SCIP_PYTHON", &script);

    let report = run_index(&repo_root, &data_dir).expect("fake scip-python index run");
    assert_eq!(report.definitions, 1);
    let meta = SubstrateMeta::load(&data_dir).expect("load published metadata");
    assert_eq!(meta.producers.len(), 1);
    assert_eq!(meta.producers[0].name, "scip-python");
    assert!(producer_index_path_in(
        &meta
            .artifact_root(&data_dir)
            .expect("published artifact root"),
        "scip-python"
    )
    .is_file());

    let substrate = Substrate::load(&data_dir, &repo_root).expect("load python substrate");
    let definition = substrate
        .definition_for_symbol(symbol)
        .expect("python definition resolves");
    assert!(definition.is_public);
    assert_eq!(definition.signature.as_deref(), Some("def greet(name):"));
    let resolution = substrate
        .resolve(
            "main.py",
            moosedev::code::substrate::Position { line: 0, col: 5 },
        )
        .expect("position resolves inside greet");
    assert_eq!(resolution.symbol, symbol);
}

#[cfg(unix)]
#[test]
fn producer_logs_upstream_diagnostics_without_dumping_them() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let _verbose_restore = EnvRestore::remove("MOOSEDEV_INDEX_VERBOSE");
    let repo_root = git_repo("producer-repo");
    let data_dir = fresh_dir("producer-data");
    let prepared_index = data_dir.join("prepared.scip");
    std::fs::write(
        &prepared_index,
        index_with_definitions(1)
            .write_to_bytes()
            .expect("serialize prepared index"),
    )
    .expect("write prepared index");
    let script = data_dir.join("fake-producer.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho 'ERROR upstream issue' >&2\necho 'Duplicate symbol upstream issue' >&2\ncp '{}' \"$4\"\n",
            prepared_index.display()
        ),
    )
    .expect("write fake producer");
    let mut permissions = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("make fake producer executable");
    let _restore = EnvRestore::set("MOOSEDEV_SCIP_PRODUCER", &script);

    let report = run_index(&repo_root, &data_dir).expect("fake producer index run");
    assert_eq!(report.definitions, 1);
    assert_eq!(noteworthy_diagnostics(&data_dir), Some(2));
    assert!(
        producer_index_path_in(
            &SubstrateMeta::load(&data_dir)
                .expect("load published metadata")
                .artifact_root(&data_dir)
                .expect("published artifact root"),
            "rust-analyzer"
        )
        .is_file(),
        "index should be promoted"
    );
    let log = std::fs::read_to_string(index_log_path(&data_dir)).expect("read producer log");
    assert!(log.contains("ERROR upstream issue"));
    assert!(log.contains("Duplicate symbol upstream issue"));
}
