//! MOOSEDev — neurosymbolic MCP sidecar built on the MOOSE engine.
//!
//! Thin entry point: bootstrap the durable knowledge-graph state, then serve the
//! MCP protocol over stdio (`rmcp`). **stdout is the JSON-RPC channel**, so all
//! diagnostics are routed to stderr. The server surface and tools live in the
//! `moosedev` library crate (see `src/lib.rs` and `spec/MOOSEDev_design.md`).

use std::path::Path;
use std::sync::Arc;

use moosedev::graph::AppState;
use moosedev::mcp::MooseDevServer;
use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs MUST go to stderr — stdout carries the MCP JSON-RPC framing.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moosedev=info,rmcp=warn")),
        )
        .init();

    let data_dir = std::path::PathBuf::from(
        std::env::var("MOOSEDEV_DATA_DIR").unwrap_or_else(|_| "data".to_string()),
    );
    // Where the shipped ontologies live. Defaults to the crate's `ontologies/`
    // dir (dev/`cargo run`); override with `MOOSEDEV_ONTOLOGY_DIR` for a deployed
    // binary that ships them elsewhere.
    let ontology_dir = match std::env::var("MOOSEDEV_ONTOLOGY_DIR") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies"),
    };

    tracing::info!(
        "MOOSEDev: bootstrapping state (data dir: {})…",
        data_dir.display()
    );
    let mut state = AppState::bootstrap(&data_dir, &ontology_dir)?;
    // Build the alignment index (loads the embedding model). Non-fatal by design:
    // if the model can't load (e.g. offline with no bundled weights), the
    // alignment tools report it per call, but the rest of the server (capture,
    // query, context, provenance) must still start.
    tracing::info!("MOOSEDev: building ontology alignment index (embedding vectors)…");
    if let Err(e) = state.build_alignment_index().await {
        tracing::warn!(
            "alignment index unavailable — align_concepts/suggest_mappings disabled: {e}"
        );
    }

    tracing::info!("MOOSEDev MCP server starting (stdio transport)…");
    let service = MooseDevServer::new(Arc::new(state)).serve(stdio()).await?;
    service.waiting().await?;

    tracing::info!("MOOSEDev MCP server shut down cleanly.");
    Ok(())
}
