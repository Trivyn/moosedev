//! Grounded capture at decision points (v2.2 CAPTURE verb, AD `145af7e9`).
//!
//! Extracts a **proposed** `ArchitecturalDecision` from what actually happened —
//! the host's own summary text plus the diff-derived changed-file list — never
//! from interrogating an LLM into justifying itself. The record is minted at
//! lifecycle status `proposed` with authorship provenance and is never
//! auto-accepted; a human ratifies or rejects it in the inbox.
//!
//! Entity links are queued as `ProposedLink`s rather than written as real
//! edges: one `concerns` proposal per changed file (anchored at the file's
//! module — the granularity the file-level evidence honestly supports), plus
//! one per caller-identified entity symbol. So nothing a capture writes can
//! reach a dossier or move the why-coverage numerator before ratification —
//! the same D1 interlock the v2.1 queue established.
//!
//! `isMotivatedBy` is link-to-existing only: it is asserted when the caller
//! names a Requirement that resolves, and omitted otherwise — never invented.

use chrono::{DateTime, Utc};
use oxigraph::model::{GraphNameRef, NamedNodeRef, Term};

use crate::provenance;

use super::capture::{record_instance_with_relation_args, require_information_record, RecordInput};
use super::context::first_literal;
use super::lifecycle::in_working_set;
use super::proposals::propose_link;
use super::state::AppState;
use super::PROJECT_KG_GRAPH_IRI;

/// What one decision-point capture wrote — reported in full, no silent drops.
#[derive(Debug, Clone)]
pub struct GroundedCapture {
    /// IRI of the minted `proposed` ArchitecturalDecision.
    pub record_iri: String,
    pub title: String,
    /// `ProposedLink` IRIs queued for ratification.
    pub proposed_links: Vec<String>,
    /// Changed files (or `symbol:` selectors) that could not be anchored in
    /// the substrate — reported so the caller can say so, never dropped.
    pub unanchored: Vec<String>,
}

/// Find the newest authoritative typed record written by `author` after
/// `since`. Automatic grounded captures are always `proposed`, so the
/// working-set predicate is the symbolic discriminator: a deliberate capture
/// path already produced usable knowledge and the safety net should abstain.
///
/// Malformed legacy timestamps are ignored rather than turning a best-effort
/// safety net into a write outage. Ties are broken by IRI for deterministic
/// behavior across store iteration orders.
pub fn working_record_authored_since(
    state: &AppState,
    author: &str,
    since: DateTime<Utc>,
) -> anyhow::Result<Option<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let timestamp = NamedNodeRef::new(&state.capture.timestamp)?;
    let mut newest: Option<(DateTime<Utc>, String)> = None;

    for quad in state
        .store
        .quads_for_pattern(
            None,
            Some(timestamp),
            None,
            Some(GraphNameRef::NamedNode(graph)),
        )
        .flatten()
    {
        let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        let Term::Literal(literal) = quad.object else {
            continue;
        };
        let Ok(when) = DateTime::parse_from_rfc3339(literal.value()) else {
            continue;
        };
        let when = when.with_timezone(&Utc);
        if when <= since || require_information_record(state, &subject).is_err() {
            continue;
        }
        if first_literal(&state.store, subject.as_str(), &state.capture.author).as_deref()
            != Some(author)
        {
            continue;
        }
        let status = first_literal(&state.store, subject.as_str(), &state.capture.status)
            .unwrap_or_default();
        if !in_working_set(&status) {
            continue;
        }

        let candidate = (when, subject.as_str().to_string());
        if newest.as_ref().is_none_or(|current| candidate > *current) {
            newest = Some(candidate);
        }
    }

    Ok(newest.map(|(_, iri)| iri))
}

/// Capture one decision point as a `proposed` record plus queued links.
///
/// * `files` — repo-relative changed paths (diff-derived by the adapter).
/// * `summary` — the host's own summary text, used verbatim as the title seed.
/// * `requirement` — optional existing Requirement (IRI or exact title) for
///   `isMotivatedBy`; an unresolvable value fails the whole capture honestly.
/// * `entities` — optional substrate symbols the caller attributes the
///   decision to, each queued as its own `concerns` proposal.
#[allow(clippy::too_many_arguments)]
pub fn capture_decision_point(
    state: &AppState,
    files: &[String],
    summary: Option<&str>,
    requirement: Option<&str>,
    entities: &[String],
    author: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<GroundedCapture> {
    let files: Vec<&str> = files
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .collect();
    let summary = summary.map(str::trim).filter(|s| !s.is_empty());
    if files.is_empty() && summary.is_none() {
        anyhow::bail!(
            "a decision point needs at least one changed file or a host summary to ground the capture"
        );
    }

    let title = match summary {
        Some(summary) => cap_title(summary),
        None => cap_title(&format!("Decision point: {}", files.join(", "))),
    };
    // The claim leads: the host's summary IS the decision content a ratifier
    // judges; provenance boilerplate and the file list are supporting detail.
    let mut description = String::new();
    if let Some(summary) = summary {
        description.push_str(summary);
        description.push_str("\n\n");
    }
    description.push_str(
        "Grounded capture at a decision point (proposed; ratify or reject in the workbench inbox).",
    );
    if !files.is_empty() {
        description.push_str(&format!("\n\nFiles changed: {}.", files.join(", ")));
    }

    let relations: Vec<(String, String)> = requirement
        .map(|r| vec![("isMotivatedBy".to_string(), r.to_string())])
        .unwrap_or_default();

    let input = RecordInput {
        class_iri: state.resolve_class("ArchitecturalDecision")?,
        class_local: "ArchitecturalDecision".to_string(),
        properties: vec![
            (moose::RDFS_LABEL.to_string(), title.clone()),
            (state.capture.title.clone(), title.clone()),
            (state.capture.description.clone(), description),
            (state.capture.status.clone(), "proposed".to_string()),
        ],
    };
    let outcome = record_instance_with_relation_args(state, &input, &relations, author, when)?;

    if let Err(e) = provenance::record_provenance(&state.store, &outcome.iri, author) {
        tracing::warn!(
            "grounded capture: provenance stamp failed for {}: {e}",
            outcome.iri
        );
    }

    // Queue entity links as proposals — never real edges (D1 interlock).
    let mut proposed_links = Vec::new();
    let mut unanchored = Vec::new();
    let substrate = state.substrate();

    for file in &files {
        // Anchor at the file's OUTERMOST module (shortest symbol path) — not
        // an inner `mod tests` that happens to appear first in file order.
        // `tests` modules are never anchor candidates at all: in a `mod.rs`
        // whose own module is declared in the parent file, `tests` can be the
        // only in-file module, and "this change concerns lsp::tests" is a
        // misleading card. No candidate → the file reports as unanchored.
        let module = substrate.as_ref().and_then(|s| {
            s.definitions_in_file(file)
                .into_iter()
                .filter(|d| d.entry.is_module)
                .filter(|d| d.entry.display_name.as_deref() != Some("tests"))
                .min_by_key(|d| d.entry.symbol.matches('/').count())
        });
        match module {
            Some(def) => proposed_links.push(propose_link(
                state,
                &outcome.iri,
                "concerns",
                &def.entry.symbol,
                file,
                "file changed at this decision point",
                author,
                when,
            )?),
            None => unanchored.push(file.to_string()),
        }
    }

    for symbol in entities.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let definition = substrate.as_ref().and_then(|s| {
            s.definitions()
                .into_iter()
                .find(|d| d.symbol == symbol || d.normalized_symbol == symbol)
        });
        match definition {
            Some(def) => proposed_links.push(propose_link(
                state,
                &outcome.iri,
                "concerns",
                &def.symbol,
                &def.file,
                "entity identified by the host at this decision point",
                author,
                when,
            )?),
            None => unanchored.push(format!("symbol:{symbol}")),
        }
    }

    state.note_project_write();
    Ok(GroundedCapture {
        record_iri: outcome.iri,
        title,
        proposed_links,
        unanchored,
    })
}

/// Titles are names, not claims — keep the host's words but cap the length.
fn cap_title(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= 100 {
        return text.to_string();
    }
    let capped: String = text.chars().take(97).collect();
    format!("{}…", capped.trim_end())
}
