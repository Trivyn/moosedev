use super::*;
use super::{fs_ops::*, output::*, sandbox::*, scratch::*, workspace::*};
use crate::harness::progress::Progress;
use std::{
    fs::{self, File},
    path::PathBuf,
};

#[cfg(target_os = "macos")]
#[test]
fn scratch_flags_are_repaired_only_on_exclusively_owned_entries() {
    use std::os::fd::AsRawFd;
    let scratch = Fixture::new();
    let outside = Fixture::new();
    let task = TaskScratch::open(&scratch.0).unwrap();
    fs::write(task.path.join("build/flagged"), "warm").unwrap();
    let flagged = File::open(task.path.join("build/flagged")).unwrap();
    cvt(unsafe { libc::fchflags(flagged.as_raw_fd(), libc::UF_IMMUTABLE | libc::UF_APPEND) })
        .unwrap();
    let build = File::open(task.path.join("build")).unwrap();
    cvt(unsafe { libc::fchflags(build.as_raw_fd(), libc::UF_IMMUTABLE) }).unwrap();
    drop(task);
    cleanup_task(&scratch.0).unwrap();
    assert!(!scratch.0.exists());
    fs::write(outside.0.join("flagged"), "outside").unwrap();
    let external = File::open(outside.0.join("flagged")).unwrap();
    let task = TaskScratch::open(&scratch.0).unwrap();
    fs::hard_link(outside.0.join("flagged"), task.path.join("build/alias")).unwrap();
    cvt(unsafe { libc::fchflags(external.as_raw_fd(), libc::UF_IMMUTABLE) }).unwrap();
    drop(task);
    let result = cleanup_task(&scratch.0);
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstat(external.as_raw_fd(), &mut stat) }).unwrap();
    // Reset our fake external fixture before assertions so failure cannot
    // leave an immutable file behind in the test environment.
    cvt(unsafe { libc::fchflags(external.as_raw_fd(), 0) }).unwrap();
    assert!(
        result.is_err(),
        "cleanup modified an external hardlinked inode"
    );
    assert_ne!(stat.st_flags & libc::UF_IMMUTABLE, 0);
    cleanup_task(&scratch.0).unwrap();
}

#[test]
fn command_timeout_defaults_and_bounds_are_explicit() {
    assert_eq!(
        parse_command_timeout(Err(std::env::VarError::NotPresent)).unwrap(),
        Duration::from_secs(900)
    );
    for (input, seconds) in [("1", 1), ("900", 900), ("86400", 86400)] {
        assert_eq!(
            parse_command_timeout(Ok(input.into())).unwrap(),
            Duration::from_secs(seconds)
        );
    }
    for input in ["0", "86401", "-1", "NaN", "18446744073709551616", ""] {
        assert!(
            parse_command_timeout(Ok(input.into())).is_err(),
            "accepted {input}"
        );
    }
}

#[test]
fn permission_requests_are_canonical_deduplicated_and_typed() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    fs::create_dir(external.0.join("directory")).unwrap();
    fs::write(external.0.join("directory/file"), "readable").unwrap();
    fs::write(external.0.join("writable"), "old").unwrap();
    let directory = external.0.join("directory").display().to_string();
    let nested = external.0.join("directory/file").display().to_string();
    let writable = external.0.join("writable").display().to_string();
    let permissions = CommandPermissions::requested(
        &project.0,
        &scratch.0,
        &[nested, directory.clone(), directory.clone()],
        std::slice::from_ref(&writable),
        true,
    )
    .unwrap();
    assert_eq!(
        permissions.read_paths,
        vec![external.0.join("directory").canonicalize().unwrap()]
    );
    assert_eq!(
        permissions.write_paths,
        vec![external.0.join("writable").canonicalize().unwrap()]
    );
    assert!(permissions.read_paths[0].is_dir());
    assert!(permissions.write_paths[0].is_file());
    assert!(permissions.network);
}

#[test]
fn permission_requests_reject_nonexistent_root_and_protected_overlaps() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    let outside = Fixture::new();
    let missing = outside.0.join("missing").display().to_string();
    assert!(
        CommandPermissions::requested(&project.0, &scratch.0, &[missing], &[], false)
            .unwrap_err()
            .to_string()
            .contains("canonicalize external read path")
    );
    for protected in [&project.0, &scratch.0] {
        let protected = protected.display().to_string();
        assert!(CommandPermissions::requested(
            &project.0,
            &scratch.0,
            std::slice::from_ref(&protected),
            &[],
            false,
        )
        .unwrap_err()
        .to_string()
        .contains("overlaps the protected"));
    }
    let root = if cfg!(windows) {
        Path::new(r"C:\")
    } else {
        Path::new("/")
    };
    if root.exists() {
        assert!(CommandPermissions::requested(
            &project.0,
            &scratch.0,
            &[root.display().to_string()],
            &[],
            false,
        )
        .is_err());
    }
    assert!(CommandPermissions::requested(
        &project.0,
        &scratch.0,
        &["relative/path".into()],
        &[],
        false,
    )
    .is_err());
}

#[test]
fn permission_requests_normalize_a_not_yet_created_scratch_path() {
    let project = Fixture::new();
    let outside = Fixture::new();
    let scratch = project.0.join(".moosedev/harness/scratch/task");
    let permissions = CommandPermissions::requested(
        &project.0,
        &scratch,
        &[outside.0.display().to_string()],
        &[],
        false,
    )
    .unwrap();
    assert_eq!(
        permissions.read_paths,
        vec![outside.0.canonicalize().unwrap()]
    );
    assert!(CommandPermissions::requested(
        &project.0,
        &scratch,
        &[project.0.display().to_string()],
        &[],
        false,
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_scope_is_reported_and_an_aliasing_hardlink_is_refused() {
    use std::os::unix::fs::symlink;

    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    fs::write(project.0.join("protected"), "secret").unwrap();
    fs::create_dir(external.0.join("scope")).unwrap();
    symlink(project.0.join("protected"), external.0.join("scope/alias")).unwrap();
    let scope = external.0.join("scope").display().to_string();
    // A symlink cannot expand the grant: the sandbox matches the resolved path
    // (proved by `a_symlink_out_of_a_granted_directory_reaches_nothing`), so
    // this is reported for the human to weigh, not refused on their behalf.
    let (permissions, findings) = CommandPermissions::surveyed(
        &project.0,
        &scratch.0,
        std::slice::from_ref(&scope),
        &[],
        false,
    )
    .unwrap();
    assert_eq!(permissions.read_paths.len(), 1);
    assert_eq!(findings.escaping_symlink_count, 1);
    assert!(
        findings.escaping_symlinks[0].ends_with("scope/alias"),
        "{findings:?}"
    );
    assert!(findings.lines()[0].contains("lead outside"), "{findings:?}");
    // A hardlink has no resolution step, so the grant really would expose the
    // linked bytes. This one is on the workspace's own device, so it is an
    // alias and is refused.
    fs::remove_file(external.0.join("scope/alias")).unwrap();
    fs::hard_link(project.0.join("protected"), external.0.join("scope/alias")).unwrap();
    assert!(CommandPermissions::surveyed(
        &project.0,
        &scratch.0,
        std::slice::from_ref(&scope),
        &[],
        false
    )
    .unwrap_err()
    .to_string()
    .contains("hardlinked file"));
    // The structural pass never walks the tree, so it is unaffected either way.
    assert!(CommandPermissions::requested(&project.0, &scratch.0, &[scope], &[], false).is_ok());
}

/// `nlink > 1` is ordinary: uv hardlinks a venv out of its cache, cargo
/// hardlinks its own artifacts. It only matters when the other link could be
/// inside a protected tree, and a hardlink cannot cross filesystems.
#[cfg(unix)]
#[test]
fn a_hardlink_on_another_filesystem_cannot_alias_and_is_not_refused() {
    use std::os::unix::fs::MetadataExt;

    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    fs::create_dir(external.0.join("scope")).unwrap();
    fs::write(external.0.join("scope/one"), "content").unwrap();
    fs::hard_link(external.0.join("scope/one"), external.0.join("scope/two")).unwrap();
    assert_eq!(
        fs::metadata(external.0.join("scope/one")).unwrap().nlink(),
        2
    );
    let scope = external.0.join("scope").display().to_string();
    // Same device here, and neither link is in a protected tree, so the pair is
    // still refused today; the device narrowing is what spares a real venv.
    let same_device = fs::metadata(&project.0).unwrap().dev()
        == fs::metadata(external.0.join("scope")).unwrap().dev();
    let surveyed = CommandPermissions::surveyed(&project.0, &scratch.0, &[scope], &[], false);
    assert_eq!(surveyed.is_ok(), !same_device, "{surveyed:?}");
}

#[cfg(unix)]
#[test]
fn scratch_parent_symlinks_cannot_redirect_creation_or_cleanup() {
    use std::os::unix::fs::symlink;
    let project = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("keep"), "untouched").unwrap();
    for parent in [
        ".moosedev",
        ".moosedev/harness",
        ".moosedev/harness/scratch",
    ] {
        let alias = project.0.join(parent);
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink(&outside.0, &alias).unwrap();
        let scratch = project.0.join(".moosedev/harness/scratch/task");
        assert!(TaskScratch::open(&scratch).is_err(), "followed {parent}");
        assert!(cleanup_task(&scratch).is_err(), "cleanup followed {parent}");
        assert_eq!(
            fs::read_to_string(outside.0.join("keep")).unwrap(),
            "untouched"
        );
        assert!(!outside.0.join("task").exists());
        fs::remove_file(alias).unwrap();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn build_generations_copy_inodes_and_reject_fifo_socket_and_external_aliases() {
    use std::os::{
        fd::AsRawFd,
        unix::{
            fs::{symlink, MetadataExt},
            net::UnixListener,
        },
    };
    let scratch = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("secret"), "private").unwrap();
    let task = TaskScratch::open(&scratch.0).unwrap();
    fs::write(task.path.join("build/artifact"), "warm").unwrap();
    task.rotate_build().unwrap();
    let old = File::open(task.path.join("build/artifact")).unwrap();
    let old_metadata = old.metadata().unwrap();
    let build =
        open_owned_directory(&task.directory, &build_backing(&task.directory).unwrap()).unwrap();
    cvt(unsafe {
        libc::mkfifoat(
            build.as_raw_fd(),
            CString::new("fifo").unwrap().as_ptr(),
            0o600,
        )
    })
    .unwrap();
    let _socket = UnixListener::bind(task.path.join("build/socket")).unwrap();
    symlink(outside.0.join("secret"), task.path.join("build/alias")).unwrap();
    fs::hard_link(outside.0.join("secret"), task.path.join("build/hardalias")).unwrap();
    drop(task);
    let task = TaskScratch::open(&scratch.0).unwrap();
    task.rotate_build().unwrap();
    let new_metadata = fs::metadata(task.path.join("build/artifact")).unwrap();
    assert_ne!(old_metadata.ino(), new_metadata.ino());
    assert_eq!(
        old_metadata.modified().unwrap(),
        new_metadata.modified().unwrap()
    );
    assert_eq!(
        fs::read_to_string(task.path.join("build/artifact")).unwrap(),
        "warm"
    );
    for name in ["fifo", "socket", "alias", "hardalias"] {
        assert!(!task.path.join("build").join(name).exists());
    }
    drop(task);
    cleanup_task(&scratch.0).unwrap();
}

#[cfg(unix)]
#[test]
fn cache_enumeration_shares_entry_and_time_limits_across_passes() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a"), "one").unwrap();
    fs::write(fixture.0.join("b"), "two").unwrap();
    let directory = File::open(&fixture.0).unwrap();
    let mut budget = WalkBudget::new(Duration::from_secs(5));
    budget.remaining = 1;
    assert!(child_names_bounded(&directory, &mut budget)
        .unwrap_err()
        .to_string()
        .contains("entry limit"));
    let mut budget = WalkBudget::new(Duration::from_secs(5));
    budget.remaining = 2;
    assert!(sanitize_build_bounded(&directory, &mut budget)
        .unwrap_err()
        .to_string()
        .contains("entry limit"));
    assert_eq!(
        budget.remaining, 0,
        "the second pass must share the first pass's budget"
    );
    let mut budget = WalkBudget::new(Duration::ZERO);
    assert!(child_names_bounded(&directory, &mut budget)
        .unwrap_err()
        .to_string()
        .contains("time limit"));
    sanitize_build(&directory).unwrap();
    assert_eq!(fs::read_to_string(fixture.0.join("a")).unwrap(), "one");
}

#[cfg(unix)]
#[test]
fn large_directory_cleanup_makes_progress_across_bounded_batches() {
    let fixture = Fixture::new();
    let cache = fixture.0.join("cache");
    fs::create_dir(&cache).unwrap();
    for index in 0..4100 {
        fs::write(cache.join(index.to_string()), "disposable").unwrap();
    }
    let parent = File::open(&fixture.0).unwrap();
    assert!(remove_child(&parent, "cache").is_err());
    let remaining = fs::read_dir(&cache).unwrap().count();
    assert_eq!(
        remaining, 4,
        "first cleanup must delete its batch before returning"
    );
    remove_child(&parent, "cache").unwrap();
    assert!(!cache.exists());
}

#[tokio::test]
async fn output_progress_precedes_eof_and_preserves_split_unicode() {
    use tokio::io::AsyncWriteExt;
    let (input, mut writer) = tokio::io::duplex(64);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let read = tokio::spawn(bounded_output(input, Some(sender)));
    writer.write_all(b"hello \xf0\x9f").await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, Progress::CommandOutput(text) if text == "hello "));
    writer.write_all(b"\xab\x8e!").await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, Progress::CommandOutput(text) if text == "🫎!"));
    drop(writer);
    assert_eq!(
        String::from_utf8(read.await.unwrap().unwrap()).unwrap(),
        "hello 🫎!"
    );
}

#[tokio::test]
async fn output_progress_is_bounded_but_input_is_drained() {
    let bytes = vec![b'x'; MAX_OUTPUT * 3];
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let result = bounded_output(bytes.as_slice(), Some(sender))
        .await
        .unwrap();
    let mut observed = String::new();
    while let Some(Progress::CommandOutput(text)) = receiver.recv().await {
        observed.push_str(&text);
    }
    assert_eq!(observed.as_bytes(), result);
    assert_eq!(result.len(), MAX_OUTPUT + b"\n[output truncated]\n".len());
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // macOS's default temporary-directory prefix exceeds sockaddr_un's
        // path budget once a UUID and socket name are appended.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let path = base.join(format!("mdx-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn workspace(&self) -> Workspace {
        Workspace::new(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cas_create_update_delete_and_permissions() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    assert_eq!(workspace.read("code.rs").unwrap(), None);
    workspace.apply("code.rs", None, Some("old")).unwrap();
    assert!(workspace.apply("code.rs", None, Some("clobber")).is_err());
    assert!(workspace.apply("code.rs", Some("stale"), None).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(fixture.0.join("code.rs"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    workspace
        .apply("code.rs", Some("old"), Some("new"))
        .unwrap();
    assert_eq!(workspace.read("code.rs").unwrap().as_deref(), Some("new"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(fixture.0.join("code.rs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    workspace.apply("code.rs", Some("new"), None).unwrap();
    assert_eq!(workspace.read("code.rs").unwrap(), None);
    assert_eq!(workspace.read("new/deep/file.rs").unwrap(), None);
    workspace
        .apply("new/deep/file.rs", None, Some("new file"))
        .unwrap();
    assert_eq!(
        workspace.read("new/deep/file.rs").unwrap().as_deref(),
        Some("new file")
    );
}

#[test]
fn rejects_protected_paths_and_traversal() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    for file in [
        "",
        "../secret",
        "/etc/passwd",
        "a/../../b",
        ".git/config",
        "src/.GIT/config",
        ".moosedev/kg.nq",
        "target/build",
        "node_modules/package/index.js",
        ".env",
        ".env.local",
        ".codex/config.toml",
        ".ssh/id_ed25519",
        ".gnupg/private-keys-v1.d/key",
        ".aws/credentials",
        ".azure/accessTokens.json",
        ".kube/config",
        "a\\b",
        "nul\0",
    ] {
        assert!(workspace.read(file).is_err(), "read accepted {file:?}");
        assert!(
            workspace.apply(file, None, Some("bad")).is_err(),
            "write accepted {file:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinks_ancestors_and_hardlinks() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("secret"), "untouched").unwrap();
    symlink(&outside.0, fixture.0.join("ancestor")).unwrap();
    symlink(outside.0.join("secret"), fixture.0.join("link")).unwrap();
    fs::hard_link(outside.0.join("secret"), fixture.0.join("hard")).unwrap();
    let workspace = fixture.workspace();
    for file in ["ancestor/secret", "ancestor/new", "link", "hard"] {
        assert!(workspace.read(file).is_err());
        assert!(workspace.apply(file, None, Some("changed")).is_err());
    }
    assert!(workspace.files().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(outside.0.join("secret")).unwrap(),
        "untouched"
    );
}

#[test]
fn bounds_files_and_omits_binary_or_protected_entries() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("source"), "source").unwrap();
    fs::write(fixture.0.join("binary"), [0xff, 0xfe]).unwrap();
    File::create(fixture.0.join("large"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    fs::create_dir(fixture.0.join(".moosedev")).unwrap();
    fs::write(fixture.0.join(".moosedev/kg.nq"), "knowledge").unwrap();
    assert_eq!(fixture.workspace().files().unwrap(), vec!["source"]);
    assert!(fixture.workspace().read("large").is_err());
    assert!(fixture.workspace().read("binary").is_err());
}

#[tokio::test]
async fn output_drains_after_reaching_limit() {
    let input = vec![b'x'; MAX_OUTPUT * 3];
    let output = bounded_output(input.as_slice(), None).await.unwrap();
    assert!(output.len() < MAX_OUTPUT + 100);
    assert!(output.ends_with(b"[output truncated]\n"));
}

#[test]
fn runtime_allowlist_excludes_user_credentials_and_unrelated_system_files() {
    let mut denied = vec![
        PathBuf::from("/etc/hosts"),
        PathBuf::from("/private/etc/hosts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        for suffix in [
            ".ssh/id_ed25519",
            ".cargo/credentials.toml",
            ".aws/credentials",
        ] {
            denied.push(Path::new(&home).join(suffix));
        }
    }
    for denied in denied {
        assert!(
            !readable_directories()
                .iter()
                .any(|allowed| denied.starts_with(allowed)),
            "runtime directory grants credential read: {}",
            denied.display()
        );
        assert!(!readable_files().contains(&denied));
    }
}

#[cfg(unix)]
#[test]
fn only_a_regular_secret_free_cargo_config_joins_the_runtime_allowlist() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.toml");
    fs::write(&config, "[net]\ngit-fetch-with-cli = true\n").unwrap();
    assert!(secret_free_cargo_config(&config));

    for secret in [
        "[registry]\ntoken = \"fake\"\n",
        "[registries.private]\nindex = \"sparse+https://example.invalid/\"\nsecret-key = \"fake\"\n",
        "not = [valid",
    ] {
        fs::write(&config, secret).unwrap();
        assert!(!secret_free_cargo_config(&config), "{secret}");
    }

    let clean = fixture.0.join("clean.toml");
    fs::write(&clean, "[build]\njobs = 2\n").unwrap();
    let alias = fixture.0.join("alias.toml");
    std::os::unix::fs::symlink(&clean, &alias).unwrap();
    assert!(secret_free_cargo_config(&clean));
    assert!(!secret_free_cargo_config(&alias));
    assert!(!secret_free_cargo_config(&fixture.0.join("absent.toml")));
}

/// Task scratch normally lives under HOME, where Cargo's ancestor walk finds
/// the user's configuration. Must run outside a parent sandbox.
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn confinement_reads_cargo_configuration_but_never_cargo_credentials() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let config = home.join(".cargo/config.toml");
    if !secret_free_cargo_config(&config) {
        eprintln!("skipped: no secret-free {}", config.display());
        return;
    }
    // Under HOME, as a real project and its task scratch are.
    let under_home = |name: &str| {
        let path = home.join(format!(".mdx-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Fixture(path)
    };
    let (fixture, scratch) = (under_home("project"), under_home("scratch"));
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"harness-check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n",
    )
    .unwrap();
    fs::write(fixture.0.join("lib.rs"), "").unwrap();
    let metadata = command(
        &fixture.0,
        &scratch.0,
        "cargo metadata --offline --no-deps --format-version 1 >/dev/null",
    )
    .await
    .unwrap();
    assert!(metadata.success, "{}", metadata.output);
    let credentials = home.join(".cargo/credentials.toml");
    if credentials.is_file() {
        let denied = command(
            &fixture.0,
            &scratch.0,
            &format!("cat '{}' >/dev/null", credentials.display()),
        )
        .await
        .unwrap();
        assert!(!denied.success, "cargo credentials were readable");
    }
    cleanup_task(&scratch.0).unwrap();
}

#[cfg(unix)]
#[test]
fn command_snapshot_omits_protected_paths_and_aliases_and_preserves_binary_fixtures() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let output = Fixture::new();
    fs::write(fixture.0.join("source.rs"), "source").unwrap();
    fs::write(fixture.0.join("binary.fixture"), [0xff, 0x00, 0xfe]).unwrap();
    fs::write(fixture.0.join(".env"), "fake project credential").unwrap();
    fs::create_dir(fixture.0.join(".ssh")).unwrap();
    fs::write(
        fixture.0.join(".ssh/id_ed25519"),
        "fake project private key",
    )
    .unwrap();
    fs::write(outside.0.join("credential"), "fake host credential").unwrap();
    symlink(&outside.0, fixture.0.join("linked-directory")).unwrap();
    symlink(outside.0.join("credential"), fixture.0.join("linked-file")).unwrap();
    fs::hard_link(
        outside.0.join("credential"),
        fixture.0.join("hardlinked-file"),
    )
    .unwrap();
    let destination = output.0.join("source");
    snapshot_source(&fixture.workspace(), &destination).unwrap();
    assert_eq!(
        fs::read(destination.join("binary.fixture")).unwrap(),
        [0xff, 0x00, 0xfe]
    );
    assert_eq!(
        fs::read_to_string(destination.join("source.rs")).unwrap(),
        "source"
    );
    for name in [
        ".env",
        ".ssh",
        "linked-directory",
        "linked-file",
        "hardlinked-file",
    ] {
        assert!(!destination.join(name).exists(), "snapshot exposed {name}");
    }
}

#[cfg(unix)]
#[test]
fn snapshots_skip_tagged_caches_but_preserve_nested_source_and_mtimes() {
    let fixture = Fixture::new();
    let output = Fixture::new();
    for name in ["build", "dist", "target"] {
        fs::create_dir_all(fixture.0.join("src").join(name)).unwrap();
        fs::write(fixture.0.join("src").join(name).join("code.rs"), "source").unwrap();
    }
    for path in ["clients/zed/target", "nested/cache-data"] {
        fs::create_dir_all(fixture.0.join(path)).unwrap();
        fs::write(
            fixture.0.join(path).join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# Generated cache\n",
        )
        .unwrap();
        // Exclusion must happen before charging file sizes or copying data.
        File::create(fixture.0.join(path).join("huge.rlib"))
            .unwrap()
            .set_len(600 * 1024 * 1024)
            .unwrap();
        fs::write(fixture.0.join(path).join("generated.rs"), "cached").unwrap();
    }
    snapshot_source(&fixture.workspace(), &output.0.join("source")).unwrap();
    let files = fixture.workspace().files().unwrap();
    assert_eq!(files.len(), 3);
    for path in ["clients/zed/target", "nested/cache-data"] {
        assert!(!output.0.join("source").join(path).exists());
    }
    for name in ["build", "dist", "target"] {
        let path = Path::new("src").join(name).join("code.rs");
        assert!(files.contains(&path.to_string_lossy().into_owned()));
        assert_eq!(
            fixture
                .workspace()
                .read(path.to_str().unwrap())
                .unwrap()
                .as_deref(),
            Some("source")
        );
        assert_eq!(
            fs::metadata(fixture.0.join(&path))
                .unwrap()
                .modified()
                .unwrap(),
            fs::metadata(output.0.join("source").join(path))
                .unwrap()
                .modified()
                .unwrap()
        );
    }
}

#[cfg(unix)]
#[test]
fn cache_markers_require_an_unaliased_regular_file_and_exact_signature() {
    use std::os::{unix::fs::symlink, unix::net::UnixListener};
    let fixture = Fixture::new();
    let directory = File::open(&fixture.0).unwrap();
    let marker = fixture.0.join("CACHEDIR.TAG");
    let signature = b"Signature: 8a477f597d28d172789f06886806bc55";
    assert!(!cache_directory(&directory));
    for invalid in [
        b"".as_slice(),
        &signature[..42],
        b"signature: 8a477f597d28d172789f06886806bc55",
        b" Signature: 8a477f597d28d172789f06886806bc55",
    ] {
        fs::write(&marker, invalid).unwrap();
        assert!(!cache_directory(&directory));
    }
    fs::write(&marker, signature).unwrap();
    assert!(cache_directory(&directory), "newline is not required");
    fs::remove_file(&marker).unwrap();

    let external = Fixture::new();
    fs::write(external.0.join("tag"), signature).unwrap();
    symlink(external.0.join("tag"), &marker).unwrap();
    assert!(!cache_directory(&directory));
    fs::remove_file(&marker).unwrap();
    fs::hard_link(external.0.join("tag"), &marker).unwrap();
    assert!(!cache_directory(&directory));
    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    assert!(!cache_directory(&directory));
    fs::remove_dir(&marker).unwrap();
    let socket = UnixListener::bind(&marker).unwrap();
    assert!(!cache_directory(&directory));
    drop(socket);
    fs::remove_file(&marker).unwrap();
    let name = CString::new(marker.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(!cache_directory(&directory), "FIFO markers must not block");
    fs::remove_file(&marker).unwrap();

    // An invalid marker is ordinary source, not a reason to hide a directory.
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/CACHEDIR.TAG"), "not a cache").unwrap();
    fs::write(fixture.0.join("src/code.rs"), "source").unwrap();
    let output = Fixture::new();
    snapshot_source(&fixture.workspace(), &output.0.join("source")).unwrap();
    assert!(fixture
        .workspace()
        .files()
        .unwrap()
        .contains(&"src/code.rs".into()));
    assert!(output.0.join("source/src/code.rs").exists());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "copies the current repository and requires functional OS confinement"]
async fn populated_repository_command_excludes_nested_cargo_cache() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = Fixture::new();
    let result = command(root, &scratch.0,
        "test -f Cargo.toml && test -f src/harness/executor.rs && test -f clients/zed/Cargo.toml && test ! -e clients/zed/target").await.unwrap();
    assert!(result.success, "{}", result.output);
    let source = scratch.0.join("source");
    assert!(!source.join("clients/zed/target").exists());
    let mut directories = vec![source];
    let mut bytes = 0;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                directories.push(entry.path());
            } else {
                bytes += entry.metadata().unwrap().len();
            }
        }
    }
    eprintln!("Current repository source snapshot: {bytes} bytes; nested Cargo cache excluded");
    assert!(bytes <= 512 * 1024 * 1024);
    cleanup_task(&scratch.0).unwrap();
}

#[cfg(unix)]
#[test]
fn scratch_reuse_and_cleanup_never_follow_command_created_aliases() {
    use std::os::unix::fs::symlink;
    let scratch = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("secret"), "untouched").unwrap();
    let task = TaskScratch::open(&scratch.0).unwrap();
    fs::write(task.path.join("build/artifact"), "warm").unwrap();
    fs::hard_link(
        task.path.join("build/artifact"),
        task.path.join("build/artifact-link"),
    )
    .unwrap();
    symlink(&outside.0, task.path.join("build/directory-alias")).unwrap();
    fs::hard_link(outside.0.join("secret"), task.path.join("build/hard-alias")).unwrap();
    fs::create_dir(task.path.join("legacy-command")).unwrap();
    symlink(&outside.0, task.path.join("legacy-command/alias")).unwrap();
    drop(task);
    let task = TaskScratch::open(&scratch.0).unwrap();
    assert_eq!(
        fs::read_to_string(task.path.join("build/artifact")).unwrap(),
        "warm"
    );
    assert_eq!(
        fs::read_to_string(task.path.join("build/artifact-link")).unwrap(),
        "warm"
    );
    assert!(!task.path.join("build/directory-alias").exists());
    assert!(!task.path.join("build/hard-alias").exists());
    assert!(!task.path.join("legacy-command").exists());
    assert!(
        TaskScratch::open(&scratch.0).is_err(),
        "concurrent scratch use was allowed"
    );
    assert!(
        cleanup_task(&scratch.0).is_err(),
        "active scratch was removed"
    );
    drop(task);
    cleanup_task(&scratch.0).unwrap();
    assert_eq!(
        fs::read_to_string(outside.0.join("secret")).unwrap(),
        "untouched"
    );
    fs::create_dir(&scratch.0).unwrap();
    symlink(&outside.0, scratch.0.join("build")).unwrap();
    assert!(
        TaskScratch::open(&scratch.0).is_err(),
        "replaced build root was followed"
    );
    cleanup_task(&scratch.0).unwrap();
    assert_eq!(
        fs::read_to_string(outside.0.join("secret")).unwrap(),
        "untouched"
    );
}

#[cfg(unix)]
#[test]
fn scratch_repairs_owned_permissions_and_cleans_deep_trees_without_recursion() {
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
    let scratch = Fixture::new();
    let task = TaskScratch::open(&scratch.0).unwrap();
    fs::create_dir(task.path.join("build/locked")).unwrap();
    fs::write(task.path.join("build/locked/artifact"), "warm").unwrap();
    for path in ["build/locked", "build"] {
        fs::set_permissions(task.path.join(path), fs::Permissions::from_mode(0o0)).unwrap();
    }
    drop(task);
    fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o0)).unwrap();
    let task = TaskScratch::open(&scratch.0).unwrap();
    assert_eq!(
        fs::read_to_string(task.path.join("build/locked/artifact")).unwrap(),
        "warm"
    );
    let temporary = TemporaryDirectory::new(&task.path, "command").unwrap();
    let mut parent = File::open(&temporary.path).unwrap();
    let name = CString::new("d").unwrap();
    // Descriptor-relative construction reaches beyond ordinary PATH_MAX and
    // the old recursion guard without keeping one fd open per ancestor.
    for _ in 0..600 {
        cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) }).unwrap();
        let child = open_owned_directory(&parent, &name).unwrap();
        parent
            .set_permissions(fs::Permissions::from_mode(0o0))
            .unwrap();
        parent = child;
    }
    drop(parent);
    let temporary_path = temporary.path.clone();
    drop(temporary);
    // Drop has a deadline. Explicit cleanup retries must make progress even
    // when repairing an unusually deep tree takes more than one budget.
    for _ in 0..3 {
        if !temporary_path.exists() {
            break;
        }
        let name = CString::new(temporary_path.file_name().unwrap().as_encoded_bytes()).unwrap();
        let _ = remove_child_name(&task.directory, &name);
    }
    assert!(
        !temporary_path.exists(),
        "deep command scratch did not recover through bounded cleanup retries"
    );
    for path in ["build/locked", "build"] {
        fs::set_permissions(task.path.join(path), fs::Permissions::from_mode(0o0)).unwrap();
    }
    drop(task);
    fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o0)).unwrap();
    cleanup_task(&scratch.0).unwrap();
    assert!(!scratch.0.exists());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn confinement_denies_host_and_project_secret_reads_including_aliases() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let scratch = Fixture::new();
    fs::create_dir_all(outside.0.join(".ssh")).unwrap();
    fs::create_dir_all(outside.0.join(".cargo")).unwrap();
    fs::write(
        outside.0.join(".ssh/id_ed25519"),
        "FAKE_HOST_SECRET_SENTINEL",
    )
    .unwrap();
    fs::write(
        outside.0.join(".cargo/credentials.toml"),
        "FAKE_CARGO_SECRET_SENTINEL",
    )
    .unwrap();
    fs::write(fixture.0.join(".env"), "FAKE_PROJECT_SECRET_SENTINEL").unwrap();
    fs::create_dir_all(fixture.0.join(".moosedev")).unwrap();
    fs::write(fixture.0.join(".moosedev/kg.nq"), "FAKE_GRAPH_SENTINEL").unwrap();
    fs::create_dir_all(fixture.0.join(".git")).unwrap();
    fs::write(fixture.0.join(".git/config"), "FAKE_GIT_SECRET_SENTINEL").unwrap();
    fs::write(fixture.0.join("source"), "public-source").unwrap();
    symlink(outside.0.join(".ssh/id_ed25519"), fixture.0.join("alias")).unwrap();
    fs::hard_link(
        outside.0.join(".ssh/id_ed25519"),
        fixture.0.join("hardalias"),
    )
    .unwrap();
    let outside = outside.0.canonicalize().unwrap();
    let original = fixture.0.canonicalize().unwrap();
    let command_text = format!(
        "set -e; cat source; \
        for denied in .env .moosedev/kg.nq .git/config alias hardalias /etc/hosts \
            '{outside}/.ssh/id_ed25519' '{outside}/.cargo/credentials.toml' '{original}/.env'; do \
            if cat \"$denied\"; then exit 21; fi; done; \
        ln -s '{outside}/.ssh/id_ed25519' \"$TMPDIR/alias\"; \
        if cat \"$TMPDIR/alias\"; then exit 22; fi; \
        if ln '{outside}/.ssh/id_ed25519' \"$TMPDIR/hardalias\"; then exit 23; fi; \
        printf success",
        outside = outside.display(),
        original = original.display(),
    );
    let result = command(&fixture.0, &scratch.0, &command_text)
        .await
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("public-source"), "{}", result.output);
    assert!(result.output.contains("success"), "{}", result.output);
    assert!(
        !result.output.contains("SENTINEL"),
        "secret escaped: {}",
        result.output
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn confinement_honors_only_explicit_external_file_and_directory_grants() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    fs::write(project.0.join("source"), "unchanged").unwrap();
    fs::write(external.0.join("readable"), "approved-read").unwrap();
    fs::write(external.0.join("hidden"), "must-not-read").unwrap();
    fs::create_dir(external.0.join("writable")).unwrap();
    fs::write(external.0.join("writable/existing"), "old").unwrap();
    let readable = external.0.join("readable").display().to_string();
    let hidden = external.0.join("hidden").display().to_string();
    let writable = external.0.join("writable").display().to_string();
    let permissions = CommandPermissions::requested(
        &project.0,
        &scratch.0,
        std::slice::from_ref(&readable),
        std::slice::from_ref(&writable),
        false,
    )
    .unwrap();
    let command_text = format!(
        "set -e; cat '{readable}'; ! cat '{hidden}'; ! sh -c \"echo bad > '{readable}'\"; \
         echo changed > '{writable}/existing'; echo created > '{writable}/new'; \
         rm '{writable}/existing'; test ! -e '{writable}/existing'; printf success"
    );
    let result =
        command_with_permissions(&project.0, &scratch.0, &command_text, &permissions, None)
            .await
            .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("approved-read"), "{}", result.output);
    assert!(
        !result.output.contains("must-not-read"),
        "{}",
        result.output
    );
    assert_eq!(
        fs::read_to_string(external.0.join("readable")).unwrap(),
        "approved-read"
    );
    assert_eq!(
        fs::read_to_string(external.0.join("writable/new")).unwrap(),
        "created\n"
    );
    assert!(!external.0.join("writable/existing").exists());
    assert_eq!(
        fs::read_to_string(project.0.join("source")).unwrap(),
        "unchanged"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires curl, internet access and a functional OS sandbox"]
async fn network_grant_resolves_names_and_verifies_tls() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    fs::write(project.0.join("source"), "source").unwrap();
    let fetch = "curl -sS --max-time 10 -o /dev/null -w '%{http_code}' https://example.com/";
    let granted = CommandPermissions::requested(&project.0, &scratch.0, &[], &[], true).unwrap();
    let result = command_with_permissions(&project.0, &scratch.0, fetch, &granted, None)
        .await
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("200"), "{}", result.output);
    // Without the grant the same fetch stays confined.
    let result = command(&project.0, &scratch.0, fetch).await.unwrap();
    assert!(!result.success, "{}", result.output);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires curl and functional OS sandbox; run explicitly outside nested sandbox"]
async fn confinement_enables_host_network_only_when_granted() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    fs::write(project.0.join("source"), "source").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let permissions =
        CommandPermissions::requested(&project.0, &scratch.0, &[], &[], true).unwrap();
    let result = command_with_permissions(
        &project.0,
        &scratch.0,
        &format!(
            "curl --max-time 1 http://{} >/dev/null 2>&1 || true",
            listener.local_addr().unwrap()
        ),
        &permissions,
        None,
    )
    .await
    .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(
        listener.accept().is_ok(),
        "network grant did not reach listener"
    );
    let socket_path = external.0.join("daemon.sock");
    let unix_listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    unix_listener.set_nonblocking(true).unwrap();
    let result = command_with_permissions(
        &project.0,
        &scratch.0,
        &format!(
            "curl --max-time 1 --unix-socket '{}' http://localhost",
            socket_path.display()
        ),
        &permissions,
        None,
    )
    .await
    .unwrap();
    assert!(!result.success, "filesystem-ungranted socket was reachable");
    assert_eq!(
        unix_listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let socket_permissions = CommandPermissions::requested(
        &project.0,
        &scratch.0,
        &[external.0.display().to_string()],
        &[],
        true,
    )
    .unwrap();
    let result = command_with_permissions(
        &project.0,
        &scratch.0,
        &format!(
            "curl --max-time 1 --unix-socket '{}' http://localhost >/dev/null 2>&1 || true",
            socket_path.display()
        ),
        &socket_permissions,
        None,
    )
    .await
    .unwrap();
    assert!(result.success, "{}", result.output);
    // Linux shares the host network namespace under a grant, so it admits only
    // IP sockets there; macOS scopes Unix sockets by their filesystem path.
    #[cfg(target_os = "linux")]
    assert_eq!(
        unix_listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    #[cfg(target_os = "macos")]
    {
        assert!(
            unix_listener.accept().is_ok(),
            "filesystem-granted Unix socket was unreachable"
        );
        // One socket can be granted by its exact path, without its directory.
        let exact = CommandPermissions::requested(
            &project.0,
            &scratch.0,
            &[socket_path.display().to_string()],
            &[],
            true,
        )
        .unwrap();
        assert_eq!(exact.read_paths, vec![socket_path.canonicalize().unwrap()]);
        fs::write(external.0.join("sibling"), "not granted").unwrap();
        let result = command_with_permissions(
            &project.0,
            &scratch.0,
            &format!(
                "curl --max-time 1 --unix-socket '{}' http://localhost >/dev/null 2>&1; cat '{}'",
                socket_path.display(),
                external.0.join("sibling").display()
            ),
            &exact,
            None,
        )
        .await
        .unwrap();
        assert!(
            unix_listener.accept().is_ok(),
            "exactly granted Unix socket was unreachable"
        );
        assert!(!result.success, "socket grant exposed its directory");
    }
}

/// Must run outside a parent sandbox which forbids installing OS sandboxes.
#[tokio::test]
#[ignore = "requires Rust toolchain and functional OS sandbox; run explicitly"]
async fn confinement_can_run_rust_checks_without_writing_source() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    fs::write(fixture.0.join("Cargo.toml"), "[package]\nname = \"harness-check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
    fs::write(
        fixture.0.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"harness-check\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        fixture.0.join("lib.rs"),
        "#[test]\nfn arithmetic() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    let before = fixture.workspace().files().unwrap();
    let result = command(&fixture.0, &scratch.0, "cargo test --offline --locked")
        .await
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("1 passed"), "{}", result.output);
    assert_eq!(fixture.workspace().files().unwrap(), before);
    assert!(!fixture.0.join("target").exists());
    let snapshot = scratch.0.join("source/lib.rs");
    let modified = fs::metadata(&snapshot).unwrap().modified().unwrap();
    fs::write(fixture.0.join("obsolete.txt"), "remove me").unwrap();
    let warm = command(
        &fixture.0,
        &scratch.0,
        "cargo test --offline --locked --verbose",
    )
    .await
    .unwrap();
    assert!(warm.success, "{}", warm.output);
    assert!(
        warm.output.contains("Fresh harness-check"),
        "cache was cold: {}",
        warm.output
    );
    assert_eq!(
        fs::metadata(&snapshot).unwrap().modified().unwrap(),
        modified
    );
    assert!(scratch.0.join("source/obsolete.txt").exists());
    fs::remove_file(fixture.0.join("obsolete.txt")).unwrap();
    fs::write(
        fixture.0.join("lib.rs"),
        "#[test] fn changed() { assert_eq!(2 + 2, 5); }\n",
    )
    .unwrap();
    let changed = command(&fixture.0, &scratch.0, "cargo test --offline --locked")
        .await
        .unwrap();
    assert!(
        !changed.success,
        "changed source was ignored: {}",
        changed.output
    );
    assert!(changed.output.contains("changed"), "{}", changed.output);
    assert!(!scratch.0.join("source/obsolete.txt").exists());
    assert_eq!(
        fs::read_dir(&scratch.0).unwrap().count(),
        if cfg!(target_os = "macos") { 3 } else { 2 },
        "ephemeral command directories leaked"
    );
    cleanup_task(&scratch.0).unwrap();
    assert!(!scratch.0.exists());
}

/// Must run outside a parent sandbox which forbids installing OS sandboxes.
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn confinement_blocks_source_graph_network_and_allows_scratch() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    fs::write(fixture.0.join("source"), "unchanged").unwrap();
    fs::create_dir(fixture.0.join(".moosedev")).unwrap();
    fs::write(fixture.0.join(".moosedev/kg.nq"), "unchanged").unwrap();
    // The final status proves scratch writes worked, not just sandbox failure.
    let result = command(&fixture.0, &scratch.0,
        "test -z \"$SECRET_TOKEN\" && ! (echo bad > source) && ! (echo bad > .moosedev/kg.nq) && echo good > \"$TMPDIR/allowed\" && cat \"$TMPDIR/allowed\"").await.unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("good"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("source")).unwrap(),
        "unchanged"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join(".moosedev/kg.nq")).unwrap(),
        "unchanged"
    );
    for escape in [
        "mv \"$CARGO_TARGET_DIR\" \"$CARGO_TARGET_DIR-moved\"",
        "mv \"$PWD\" \"$PWD-moved\"",
        "ln source \"$TMPDIR/alias\" && echo bad > \"$TMPDIR/alias\"",
        "ln -s \"$PWD/.moosedev/kg.nq\" \"$TMPDIR/alias\" && echo bad > \"$TMPDIR/alias\"",
    ] {
        let result = command(&fixture.0, &scratch.0, escape).await.unwrap();
        assert!(
            !result.success,
            "scratch alias bypassed confinement: {}",
            result.output
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.0.join("source")).unwrap(),
        "unchanged"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join(".moosedev/kg.nq")).unwrap(),
        "unchanged"
    );
    let curl = command(&fixture.0, &scratch.0, "command -v curl")
        .await
        .unwrap();
    assert!(curl.success, "integration test requires curl");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let network = command(
        &fixture.0,
        &scratch.0,
        &format!(
            "curl --max-time 2 http://{}",
            listener.local_addr().unwrap()
        ),
    )
    .await
    .unwrap();
    assert!(!network.success);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    #[cfg(unix)]
    {
        let socket_path = fixture.0.join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = command(
            &fixture.0,
            &scratch.0,
            "curl --max-time 2 --unix-socket daemon.sock http://localhost",
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn timeout_kills_process_group() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    let result = run_command(
        &fixture.0,
        &scratch.0,
        "sleep 30 & echo $! > \"$CARGO_TARGET_DIR/child\"; wait",
        Duration::from_millis(500),
        &CommandPermissions::default(),
        None,
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("timeout"));
    assert_child_terminated(&scratch).await;
}

#[cfg(unix)]
async fn assert_child_terminated(scratch: &Fixture) {
    let pid: libc::pid_t = fs::read_to_string(scratch.0.join("build/child"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..40 {
        if unsafe { libc::kill(pid, 0) } < 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("command descendant {pid} survived cancellation");
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn cancellation_kills_process_group() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    let root = fixture.0.clone();
    let temporary = scratch.0.clone();
    let task = tokio::spawn(async move {
        command(
            &root,
            &temporary,
            "sleep 30 & echo $! > \"$CARGO_TARGET_DIR/child\"; wait",
        )
        .await
    });
    let mut started = false;
    for _ in 0..100 {
        started = scratch.0.join("build/child").is_file();
        if started {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();
    let _ = task.await;
    assert!(started, "sandboxed command never started");
    assert_child_terminated(&scratch).await;
}

/// A project without a lockfile gets an empty one in its snapshot; the one the
/// toolchain generates is carried into the next snapshot; a project lockfile
/// always wins.
#[test]
fn a_generated_lockfile_is_carried_between_snapshots_until_the_project_has_one() {
    let scratch = Fixture::new();
    let previous = scratch.0.join("source");
    let staged = scratch.0.join("staged");
    fs::create_dir_all(&previous).unwrap();
    fs::create_dir_all(&staged).unwrap();
    fs::create_dir_all(scratch.0.join("build")).unwrap();

    // Not a Cargo project: nothing is added and nothing is writable.
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert!(!staged.join("Cargo.lock").exists());
    assert!(writable_snapshot_files(&staged).is_empty());

    // A Cargo project without a lockfile carries an empty one.
    fs::write(staged.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert_eq!(fs::read(staged.join("Cargo.lock")).unwrap(), b"");
    assert_eq!(
        writable_snapshot_files(&staged),
        vec![staged.join("Cargo.lock")]
    );

    // The toolchain filled the previous snapshot's lockfile: carried.
    fs::remove_file(staged.join("Cargo.lock")).unwrap();
    fs::write(previous.join("Cargo.lock"), "version = 4\n").unwrap();
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert_eq!(
        fs::read_to_string(staged.join("Cargo.lock")).unwrap(),
        "version = 4\n"
    );

    // A build-side copy (the Linux backing) is preferred over the snapshot's.
    fs::remove_file(staged.join("Cargo.lock")).unwrap();
    fs::write(scratch.0.join("build/Cargo.lock"), "version = 4\n# build\n").unwrap();
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert_eq!(
        fs::read_to_string(staged.join("Cargo.lock")).unwrap(),
        "version = 4\n# build\n"
    );

    // A project lockfile in the snapshot is left alone.
    fs::write(staged.join("Cargo.lock"), "version = 4\n# project\n").unwrap();
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert_eq!(
        fs::read_to_string(staged.join("Cargo.lock")).unwrap(),
        "version = 4\n# project\n"
    );
}

/// Every lockfile root gets a writable lockfile, not only the top level: a
/// standalone crate in a subdirectory, and not a workspace member, whose
/// workspace root owns the lockfile. badciv f2fe1f61 built a crate in
/// `badciv-map/` with no root manifest and every build was denied.
#[test]
fn every_cargo_lockfile_root_gets_a_writable_carried_lockfile() {
    let scratch = Fixture::new();
    let previous = scratch.0.join("source");
    let staged = scratch.0.join("staged");
    let manifest = |dir: &Path, text: &str| {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("Cargo.toml"), text).unwrap();
    };
    fs::create_dir_all(&previous).unwrap();
    fs::create_dir_all(scratch.0.join("build")).unwrap();
    // A standalone crate beside a workspace with a member, and build output
    // and hidden directories that are never walked.
    manifest(&staged.join("tool"), "[package]\nname = \"tool\"\n");
    manifest(&staged.join("game"), "[workspace]\nmembers = [\"core\"]\n");
    manifest(&staged.join("game/core"), "[package]\nname = \"core\"\n");
    // A workspace of its own inside another's tree owns its lockfile.
    manifest(
        &staged.join("game/tools"),
        "[workspace]\n[package]\nname = \"tools\"\n",
    );
    manifest(&staged.join("target/debug/x"), "[package]\nname = \"x\"\n");
    manifest(&staged.join(".hidden"), "[package]\nname = \"h\"\n");
    assert_eq!(
        cargo_lockfile_roots(&staged),
        vec![
            PathBuf::from("game"),
            PathBuf::from("game/tools"),
            PathBuf::from("tool")
        ]
    );

    fs::create_dir_all(previous.join("tool")).unwrap();
    fs::write(previous.join("tool/Cargo.lock"), "version = 4\n# tool\n").unwrap();
    carry_generated_lockfile(&scratch.0, &previous, &staged).unwrap();
    assert_eq!(
        fs::read_to_string(staged.join("tool/Cargo.lock")).unwrap(),
        "version = 4\n# tool\n"
    );
    assert_eq!(fs::read(staged.join("game/Cargo.lock")).unwrap(), b"");
    assert!(!staged.join("game/core/Cargo.lock").exists());
    assert!(!staged.join("Cargo.lock").exists());
    assert_eq!(
        writable_snapshot_files(&staged),
        vec![
            staged.join("game/Cargo.lock"),
            staged.join("game/tools/Cargo.lock"),
            staged.join("tool/Cargo.lock")
        ]
    );
    // A nested root's build-side copy has a path of its own.
    assert_eq!(
        lockfile_backing(&scratch.0, Path::new("tool")),
        scratch.0.join("build/lockfiles/tool/Cargo.lock")
    );
    assert_eq!(
        lockfile_backing(&scratch.0, Path::new("")),
        scratch.0.join("build/Cargo.lock")
    );
}

/// Must run outside a parent sandbox which forbids installing OS sandboxes.
#[tokio::test]
#[ignore = "requires Rust toolchain and functional OS sandbox; run explicitly"]
async fn confinement_builds_a_crate_in_a_subdirectory_without_a_lockfile() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    fs::create_dir_all(fixture.0.join("nested")).unwrap();
    fs::write(fixture.0.join("nested/Cargo.toml"), "[package]\nname = \"nested-check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
    fs::write(
        fixture.0.join("nested/lib.rs"),
        "#[test]\nfn arithmetic() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    let result = command(&fixture.0, &scratch.0, "cd nested && cargo test --offline")
        .await
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("1 passed"), "{}", result.output);
    assert!(
        !fixture.0.join("nested/Cargo.lock").exists(),
        "live tree untouched"
    );
    // The generated lockfile is carried, so the next command can be locked.
    let locked = command(
        &fixture.0,
        &scratch.0,
        "cd nested && cargo test --offline --locked",
    )
    .await
    .unwrap();
    assert!(locked.success, "{}", locked.output);
}

/// A write grant may name a file that does not exist yet in a directory that
/// does; a read grant, or a write into a missing directory, still must exist.
#[test]
fn permission_requests_accept_a_new_file_to_write_but_not_to_read() {
    let project = Fixture::new();
    let scratch = Fixture::new();
    let outside = Fixture::new();
    let new_file = outside.0.join("report.txt").display().to_string();
    let permissions = CommandPermissions::requested(
        &project.0,
        &scratch.0,
        &[],
        std::slice::from_ref(&new_file),
        false,
    )
    .unwrap();
    assert_eq!(
        permissions.write_paths,
        vec![outside.0.canonicalize().unwrap().join("report.txt")]
    );
    assert!(!permissions.write_paths[0].exists());
    assert!(
        CommandPermissions::requested(&project.0, &scratch.0, &[new_file], &[], false)
            .unwrap_err()
            .to_string()
            .contains("canonicalize external read path")
    );
    let missing_parent = outside.0.join("missing/report.txt").display().to_string();
    assert!(
        CommandPermissions::requested(&project.0, &scratch.0, &[], &[missing_parent], false)
            .unwrap_err()
            .to_string()
            .contains("canonicalize external write path")
    );
}

/// Must run outside a parent sandbox which forbids installing OS sandboxes.
#[tokio::test]
#[ignore = "requires Rust toolchain and functional OS sandbox; run explicitly"]
async fn confinement_lets_cargo_generate_a_lockfile_the_project_lacks() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    fs::write(fixture.0.join("Cargo.toml"), "[package]\nname = \"greenfield\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
    fs::write(fixture.0.join("lib.rs"), "pub fn answer() -> u8 { 42 }\n").unwrap();
    let result = command(&fixture.0, &scratch.0, "cargo check --offline")
        .await
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(
        !fixture.0.join("Cargo.lock").exists(),
        "the project is untouched"
    );
    let generated = fs::read_to_string(scratch.0.join("source/Cargo.lock")).unwrap();
    assert!(generated.contains("greenfield"), "{generated}");
    // The next command sees the same lockfile without regenerating it.
    let again = command(&fixture.0, &scratch.0, "cat Cargo.lock")
        .await
        .unwrap();
    assert!(again.success, "{}", again.output);
    assert!(again.output.contains("greenfield"), "{}", again.output);
    // Anything else in the snapshot stays read-only.
    let denied = command(&fixture.0, &scratch.0, "echo x > lib.rs")
        .await
        .unwrap();
    assert!(!denied.success, "{}", denied.output);
    cleanup_task(&scratch.0).unwrap();
}

/// The premise the escaping-symlink finding rests on: seatbelt and landlock
/// both match a granted subpath AFTER resolution, so a symlink leading out of
/// a granted directory reaches a path no rule allows and `(deny default)` ends
/// it. The permission set is built by hand because `requested` refuses this
/// shape outright; what is under test is the sandbox, not the validator.
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires functional OS sandbox; run explicitly outside nested sandbox"]
async fn a_symlink_out_of_a_granted_directory_reaches_nothing() {
    use std::os::unix::fs::symlink;

    let project = Fixture::new();
    let scratch = Fixture::new();
    let external = Fixture::new();
    let elsewhere = Fixture::new();
    fs::write(elsewhere.0.join("secret"), "SENTINEL").unwrap();
    fs::create_dir(external.0.join("scope")).unwrap();
    fs::write(external.0.join("scope/plain"), "granted-content").unwrap();
    symlink(elsewhere.0.join("secret"), external.0.join("scope/escape")).unwrap();
    let permissions = CommandPermissions {
        read_paths: vec![external.0.join("scope").canonicalize().unwrap()],
        write_paths: vec![],
        network: false,
    };
    let scope = external.0.join("scope").display().to_string();
    let command_text =
        format!("cat '{scope}/plain'; ! cat '{scope}/escape'; printf resolved-and-denied");
    let result =
        command_with_permissions(&project.0, &scratch.0, &command_text, &permissions, None)
            .await
            .unwrap();
    assert!(result.success, "{}", result.output);
    assert!(
        result.output.contains("granted-content"),
        "{}",
        result.output
    );
    assert!(
        result.output.contains("resolved-and-denied"),
        "{}",
        result.output
    );
    assert!(
        !result.output.contains("SENTINEL"),
        "the symlink escaped its granted subpath: {}",
        result.output
    );
}

#[test]
fn a_pipeline_fails_when_any_stage_fails() {
    // The shell line itself, outside any sandbox: a filter in the last stage
    // must not turn a failing command into a success.
    let run = |command: &str| {
        let [shell, flag, line] = shell_argv(command);
        std::process::Command::new(shell)
            .args([flag, line])
            .status()
            .unwrap()
            .success()
    };
    assert!(!run("false | true"));
    assert!(!run("echo failed >&2; exit 3 | cat"));
    assert!(run("true | true"));
    assert!(run("printf 'a\\nb\\n' | tail -1"));
}

#[tokio::test]
#[ignore = "requires OS sandbox execution outside a parent sandbox"]
async fn confined_pipeline_reports_its_failing_stage() {
    let fixture = Fixture::new();
    let scratch = Fixture::new();
    let result = command(&fixture.0, &scratch.0, "false 2>&1 | tail -1")
        .await
        .unwrap();
    assert!(!result.success, "{}", result.output);
}
