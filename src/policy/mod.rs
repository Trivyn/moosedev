//! Host-independent active-agency policy engine (v2.2, spec §4.1).
//!
//! One engine in the daemon, reading only the graph + substrate through
//! [`AppState`]. Input: a typed [`PolicyEvent`] (entity touched, edit proposed,
//! decision point) reported by a host adapter. Output: a typed
//! [`PolicyDecision`] the adapter *enacts* — adapters contain zero policy
//! (Consequence `0c447d67`); they translate the verdict into their host's
//! contract and nothing else. The decision carries the full typed outcome so
//! each host can degrade gracefully (gate where blocking exists, warn-and-inject
//! where only observation exists).
//!
//! v2.2 gates on the Constraints already linked in the graph
//! (`constrains`/`violates` + lifecycle status). The judgment stratum
//! (role/criticality) slots in additively in a later slice — this module reads
//! records generically and needs no rework to consume them (AD `7dd7d3c6`).

pub mod fires;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::{
    direct_records_for_entity, entities_by_symbol, first_literal, get_entity_dossier, local_name,
    render_markdown, resolve_target_entity, AppState, CodeTerms, DossierTarget, RecordSummary,
};

use fires::{append_fire, FireEvent};

/// One host event reported by an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyEvent {
    /// The agent touched (read/navigated) code — the PUSH verb's input.
    /// Positions are 1-based when present; without one, the whole file's
    /// knowledge-bearing entities are pushed.
    EntityTouched {
        file: String,
        #[serde(default)]
        line: Option<u32>,
        #[serde(default)]
        col: Option<u32>,
    },
    /// The agent proposed an edit — the GATE verb's input. Hosts rarely have a
    /// cursor at hook time: `anchor` carries the edit's own text (e.g. the
    /// `old_string` of an Edit call) and is located in the on-disk file to find
    /// the definitions it overlaps; with neither position nor anchor, every
    /// definition in the file is a gate candidate (conservative).
    EditProposed {
        file: String,
        #[serde(default)]
        line: Option<u32>,
        #[serde(default)]
        col: Option<u32>,
        #[serde(default)]
        anchor: Option<String>,
    },
    /// The session reached an explicit decision point (session end, checkpoint)
    /// — the CAPTURE verb's input. Returns a [`CaptureSpec`]; the actual write
    /// is a separate, deliberate call so evaluation stays read-only.
    DecisionPoint {
        files: Vec<String>,
        #[serde(default)]
        summary: Option<String>,
    },
}

/// How strongly a gate blocks. `RequirePlan` is reserved: v2.2 has no
/// host-side "plan present" signal, so it is never emitted yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDisposition {
    Deny,
    RequirePlan,
    RequireRatification,
}

/// A knowledge record cited by a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    pub iri: String,
    pub kind: String,
    pub title: String,
    /// Workbench deep link, when the HTTP UI has published an address; hosts
    /// render it as the diagnostic's code link.
    pub workbench_url: Option<String>,
}

/// One host-independent diagnostic (the WARN verb). Produced by the shared
/// file-diagnostics path; severity vocabulary matches the LSP discipline
/// (never above Information).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDiagnostic {
    /// `information` or `hint` — the only severities policy emits (spec §5.4).
    pub severity: String,
    pub message: String,
    pub record: RecordRef,
    /// Normalized substrate symbol of the entity the diagnostic anchors to.
    pub symbol: String,
}

/// What a `DecisionPoint` asks the capture path to extract. Deliberately
/// structural: the files touched and the host's own summary text — never an
/// interrogated LLM justification (AD `145af7e9`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSpec {
    pub files: Vec<String>,
    pub summary: Option<String>,
}

/// The typed verdict an adapter enacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Nothing to do — silence is the default (no fire is logged).
    Allow,
    /// PUSH: inject this dossier markdown. The bytes are produced by the same
    /// `get_entity_dossier` + `render_markdown` path hover uses, so push and
    /// hover show identical content by construction.
    Inject {
        dossier_markdown: String,
        entities: Vec<String>,
        records: Vec<RecordRef>,
    },
    /// WARN: host-independent diagnostics for hosts with an annotation surface.
    Warn { diagnostics: Vec<PolicyDiagnostic> },
    /// GATE: block (or escalate) the proposed edit, citing the records that
    /// justify it and the entities they govern.
    Gate {
        disposition: GateDisposition,
        reason: String,
        records: Vec<RecordRef>,
        entities: Vec<String>,
    },
    /// CAPTURE: extract a proposed record from this decision point.
    CaptureTrigger { spec: CaptureSpec },
}

/// Evaluate one host event against the graph. Pure read: no writes, no
/// telemetry. `repo_root` is the working tree the substrate was indexed from
/// (used only to locate an `anchor` inside a file's current on-disk content).
pub fn evaluate(
    state: &AppState,
    repo_root: &Path,
    event: &PolicyEvent,
) -> anyhow::Result<PolicyDecision> {
    match event {
        PolicyEvent::EntityTouched { file, line, col } => push_decision(state, file, *line, *col),
        PolicyEvent::EditProposed {
            file,
            line,
            col,
            anchor,
        } => gate_decision(state, repo_root, file, *line, *col, anchor.as_deref()),
        PolicyEvent::DecisionPoint { files, summary } => Ok(PolicyDecision::CaptureTrigger {
            spec: CaptureSpec {
                files: files.clone(),
                summary: summary.clone(),
            },
        }),
    }
}

/// [`evaluate`], plus best-effort fire telemetry for decisions that acted
/// (push-fire, gate-fire). `Allow` and `CaptureTrigger` log nothing here —
/// capture fires when the proposed record is actually written.
pub fn evaluate_and_fire(
    state: &AppState,
    repo_root: &Path,
    event: &PolicyEvent,
    host: &str,
) -> anyhow::Result<PolicyDecision> {
    let decision = evaluate(state, repo_root, event)?;
    let fire = match &decision {
        PolicyDecision::Inject {
            entities, records, ..
        } => Some(("push", "inject", entities, records)),
        PolicyDecision::Gate {
            disposition,
            records,
            entities,
            ..
        } => {
            let enacted = match disposition {
                GateDisposition::Deny => "deny",
                GateDisposition::RequirePlan => "require_plan",
                GateDisposition::RequireRatification => "require_ratification",
            };
            Some(("gate", enacted, entities, records))
        }
        PolicyDecision::Allow
        | PolicyDecision::Warn { .. }
        | PolicyDecision::CaptureTrigger { .. } => None,
    };
    if let Some((verb, enacted, entities, records)) = fire {
        append_fire(
            &state.data_dir,
            &FireEvent {
                ts: chrono::Utc::now().to_rfc3339(),
                verb,
                host: host.to_string(),
                entity: entities.first().cloned(),
                decision: enacted.to_string(),
                records_cited: records.iter().map(|r| r.iri.clone()).collect(),
                summary: None,
                files: Vec::new(),
            },
        );
    }
    Ok(decision)
}

// ---------------------------------------------------------------------------
// PUSH
// ---------------------------------------------------------------------------

/// Entity-exact dossier on touch. With a position: the one entity under it.
/// Without: every knowledge-bearing entity defined in the file. Silence when
/// nothing is linked.
fn push_decision(
    state: &AppState,
    file: &str,
    line: Option<u32>,
    col: Option<u32>,
) -> anyhow::Result<PolicyDecision> {
    let targets: Vec<DossierTarget> = match (line, col) {
        (Some(line), Some(col)) => vec![DossierTarget::Position {
            file: file.to_string(),
            line,
            col,
        }],
        _ => file_entity_iris(state, file)?
            .into_iter()
            .map(DossierTarget::Iri)
            .collect(),
    };

    let mut sections = Vec::new();
    let mut entities = Vec::new();
    let mut records = Vec::new();
    for target in &targets {
        let Some(dossier) = get_entity_dossier(state, target)? else {
            continue;
        };
        sections.push(render_markdown(&dossier));
        for record in &dossier.direct_records {
            records.push(record_ref(record));
        }
        entities.push(dossier.entity_iri);
    }
    if sections.is_empty() {
        return Ok(PolicyDecision::Allow);
    }
    Ok(PolicyDecision::Inject {
        dossier_markdown: sections.join("\n"),
        entities,
        records,
    })
}

// ---------------------------------------------------------------------------
// GATE
// ---------------------------------------------------------------------------

/// One entity considered by the gate, with a human name for the reason text.
struct GateCandidate {
    iri: String,
    name: String,
}

/// Gate a proposed edit on the Constraints linked to the entities it touches
/// and their ratified criticality judgments.
///
/// Semantics (AD `4d57fbad`, extended by the judgment stratum):
/// - an `accepted` record linked by `violates` → `Deny` (the graph asserts an
///   actual violation);
/// - an `accepted` Constraint linked by `constrains` → `RequireRatification`
///   (a ratified contract governs the entity; presence of a contract is not
///   proof the edit breaks it, so escalate to a human instead of hard-denying);
/// - a RATIFIED criticality-high judgment → `RequireRatification` citing the
///   judgment (spec §4.2: differential process on critical entities). Proposed
///   judgments can never gate — no edge exists until a human accepts.
/// - otherwise → `Allow`.
fn gate_decision(
    state: &AppState,
    repo_root: &Path,
    file: &str,
    line: Option<u32>,
    col: Option<u32>,
    anchor: Option<&str>,
) -> anyhow::Result<PolicyDecision> {
    let candidates = gate_candidates(state, repo_root, file, line, col, anchor)?;
    let judgments = crate::graph::judgments_by_subject(state)?;

    // (entity iri, entity name, cited record) per gating link found.
    let mut deny: Vec<(String, String, RecordRef)> = Vec::new();
    let mut ratify: Vec<(String, String, RecordRef)> = Vec::new();
    for candidate in &candidates {
        for record in direct_records_for_entity(state, &candidate.iri)? {
            if !record.status.eq_ignore_ascii_case("accepted") {
                continue;
            }
            match record.predicate_local.as_str() {
                "violates" => deny.push((
                    candidate.iri.clone(),
                    candidate.name.clone(),
                    record_ref(&record),
                )),
                "constrains" if record.kind == "Constraint" => ratify.push((
                    candidate.iri.clone(),
                    candidate.name.clone(),
                    record_ref(&record),
                )),
                _ => {}
            }
        }
        // Ratified criticality-high gates; the accepted judgment node is the
        // citation (it carries the ratification provenance).
        if let Some(entity_judgments) = judgments.get(&candidate.iri) {
            for judgment in entity_judgments {
                if judgment.status == "accepted"
                    && judgment.predicate_local == "hasCriticality"
                    && judgment.target_local == "high"
                {
                    ratify.push((
                        candidate.iri.clone(),
                        candidate.name.clone(),
                        RecordRef {
                            iri: judgment.proposal_iri.clone(),
                            kind: "Judgment".to_string(),
                            title: "criticality: high (ratified judgment)".to_string(),
                            workbench_url: None,
                        },
                    ));
                }
            }
        }
    }

    if !deny.is_empty() {
        return Ok(gate(
            GateDisposition::Deny,
            format!(
                "recorded constraint violation: {}. Resolve the violation before editing.",
                cite(&deny)
            ),
            deny,
        ));
    }
    if !ratify.is_empty() {
        return Ok(gate(
            GateDisposition::RequireRatification,
            format!(
                "a ratified contract governs this code: {}. Confirm the edit respects it before proceeding.",
                cite(&ratify)
            ),
            ratify,
        ));
    }
    Ok(PolicyDecision::Allow)
}

/// Assemble a `Gate` decision from the collected (entity, name, record) hits.
fn gate(
    disposition: GateDisposition,
    reason: String,
    hits: Vec<(String, String, RecordRef)>,
) -> PolicyDecision {
    let mut entities: Vec<String> = hits.iter().map(|(iri, _, _)| iri.clone()).collect();
    entities.dedup();
    let mut records: Vec<RecordRef> = hits.into_iter().map(|(_, _, r)| r).collect();
    records.dedup();
    PolicyDecision::Gate {
        disposition,
        reason,
        records,
        entities,
    }
}

/// `entity is governed by "title"` citations, deduplicated, for reason text.
fn cite(items: &[(String, String, RecordRef)]) -> String {
    let mut parts: Vec<String> = items
        .iter()
        .map(|(_, name, record)| format!("`{name}` \u{2192} \"{}\"", record.title))
        .collect();
    parts.sort();
    parts.dedup();
    parts.join("; ")
}

/// Resolve the entities a proposed edit could touch, most precise first:
/// an explicit 1-based position; then an `anchor` snippet located in the
/// on-disk file (all definitions overlapping its line span); then every
/// minted definition in the file.
fn gate_candidates(
    state: &AppState,
    repo_root: &Path,
    file: &str,
    line: Option<u32>,
    col: Option<u32>,
    anchor: Option<&str>,
) -> anyhow::Result<Vec<GateCandidate>> {
    let terms = CodeTerms::resolve(state)?;

    if let (Some(line), Some(col)) = (line, col) {
        let target = DossierTarget::Position {
            file: file.to_string(),
            line,
            col,
        };
        let Some(iri) = resolve_target_entity(state, &terms, &target)? else {
            return Ok(Vec::new());
        };
        let name = entity_display_name(state, &iri);
        return Ok(vec![GateCandidate { iri, name }]);
    }

    let Some(substrate) = state.substrate() else {
        return Ok(Vec::new());
    };
    let entities = entities_by_symbol(state, &terms)?;
    let mut defs = substrate.definitions_in_file(file);

    if let Some(anchor) = anchor {
        if let Some((start_line, end_line)) = anchor_line_span(repo_root, file, anchor) {
            let overlapping: Vec<_> = defs
                .iter()
                .filter(|d| d.range.start.line <= end_line && d.range.end.line >= start_line)
                .cloned()
                .collect();
            // An anchor that misses every definition (comment, whitespace,
            // import block) falls back to the conservative whole-file scan.
            if !overlapping.is_empty() {
                defs = overlapping;
            }
        }
    }

    Ok(defs
        .into_iter()
        .filter_map(|def| {
            let iri = entities.get(&def.entry.normalized_symbol)?.clone();
            let name = def
                .entry
                .display_name
                .unwrap_or_else(|| def.entry.normalized_symbol.clone());
            Some(GateCandidate { iri, name })
        })
        .collect())
}

/// Zero-based line span of the first occurrence of `anchor` in the on-disk
/// file, or `None` when the file is unreadable or the anchor is absent.
fn anchor_line_span(repo_root: &Path, file: &str, anchor: &str) -> Option<(u32, u32)> {
    let content = std::fs::read_to_string(repo_root.join(file)).ok()?;
    let offset = content.find(anchor)?;
    let start_line = content[..offset].bytes().filter(|b| *b == b'\n').count() as u32;
    let anchor_lines = anchor.bytes().filter(|b| *b == b'\n').count() as u32;
    Some((start_line, start_line + anchor_lines))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Minted CodeEntity IRIs for every definition in one file.
fn file_entity_iris(state: &AppState, file: &str) -> anyhow::Result<Vec<String>> {
    let Some(substrate) = state.substrate() else {
        return Ok(Vec::new());
    };
    let terms = CodeTerms::resolve(state)?;
    let entities = entities_by_symbol(state, &terms)?;
    Ok(substrate
        .definitions_in_file(file)
        .into_iter()
        .filter_map(|def| entities.get(&def.entry.normalized_symbol).cloned())
        .collect())
}

fn entity_display_name(state: &AppState, iri: &str) -> String {
    first_literal(&state.store, iri, moose::RDFS_LABEL)
        .unwrap_or_else(|| local_name(iri).to_string())
}

fn record_ref(record: &RecordSummary) -> RecordRef {
    RecordRef {
        iri: record.iri.clone(),
        kind: record.kind.clone(),
        title: record.title.clone(),
        workbench_url: record.workbench_url.clone(),
    }
}

// ---------------------------------------------------------------------------
// WARN
// ---------------------------------------------------------------------------

/// Which WARN families to shape (mirrors the LSP init-option toggles).
#[derive(Debug, Clone, Copy)]
pub struct WarnConfig {
    pub constraints: bool,
    pub stale_rationale: bool,
}

/// Shape the host-independent WARN diagnostics for one entity's linked records.
///
/// Severity discipline (spec §5.4): policy emits nothing above `information` —
/// constraint proximity is `information`, stale rationale is `hint`. The caller
/// supplies the file's last-commit time for the staleness comparison and maps
/// `symbol` to a host-specific range.
pub fn entity_diagnostics(
    records: &[RecordSummary],
    file_commit: Option<chrono::DateTime<chrono::FixedOffset>>,
    config: &WarnConfig,
    symbol: &str,
) -> Vec<PolicyDiagnostic> {
    let mut out = Vec::new();
    if config.constraints {
        for record in records.iter().filter(|record| record.kind == "Constraint") {
            let message = match &record.description {
                Some(description) => format!(
                    "constrained by \"{}\": {}",
                    record.title,
                    description_claim(description)
                ),
                None => format!(
                    "constrained by \"{}\" ({})",
                    record.title,
                    short_record_id(&record.iri)
                ),
            };
            out.push(PolicyDiagnostic {
                severity: "information".to_string(),
                message,
                record: record_ref(record),
                symbol: symbol.to_string(),
            });
        }
    }
    if config.stale_rationale {
        if let Some(record) = stale_rationale_record(records, file_commit) {
            out.push(PolicyDiagnostic {
                severity: "hint".to_string(),
                message: "rationale predates later changes to this file".to_string(),
                record: record_ref(record),
                symbol: symbol.to_string(),
            });
        }
    }
    out
}

/// The newest linked record, when it predates the file's last commit — the
/// stale-rationale signal.
fn stale_rationale_record(
    records: &[RecordSummary],
    file_commit: Option<chrono::DateTime<chrono::FixedOffset>>,
) -> Option<&RecordSummary> {
    let record = newest_record(records)?;
    let instant = chrono::DateTime::parse_from_rfc3339(&record.timestamp).ok()?;
    (instant < file_commit?).then_some(record)
}

fn newest_record(records: &[RecordSummary]) -> Option<&RecordSummary> {
    records
        .iter()
        .filter_map(|record| {
            chrono::DateTime::parse_from_rfc3339(&record.timestamp)
                .ok()
                .map(|instant| (instant, record))
        })
        .max_by_key(|(instant, _)| *instant)
        .map(|(_, record)| record)
}

fn description_claim(description: &str) -> String {
    // First sentence or 140 chars, whichever ends first — a late first period
    // must not produce a paragraph-length diagnostic message.
    let capped = description.chars().take(140).collect::<String>();
    match capped.find('.') {
        Some(end) => capped[..=end].trim().to_string(),
        None => capped.trim().to_string(),
    }
}

/// Short display id for a record IRI, shown as a diagnostic code.
pub fn short_record_id(iri: &str) -> String {
    iri.rsplit(['/', '#'])
        .next()
        .unwrap_or(iri)
        .chars()
        .take(8)
        .collect()
}
