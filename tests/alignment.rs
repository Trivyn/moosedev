//! Concept alignment end-to-end (L1 + L2, no LLM): build the alignment index from
//! the shipped ontologies, then align a concept and confirm it resolves to — or at
//! least surfaces as a candidate — the expected architecture class. Deterministic
//! (arctic-s embeddings + cosine are fixed), but loads the embedding backbone, so
//! it's one of the slower tests.

use std::path::Path;

use moose::alignment::AlignmentOutcome;
use moosedev::alignment::align_concept;
use moosedev::graph::AppState;

/// True if the outcome resolved to, or lists as a candidate, a class IRI ending in `suffix`.
fn hit(outcome: &AlignmentOutcome, suffix: &str) -> bool {
    match outcome {
        AlignmentOutcome::Resolved { iri, .. } => iri.ends_with(suffix),
        AlignmentOutcome::Undecided { top_candidates, .. } => {
            top_candidates.iter().any(|c| c.iri.ends_with(suffix))
        }
        _ => false,
    }
}

#[tokio::test]
async fn aligns_concept_to_architecture_class() {
    let dir = std::env::temp_dir().join(format!("moosedev-align-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let onto = Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");

    let mut state = AppState::bootstrap(&dir, &onto).expect("bootstrap");
    state
        .build_alignment_index()
        .await
        .expect("build alignment index");

    // A concept that closely matches an existing class should resolve to it (or at
    // least surface it as a candidate) via L1/L2 — no LLM in the loop.
    let outcome = align_concept(
        &state,
        "Architectural Decision",
        Some("A choice made about the system's structure or technology, with rationale."),
        Vec::new(),
    )
    .await
    .expect("align concept");
    assert!(
        hit(&outcome, "ArchitecturalDecision"),
        "expected alignment to ArchitecturalDecision; got {outcome:?}"
    );

    // The alignment tool requires the index; with it built, a second call works too.
    let lesson = align_concept(&state, "Lesson Learned", None, Vec::new())
        .await
        .expect("align lesson");
    assert!(
        !matches!(lesson, AlignmentOutcome::Unavailable { .. }),
        "a clearly ontology-relevant concept should not be Unavailable; got {lesson:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
