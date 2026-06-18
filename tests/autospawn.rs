//! Real-binary coverage for `--connect` auto-spawning a detached `--serve`
//! backend. These tests specifically guard stdout cleanliness, detachment, and
//! the opt-out path.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use moosedev::runtime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_moosedev")
}

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("moosedev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn read_pidfile(pidfile: &Path) -> u32 {
    for _ in 0..200 {
        if let Ok(contents) = std::fs::read_to_string(pidfile) {
            if let Ok(pid) = contents.trim().parse() {
                return pid;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("pidfile {} was not written", pidfile.display());
}

fn kill_signal(pid: u32, signal: &str) {
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .unwrap_or_else(|e| panic!("run kill -{signal} {pid}: {e}"));
    assert!(status.success(), "kill -{signal} {pid} failed: {status}");
}

fn kill_process_group(pgid: u32, signal: &str) {
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(format!("-{pgid}"))
        .status()
        .unwrap_or_else(|e| panic!("run kill -{signal} -{pgid}: {e}"));
    assert!(
        status.success(),
        "kill -{signal} process group {pgid} failed: {status}"
    );
}

async fn wait_for_socket_removed(socket: &Path) {
    for _ in 0..100 {
        if !socket.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("socket {} was not removed", socket.display());
}

#[tokio::test]
async fn connect_auto_spawns_backend() {
    let data_dir = fresh_data_dir("autospawn");
    let socket = runtime::socket_path_for(&data_dir);
    let pidfile = runtime::pidfile_path_for(&data_dir);

    let mut child = tokio::process::Command::new(binary())
        .arg("--connect")
        .env("MOOSEDEV_DATA_DIR", &data_dir)
        .env("MOOSEDEV_ONTOLOGY_DIR", ontology_dir())
        .env_remove("MOOSEDEV_SOCKET")
        .env_remove("MOOSEDEV_NO_AUTOSPAWN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn moosedev --connect");
    let proxy_pid = child.id().expect("proxy pid");

    let mut stdin = child.stdin.take().expect("proxy stdin");
    let stdout = child.stdout.take().expect("proxy stdout");
    let mut stdout = BufReader::new(stdout).lines();

    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"autospawn-test","version":"0.1.0"}}}"#;
    stdin
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .expect("write initialize");
    stdin.flush().await.expect("flush initialize");

    let line = tokio::time::timeout(Duration::from_secs(40), stdout.next_line())
        .await
        .expect("initialize response timed out")
        .expect("read initialize response")
        .expect("initialize response line");

    assert!(line.contains("\"result\""), "initialize response: {line}");
    assert!(
        line.contains("\"serverInfo\""),
        "initialize response: {line}"
    );

    kill_process_group(proxy_pid, "TERM");
    let _ = child.wait().await;

    let pid = read_pidfile(&pidfile).await;
    kill_signal(pid, "0");
    UnixStream::connect(&socket)
        .await
        .expect("detached backend still accepts after proxy exit");

    kill_signal(pid, "TERM");
    wait_for_socket_removed(&socket).await;
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn connect_respects_no_autospawn() {
    let data_dir = fresh_data_dir("no-autospawn");
    let socket = runtime::socket_path_for(&data_dir);
    let pidfile = runtime::pidfile_path_for(&data_dir);

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(binary())
            .arg("--connect")
            .env("MOOSEDEV_DATA_DIR", &data_dir)
            .env("MOOSEDEV_ONTOLOGY_DIR", ontology_dir())
            .env("MOOSEDEV_NO_AUTOSPAWN", "1")
            .env_remove("MOOSEDEV_SOCKET")
            .output(),
    )
    .await
    .expect("--connect with autospawn disabled timed out")
    .expect("run moosedev --connect");

    assert!(
        !output.status.success(),
        "--connect should fail without a backend when auto-spawn is disabled"
    );
    assert!(
        !pidfile.exists(),
        "disabled auto-spawn should not create {}",
        pidfile.display()
    );
    assert!(
        !socket.exists(),
        "disabled auto-spawn should not create {}",
        socket.display()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MOOSEDEV_NO_AUTOSPAWN"),
        "stderr should explain opt-out: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}
