//! Programmatic memory boundary for the harness. The model never calls these
//! routes: the runner asks for `context`, submits typed captures (`capture`),
//! has a human `review` them, and reads `checkpoint`s; `intent`, `associate`
//! and `capture_type` derive associations and typed proposals symbolically,
//! with `scope`, `candidates` and `anchors` as their snapshot-bound projections. This
//! file keeps the shared operation journal types and the small helpers every
//! submodule uses.
use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Quad, Term};
use serde::{Deserialize, Serialize};

use super::protocol::*;
use crate::api::error::ApiError;
use crate::graph::{self, AppState, CaptureStamp, RecordInput, PROJECT_KG_GRAPH_IRI};
use crate::policy::{self, PolicyDecision, PolicyEvent};

mod anchors;
pub mod associate;
pub mod candidates;
mod capture;
pub mod capture_type;
mod checkpoint;
mod context;
pub mod ground;
pub mod intent;
mod journal;
pub mod reconcile_score;
mod review;
mod revision;
pub mod scope;

pub use capture::{capture_operation, capture_v2, capture_v2_operation};
pub use checkpoint::{checkpoint, checkpoint_snapshot, publish_checkpoint, CheckpointQuery};
pub use context::{accepted_revision, context, context_snapshot};
pub use review::{review, review_operation};

use journal::{journal_path, load, lock_operations, save_operation, validate_id};

const AUTHOR: &str = "moosedev-harness";
const REVIEWER: &str = "moosedev-harness-human";

#[derive(Serialize, Deserialize)]
struct Operation {
    request: CaptureRequest,
    owner_id: String,
    timestamp: String,
    entries: Vec<Entry>,
    review: Option<bool>,
    captured: bool,
    reviewed: bool,
    #[serde(default)]
    review_base_revision: Option<String>,
    #[serde(default)]
    review_result_revision: Option<String>,
    #[serde(default)]
    review_claims: Option<Vec<String>>,
    /// Normalized symbols of this operation's code links that had no entity
    /// when the review base was recorded: entities minted for them by this
    /// acceptance are its own writes.
    #[serde(default)]
    review_unminted_symbols: Vec<String>,
    /// Existing records a restated note links to the changed definitions;
    /// reviewed and attested with the rest of the operation.
    #[serde(default)]
    restated: Vec<RestatedEntry>,
    /// Quads between each restated record and its links' existing entities
    /// when the review base was recorded; the acceptance's own are the rest.
    #[serde(default)]
    review_restated_edges: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    response: CapturedProposal,
    class: String,
    relations: Vec<(String, String)>,
    rationale: Option<String>,
    // Freeze anchors when preparing the operation; retries must not re-resolve
    // against a different index generation.
    anchors: Vec<(String, String)>,
    /// Receipt-backed derived relations, annotated with confidence after the
    /// record is written.
    #[serde(default)]
    reconciled: Vec<ReconciledRelation>,
}

#[derive(Serialize, Deserialize)]
struct RestatedEntry {
    response: RestatedLinks,
    /// The existing record's kind, which picks its link predicate.
    kind: String,
    /// `(raw symbol, file)`, frozen like a proposal's anchors.
    anchors: Vec<(String, String)>,
}

fn current_status(state: &AppState, iri: &str) -> Option<String> {
    graph::first_literal(&state.store, iri, &state.capture.status)
}

fn validate_path(file: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !file.is_empty()
            && Path::new(file)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "expected a repository-relative file path"
    );
    Ok(())
}

fn durable_flush(state: &AppState) -> anyhow::Result<()> {
    state.store.flush()?;
    crate::canonical::write_through(&state.store, &state.data_dir)?;
    std::fs::File::open(crate::canonical::canonical_path(&state.data_dir))?.sync_all()?;
    std::fs::File::open(crate::canonical::stamp_path(&state.data_dir))?.sync_all()?;
    std::fs::File::open(&state.data_dir)?.sync_all()?;
    Ok(())
}
