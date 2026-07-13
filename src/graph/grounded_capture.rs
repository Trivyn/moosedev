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

use crate::provenance;

use super::capture::{record_instance_with_relation_args, RecordInput};
use super::proposals::propose_link;
use super::state::AppState;

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
    let mut description = String::from(
        "Grounded capture at a decision point (proposed; ratify or reject in the workbench inbox).",
    );
    if !files.is_empty() {
        description.push_str(&format!("\n\nFiles changed: {}.", files.join(", ")));
    }
    if let Some(summary) = summary {
        description.push_str(&format!("\n\nHost summary: {summary}"));
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
        tracing::warn!("grounded capture: provenance stamp failed for {}: {e}", outcome.iri);
    }

    // Queue entity links as proposals — never real edges (D1 interlock).
    let mut proposed_links = Vec::new();
    let mut unanchored = Vec::new();
    let substrate = state.substrate();

    for file in &files {
        let module = substrate
            .as_ref()
            .and_then(|s| {
                s.definitions_in_file(file)
                    .into_iter()
                    .find(|d| d.entry.is_module)
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
