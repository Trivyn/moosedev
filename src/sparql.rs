//! Read-only SPARQL over the local MOOSEDev store, run by MOOSE's bounded
//! engine (`moose-mcp`'s `sparql` tool).
//!
//! moose-mcp owns the hard parts:
//! - **Bounds:** a timeout, a row cap, a concurrency limit, and size and nesting
//!   checks that run before parsing.
//! - **Rejections:** `FROM` / `FROM NAMED`, `SERVICE` and updates.
//! - **Diagnostics:** an empty result explains query terms that no queryable
//!   graph uses, and a parse error names undeclared prefixes.
//!
//! moosedev keeps two things:
//! - **Scope:** which graphs are queryable ([`QUERYABLE_GRAPHS`]).
//! - **Reply:** the full result goes into the reply text. moose-mcp's own text
//!   is a 20-row preview, with the rest only in `structuredContent`.

use std::sync::Arc;

use moose::entity_index::EntityIndexCache;
use moose::traits::EngineConfig;
use moose::types::LlmAssistLevel;
use moose_mcp::{EngineHandles, McpHostConfig, MooseTools, ToolKind};
use oxigraph::store::Store;
use rmcp::model::{CallToolResult, RawContent};
use serde_json::{json, Value};

use crate::graph::PROJECT_KG_GRAPH_IRI;
use crate::llm::OpenAiCompatClient;
use crate::ontology::{
    MooseDevOntologyResolver, ARCH_DOMAIN_GRAPH_IRI, ARCH_SHAPES_GRAPH_IRI, CODE_DOMAIN_GRAPH_IRI,
    CODE_SHAPES_GRAPH_IRI, SE_DOMAIN_GRAPH_IRI, SE_SHAPES_GRAPH_IRI,
};
use crate::provenance::PROVENANCE_GRAPH_IRI;

/// The graphs a query sees. The default graph is their union, and `GRAPH ?g`
/// ranges over them.
///
/// Hidden: MOOSE's own engine ontology, chat-session graphs and the user
/// overlay. These are engine state, not project knowledge.
pub const QUERYABLE_GRAPHS: [&str; 8] = [
    PROJECT_KG_GRAPH_IRI,
    PROVENANCE_GRAPH_IRI,
    SE_DOMAIN_GRAPH_IRI,
    ARCH_DOMAIN_GRAPH_IRI,
    CODE_DOMAIN_GRAPH_IRI,
    SE_SHAPES_GRAPH_IRI,
    ARCH_SHAPES_GRAPH_IRI,
    CODE_SHAPES_GRAPH_IRI,
];

/// A successful query: the full result as text, plus moose-mcp's advisory
/// note when an empty result can be explained.
#[derive(Debug, Clone)]
pub struct SparqlReply {
    /// SPARQL JSON for SELECT and ASK, N-Triples for CONSTRUCT and DESCRIBE.
    pub text: String,
    pub note: Option<String>,
}

/// Build the engine handle the `sparql` tool runs on, once per store.
///
/// Only `sparql` is called, and it uses no LLM, so the engine is capped at
/// `PureSymbolic`. moose-mcp's other always-on tools exist in the handle but
/// are never routed.
pub(crate) fn build_tools(
    store: &Store,
    llm: &OpenAiCompatClient,
    engine_config: &EngineConfig,
    entity_index: &Arc<EntityIndexCache>,
    model: &str,
) -> anyhow::Result<Arc<MooseTools>> {
    let mut engine_config = engine_config.clone();
    engine_config.llm_assist_level = LlmAssistLevel::PureSymbolic;
    let handles = EngineHandles {
        store: Arc::new(store.clone()),
        llm: Arc::new(llm.clone()),
        resolver: Arc::new(MooseDevOntologyResolver::new()),
        engine_config,
        entity_index: Arc::clone(entity_index),
        data_graphs: QUERYABLE_GRAPHS.iter().map(|g| g.to_string()).collect(),
        model: model.to_string(),
        session_store: None,
        vector_source: None,
        alignment_config: None,
        alignment_category_mappings: None,
    };
    let host = McpHostConfig::new("MOOSEDev").map_err(|e| anyhow::anyhow!("{e}"))?;
    let tools =
        MooseTools::new(host, handles).map_err(|e| anyhow::anyhow!("sparql engine: {e}"))?;
    Ok(Arc::new(tools))
}

/// Run a read-only query. `Err` is the message for the caller: a rejection,
/// a parse error (naming undeclared prefixes), a timeout or a failure.
pub async fn run_query(state: &crate::graph::AppState, query: &str) -> Result<SparqlReply, String> {
    // Inferred edges are queryable too, so refresh them if a write made them stale.
    state.ensure_enriched();
    let arguments = json!({ "query": query }).as_object().cloned();
    let result = state
        .sparql_tools
        .call(ToolKind::Sparql, arguments)
        .await
        .map_err(|e| e.message.to_string())?;
    reply_from(result)
}

fn reply_from(result: CallToolResult) -> Result<SparqlReply, String> {
    let texts: Vec<String> = result
        .content
        .iter()
        .filter_map(|content| match &content.raw {
            RawContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    if result.is_error == Some(true) {
        return Err(texts.join("\n"));
    }
    let mut value = result
        .structured_content
        .ok_or("the SPARQL engine returned no result")?;
    let note = value
        .as_object_mut()
        .and_then(|object| object.remove("note"))
        .and_then(|note| note.as_str().map(str::to_owned));
    Ok(SparqlReply {
        text: full_text(value),
        note,
    })
}

/// The full result as text. SELECT and ASK keep the SPARQL JSON shape
/// (`head` / `results.bindings`, or `boolean`), with a `truncated` flag at the
/// row cap. CONSTRUCT and DESCRIBE are N-Triples.
fn full_text(mut value: Value) -> String {
    let Some(object) = value.as_object_mut() else {
        return value.to_string();
    };
    let kind = object.remove("kind");
    match kind.as_ref().and_then(Value::as_str) {
        Some("graph") => {
            let ntriples = object
                .get("ntriples")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if object.get("truncated") == Some(&Value::Bool(true)) {
                format!("{ntriples}# truncated at the row cap; narrow the query\n")
            } else {
                ntriples
            }
        }
        Some("ask") => json!({ "boolean": object.get("boolean") }).to_string(),
        _ => {
            if object.get("truncated") != Some(&Value::Bool(true)) {
                object.remove("truncated");
            }
            value.to_string()
        }
    }
}
