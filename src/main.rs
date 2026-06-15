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
    let architecture_ttl =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(moosedev::ontology::DEFAULT_ARCHITECTURE_TTL);

    tracing::info!("MOOSEDev: bootstrapping state (data dir: {})…", data_dir.display());
    let state = AppState::bootstrap(&data_dir, &architecture_ttl)?;

    tracing::info!("MOOSEDev MCP server starting (stdio transport)…");
    let service = MooseDevServer::new(Arc::new(state)).serve(stdio()).await?;
    service.waiting().await?;

    tracing::info!("MOOSEDev MCP server shut down cleanly.");
    Ok(())
}
