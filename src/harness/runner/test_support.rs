//! Scaffolding shared by the runner's in-source tests: a throwaway project
//! root and a scripted daemon that advertises the current contracts.
use crate::harness::protocol::ContextResponse;
use axum::{extract::State, routing::post, Json, Router};
use std::path::PathBuf;
use std::sync::Arc;

/// A temporary project root removed on drop.
pub(crate) struct Project(pub(crate) PathBuf);

impl Project {
    pub(crate) fn new(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!("moosedev-{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn context(State(root): State<Arc<PathBuf>>) -> Json<ContextResponse> {
    Json(ContextResponse {
        capture_contracts: vec![2, 3],
        intent_contracts: vec![2],
        project_root: root.to_string_lossy().into_owned(),
        revision: "fixture".into(),
        context: String::new(),
        files: vec![],
        records: vec![],
        evidence_iris: vec![],
        governing_constraints: vec![],
    })
}

/// The daemon's context route for `root`; tests add the routes they script.
pub(crate) fn context_router() -> Router<Arc<PathBuf>> {
    Router::new().route("/api/v1/harness/context", post(context))
}

/// Serve `router` on a free local port; returns the daemon URL and the server task.
pub(crate) async fn serve(
    router: Router<Arc<PathBuf>>,
    root: &Project,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = router.with_state(Arc::new(root.0.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let daemon = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (daemon, server)
}
