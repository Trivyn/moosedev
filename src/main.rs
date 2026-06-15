//! MOOSEDev — neurosymbolic MCP sidecar built on the MOOSE engine.
//!
//! M0b entry point: serve the MCP protocol over stdio (`rmcp`). **stdout is the
//! JSON-RPC channel**, so all diagnostics are routed to stderr. The durable
//! knowledge graph and the real v1 tool surface are wired in M1+
//! (see `spec/MOOSEDev_design.md`).

mod mcp;

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

    tracing::info!("MOOSEDev MCP server starting (stdio transport)…");

    let service = mcp::MooseDevServer::new().serve(stdio()).await?;
    service.waiting().await?;

    tracing::info!("MOOSEDev MCP server shut down cleanly.");
    Ok(())
}
