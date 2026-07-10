use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use moosedev::code::substrate::producer::noteworthy_diagnostics;
use moosedev::code::substrate::{
    index_log_path, producer_index_path, run_index, Substrate, SubstrateMeta, STALE_CHECK_TTL,
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
    write_substrate(
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
    write_substrate(
        &data_dir,
        &index_with_definitions(2),
        &head,
        second_indexed_at,
    );
    let reloaded = state.substrate().expect("reloaded substrate");
    assert_eq!(reloaded.stats().definitions, 2);

    std::fs::write(
        producer_index_path(&data_dir, "rust-analyzer"),
        b"not a SCIP index",
    )
    .expect("corrupt SCIP index");
    let corrupt_meta = SubstrateMeta::single(
        "rust-analyzer",
        head,
        second_indexed_at + ChronoDuration::seconds(1),
        3,
        3,
    );
    corrupt_meta.save(&data_dir).expect("bump corrupt metadata");
    let retained = state.substrate().expect("retained substrate");
    assert!(Arc::ptr_eq(&reloaded, &retained));
    assert_eq!(retained.stats().definitions, 2);
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
        producer_index_path(&data_dir, "rust-analyzer").is_file(),
        "index should be promoted"
    );
    let log = std::fs::read_to_string(index_log_path(&data_dir)).expect("read producer log");
    assert!(log.contains("ERROR upstream issue"));
    assert!(log.contains("Duplicate symbol upstream issue"));
}
