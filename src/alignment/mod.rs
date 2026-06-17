//! Concept alignment — map a new concept to an existing class in the project's
//! architecture ontology via MOOSE's three-tier sensor sieve (L1 keyword →
//! L2 embedding → L3 LLM). v1 runs L1 + L2 only (symbolic-first, invariant #1);
//! the LLM tier is off, so alignment is deterministic and offline.

use moose::alignment::{
    suggest_parent, AlignmentConfig, AlignmentOutcome, CategoryMappings, LeafClassInput,
};

use crate::graph::AppState;

/// Align a new concept (label + optional definition) under the best-matching
/// class in the architecture vocabulary. Returns MOOSE's full outcome for the
/// caller to format. Errors if the vector store hasn't been built.
pub async fn align_concept(
    state: &AppState,
    label: &str,
    definition: Option<&str>,
    surface_labels: Vec<String>,
) -> anyhow::Result<AlignmentOutcome> {
    let vec_store = state.vector_store.as_ref().ok_or_else(|| {
        anyhow::anyhow!("alignment index not built — call AppState::build_alignment_index first")
    })?;

    let leaf = LeafClassInput {
        iri: format!("urn:moosedev:leaf:{}", slug(label)),
        label: label.to_string(),
        definition: definition.map(str::to_string),
        surface_labels,
        expected_category: None,
        sampled_values: Vec::new(),
    };

    // L1 + L2 only (no LLM); an empty CategoryMappings falls back cleanly.
    Ok(suggest_parent(
        &leaf,
        &state.arch_vocab,
        vec_store,
        None,
        &CategoryMappings::new(),
        &AlignmentConfig::default(),
    )
    .await)
}

/// Render an [`AlignmentOutcome`] as agent-readable text, preserving the deciding
/// sensor + rationale (resolved) or the ranked candidates (undecided) so the
/// reasoning stays auditable (invariant #6).
pub fn format_outcome(state: &AppState, label: &str, outcome: &AlignmentOutcome) -> String {
    match outcome {
        AlignmentOutcome::Resolved {
            iri,
            sensor,
            rationale,
            ..
        } => format!(
            "\"{label}\" aligns under {} <{iri}>\n  sensor: {sensor:?}\n  rationale: {rationale}",
            class_label(state, iri),
        ),
        AlignmentOutcome::Undecided {
            reason,
            top_candidates,
            ..
        } => {
            let mut out = format!("\"{label}\" — ambiguous ({reason}). Candidate classes:");
            for c in top_candidates {
                out.push_str(&format!("\n  • {} <{}>", c.label, c.iri));
            }
            out
        }
        AlignmentOutcome::Unavailable { reason, .. } => {
            format!("\"{label}\" — no alignment found: {reason}")
        }
        // `AlignmentOutcome` is `#[non_exhaustive]`; surface any future variant.
        _ => format!("\"{label}\" — unrecognized alignment outcome: {outcome:?}"),
    }
}

/// Human label for a class IRI, from the architecture vocabulary (else local name).
fn class_label(state: &AppState, iri: &str) -> String {
    state
        .arch_vocab
        .classes
        .iter()
        .find(|c| c.iri == iri)
        .and_then(|c| c.label.clone())
        .unwrap_or_else(|| iri.rsplit(['/', '#']).next().unwrap_or(iri).to_string())
}

/// URN-safe slug for a leaf concept's synthetic IRI.
fn slug(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}
