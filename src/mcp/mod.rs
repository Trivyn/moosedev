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

const SERVER_INSTRUCTIONS: &str = "MOOSEDev is durable, authoritative structured project memory. Before non-trivial work, call `get_relevant_context` with no `topic` for a broad current inventory; set `limit` up to 100, or use `sparql` when completeness matters. Use `get_relevant_context` for fast hybrid recall, `query` for one focused relationship question, and `sparql` for exact or exhaustive structural reads. Before editing a specific function, type, or module, call `get_entity_dossier`; an empty dossier does not replace project recall. Run `align_concepts` before introducing a new knowledge term. After capturing a record, report its kind, title, and IRI. Correct existing knowledge with `supersede_decision` or `retract_decision` instead of duplicating it, and run `validate_against_architecture` after any graph write.";

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

fn format_path_list(paths: &[String]) -> String {
    if paths.is_empty() {
        "(none)".to_string()
    } else {
        format!("[{}]", paths.join(", "))
    }
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
    /// `{predicate, target}`: `predicate` is an object-property local name (e.g.
    /// "isMotivatedBy", "violates", "learnedFrom", "concerns", "dependsOn");
    /// `target` is an existing typed node's IRI or exact label/title. Validated
    /// against the SHACL domain/range — an illegal or unresolvable relation fails
    /// the whole capture.
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
/// target typed node (by IRI or exact label/title).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
pub struct RelationArg {
    /// Object-property local name from the architecture ontology (as in `relate`).
    pub predicate: String,
    /// The relation's target: an existing typed node IRI, or its exact label/title.
    pub target: String,
}

/// Arguments for the `query` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryArgs {
    /// One short, focused, single-purpose question whose answer requires
    /// synthesizing relationships in the project knowledge graph.
    pub question: String,
}

/// Arguments for the `get_relevant_context` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRelevantContextArgs {
    /// Topic/focus for fast hybrid relevance recall. Omit for a broad current
    /// inventory, bounded by `limit`.
    pub topic: Option<String>,
    /// Maximum number of items to return (default 10; clamped to 1..=100).
    pub limit: Option<usize>,
    /// Include records outside the working set too (superseded, deprecated,
    /// rejected, and still-proposed ones). Defaults to false — only ratified
    /// current knowledge is returned, with each item's supersedes link shown;
    /// pending proposals live in `pending_ratifications`, not here.
    pub include_history: Option<bool>,
}

/// Arguments for the `supersede_decision` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SupersedeArgs {
    /// IRI of the existing typed knowledge record being replaced; it will be
    /// marked "superseded" but preserved as history.
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
    /// `playsRole` and `hasCriticality` are ratification-only and rejected here.
    pub predicate: String,
    /// IRI of the object record (the relationship's target).
    pub object_iri: String,
}

/// Arguments for the `link_code` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkCodeArgs {
    /// IRI of the typed KG record to link.
    pub record_iri: String,
    /// Object-property local name for the link. Defaults to "concerns"; use
    /// "constrains" when attaching a Constraint to code.
    pub predicate: Option<String>,
    /// Repo-relative path as stored in the substrate index; provide together
    /// with `line` and `col`, instead of `symbol`.
    pub file: Option<String>,
    /// 1-based source line for a position selector.
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// 1-based UTF-8 byte column for a position selector.
    #[schemars(range(min = 1))]
    pub col: Option<u32>,
    /// SCIP symbol, raw or version-normalized, as an alternative to a file position.
    pub symbol: Option<String>,
}

/// Arguments for the `declare_component_paths` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeclareComponentPathsArgs {
    /// SystemComponent IRI, or its exact name (rdfs:label).
    pub component: String,
    /// Repo-relative paths the component covers. Trailing '/' = directory
    /// prefix (e.g. "src/code/"); no trailing slash = exact file path. Must
    /// contain at least one path.
    #[schemars(length(min = 1), inner(length(min = 1)))]
    pub paths: Vec<String>,
}

/// Arguments for the `get_entity_dossier` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetEntityDossierArgs {
    /// Repo-relative path as stored in the substrate index; provide together
    /// with `line` and `col`, instead of `symbol` or `iri`.
    pub file: Option<String>,
    /// 1-based source line for a position selector.
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// 1-based UTF-8 byte column for a position selector.
    #[schemars(range(min = 1))]
    pub col: Option<u32>,
    /// SCIP symbol, raw or version-normalized, as an alternative selector.
    pub symbol: Option<String>,
    /// CodeEntity IRI as an alternative selector.
    pub iri: Option<String>,
}

/// Arguments for the `capture_decision_point` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CaptureDecisionPointArgs {
    /// Repo-relative changed files (diff-derived by the host adapter). At least
    /// one changed file or a `summary` must ground the proposal.
    pub files: Option<Vec<String>>,
    /// The host's own summary text for the decision point — used as the title
    /// seed; never an interrogated justification.
    pub summary: Option<String>,
    /// Optional existing Requirement (IRI or exact title) for `isMotivatedBy`.
    /// An unresolvable value fails the capture; it is never invented.
    pub requirement: Option<String>,
    /// Optional substrate symbols the decision concerns; each is queued as its
    /// own `concerns` link proposal.
    pub entities: Option<Vec<String>>,
    /// Host adapter identity for fire telemetry (defaults to "mcp").
    pub host: Option<String>,
    /// Optional author override (defaults to the MCP client name).
    pub author: Option<String>,
}

/// Arguments for the `evaluate_policy` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EvaluatePolicyArgs {
    /// Host adapter reporting the event (e.g. "claude-code", "opencode");
    /// recorded in fire telemetry. Defaults to "mcp".
    pub host: Option<String>,
    /// Event kind: "entity_touched" (push), "edit_proposed" (gate), or
    /// "decision_point" (capture).
    #[schemars(extend("enum" = ["entity_touched", "edit_proposed", "decision_point"]))]
    pub event: String,
    /// Repo-relative file required by `entity_touched` and `edit_proposed`.
    pub file: Option<String>,
    /// Optional 1-based line for a precise position; provide with `col`.
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// Optional 1-based UTF-8 byte column for a precise position; provide with `line`.
    #[schemars(range(min = 1))]
    pub col: Option<u32>,
    /// Optional edit-text anchor (e.g. an Edit tool's old_string), located in
    /// the on-disk file to scope an `edit_proposed` gate to overlapping definitions.
    pub anchor: Option<String>,
    /// Changed files for a `decision_point` event.
    pub files: Option<Vec<String>>,
    /// Host-provided summary text for a `decision_point` event.
    pub summary: Option<String>,
}

/// Arguments for the `get_provenance` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetProvenanceArgs {
    /// Non-empty IRI of the recorded item to fetch edit provenance for.
    pub iri: String,
}

/// Arguments for the `align_concepts` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AlignConceptsArgs {
    /// Non-empty label of the new concept to align (e.g. "Design Decision").
    pub label: String,
    /// Optional definition / description to sharpen the match.
    pub definition: Option<String>,
    /// Optional additional surface forms (synonyms) for the concept.
    pub surface_labels: Option<Vec<String>>,
}

/// Arguments for the `suggest_mappings` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestMappingsArgs {
    /// Non-empty label of the concept to find candidate class mappings for.
    pub label: String,
    /// Optional definition / description to sharpen the candidates.
    pub definition: Option<String>,
}

/// Arguments for the `sparql` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SparqlArgs {
    /// Non-empty read-only SPARQL query to run against the local project store.
    pub query: String,
}

/// Arguments for the `suggest_links` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestLinksArgs {
    /// IRI of a single record to suggest links for. Omit to SCAN the project graph
    /// for under-linked records (those the shapes say SHOULD carry a link).
    pub iri: Option<String>,
    /// Maximum suggestions per record (default 5; clamped to 1..=25).
    pub top_n: Option<usize>,
    /// In scan mode, the maximum number of under-linked records to inspect
    /// (default 20; clamped to 1..=200).
    pub max_records: Option<usize>,
}

/// Arguments for the `export_graph` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportGraphArgs {
    /// Output format: nq, nt, or ttl. Defaults to nq.
    pub format: Option<String>,
    /// Named graph scope: project, provenance, or all. Defaults to project.
    pub graph: Option<String>,
    /// Optional output path. Prefer an absolute path. If present, writes or
    /// overwrites that file; if omitted, returns the RDF text inline.
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
    #[tool(
        description = "Health check; returns 'pong'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn ping(&self) -> Result<CallToolResult, McpError> {
        Ok(tool_ok("pong"))
    }

    /// Report the pending ratification queue depth (read-only).
    #[tool(
        description = "Count the pending proposals awaiting human ratification (record→code-entity links and proposed records). Read-only; returns the count plus a short preview. Judgment proposals (role/criticality classifications) never count here — they wait quietly in the workbench inbox and are tallied separately. Ratify or reject in the workbench Ratifications page.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn pending_ratifications(&self) -> Result<CallToolResult, McpError> {
        let proposals = match graph::list_proposals(&self.state, Some("proposed")) {
            Ok(proposals) => proposals,
            Err(e) => {
                return Ok(tool_error(format!(
                    "failed to read the ratification queue: {e}"
                )))
            }
        };
        // Never-nudge: judgments wait quietly; only links + records demand.
        let (judgments, nudging): (Vec<_>, Vec<_>) = proposals
            .into_iter()
            .partition(|p| p.kind == graph::ProposalKind::Judgment);
        let judgment_note = if judgments.is_empty() {
            String::new()
        } else {
            format!(
                "\n({} judgment proposal(s) waiting quietly — browse them in the workbench inbox.)",
                judgments.len()
            )
        };
        if nudging.is_empty() {
            return Ok(tool_ok(format!(
                "0 pending ratifications — the inbox is empty.{judgment_note}"
            )));
        }
        let mut out = format!("{} pending ratification(s):\n", nudging.len());
        for proposal in nudging.iter().take(10) {
            match proposal.kind {
                graph::ProposalKind::Link => out.push_str(&format!(
                    "  - [link] {} → {} ({})\n",
                    proposal.subject_iri, proposal.target_symbol, proposal.target_path
                )),
                graph::ProposalKind::Record => out.push_str(&format!(
                    "  - [record] {} \"{}\"\n",
                    proposal.record_class.as_deref().unwrap_or("Record"),
                    proposal.label
                )),
                graph::ProposalKind::Judgment => unreachable!("partitioned out above"),
            }
        }
        if nudging.len() > 10 {
            out.push_str(&format!("  … and {} more\n", nudging.len() - 10));
        }
        out.push_str("Ratify or reject them in the workbench Ratifications page.");
        out.push_str(&judgment_note);
        Ok(tool_ok(out))
    }

    /// Evaluate one host event against the active-agency policy engine.
    #[tool(
        description = "Host-adapter integration tool: evaluate one host event against the graph-driven policy engine and return the typed verdict to enact. Do not use it for ordinary knowledge queries. Use `file` for `entity_touched` and `edit_proposed`; use `files` and optional `summary` for `decision_point`. Gate verdicts cite governing accepted Constraints. It does not modify the project graph, but enacted decisions may append fire telemetry to `.moosedev/fires.jsonl`; returns JSON allow/inject/gate/capture_trigger output.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn evaluate_policy(
        &self,
        Parameters(args): Parameters<EvaluatePolicyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let host = args.host.unwrap_or_else(|| "mcp".to_string());
        let event = match args.event.as_str() {
            "entity_touched" => {
                let Some(file) = args.file else {
                    return Ok(tool_error("entity_touched requires `file`"));
                };
                crate::policy::PolicyEvent::EntityTouched {
                    file,
                    line: args.line,
                    col: args.col,
                }
            }
            "edit_proposed" => {
                let Some(file) = args.file else {
                    return Ok(tool_error("edit_proposed requires `file`"));
                };
                crate::policy::PolicyEvent::EditProposed {
                    file,
                    line: args.line,
                    col: args.col,
                    anchor: args.anchor,
                }
            }
            "decision_point" => crate::policy::PolicyEvent::DecisionPoint {
                files: args.files.unwrap_or_default(),
                summary: args.summary,
            },
            other => {
                return Ok(tool_error(format!(
                    "unknown event kind {other:?}; use entity_touched, edit_proposed, or decision_point"
                )))
            }
        };
        let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        match crate::policy::evaluate_and_fire(&self.state, &repo_root, &event, &host) {
            Ok(decision) => match serde_json::to_string_pretty(&decision) {
                Ok(json) => Ok(tool_ok(json)),
                Err(e) => Ok(tool_error(format!("failed to serialize decision: {e}"))),
            },
            Err(e) => Ok(tool_error(format!("policy evaluation failed: {e}"))),
        }
    }

    /// Grounded capture at a decision point — proposed record + queued links.
    #[tool(
        description = "At a deliberate decision checkpoint, create a PROPOSED ArchitecturalDecision from the caller's summary and changed files, and queue entity links for human ratification. Use only when the session produced a real decision worth review; use `record_important_decision` for knowledge that should be accepted immediately, and `evaluate_policy` for automatic host-event handling. `requirement`, when supplied, must resolve to an existing Requirement. Writes a proposed record, ratification proposals, and capture telemetry; nothing takes effect until a human ratifies it. Returns the record IRI, queued-link count, and unanchored inputs.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn capture_decision_point(
        &self,
        Parameters(args): Parameters<CaptureDecisionPointArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let author = resolve_author(&args.author, &context);
        let host = args.host.unwrap_or_else(|| "mcp".to_string());
        let files = args.files.unwrap_or_default();
        let entities = args.entities.unwrap_or_default();
        let captured = match graph::capture_decision_point(
            &self.state,
            &files,
            args.summary.as_deref(),
            args.requirement.as_deref(),
            &entities,
            &author,
            Utc::now(),
        ) {
            Ok(captured) => captured,
            Err(e) => return Ok(tool_error(format!("grounded capture failed: {e}"))),
        };

        if let Err(e) = self.state.index_record(&captured.record_iri).await {
            tracing::warn!("dense index update failed for {}: {e}", captured.record_iri);
        }
        crate::policy::fires::append_fire_best_effort(
            &self.state.data_dir,
            &crate::policy::fires::FireEvent {
                ts: Utc::now().to_rfc3339(),
                verb: "capture",
                host,
                entity: None,
                decision: "proposed".to_string(),
                records_cited: vec![captured.record_iri.clone()],
                summary: None,
                files: Vec::new(),
            },
        );

        let mut out = format!(
            "Proposed ArchitecturalDecision \"{}\" → {}\n",
            captured.title, captured.record_iri
        );
        if !captured.proposed_links.is_empty() {
            out.push_str(&format!(
                "Queued {} link proposal(s) for ratification.\n",
                captured.proposed_links.len()
            ));
        }
        if !captured.unanchored.is_empty() {
            out.push_str(&format!(
                "Not anchored in the substrate (no link queued): {}.\n",
                captured.unanchored.join(", ")
            ));
        }
        out.push_str("Ratify or reject in the workbench Ratifications page.");
        Ok(tool_ok(out))
    }

    /// Record a typed knowledge item into the durable project knowledge graph.
    #[tool(
        description = "Create a durable typed knowledge record directly in the project graph; the record is accepted by default unless `status` is supplied. Use this after broad recall or an exact SPARQL check confirms the knowledge is new, and call `align_concepts` first when introducing a term. Inline `relations` link existing typed nodes atomically; ArchitecturalDecision alternatives and consequences are minted as typed linked nodes in the same call. This writes the graph and returns the new record IRI and applied links; run `validate_against_architecture` afterward. Use `capture_decision_point` instead when the record must remain proposed for human ratification.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        description = "Replace an existing typed knowledge record when prior knowledge has changed. Atomically creates a replacement of the same class, captures the required rationale as a typed Rationale, links the history in both directions, and marks the old record superseded without deleting it. Use `retract_decision` when there is no successor, and `record_important_decision` only for genuinely new knowledge. Writes the graph and returns the old, new, and rationale IRIs; run `validate_against_architecture` afterward.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        description = "Retract a recorded knowledge item that should no longer apply (e.g. a duplicate, or a decision abandoned WITHOUT a replacement). Marks it 'deprecated' so it drops out of the current working set, captures WHY as a linked Rationale, and preserves the record as history (never deleted). Use supersede_decision instead when a replacement record exists. Pass the item's IRI as `iri` and the reason as `rationale`.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        description = "Assert an idempotent, ontology-declared object-property edge between two existing typed graph nodes. Use this when evidence supports the relationship; use `suggest_links` when candidate relationships are unknown, and `link_code` when attaching a record to source code. The predicate and SHACL domain/range are validated, and judgment predicates remain ratification-only. Writes the edge and returns the resolved subject–predicate–object relationship.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        // Judgment predicates are ratification-only: a bare playsRole /
        // hasCriticality edge would carry no proposal node (no confidence,
        // no ratification provenance) and would be invisible to the dossier
        // badges and the policy gate, which read judgment nodes.
        if matches!(predicate.as_str(), "playsRole" | "hasCriticality") {
            return Ok(tool_error(format!(
                "{predicate:?} is ratification-only: judgments are proposed by `moosedev classify` \
                 (or recategorized in the workbench) and materialize on human acceptance — never \
                 asserted directly"
            )));
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

    /// Link a typed knowledge record to a CodeEntity resolved from the substrate.
    #[tool(
        description = "Attach a typed knowledge record to the CodeEntity resolved by exactly one selector: `file` + 1-based `line` + 1-based UTF-8 byte `col`, or SCIP `symbol`. Use `constrains` for a Constraint and `concerns` for other entity-specific records unless another ontology predicate is intended. The call is idempotent, may lazily mint the CodeEntity, and auto-orients intent predicates such as `realizes`, `satisfies`, and `embodies`. Writes the graph and returns the entity, edge, and substrate-staleness status.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn link_code(
        &self,
        Parameters(args): Parameters<LinkCodeArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let record_iri = args.record_iri.trim().to_string();
        if record_iri.is_empty() {
            return Ok(tool_error("`record_iri` must not be empty"));
        }
        let predicate = args
            .predicate
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("concerns")
            .to_string();

        let file = args.file.map(|s| s.trim().to_string());
        let symbol = args.symbol.map(|s| s.trim().to_string());
        let position_supplied = file.is_some() || args.line.is_some() || args.col.is_some();
        let symbol_supplied = symbol.as_deref().is_some_and(|s| !s.is_empty());
        if symbol.as_deref().is_some_and(str::is_empty) {
            return Ok(tool_error("`symbol` must not be empty when provided"));
        }
        if position_supplied {
            let mut missing = Vec::new();
            if file.as_deref().is_none_or(str::is_empty) {
                missing.push("file");
            }
            if args.line.is_none() {
                missing.push("line");
            }
            if args.col.is_none() {
                missing.push("col");
            }
            if !missing.is_empty() {
                return Ok(tool_error(format!(
                    "position selector requires `file`, `line`, and `col`; missing {}",
                    missing.join(", ")
                )));
            }
        }
        if position_supplied && symbol_supplied {
            return Ok(tool_error(
                "pass exactly one selector: `file` + `line` + `col`, or `symbol`",
            ));
        }
        if !position_supplied && !symbol_supplied {
            return Ok(tool_error(
                "pass exactly one selector: `file` + `line` + `col`, or `symbol`",
            ));
        }

        let selector = if position_supplied {
            graph::CodeSelector::Position {
                file: file.expect("validated file"),
                line: args.line.expect("validated line"),
                col: args.col.expect("validated col"),
            }
        } else {
            graph::CodeSelector::Symbol(symbol.expect("validated symbol"))
        };
        let agent = resolve_author(&None, &context);

        match graph::link_code(&self.state, &record_iri, &predicate, &selector, &agent) {
            Ok(out) => {
                self.state.note_project_write();
                let stale_note = if out.substrate_stale {
                    "\nwarning: substrate is stale; positions may have drifted, re-run `moosedev index`."
                } else {
                    ""
                };
                Ok(tool_ok(format!(
                    "Linked CodeEntity \"{}\" ({}) [created: {}]\nedge: {} -{}-> {}{}",
                    out.entity_name,
                    out.entity_iri,
                    out.created,
                    out.subject_iri,
                    out.predicate_local,
                    out.object_iri,
                    stale_note
                )))
            }
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }

    /// Declare repo path coverage for a SystemComponent.
    #[tool(
        description = "Bootstrap/admin tool: add repo-relative path coverage to an existing SystemComponent so code entities can derive their owning component through `realizes`. A trailing `/` means directory prefix; otherwise the path is an exact file. This is an idempotent, add-only graph write; when coverage overlaps, the longest matching path wins. Returns paths added and paths already covered.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn declare_component_paths(
        &self,
        Parameters(args): Parameters<DeclareComponentPathsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let component = args.component.trim().to_string();
        if component.is_empty() {
            return Ok(tool_error("`component` must not be empty"));
        }
        if args.paths.is_empty() {
            return Ok(tool_error("`paths` must not be empty"));
        }

        match graph::declare_component_paths(&self.state, &component, &args.paths) {
            Ok(out) => {
                self.state.note_project_write();
                Ok(tool_ok(format!(
                    "Declared coverage for \"{}\" ({})\nadded: {}\nalready covered: {}",
                    out.component_name,
                    out.component_iri,
                    format_path_list(&out.added),
                    format_path_list(&out.already_covered)
                )))
            }
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }

    /// Get the recorded knowledge directly linked to a CodeEntity.
    #[tool(
        description = "Retrieve the recorded knowledge governing one code entity, including directly linked records and records inherited through its realized SystemComponent. Call this before editing a specific function, type, or module, and treat returned Constraints as hard rules. Pass exactly one selector: `file` + 1-based `line` + 1-based UTF-8 byte `col`, `symbol`, or CodeEntity `iri`. Read-only and entity-specific: a no-recorded-knowledge result does not replace project recall.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_entity_dossier(
        &self,
        Parameters(args): Parameters<GetEntityDossierArgs>,
    ) -> Result<CallToolResult, McpError> {
        let file = args.file.map(|s| s.trim().to_string());
        let symbol = args.symbol.map(|s| s.trim().to_string());
        let iri = args.iri.map(|s| s.trim().to_string());
        let position_supplied = file.is_some() || args.line.is_some() || args.col.is_some();
        let symbol_supplied = symbol.is_some();
        let iri_supplied = iri.is_some();

        if symbol.as_deref().is_some_and(str::is_empty) {
            return Ok(tool_error("`symbol` must not be empty when provided"));
        }
        if iri.as_deref().is_some_and(str::is_empty) {
            return Ok(tool_error("`iri` must not be empty when provided"));
        }
        if position_supplied {
            let mut missing = Vec::new();
            if file.as_deref().is_none_or(str::is_empty) {
                missing.push("file");
            }
            if args.line.is_none() {
                missing.push("line");
            }
            if args.col.is_none() {
                missing.push("col");
            }
            if !missing.is_empty() {
                return Ok(tool_error(format!(
                    "position selector requires `file`, `line`, and `col`; missing {}",
                    missing.join(", ")
                )));
            }
        }

        let selector_count = position_supplied as u8 + symbol_supplied as u8 + iri_supplied as u8;
        if selector_count != 1 {
            return Ok(tool_error(
                "pass exactly one selector: `file` + `line` + `col`, `symbol`, or `iri`",
            ));
        }

        let position_file = position_supplied.then(|| file.clone().expect("validated file"));
        let target = if position_supplied {
            graph::DossierTarget::Position {
                file: file.expect("validated file"),
                line: args.line.expect("validated line"),
                col: args.col.expect("validated col"),
            }
        } else if let Some(symbol) = symbol {
            graph::DossierTarget::Symbol(symbol)
        } else {
            graph::DossierTarget::Iri(iri.expect("validated iri"))
        };

        match graph::get_entity_dossier(&self.state, &target) {
            Ok(Some(dossier)) => Ok(tool_ok(graph::render_markdown(&dossier))),
            Ok(None) => {
                if let Some(file) = position_file {
                    let Some(substrate) = self.state.substrate() else {
                        return Ok(tool_ok(
                            "code substrate unavailable; run `moosedev index`; the backend will load it automatically when indexing completes, but records cannot be anchored here yet.",
                        ));
                    };
                    if !substrate.can_anchor(&file) {
                        return Ok(tool_ok(format!(
                            "`{file}` is not in the code substrate (indexed: {}); records cannot be anchored here yet.",
                            substrate.describe_coverage()
                        )));
                    }
                }
                Ok(tool_ok(
                    "No recorded knowledge is linked to this code; attach records with `link_code`.",
                ))
            }
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }

    /// Suggest typed links to add to the project knowledge graph (suggest-only).
    #[tool(
        description = "Return ranked, ontology-legal hypotheses for unasserted relationships between recorded items. Suggestions are candidates, not proven facts, and this tool writes nothing: review the candidate, then call `relate` to assert a chosen edge. Pass `iri` for one record, or omit it to scan shape-identified under-linked records. Returns proposed `relate` arguments and any missing-link gap notes.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Answer one short, focused natural-language question by planning walks across relationships in the project knowledge graph and synthesizing the resulting records. Use this when `get_relevant_context` recall is insufficient and the answer needs relationship reasoning; use `sparql` instead for exact, bulk, or deterministic structural reads. Read-only; returns a synthesized answer, confidence, and an auditable symbolic reasoning trace.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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

    /// Retrieve recorded project knowledge relevant to a topic or browse a
    /// bounded current inventory.
    #[tool(
        description = "Retrieve authoritative current project knowledge using fast hybrid topic matching plus bounded typed relationship expansion. Start here for recall and browsing: omit `topic` for a broad current inventory (default 10, maximum 100), or provide a topic for a shallow, paraphrase-tolerant probe. A topic-scoped empty result means no record cleared the relevance floor, not that the graph is empty. Use `query` for relationship synthesis and `sparql` for exact, complete structural reads. Read-only; non-current lifecycle states are excluded unless `include_history` is true.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Get the edit provenance (which agent recorded it, and when) for a knowledge item, by IRI.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Before recording a new knowledge term, align its label, optional definition, and surface forms to existing classes in the project's architecture ontology so the model does not drift. This is the canonical alignment tool for new callers. Read-only; returns the resolved parent class with the deciding sensor and rationale, or ranked candidates when ambiguous.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Compatibility alias for `align_concepts` using the same alignment engine. New callers should use `align_concepts`, which also accepts surface labels. Read-only; returns the resolved ontology class and rationale, or ranked candidates when ambiguous.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Run an exact, read-only SPARQL query over the local knowledge store. Use this for deterministic structural reads, exhaustive listings, deduplication checks, and explicit named-graph selection; use `get_relevant_context` for fast topical recall and `query` for relationship synthesis. The default graph is the union of named graphs unless `FROM` is specified. SELECT/ASK return SPARQL JSON; CONSTRUCT/DESCRIBE return N-Triples. Key graphs: project https://moosedev.dev/kg/project, provenance https://moosedev.dev/kg/provenance, shapes https://moosedev.dev/kg/ontology/software-architecture/shapes.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        description = "Serialize the project knowledge graph to RDF text for BACKUP, version control, or transfer — this is NOT a read or search path. To read, search, or traverse knowledge — to answer a question, render a document, or inspect records — use `sparql`, `get_relevant_context`, or `query` instead; do NOT dump the whole graph into context (that defeats the point of structured, queryable memory). Defaults to canonical N-Quads for the project graph. N-Triples is deterministic after graph names are dropped; Turtle is human-readable, not byte-canonical. Optional args: format nq|nt|ttl, graph project|provenance|all, path for writing to a file instead of returning inline text (absolute paths recommended).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
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
        description = "Check the durable project knowledge graph against the loaded architecture SHACL shapes. Run this after graph writes or lifecycle changes to catch structural and ontology violations. Read-only and symbolic; validates recorded knowledge, not source code, and returns a deterministic validation report.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
        info.instructions = Some(SERVER_INSTRUCTIONS.to_string());
        info
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_when, MooseDevServer, SERVER_INSTRUCTIONS};
    use rmcp::model::Tool;
    use serde_json::Value;

    const TOOL_NAMES: [&str; 20] = [
        "align_concepts",
        "capture_decision_point",
        "declare_component_paths",
        "evaluate_policy",
        "export_graph",
        "get_entity_dossier",
        "get_provenance",
        "get_relevant_context",
        "link_code",
        "pending_ratifications",
        "ping",
        "query",
        "record_important_decision",
        "relate",
        "retract_decision",
        "sparql",
        "suggest_links",
        "suggest_mappings",
        "supersede_decision",
        "validate_against_architecture",
    ];

    fn tool_catalog() -> Vec<Tool> {
        MooseDevServer::tool_router().list_all()
    }

    fn tool<'a>(catalog: &'a [Tool], name: &str) -> &'a Tool {
        catalog
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
    }

    fn schema(tool: &Tool) -> Value {
        Value::Object((*tool.input_schema).clone())
    }

    fn description<'a>(catalog: &'a [Tool], name: &str) -> &'a str {
        tool(catalog, name)
            .description
            .as_deref()
            .unwrap_or_else(|| panic!("missing description for {name}"))
    }

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
    fn mcp_tool_catalog_has_complete_descriptions_and_annotations() {
        let catalog = tool_catalog();
        let mut names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, TOOL_NAMES);

        for tool in &catalog {
            assert!(
                tool.description
                    .as_deref()
                    .is_some_and(|description| !description.trim().is_empty()),
                "{} needs a non-empty operational description",
                tool.name
            );
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} needs MCP annotations", tool.name));
            assert_eq!(
                annotations.open_world_hint,
                Some(false),
                "{} operates on the closed project-memory domain",
                tool.name
            );

            for (property, property_schema) in tool.input_schema["properties"]
                .as_object()
                .into_iter()
                .flatten()
            {
                assert!(
                    property_schema["description"]
                        .as_str()
                        .is_some_and(|description| !description.trim().is_empty()),
                    "{}.{} needs a non-empty schema description",
                    tool.name,
                    property
                );
            }
        }
    }

    #[test]
    fn mcp_tool_annotations_match_effects() {
        let catalog = tool_catalog();
        let read_only = [
            "align_concepts",
            "get_entity_dossier",
            "get_provenance",
            "get_relevant_context",
            "pending_ratifications",
            "ping",
            "query",
            "sparql",
            "suggest_links",
            "suggest_mappings",
            "validate_against_architecture",
        ];
        let additive_non_idempotent = [
            "capture_decision_point",
            "evaluate_policy",
            "record_important_decision",
        ];
        let additive_idempotent = ["declare_component_paths", "link_code", "relate"];
        let destructive = ["export_graph", "retract_decision", "supersede_decision"];

        for name in read_only {
            let annotations = tool(&catalog, name).annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(true), "{name}");
            assert_eq!(annotations.destructive_hint, Some(false), "{name}");
            assert_eq!(annotations.idempotent_hint, Some(true), "{name}");
        }
        for name in additive_non_idempotent {
            let annotations = tool(&catalog, name).annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(false), "{name}");
            assert_eq!(annotations.destructive_hint, Some(false), "{name}");
            assert_eq!(annotations.idempotent_hint, Some(false), "{name}");
        }
        for name in additive_idempotent {
            let annotations = tool(&catalog, name).annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(false), "{name}");
            assert_eq!(annotations.destructive_hint, Some(false), "{name}");
            assert_eq!(annotations.idempotent_hint, Some(true), "{name}");
        }
        for name in destructive {
            let annotations = tool(&catalog, name).annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(false), "{name}");
            assert_eq!(annotations.destructive_hint, Some(true), "{name}");
            assert_eq!(annotations.idempotent_hint, Some(false), "{name}");
        }
    }

    #[test]
    fn mcp_descriptions_encode_routing_boundaries() {
        let catalog = tool_catalog();
        for name in ["get_relevant_context", "query", "sparql"] {
            let description = description(&catalog, name);
            for peer in ["get_relevant_context", "query", "sparql"] {
                if peer != name {
                    assert!(
                        description.contains(peer),
                        "{name} must route against {peer}"
                    );
                }
            }
        }

        assert!(description(&catalog, "query").contains("focused"));
        assert!(
            description(&catalog, "record_important_decision").contains("capture_decision_point")
        );
        assert!(
            description(&catalog, "capture_decision_point").contains("record_important_decision")
        );
        assert!(description(&catalog, "supersede_decision").contains("retract_decision"));
        assert!(description(&catalog, "get_entity_dossier").contains("before editing"));
        assert!(description(&catalog, "suggest_links").contains("not proven facts"));
        assert!(description(&catalog, "suggest_mappings").contains("Compatibility alias"));
        assert!(description(&catalog, "evaluate_policy").contains("Host-adapter"));
        assert!(!description(&catalog, "evaluate_policy").contains("warn"));
        assert!(description(&catalog, "capture_decision_point").contains("queued-link count"));
        assert!(description(&catalog, "get_relevant_context").contains("maximum 100"));
        assert!(SERVER_INSTRUCTIONS.contains("limit` up to 100"));

        for required in [
            "get_relevant_context",
            "query",
            "sparql",
            "get_entity_dossier",
            "align_concepts",
            "validate_against_architecture",
        ] {
            assert!(
                SERVER_INSTRUCTIONS.contains(required),
                "server instructions must mention {required}"
            );
        }
    }

    #[test]
    fn mcp_input_schema_exposes_operational_constraints() {
        let catalog = tool_catalog();

        let policy = schema(tool(&catalog, "evaluate_policy"));
        assert_eq!(
            policy["properties"]["event"]["enum"],
            serde_json::json!(["entity_touched", "edit_proposed", "decision_point"])
        );
        for field in ["line", "col"] {
            assert_eq!(policy["properties"][field]["minimum"], 1);
        }

        for name in ["link_code", "get_entity_dossier"] {
            let schema = schema(tool(&catalog, name));
            for field in ["line", "col"] {
                assert_eq!(schema["properties"][field]["minimum"], 1, "{name}.{field}");
            }
        }

        let paths = schema(tool(&catalog, "declare_component_paths"));
        assert_eq!(paths["properties"]["paths"]["minItems"], 1);
        assert_eq!(paths["properties"]["paths"]["items"]["minLength"], 1);

        let record = schema(tool(&catalog, "record_important_decision"));
        let relation_items = &record["properties"]["relations"]["items"];
        assert!(
            relation_items.get("$ref").is_none(),
            "relations must be inline"
        );
        assert_eq!(
            relation_items["required"],
            serde_json::json!(["predicate", "target"])
        );
        assert_eq!(relation_items["properties"]["predicate"]["type"], "string");
        assert_eq!(relation_items["properties"]["target"]["type"], "string");

        let query = schema(tool(&catalog, "query"));
        assert!(query["properties"]["question"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("focused")));
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
