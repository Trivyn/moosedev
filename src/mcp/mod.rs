//! MCP server surface for MOOSEDev (stdio transport via `rmcp`).
//!
//! Exposes the durable knowledge-graph tools over MCP. Tool handlers stay thin:
//! validate input, map it to the domain layer, format the result. The typed
//! write goes through MOOSE's cache-coherent `kg::assert_instance` (via `graph`).

use std::sync::Arc;

use chrono::Utc;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};

use crate::alignment;
use crate::export::{self, ExportFormat, ExportScope};
use crate::graph::{self, AppState, RecordInput, SupersedeInput};
use crate::provenance;
use crate::{sparql, validation};

/// Tool result helpers — the `vec![Content::text(..)]` wrapping repeats across
/// every handler, so name it once.
fn tool_ok(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(message.into())])
}
fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message.into())])
}

/// Format link suggestions as confirm-ready `relate` lines. Empty when there are
/// none, so callers can stay silent (honest empty state, invariant #6).
fn format_suggestions(suggestions: &[graph::LinkSuggestion]) -> String {
    let mut out = String::new();
    for s in suggestions {
        // Render in subject→object order with both endpoints named, so the arrow
        // points the same way as the `relate` below it (an Inverse-direction edge
        // shown as `predicate → candidate` reads as the reverse, illegal orientation).
        out.push_str(&format!(
            "\n  • \"{}\" ({}) -{}-> \"{}\" ({})\n      relate subject_iri={} predicate={} object_iri={}",
            s.subject_title, s.subject_kind, s.predicate_local, s.object_title, s.object_kind,
            s.subject_iri, s.predicate_local, s.object_iri
        ));
    }
    out
}

/// Best-effort capture-time nudge: up to three legal, unasserted links for the new
/// record. Never fails the write — a suggestion error just yields no note.
async fn capture_suggestion_note(state: &graph::AppState, iri: &str) -> String {
    let suggestions =
        graph::suggest_links_for_record(state, iri, 3, graph::dense_floor(), None).await;
    if suggestions.is_empty() {
        return String::new();
    }
    format!(
        "\n\nUnconfirmed suggested links (confirm with `relate`, or ignore):{}",
        format_suggestions(&suggestions)
    )
}

/// Render structured context items into a readable block for the agent.
fn format_context(items: &[graph::ContextItem]) -> String {
    let plural = if items.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "Relevant recorded knowledge ({} item{plural}):\n",
        items.len()
    );
    for item in items {
        out.push_str(&format!("\n• {} — \"{}\"\n", item.kind, item.label));
        for (key, value) in &item.properties {
            out.push_str(&format!("  {key}: {value}\n"));
        }
        out.push_str(&format!("  {}\n", item.iri));
    }
    out
}

/// Resolve the record timestamp. Precedence: an explicit RFC3339 `timestamp` arg -> a
/// driver-provided default file (the temporal bootstrap writes each episode's commit date there,
/// so the timeline is deterministic and not the LLM's responsibility) -> now (normal live capture).
fn resolve_when(ts: &Option<String>) -> Result<chrono::DateTime<Utc>, String> {
    let raw = ts
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| read_capture_default("MOOSEDEV_CAPTURE_TS_FILE"));
    match raw {
        None => Ok(Utc::now()),
        Some(s) => chrono::DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| format!("invalid timestamp (expected RFC3339): {e}")),
    }
}

/// Resolve the author. Precedence: an explicit `author` arg -> a driver-provided default file
/// (the temporal bootstrap's per-episode commit author) -> the MCP client name (live capture).
fn resolve_author(arg: &Option<String>, context: &RequestContext<RoleServer>) -> String {
    arg.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| read_capture_default("MOOSEDEV_CAPTURE_AUTHOR_FILE"))
        .unwrap_or_else(|| {
            context
                .peer
                .peer_info()
                .map(|ci| ci.client_info.name.clone())
                .unwrap_or_else(|| "unknown-mcp-client".to_string())
        })
}

/// Read a driver-provided capture default from the file named by `env_var`, if set and non-empty.
/// Used only by the temporal bootstrap to inject each episode's commit date/author
/// deterministically; live capture sets neither env var, so the normal now/client defaults apply.
fn read_capture_default(env_var: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Soft nudge appended to record/supersede results when a title is long. Titles should be a short
/// handle (a NAME); the claim belongs in the description — a claim-as-title bloats the 2×-weighted
/// `rdfs:label` (saturating lexical retrieval) and the verbose label dilutes NLQ grounding. The note
/// is advisory: the record is still written.
fn long_title_note(title: &str) -> String {
    let n = title.chars().count();
    if n > 100 {
        format!(
            "\n\nnote: title is {n} chars — a title should be a short handle (≤80, a NAME not the \
             claim). Move the claim to the start of the description."
        )
    } else {
        String::new()
    }
}

/// Arguments for the `record_important_decision` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecordDecisionArgs {
    /// Knowledge class to record — a class in the architecture ontology.
    /// Defaults to "ArchitecturalDecision".
    pub kind: Option<String>,
    /// Short human-readable title — a NAME/handle (≤~80 chars), NOT the claim. It is `rdfs:label`,
    /// weighted 2× in retrieval and used heavily for NLQ grounding, so keep it a distinctive name;
    /// put the claim and detail in `description`.
    pub title: String,
    /// Optional longer description / body. LEAD with the one-line claim, then the why + evidence.
    pub description: Option<String>,
    /// Optional lifecycle status (defaults to "accepted"; e.g. "accepted", "superseded").
    pub status: Option<String>,
    /// Optional RFC3339 timestamp to stamp the record with (defaults to now).
    /// Used by the temporal bootstrap to replay historical commit dates.
    pub timestamp: Option<String>,
    /// Optional author to attribute the record to (defaults to the MCP client name).
    pub author: Option<String>,
    /// Optional forward relations to assert from the new record, linking it into
    /// the graph at capture time (invariant #2 — typed links, not prose). Each is a
    /// `{predicate, target}`: `predicate` is a record→record object-property local
    /// name (e.g. "isMotivatedBy", "violates", "learnedFrom", "dependsOn"); `target`
    /// is an existing recorded item's IRI or its exact title. Validated against the
    /// SHACL domain/range — an illegal or unresolvable relation fails the whole
    /// capture. (Targets must already be recorded items; to link an auxiliary node
    /// whose range isn't a record — e.g. concerns→SystemComponent — use `relate`.)
    pub relations: Option<Vec<RelationArg>>,
    /// Alternatives you ACTUALLY weighed and rejected for this decision (the road not
    /// taken). One string per alternative — each mints an `Alternative` node linked via
    /// `weighs`, in this same call. OMIT if you did not genuinely consider one; do NOT
    /// invent an alternative to fill this field.
    pub alternatives_considered: Option<Vec<String>>,
    /// Consequences / accepted trade-offs that RESULT from this decision. One string
    /// each — mints a `Consequence` node linked via `resultsIn`, in this same call.
    /// OMIT if none is known yet; do not pad.
    pub consequences: Option<Vec<String>>,
}

/// One inline relation for `record_important_decision`: an object property plus its
/// target record (by IRI or exact title).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelationArg {
    /// Object-property local name from the architecture ontology (as in `relate`).
    pub predicate: String,
    /// The relation's target: an existing record IRI, or its exact title.
    pub target: String,
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
    /// Include superseded/deprecated records too. Defaults to false — only the
    /// current working set is returned, with each item's supersedes link shown.
    pub include_history: Option<bool>,
}

/// Arguments for the `supersede_decision` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SupersedeArgs {
    /// IRI of the existing decision being replaced (it will be marked "superseded").
    pub superseded_iri: String,
    /// Short human-readable title for the new (replacement) decision — a NAME/handle (≤~80 chars),
    /// NOT the claim (put the claim at the start of `description`).
    pub title: String,
    /// Why the decision changed — captured as a linked Rationale node. Required.
    pub rationale: String,
    /// Optional longer description / body for the new decision.
    pub description: Option<String>,
    /// Ignored: the replacement always inherits the superseded item's class
    /// (type-preserving). Kept for backward compatibility.
    pub kind: Option<String>,
    /// Optional RFC3339 timestamp to stamp the replacement + rationale with (defaults to now).
    pub timestamp: Option<String>,
    /// Optional author to attribute the replacement to (defaults to the MCP client name).
    pub author: Option<String>,
}

/// Arguments for the `retract_decision` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RetractArgs {
    /// IRI of the recorded item to retract — it is marked "deprecated" (dropped
    /// from the current working set) but preserved as history (never deleted).
    pub iri: String,
    /// Why the item is being withdrawn — captured as a linked Rationale. Required.
    pub rationale: String,
    /// Optional RFC3339 timestamp to stamp the rationale with (defaults to now).
    pub timestamp: Option<String>,
    /// Optional author to attribute the retraction to (defaults to the MCP client name).
    pub author: Option<String>,
}

/// Arguments for the `relate` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelateArgs {
    /// IRI of the subject record (the relationship's source).
    pub subject_iri: String,
    /// The relationship: an object-property local name from the architecture
    /// ontology — e.g. "violates", "isMotivatedBy", "concerns", "dependsOn".
    pub predicate: String,
    /// IRI of the object record (the relationship's target).
    pub object_iri: String,
}

/// Arguments for the `get_provenance` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetProvenanceArgs {
    /// IRI of the recorded item to fetch edit provenance for.
    pub iri: String,
}

/// Arguments for the `align_concepts` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AlignConceptsArgs {
    /// Label of the new concept to align (e.g. "Design Decision").
    pub label: String,
    /// Optional definition / description to sharpen the match.
    pub definition: Option<String>,
    /// Optional additional surface forms (synonyms) for the concept.
    pub surface_labels: Option<Vec<String>>,
}

/// Arguments for the `suggest_mappings` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestMappingsArgs {
    /// Label of the concept to find candidate class mappings for.
    pub label: String,
    /// Optional definition / description to sharpen the candidates.
    pub definition: Option<String>,
}

/// Arguments for the `sparql` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SparqlArgs {
    /// Read-only SPARQL query to run against the local project store.
    pub query: String,
}

/// Arguments for the `suggest_links` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestLinksArgs {
    /// IRI of a single record to suggest links for. Omit to SCAN the project graph
    /// for under-linked records (those the shapes say SHOULD carry a link).
    pub iri: Option<String>,
    /// Maximum suggestions per record (default 5).
    pub top_n: Option<usize>,
    /// In scan mode, the maximum number of under-linked records to inspect
    /// (default 20).
    pub max_records: Option<usize>,
}

/// Arguments for the `export_graph` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportGraphArgs {
    /// Output format: nq, nt, or ttl. Defaults to nq.
    pub format: Option<String>,
    /// Named graph scope: project, provenance, or all. Defaults to project.
    pub graph: Option<String>,
    /// Optional output path. Prefer an absolute path. If omitted, the RDF text is returned inline.
    pub path: Option<String>,
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
        description = "Record a typed architectural decision (or other knowledge class) into the durable project knowledge graph. For an ArchitecturalDecision, capture its cluster in the SAME call: `alternatives_considered` (options you weighed and rejected) and `consequences` (trade-offs that result) are each minted as a typed node and linked — record what you actually weighed; omit when there is none (do not invent)."
    )]
    async fn record_important_decision(
        &self,
        Parameters(args): Parameters<RecordDecisionArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let kind = args
            .kind
            .unwrap_or_else(|| "ArchitecturalDecision".to_string());
        let title = args.title.trim().to_string();
        if title.is_empty() {
            return Ok(tool_error("`title` must not be empty"));
        }
        let title_note = long_title_note(&title);
        let class_iri = match self.state.resolve_class(&kind) {
            Ok(iri) => iri,
            Err(e) => return Ok(tool_error(e.to_string())),
        };

        // Map the decision's fields to (predicate, value) assertions, using the
        // capture predicates resolved from the ontology at bootstrap. Adding a new
        // knowledge class means a new mapping here — not a change to the generic
        // `graph::record_instance` writer. The title is written both as
        // `rdfs:label` (the canonical instance label MOOSE indexes). The graph
        // writer also mirrors it into the class-specific typed property declared
        // by the ontology's `labelProperty` annotation, falling back to `hasTitle`.
        let cap = &self.state.capture;
        let mut properties = vec![
            (moose::RDFS_LABEL.to_string(), title.clone()),
            (cap.title.clone(), title),
        ];
        if let Some(desc) = args.description.filter(|s| !s.trim().is_empty()) {
            properties.push((cap.description.clone(), desc));
        }
        if let Some(status) = args.status.filter(|s| !s.trim().is_empty()) {
            properties.push((cap.status.clone(), status));
        }

        let agent = resolve_author(&args.author, &context);
        let now = match resolve_when(&args.timestamp) {
            Ok(t) => t,
            Err(msg) => return Ok(tool_error(msg)),
        };
        let input = RecordInput {
            class_iri,
            class_local: kind.clone(),
            properties,
        };
        let relations: Vec<(String, String)> = args
            .relations
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.predicate, r.target))
            .collect();
        // Inline decision cluster: mint Alternatives/Consequences and link them in the
        // SAME call (weighs/resultsIn), so a decision's cluster is captured without a
        // mint-then-relate dance. isMotivatedBy stays link-to-existing (via `relations`)
        // — Requirements are reusable hubs, not minted per decision.
        let alternatives = args.alternatives_considered.unwrap_or_default();
        let consequences = args.consequences.unwrap_or_default();
        let cluster = [
            graph::ClusterSlot {
                predicate_local: "weighs",
                range_class_local: "Alternative",
                labels: &alternatives,
            },
            graph::ClusterSlot {
                predicate_local: "resultsIn",
                range_class_local: "Consequence",
                labels: &consequences,
            },
        ];
        match graph::record_decision_with_cluster(
            &self.state,
            &input,
            &relations,
            &cluster,
            &agent,
            now,
        ) {
            Ok((outcome, minted)) => {
                let iri = outcome.iri;
                // Best-effort edit provenance: who (the MCP client) asserted this,
                // and when. Post-write — a provenance failure must not fail the
                // record (mirrors MOOSE's `ProvenanceWriter` contract). v1 wires
                // this per write-tool; once a second write tool (or supersede/
                // retract) exists, fold it into `graph::record_instance` so every
                // write is provenanced by construction (the agent identity, from
                // the MCP `context`, would be threaded down then).
                if let Err(e) =
                    provenance::record_provenance_at(&self.state.store, &iri, &agent, now)
                {
                    tracing::warn!("provenance write failed for {iri}: {e}");
                }
                // Best-effort dense indexing so the new record is reachable by the
                // hybrid (paraphrase-tolerant) seed of get_relevant_context. Like
                // provenance, a failure must never fail the write — the symbolic
                // record is primary (invariant #1).
                if let Err(e) = self.state.index_record(&iri).await {
                    tracing::warn!("dense index failed for {iri}: {e}");
                }
                // A new record (+ any edges) invalidates the materialized inverse edges.
                self.state.note_project_write();
                let mut links: Vec<String> = outcome
                    .applied_edges
                    .iter()
                    .map(|e| format!("{} → {}", e.predicate_local, e.object_iri))
                    .collect();
                links.extend(
                    minted
                        .iter()
                        .map(|m| format!("{} → {}", m.predicate_local, m.iri)),
                );
                let edge_note = if links.is_empty() {
                    String::new()
                } else {
                    format!("\nLinked: {}", links.join(", "))
                };
                let suggestion_note = capture_suggestion_note(&self.state, &iri).await;
                Ok(tool_ok(format!(
                    "Recorded {kind} → {iri}{title_note}{edge_note}{suggestion_note}"
                )))
            }
            Err(e) => Ok(tool_error(format!("failed to record: {e}"))),
        }
    }

    /// Record a new decision that supersedes an existing one, preserving history.
    #[tool(
        description = "Record a NEW knowledge item that supersedes an existing one when a prior decision/requirement/constraint/lesson changes. The replacement is recorded with the SAME knowledge class as the superseded item (type-preserving). Links new -supersedes-> old, captures WHY it changed as a linked Rationale, and marks the old item 'superseded' — the old record is preserved (never deleted), so the history and reasoning are retained. Pass the IRI of the item being replaced as `superseded_iri`."
    )]
    async fn supersede_decision(
        &self,
        Parameters(args): Parameters<SupersedeArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let title = args.title.trim().to_string();
        let rationale = args.rationale.trim().to_string();
        let superseded_iri = args.superseded_iri.trim().to_string();
        if title.is_empty() {
            return Ok(tool_error("`title` must not be empty"));
        }
        if rationale.is_empty() {
            return Ok(tool_error(
                "`rationale` must not be empty — capture WHY the decision changed",
            ));
        }
        let title_note = long_title_note(&title);
        if superseded_iri.is_empty() {
            return Ok(tool_error("`superseded_iri` must not be empty"));
        }

        let cap = &self.state.capture;
        let mut properties = vec![
            (moose::RDFS_LABEL.to_string(), title.clone()),
            (cap.title.clone(), title),
        ];
        if let Some(desc) = args.description.filter(|s| !s.trim().is_empty()) {
            properties.push((cap.description.clone(), desc));
        }

        let agent = resolve_author(&args.author, &context);
        let now = match resolve_when(&args.timestamp) {
            Ok(t) => t,
            Err(msg) => return Ok(tool_error(msg)),
        };
        let input = SupersedeInput {
            superseded_iri,
            // Class is inferred from the superseded record (type-preserving) inside
            // graph::supersede_decision; these placeholders are ignored, as is the
            // `kind` arg — the replacement always matches the superseded item's class.
            new: RecordInput {
                class_iri: String::new(),
                class_local: String::new(),
                properties,
            },
            rationale,
        };
        match graph::supersede_decision(&self.state, &input, &agent, now) {
            Ok(out) => {
                // Provenance both minted records (best-effort, never fails the write).
                for iri in [&out.new_iri, &out.rationale_iri] {
                    if let Err(e) =
                        provenance::record_provenance_at(&self.state.store, iri, &agent, now)
                    {
                        tracing::warn!("provenance write failed for {iri}: {e}");
                    }
                    // Re-embed the replacement + its rationale so the new current
                    // record is dense-seedable. The superseded record's text is
                    // unchanged (only its status flips), so its vector stays valid;
                    // the post-fetch is_historical filter hides it by default.
                    if let Err(e) = self.state.index_record(iri).await {
                        tracing::warn!("dense index failed for {iri}: {e}");
                    }
                }
                self.state.note_project_write();
                Ok(tool_ok(format!(
                    "Superseded {} → {} (rationale {}){title_note}",
                    out.superseded_iri, out.new_iri, out.rationale_iri
                )))
            }
            Err(e) => Ok(tool_error(format!("failed to supersede: {e}"))),
        }
    }

    /// Retract a recorded knowledge item in place (mark deprecated + capture why).
    #[tool(
        description = "Retract a recorded knowledge item that should no longer apply (e.g. a duplicate, or a decision abandoned WITHOUT a replacement). Marks it 'deprecated' so it drops out of the current working set, captures WHY as a linked Rationale, and preserves the record as history (never deleted). Use supersede_decision instead when a replacement record exists. Pass the item's IRI as `iri` and the reason as `rationale`."
    )]
    async fn retract_decision(
        &self,
        Parameters(args): Parameters<RetractArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let iri = args.iri.trim().to_string();
        let rationale = args.rationale.trim().to_string();
        if iri.is_empty() {
            return Ok(tool_error("`iri` must not be empty"));
        }
        if rationale.is_empty() {
            return Ok(tool_error(
                "`rationale` must not be empty — capture WHY the item is being retracted",
            ));
        }

        let agent = resolve_author(&args.author, &context);
        let now = match resolve_when(&args.timestamp) {
            Ok(t) => t,
            Err(msg) => return Ok(tool_error(msg)),
        };
        match graph::retract_decision(&self.state, &iri, &rationale, &agent, now) {
            Ok(out) => {
                // Provenance the minted rationale node (best-effort; never fails the write).
                if let Err(e) = provenance::record_provenance_at(
                    &self.state.store,
                    &out.rationale_iri,
                    &agent,
                    now,
                ) {
                    tracing::warn!("provenance write failed for {}: {e}", out.rationale_iri);
                }
                // Embed the minted rationale. The retracted record's text is
                // unchanged (only its status flips to deprecated), so its vector
                // stays valid and remains visible to include_history callers.
                if let Err(e) = self.state.index_record(&out.rationale_iri).await {
                    tracing::warn!("dense index failed for {}: {e}", out.rationale_iri);
                }
                self.state.note_project_write();
                Ok(tool_ok(format!(
                    "Retracted {} (deprecated; rationale {})",
                    out.retracted_iri, out.rationale_iri
                )))
            }
            Err(e) => Ok(tool_error(format!("failed to retract: {e}"))),
        }
    }

    /// Link two recorded knowledge items with a typed relationship edge.
    #[tool(
        description = "Link two recorded knowledge items with a typed relationship, building the project knowledge GRAPH (not just a list). The relationship is an object property from the architecture ontology, given by its local name — e.g. an AntiPattern `violates` a Constraint, an ArchitecturalDecision `isMotivatedBy` a Requirement or `concerns` a component, one Component `dependsOn` another. Both endpoints must already be recorded items (record them first); the edge is added idempotently. Use this to connect related decisions, constraints, lessons, and patterns so memory can be TRAVERSED, not only searched."
    )]
    async fn relate(
        &self,
        Parameters(args): Parameters<RelateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let subject_iri = args.subject_iri.trim().to_string();
        let predicate = args.predicate.trim().to_string();
        let object_iri = args.object_iri.trim().to_string();
        if subject_iri.is_empty() || object_iri.is_empty() {
            return Ok(tool_error(
                "`subject_iri` and `object_iri` must not be empty",
            ));
        }
        if predicate.is_empty() {
            return Ok(tool_error(
                "`predicate` must not be empty (an object-property local name, e.g. \"violates\")",
            ));
        }
        match graph::relate(&self.state, &subject_iri, &predicate, &object_iri) {
            Ok(out) => {
                // A new edge changes what inverse-materialization yields.
                self.state.note_project_write();
                Ok(tool_ok(format!(
                    "Related {} -{}-> {}",
                    out.subject_iri, predicate, out.object_iri
                )))
            }
            Err(e) => Ok(tool_error(format!("failed to relate: {e}"))),
        }
    }

    /// Suggest typed links to add to the project knowledge graph (suggest-only).
    #[tool(
        description = "Suggest typed links to add to the project knowledge GRAPH — ranked, ontology-legal candidate relationships between recorded items, so memory can be TRAVERSED, not just searched. Suggest-only: it writes nothing; confirm a suggestion by calling `relate` with the printed arguments. Pass `iri` to get link candidates for one record; omit it to SCAN for under-linked records (records the shapes say SHOULD carry a link — e.g. an ArchitecturalDecision with no isMotivatedBy). Candidates are generated symbolically (hybrid retrieval + SHACL domain/range) and never invented."
    )]
    async fn suggest_links(
        &self,
        Parameters(args): Parameters<SuggestLinksArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Materialize inferred inverses first, so "already-linked" sees them.
        self.state.ensure_enriched();
        let top_n = args.top_n.unwrap_or(5).clamp(1, 25);
        let floor = graph::dense_floor();
        match args
            .iri
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            Some(iri) => {
                let suggestions =
                    graph::suggest_links_for_record(&self.state, &iri, top_n, floor, None).await;
                if suggestions.is_empty() {
                    Ok(tool_ok(format!(
                        "No legal unasserted links found for {iri}."
                    )))
                } else {
                    Ok(tool_ok(format!(
                        "Suggested links for {iri} (confirm with `relate`):{}",
                        format_suggestions(&suggestions)
                    )))
                }
            }
            None => {
                let max_records = args.max_records.unwrap_or(20).clamp(1, 200);
                let under = graph::under_linked_records(&self.state, max_records);
                if under.is_empty() {
                    return Ok(tool_ok(
                        "No under-linked records: every record the shapes flag already carries its expected link.".to_string(),
                    ));
                }
                let cap = 50usize;
                let mut total = 0usize;
                let mut out = format!(
                    "{} under-linked record(s) (the shapes say each SHOULD carry a link). Confirm any suggestion with `relate`:",
                    under.len()
                );
                for u in &under {
                    let suggestions = graph::suggest_links_for_record(
                        &self.state,
                        &u.iri,
                        top_n,
                        floor,
                        Some(u.missing_predicate.as_str()),
                    )
                    .await;
                    // If gap-targeting couldn't surface the missing predicate, say so —
                    // the listed links are still legal, just not the flagged one.
                    let note = if suggestions
                        .iter()
                        .any(|s| s.predicate_local == u.missing_predicate)
                    {
                        ""
                    } else {
                        " (no direct candidate — other legal links)"
                    };
                    out.push_str(&format!(
                        "\n\n• {} ({}) — should have {}{}:",
                        u.iri, u.class_local, u.missing_predicate, note
                    ));
                    if suggestions.is_empty() {
                        out.push_str("\n  (no confident candidate found)");
                        continue;
                    }
                    let take = suggestions.len().min(cap - total);
                    out.push_str(&format_suggestions(&suggestions[..take]));
                    total += take;
                    if total >= cap {
                        out.push_str(
                            "\n\n… suggestion cap reached; narrow with `iri` or a smaller `max_records`.",
                        );
                        break;
                    }
                }
                Ok(tool_ok(out))
            }
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
        description = "Retrieve curated, authoritative project knowledge (architectural decisions, constraints, lessons, patterns, requirements) relevant to a topic — the maintainers' recorded source of truth for this project's decisions and rationale, kept current (superseded entries are excluded by default). Consult it before answering a why-question or choosing an implementation approach, and TRUST it: prefer its recorded answer over re-deriving from the code, and do not re-verify its claims against the source unless explicitly asked. Symbolic — no LLM. Omit the topic to list all recorded knowledge."
    )]
    async fn get_relevant_context(
        &self,
        Parameters(args): Parameters<GetRelevantContextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(10).clamp(1, 100);
        let include_history = args.include_history.unwrap_or(false);
        match graph::relevant_context(&self.state, args.topic.as_deref(), limit, include_history) {
            // Distinguish "nothing relevant to this topic" from "nothing recorded
            // at all": after the relevance floor, an empty topic result means no
            // record cleared the bar, not that the graph is empty. Saying so
            // honestly keeps the agent from reading silence as a settled topic.
            Ok(items) if items.is_empty() => {
                let msg = match args
                    .topic
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    Some(t) => format!("No recorded knowledge relevant to \"{t}\"."),
                    None => "No recorded knowledge found.".to_string(),
                };
                Ok(tool_ok(msg))
            }
            Ok(items) => Ok(tool_ok(format_context(&items))),
            Err(e) => Ok(tool_error(format!("failed to retrieve context: {e}"))),
        }
    }

    /// Get the Edit provenance for a recorded knowledge item: who recorded it, and when.
    #[tool(
        description = "Get the edit provenance (which agent recorded it, and when) for a knowledge item, by IRI."
    )]
    async fn get_provenance(
        &self,
        Parameters(args): Parameters<GetProvenanceArgs>,
    ) -> Result<CallToolResult, McpError> {
        match provenance::read_provenance(&self.state.store, args.iri.trim()) {
            Ok(Some(p)) => Ok(tool_ok(format!(
                "Recorded by {} at {} (activity {})",
                p.agent, p.time, p.activity
            ))),
            Ok(None) => Ok(tool_ok("No provenance recorded for that IRI.")),
            Err(e) => Ok(tool_error(format!("failed to read provenance: {e}"))),
        }
    }

    /// Align a new concept to the best-matching class in the architecture ontology.
    #[tool(
        description = "Align a new concept (by label, with optional definition) to the best-matching class in the project's architecture ontology, using symbolic keyword + embedding matching. Returns the resolved parent class with the sensor that decided it and a rationale, or ranked candidate classes if ambiguous."
    )]
    async fn align_concepts(
        &self,
        Parameters(args): Parameters<AlignConceptsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let label = args.label.trim();
        if label.is_empty() {
            return Ok(tool_error("`label` must not be empty"));
        }
        let surface_labels = args.surface_labels.unwrap_or_default();
        match alignment::align_concept(
            &self.state,
            label,
            args.definition.as_deref(),
            surface_labels,
        )
        .await
        {
            Ok(outcome) => Ok(tool_ok(alignment::format_outcome(
                &self.state,
                label,
                &outcome,
            ))),
            Err(e) => Ok(tool_error(format!("alignment failed: {e}"))),
        }
    }

    /// Suggest candidate ontology classes a concept could map to (for review).
    #[tool(
        description = "Suggest which existing architecture-ontology classes a new concept could map to, for human review. Same alignment engine as align_concepts; surfaces the ranked candidate classes when the match is ambiguous."
    )]
    async fn suggest_mappings(
        &self,
        Parameters(args): Parameters<SuggestMappingsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let label = args.label.trim();
        if label.is_empty() {
            return Ok(tool_error("`label` must not be empty"));
        }
        match alignment::align_concept(&self.state, label, args.definition.as_deref(), Vec::new())
            .await
        {
            Ok(outcome) => Ok(tool_ok(alignment::format_outcome(
                &self.state,
                label,
                &outcome,
            ))),
            Err(e) => Ok(tool_error(format!("suggest_mappings failed: {e}"))),
        }
    }

    /// Run a read-only SPARQL query over the local store.
    #[tool(
        description = "Run a read-only SPARQL query over the local store. Default graph is the union of named graphs unless the query specifies FROM. SELECT/ASK return SPARQL JSON; CONSTRUCT/DESCRIBE return N-Triples. Key graph IRIs: project https://moosedev.dev/kg/project, provenance https://moosedev.dev/kg/provenance, architecture shapes https://moosedev.dev/kg/ontology/software-architecture/shapes."
    )]
    async fn sparql(
        &self,
        Parameters(args): Parameters<SparqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let query = args.query.trim();
        if query.is_empty() {
            return Ok(tool_error("`query` must not be empty"));
        }
        // Inferred edges are queryable too — refresh them if a write invalidated them.
        self.state.ensure_enriched();
        match sparql::run_query(&self.state.store, query) {
            Ok(output) => Ok(tool_ok(output)),
            Err(e) => Ok(tool_error(format!("SPARQL failed: {e}"))),
        }
    }

    /// Serialize the project knowledge graph to RDF text for backup / version
    /// control / transfer — NOT a read or search path.
    #[tool(
        description = "Serialize the project knowledge graph to RDF text for BACKUP, version control, or transfer — this is NOT a read or search path. To read, search, or traverse knowledge — to answer a question, render a document, or inspect records — use `sparql`, `get_relevant_context`, or `query` instead; do NOT dump the whole graph into context (that defeats the point of structured, queryable memory). Defaults to canonical N-Quads for the project graph. N-Triples is deterministic after graph names are dropped; Turtle is human-readable, not byte-canonical. Optional args: format nq|nt|ttl, graph project|provenance|all, path for writing to a file instead of returning inline text (absolute paths recommended)."
    )]
    async fn export_graph(
        &self,
        Parameters(args): Parameters<ExportGraphArgs>,
    ) -> Result<CallToolResult, McpError> {
        let format = match args.format.as_deref() {
            Some(raw) => match ExportFormat::parse(raw) {
                Ok(format) => format,
                Err(e) => return Ok(tool_error(e.to_string())),
            },
            None => ExportFormat::default(),
        };
        let scope = match args.graph.as_deref() {
            Some(raw) => match ExportScope::parse(raw) {
                Ok(scope) => scope,
                Err(e) => return Ok(tool_error(e.to_string())),
            },
            None => ExportScope::default(),
        };

        let dump = match export::export_graph(&self.state.store, scope, format) {
            Ok(dump) => dump,
            Err(e) => return Ok(tool_error(format!("export failed: {e}"))),
        };

        if let Some(path) = args.path.filter(|path| !path.trim().is_empty()) {
            if let Err(e) = std::fs::write(&path, dump.text) {
                return Ok(tool_error(format!("write export {path}: {e}")));
            }
            Ok(tool_ok(format!(
                "exported {} quads from {} to {}",
                dump.quad_count,
                dump.graphs.join(", "),
                path
            )))
        } else {
            Ok(tool_ok(dump.text))
        }
    }

    /// Validate recorded knowledge against the loaded architecture shapes.
    #[tool(
        description = "Validate the durable project knowledge graph against the loaded architecture SHACL shapes. Symbolic and on-demand; validates recorded knowledge, not source code."
    )]
    async fn validate_against_architecture(&self) -> Result<CallToolResult, McpError> {
        match validation::validate_project(&self.state) {
            Ok(report) => Ok(tool_ok(validation::format_report(&report))),
            Err(e) => Ok(tool_error(format!("validation failed: {e}"))),
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
            "MOOSEDev: structured, long-term project memory built on the MOOSE engine. Use sparql for deterministic read-only graph queries and validate_against_architecture to check recorded knowledge against loaded shapes.".to_string(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_when;

    #[test]
    fn resolve_when_parses_rfc3339() {
        let dt = resolve_when(&Some("2020-01-02T03:04:05Z".to_string())).unwrap();
        assert_eq!(dt.to_rfc3339(), "2020-01-02T03:04:05+00:00");
    }

    #[test]
    fn resolve_when_defaults_to_now_when_absent_or_blank() {
        assert!(resolve_when(&None).is_ok());
        assert!(resolve_when(&Some("   ".to_string())).is_ok());
    }

    #[test]
    fn resolve_when_rejects_malformed() {
        let err = resolve_when(&Some("not-a-date".to_string())).unwrap_err();
        assert!(
            err.contains("RFC3339"),
            "error names the expected format: {err}"
        );
    }

    #[test]
    fn inverse_suggestion_renders_in_relate_direction() {
        // Seed record = an AD that is the OBJECT of `learnedFrom`; the candidate
        // Lesson is the subject. The rendered line must read subject→object in the
        // same direction as the `relate` it emits, so a reader never mistakes it for
        // the reverse (illegal) `AD -learnedFrom-> Lesson`.
        let s = crate::graph::LinkSuggestion {
            predicate_local: "learnedFrom".to_string(),
            subject_iri: "https://moosedev.dev/kg/Lesson/lesson-1".to_string(),
            object_iri: "https://moosedev.dev/kg/ArchitecturalDecision/ad-1".to_string(),
            subject_title: "Comment new integration seams".to_string(),
            subject_kind: "Lesson".to_string(),
            object_title: "Split the graph module".to_string(),
            object_kind: "ArchitecturalDecision".to_string(),
            score: 0.5,
        };
        let rendered = super::format_suggestions(&[s]);

        // Headline reads subject (Lesson) before object (AD), predicate in the arrow.
        let subj = rendered.find("Comment new integration seams").unwrap();
        let obj = rendered.find("Split the graph module").unwrap();
        assert!(subj < obj, "subject must render before object:\n{rendered}");
        assert!(
            rendered.contains("-learnedFrom->"),
            "predicate belongs in the arrow:\n{rendered}"
        );
        // The relate args carry that same orientation (Lesson subject, AD object).
        assert!(
            rendered.contains("subject_iri=https://moosedev.dev/kg/Lesson/lesson-1")
                && rendered
                    .contains("object_iri=https://moosedev.dev/kg/ArchitecturalDecision/ad-1"),
            "relate line must match the rendered direction:\n{rendered}"
        );
    }
}
