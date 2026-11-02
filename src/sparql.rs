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
use oxigraph::model::{GraphNameRef, NamedNodeRef, NamedOrBlankNodeRef, TermRef};
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

/// The OWL declarations that put a term in an ontology's vocabulary. These are
/// W3C IRIs, not ontology namespaces (Constraint 19bb4d8a).
const DECLARATIONS: [&str; 3] = [
    "http://www.w3.org/2002/07/owl#Class",
    "http://www.w3.org/2002/07/owl#ObjectProperty",
    "http://www.w3.org/2002/07/owl#DatatypeProperty",
];

/// A domain ontology's term namespace, as its loaded declarations use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermNamespace {
    /// The named graph the ontology is loaded into. Not a term namespace.
    pub graph: &'static str,
    /// The IRI prefix the ontology's classes and properties share.
    pub namespace: String,
    /// A few of its class local names, alphabetically.
    pub example_classes: Vec<String>,
}

/// The term namespace of each domain ontology, read from the store.
///
/// An agent writing SPARQL needs these and cannot otherwise learn them: tool
/// output prints predicates as bare local names, and the graph IRIs the tool
/// names look like namespaces but are not (Lessons ea5e5f24, 99ac2b84). They
/// are read, not written down, because the namespaces are regenerable
/// (Constraint 19bb4d8a). Per graph, the namespace most of its declared terms
/// share; an ontology with no declarations is left out.
pub fn term_namespaces(store: &Store) -> Vec<TermNamespace> {
    [
        ARCH_DOMAIN_GRAPH_IRI,
        SE_DOMAIN_GRAPH_IRI,
        CODE_DOMAIN_GRAPH_IRI,
    ]
    .into_iter()
    .filter_map(|graph| term_namespace(store, graph))
    .collect()
}

fn term_namespace(store: &Store, graph: &'static str) -> Option<TermNamespace> {
    let graph_name = GraphNameRef::NamedNode(NamedNodeRef::new(graph).ok()?);
    let rdf_type = NamedNodeRef::new_unchecked(moose::RDF_TYPE);
    let class_kind = DECLARATIONS[0];
    let mut declared: Vec<(String, bool)> = Vec::new();
    for kind in DECLARATIONS {
        let object = TermRef::NamedNode(NamedNodeRef::new_unchecked(kind));
        for quad in store
            .quads_for_pattern(None, Some(rdf_type), Some(object), Some(graph_name))
            .flatten()
        {
            if let NamedOrBlankNodeRef::NamedNode(term) = quad.subject.as_ref() {
                declared.push((term.as_str().to_string(), kind == class_kind));
            }
        }
    }
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (term, _) in &declared {
        *counts.entry(namespace_of(term).to_string()).or_default() += 1;
    }
    // Most-shared wins; a tie goes to the alphabetically first, so it is stable.
    let namespace = counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))?
        .0
        .clone();
    let mut example_classes: Vec<String> = declared
        .iter()
        .filter(|(term, is_class)| *is_class && namespace_of(term) == namespace)
        .map(|(term, _)| term[namespace.len()..].to_string())
        .filter(|local| !local.is_empty())
        .collect();
    example_classes.sort();
    example_classes.dedup();
    example_classes.truncate(3);
    Some(TermNamespace {
        graph,
        namespace,
        example_classes,
    })
}

/// The IRI up to and including its last `#` or `/`.
fn namespace_of(iri: &str) -> &str {
    iri.rfind(['#', '/']).map_or(iri, |at| &iri[..=at])
}

/// The sentence appended to the served `sparql` description, or `None` when
/// no ontology is loaded.
pub fn namespace_guidance(store: &Store) -> Option<String> {
    let namespaces = term_namespaces(store);
    if namespaces.is_empty() {
        return None;
    }
    let listed = namespaces
        .iter()
        .map(|ns| {
            format!(
                "<{}> (classes such as {})",
                ns.namespace,
                ns.example_classes.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "Classes and properties live under these term namespaces, read from the loaded ontologies: {listed}. A graph IRI is not a term namespace: `PREFIX x: <graph-iri#>` matches nothing."
    ))
}

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
