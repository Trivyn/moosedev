//! Proves the shared-backend core (M5): one in-process `--serve` backend on a
//! temp Unix socket serves **two concurrent MCP clients** that both complete the
//! handshake, write concurrently, and each reads back the other's write over the
//! single shared graph — the scenario that fails today (the second per-client
//! stdio server can't open the RocksDB-locked store). Plus multi-project
//! isolation: two data dirs derive two distinct sockets with no cross-talk.
//!
//! These tests exercise the transport/concurrency path, so they bootstrap
//! `AppState` directly and skip `build_alignment_index` (the embedding-model load
//! is irrelevant here and slow).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moosedev::graph::AppState;
use moosedev::mcp::MooseDevServer;
use moosedev::runtime;
use rmcp::model::CallToolRequestParams;
use rmcp::{Peer, RoleClient, ServiceExt};
use serde_json::{json, Value};
use tokio::net::UnixStream;

fn ontology_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies")
}

fn fresh_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("moosedev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn build_server(data_dir: &Path) -> MooseDevServer {
    let state = AppState::bootstrap(data_dir, &ontology_dir()).expect("bootstrap app state");
    MooseDevServer::new(Arc::new(state))
}

/// Spawn the backend accept loop and return once it is accepting connections.
async fn spawn_backend(server: MooseDevServer, socket: PathBuf) -> tokio::task::JoinHandle<()> {
    let handle = tokio::spawn(async move {
        if let Err(e) = runtime::serve_unix(server, &socket).await {
            eprintln!("backend exited: {e}");
        }
    });
    handle
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("backend socket {} never became ready", socket.display());
}

/// Connect a fresh MCP client (full handshake) over the backend socket.
async fn connect_client(socket: &Path) -> rmcp::service::RunningService<RoleClient, ()> {
    let stream = UnixStream::connect(socket)
        .await
        .expect("connect to backend socket");
    ().serve(stream).await.expect("client MCP handshake")
}

/// Call a tool and return its content serialized to JSON (asserting no error).
/// `&RunningService` coerces to `&Peer<RoleClient>` via `Deref`.
async fn call(peer: &Peer<RoleClient>, name: &str, args: Value) -> String {
    let arguments = args.as_object().cloned().unwrap_or_default();
    let result = peer
        .call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(arguments))
        .await
        .unwrap_or_else(|e| panic!("call_tool {name} failed: {e}"));
    assert_ne!(
        result.is_error,
        Some(true),
        "tool {name} returned an error result: {result:?}"
    );
    serde_json::to_string(&result.content).expect("serialize tool content")
}

#[tokio::test]
async fn two_clients_share_one_backend_concurrently() {
    let data_dir = fresh_data_dir("concurrent");
    let server = build_server(&data_dir);
    // Use the real per-data-dir derivation (handles the macOS socket-path limit).
    let socket = runtime::socket_path_for(&data_dir);

    let backend = spawn_backend(server, socket.clone()).await;
    wait_for_socket(&socket).await;

    // Two independent clients, each its own connection/session — the case that
    // currently fails with separate stdio servers.
    let c1 = connect_client(&socket).await;
    let c2 = connect_client(&socket).await;

    // Both handshake and respond to a concurrent health check.
    let (p1, p2) = tokio::join!(
        call(&c1, "ping", Value::Null),
        call(&c2, "ping", Value::Null)
    );
    assert!(p1.contains("pong"), "client1 ping: {p1}");
    assert!(p2.contains("pong"), "client2 ping: {p2}");

    // Concurrent writes from both clients into the single shared graph.
    let title1 = "Concurrent client one decision";
    let title2 = "Concurrent client two decision";
    let (r1, r2) = tokio::join!(
        call(&c1, "record_important_decision", json!({ "title": title1 })),
        call(&c2, "record_important_decision", json!({ "title": title2 })),
    );
    assert!(r1.contains("Recorded"), "client1 record: {r1}");
    assert!(r2.contains("Recorded"), "client2 record: {r2}");

    // Each client reads back the OTHER's write — proving one shared live graph.
    let seen_by_c2 = call(&c2, "get_relevant_context", json!({})).await;
    assert!(
        seen_by_c2.contains(title1),
        "client2 should see client1's write; got: {seen_by_c2}"
    );
    let seen_by_c1 = call(&c1, "get_relevant_context", json!({})).await;
    assert!(
        seen_by_c1.contains(title2),
        "client1 should see client2's write; got: {seen_by_c1}"
    );

    backend.abort();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn separate_projects_do_not_interfere() {
    let dir_a = fresh_data_dir("iso-a");
    let dir_b = fresh_data_dir("iso-b");
    let server_a = build_server(&dir_a);
    let server_b = build_server(&dir_b);

    let sock_a = runtime::socket_path_for(&dir_a);
    let sock_b = runtime::socket_path_for(&dir_b);
    assert_ne!(
        sock_a, sock_b,
        "distinct data dirs must derive distinct sockets (no cross-wiring)"
    );

    let backend_a = spawn_backend(server_a, sock_a.clone()).await;
    let backend_b = spawn_backend(server_b, sock_b.clone()).await;
    wait_for_socket(&sock_a).await;
    wait_for_socket(&sock_b).await;

    let ca = connect_client(&sock_a).await;
    let cb = connect_client(&sock_b).await;

    let title_a = "Project A only decision";
    call(
        &ca,
        "record_important_decision",
        json!({ "title": title_a }),
    )
    .await;

    // Project A sees its own write; project B sees nothing of A's.
    let ctx_a = call(&ca, "get_relevant_context", json!({})).await;
    assert!(
        ctx_a.contains(title_a),
        "project A should see its write: {ctx_a}"
    );
    let ctx_b = call(&cb, "get_relevant_context", json!({})).await;
    assert!(
        !ctx_b.contains(title_a),
        "project B must not see project A's write: {ctx_b}"
    );

    backend_a.abort();
    backend_b.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
