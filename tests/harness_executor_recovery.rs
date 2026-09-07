#![cfg(all(feature = "harness", target_os = "macos"))]
//! Real macOS lifecycle probes. These spawn bounded, deliberately detached
//! children and require an OS sandbox outside the coding agent's nested sandbox.
use moosedev::harness::{executor, progress::Progress};
use std::{fs, path::PathBuf, time::Duration};

const HELPER: &str = r#"
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/attr.h>
#include <stdint.h>

static void overwrite(const char *path) {
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    if (fd >= 0) { write(fd, "old", 3); close(fd); }
}
int main(int argc, char **argv) {
    if (argc != 2) return 20;
    const char *target = getenv("CARGO_TARGET_DIR");
    const char *home = getenv("CARGO_HOME");
    char held_path[PATH_MAX], intruder[PATH_MAX], home_intruder[PATH_MAX];
    char backing[PATH_MAX], cross_link[PATH_MAX];
    snprintf(held_path, sizeof held_path, "%s/held", target);
    snprintf(intruder, sizeof intruder, "%s/intruder", target);
    snprintf(home_intruder, sizeof home_intruder, "%s/intruder", home);
    if (!strcmp(argv[1], "flags")) {
        int fd = open(held_path, O_CREAT | O_WRONLY, 0600);
        if (fd < 0) return 27;
        if (!fchflags(fd, UF_IMMUTABLE)) { fchflags(fd, 0); close(fd); return 28; }
        close(fd);
        struct attrlist attrs = {0};
        attrs.bitmapcount = ATTR_BIT_MAP_COUNT;
        attrs.commonattr = ATTR_CMN_FLAGS;
        uint32_t flags = UF_IMMUTABLE;
        if (!setattrlist(held_path, &attrs, &flags, sizeof flags, FSOPT_NOFOLLOW)) {
            chflags(held_path, 0); return 29;
        }
        return 0;
    }
    if (!realpath(target, backing)) return 21;
    snprintf(cross_link, sizeof cross_link, "%s/cross-link", backing);
    int ready[2];
    if (pipe(ready)) return 22;
    pid_t child = fork();
    if (child < 0) return 23;
    if (child == 0) {
        close(ready[0]);
        if (setsid() < 0) _exit(24);
        int held = open(held_path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
        if (held < 0) _exit(25);
        overwrite(intruder);
        overwrite(home_intruder);
        if (strcmp(argv[1], "hold")) { close(1); close(2); }
        write(ready[1], "R", 1); close(ready[1]);
        // Bound even a failed test independently of its Rust cleanup guard.
        for (int i = 0; i < 3000; ++i) {
            pwrite(held, "old", 3, 0);
            overwrite(held_path);
            overwrite(intruder);
            overwrite(home_intruder);
            // A readable future generation must not be hardlinked into the
            // old sandbox's writable backing and modified through that alias.
            unlink(cross_link);
            if (!link(held_path, cross_link)) overwrite(cross_link);
            usleep(10000);
        }
        close(held); _exit(0);
    }
    close(ready[1]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) return 26;
    close(ready[0]);
    printf("CHILD=%d\n", child); fflush(stdout);
    if (!strcmp(argv[1], "wait")) sleep(60);
    return 0;
}
"#;

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    scratch: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "moosedev-executor-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(directory.join("project")).unwrap();
        let directory = directory.canonicalize().unwrap();
        let root = directory.join("project");
        let source = root.join("escape.c");
        fs::write(&source, HELPER).unwrap();
        let output = std::process::Command::new("/usr/bin/cc")
            .arg(&source)
            .arg("-o")
            .arg(root.join("escape"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let scratch = directory.join("scratch/task");
        Self {
            directory,
            root,
            scratch,
        }
    }
    async fn run(&self, command: &str) -> executor::CommandResult {
        tokio::time::timeout(
            Duration::from_secs(15),
            executor::command(&self.root, &self.scratch, command),
        )
        .await
        .expect("command lifecycle exceeded the test deadline")
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct Detached(libc::pid_t);
impl Detached {
    fn from_output(output: &str) -> Self {
        let pid = output
            .lines()
            .find_map(|line| line.strip_prefix("CHILD="))
            .unwrap_or_else(|| panic!("missing child PID: {output}"));
        Self(pid.parse().unwrap())
    }
    fn assert_alive(&self) {
        assert_eq!(
            unsafe { libc::kill(self.0, 0) },
            0,
            "probe child exited before isolation was tested"
        );
    }
}
impl Drop for Detached {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
    }
}

const VERIFY_NEW_BACKING: &str = r#"
printf fresh > "$CARGO_TARGET_DIR/held"
rm -f "$CARGO_TARGET_DIR/intruder" "$CARGO_HOME/intruder"
sleep 0.4
test "$(cat "$CARGO_TARGET_DIR/held")" = fresh &&
test ! -e "$CARGO_TARGET_DIR/intruder" &&
test ! -e "$CARGO_HOME/intruder"
"#;

#[tokio::test]
#[ignore = "requires real macOS sandbox and C compiler; run outside nested sandbox"]
async fn detached_silent_writer_cannot_modify_the_next_command_cache() {
    let fixture = Fixture::new();
    let result = fixture.run("./escape silent").await;
    let detached = Detached::from_output(&result.output);
    assert!(result.success, "{}", result.output);
    detached.assert_alive();
    let result = fixture.run(VERIFY_NEW_BACKING).await;
    assert!(
        result.success,
        "old writer contaminated the new build or Cargo home: {}",
        result.output
    );
    detached.assert_alive();
    drop(detached);
    executor::cleanup_task(&fixture.scratch).unwrap();
    assert!(!fixture.scratch.exists());
}

#[tokio::test]
#[ignore = "requires real macOS sandbox and C compiler; run outside nested sandbox"]
async fn detached_output_has_a_drain_deadline_and_retains_evidence() {
    let fixture = Fixture::new();
    let start = std::time::Instant::now();
    let result = fixture.run("./escape hold").await;
    let detached = Detached::from_output(&result.output);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(
        !result.success,
        "incomplete output must not pass verification"
    );
    assert!(
        result.output.contains("output remained open"),
        "{}",
        result.output
    );
    assert!(!result.output.contains("900 second timeout"));
    detached.assert_alive();
    let result = fixture.run(VERIFY_NEW_BACKING).await;
    assert!(result.success, "{}", result.output);
}

#[tokio::test]
#[ignore = "requires real macOS sandbox and C compiler; run outside nested sandbox"]
async fn cancellation_with_a_detached_writer_allows_safe_task_reuse() {
    let fixture = Fixture::new();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let command = executor::command_with_progress(
        &fixture.root,
        &fixture.scratch,
        "./escape wait",
        Some(send),
    );
    let detached = {
        tokio::pin!(command);
        let mut output = String::new();
        loop {
            tokio::select! {
                result = &mut command => panic!("command completed before interruption: {result:?}"),
                event = tokio::time::timeout(Duration::from_secs(10), receive.recv()) => {
                    if let Some(Progress::CommandOutput(text)) = event.unwrap() { output.push_str(&text); }
                    if output.contains("CHILD=") && output.ends_with('\n') { break Detached::from_output(&output); }
                }
            }
        }
    }; // Drop the command future just as the controller does on Esc.
    detached.assert_alive();
    executor::cleanup_task(&fixture.scratch).unwrap();
    assert!(!fixture.scratch.exists());
    let result = fixture.run(VERIFY_NEW_BACKING).await;
    assert!(
        result.success,
        "cancelled command retained access to resumed task: {}",
        result.output
    );
}

#[tokio::test]
#[ignore = "requires real macOS sandbox; run outside nested sandbox"]
async fn command_cannot_set_immutable_build_flags() {
    let fixture = Fixture::new();
    let result = fixture.run("./escape flags").await;
    assert!(
        result.success,
        "fchflags/setattrlist was permitted: {}",
        result.output
    );
    let result = fixture.run(r#"touch "$CARGO_TARGET_DIR/artifact"; if chflags uchg "$CARGO_TARGET_DIR/artifact"; then chflags nouchg "$CARGO_TARGET_DIR/artifact"; exit 1; fi"#).await;
    assert!(
        result.success,
        "immutable flag was permitted: {}",
        result.output
    );
    executor::cleanup_task(&fixture.scratch).unwrap();
    assert!(!fixture.scratch.exists());
}
