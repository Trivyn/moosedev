//! MCP server surface for MOOSEDev (stdio transport via `rmcp`).
//!
//! M0b: a minimal server exposing a single `ping` health-check tool, to prove
//! the MCP transport end-to-end. The durable-KG state (oxigraph `Store` + MOOSE
//! caches) and the real v1 tool set (`record_important_decision`, `query`,
//! `align_concepts`, …) are wired in M1+ — see `spec/MOOSEDev_design.md` §7.

use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};

/// The MOOSEDev MCP server. Holds the generated tool router; future fields will
/// carry the durable knowledge-graph state (Store, MooseOntologyCache, configs).
#[derive(Clone)]
pub struct MooseDevServer {
    // Read by the `#[tool_handler]`-generated `ServerHandler` impl; the dead-code
    // pass doesn't attribute that macro-generated use, so silence the lint.
    #[allow(dead_code)]
    tool_router: ToolRouter<MooseDevServer>,
}

impl Default for MooseDevServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl MooseDevServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Health check — confirms the MCP transport is live. Returns "pong".
    #[tool(description = "Health check; returns 'pong'.")]
    async fn ping(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text("pong")]))
    }
}

#[tool_handler]
impl ServerHandler for MooseDevServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` (alias for `InitializeResult`) is #[non_exhaustive], so it
        // can't be built with a struct literal outside rmcp — start from Default
        // and set the fields we care about.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        // `from_build_env()` reports rmcp's own package identity; override with
        // moosedev's so MCP clients display the right server name/version.
        let mut server_impl = Implementation::from_build_env();
        server_impl.name = env!("CARGO_PKG_NAME").to_string();
        server_impl.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_impl;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "MOOSEDev: structured, long-term project memory built on the MOOSE engine."
                .to_string(),
        );
        info
    }
}
