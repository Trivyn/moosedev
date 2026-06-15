//! MCP server surface for MOOSEDev (stdio transport via `rmcp`).
//!
//! Exposes the durable knowledge-graph tools over MCP. Tool handlers stay thin:
//! validate input, map it to the domain layer, format the result. The typed
//! write goes through MOOSE's cache-coherent `kg::assert_instance` (via `graph`).

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};

use crate::graph::{self, AppState, RecordInput};

/// Tool result helpers — the `vec![Content::text(..)]` wrapping repeats across
/// every handler, so name it once.
fn tool_ok(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(message.into())])
}
fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message.into())])
}

/// Render structured context items into a readable block for the agent.
fn format_context(items: &[graph::ContextItem]) -> String {
    let plural = if items.len() == 1 { "" } else { "s" };
    let mut out = format!("Relevant recorded knowledge ({} item{plural}):\n", items.len());
    for item in items {
        out.push_str(&format!("\n• {} — \"{}\"\n", item.kind, item.label));
        for (key, value) in &item.properties {
            out.push_str(&format!("  {key}: {value}\n"));
        }
        out.push_str(&format!("  {}\n", item.iri));
    }
    out
}

/// Arguments for the `record_important_decision` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecordDecisionArgs {
    /// Knowledge class to record — a class in the architecture ontology.
    /// Defaults to "ArchitecturalDecision".
    pub kind: Option<String>,
    /// Short human-readable title for the item.
    pub title: String,
    /// Optional longer description / body.
    pub description: Option<String>,
    /// Optional lifecycle status (e.g. "proposed", "accepted", "superseded").
    pub status: Option<String>,
}

/// Arguments for the `query` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryArgs {
    /// Natural-language question to answer over the project knowledge graph.
    pub question: String,
}

/// Arguments for the `get_relevant_context` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRelevantContextArgs {
    /// Topic/focus to retrieve context for. Omit to list all recorded knowledge.
    pub topic: Option<String>,
    /// Maximum number of items to return (default 10).
    pub limit: Option<usize>,
}

/// The MOOSEDev MCP server: the generated tool router plus shared engine state.
#[derive(Clone)]
pub struct MooseDevServer {
    // Read by the `#[tool_handler]`-generated `ServerHandler` impl; the dead-code
    // pass doesn't attribute that macro-generated use, so silence the lint.
    #[allow(dead_code)]
    tool_router: ToolRouter<MooseDevServer>,
    state: Arc<AppState>,
}

#[tool_router]
impl MooseDevServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
        }
    }

    /// Health check — confirms the MCP transport is live. Returns "pong".
    #[tool(description = "Health check; returns 'pong'.")]
    async fn ping(&self) -> Result<CallToolResult, McpError> {
        Ok(tool_ok("pong"))
    }

    /// Record a typed knowledge item into the durable project knowledge graph.
    #[tool(
        description = "Record a typed architectural decision (or other knowledge class) into the durable project knowledge graph."
    )]
    async fn record_important_decision(
        &self,
        Parameters(args): Parameters<RecordDecisionArgs>,
    ) -> Result<CallToolResult, McpError> {
        let kind = args
            .kind
            .unwrap_or_else(|| "ArchitecturalDecision".to_string());
        let title = args.title.trim().to_string();
        if title.is_empty() {
            return Ok(tool_error("`title` must not be empty"));
        }
        let class_iri = match self.state.resolve_class(&kind) {
            Ok(iri) => iri,
            Err(e) => return Ok(tool_error(e.to_string())),
        };

        // Map the decision's fields to (predicate, value) assertions. Adding a
        // new knowledge class means a new mapping here — not a change to the
        // generic `graph::record_instance` writer.
        let mut properties = vec![(moose::RDFS_LABEL.to_string(), title)];
        if let Some(desc) = args.description.filter(|s| !s.trim().is_empty()) {
            properties.push((graph::ARCH_DESCRIPTION.to_string(), desc));
        }
        if let Some(status) = args.status.filter(|s| !s.trim().is_empty()) {
            properties.push((graph::ARCH_STATUS.to_string(), status));
        }

        let input = RecordInput {
            class_iri,
            class_local: kind.clone(),
            properties,
        };
        match graph::record_instance(&self.state, &input) {
            Ok(iri) => Ok(tool_ok(format!("Recorded {kind} → {iri}"))),
            Err(e) => Ok(tool_error(format!("failed to record: {e}"))),
        }
    }

    /// Ask a natural-language question over the project knowledge graph.
    #[tool(
        description = "Ask a natural-language question over the project knowledge graph. Returns an answer plus a symbolic reasoning trace."
    )]
    async fn query(
        &self,
        Parameters(args): Parameters<QueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let question = args.question.trim();
        if question.is_empty() {
            return Ok(tool_error("`question` must not be empty"));
        }
        match graph::query(&self.state, question).await {
            Ok(r) => Ok(tool_ok(format!(
                "{}\n\n(confidence: {})\n\n--- reasoning trace ---\n{}",
                r.answer, r.confidence, r.trace
            ))),
            Err(e) => Ok(tool_error(format!("query failed: {e}"))),
        }
    }

    /// Retrieve recorded project knowledge relevant to a topic (or all of it).
    #[tool(
        description = "Retrieve recorded project knowledge relevant to a topic (or all of it) as structured context. Symbolic — no LLM."
    )]
    async fn get_relevant_context(
        &self,
        Parameters(args): Parameters<GetRelevantContextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(10).clamp(1, 100);
        match graph::relevant_context(&self.state, args.topic.as_deref(), limit) {
            Ok(items) if items.is_empty() => Ok(tool_ok("No recorded knowledge found.")),
            Ok(items) => Ok(tool_ok(format_context(&items))),
            Err(e) => Ok(tool_error(format!("failed to retrieve context: {e}"))),
        }
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
            "MOOSEDev: structured, long-term project memory built on the MOOSE engine.".to_string(),
        );
        info
    }
}
