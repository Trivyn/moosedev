//! The durable derived state of a task and the bounds on its autonomous
//! recoveries.
use crate::harness::protocol::{AssociatePage, CaptureTypeResponse, CheckOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Edits outside the plan files replan autonomously this many times per task;
/// the next one parks for human guidance.
pub const MAX_SCOPE_ESCAPES: usize = 3;
/// A rejected typed capture is retyped under fresh ids this many times per
/// task; the next rejection parks for human guidance.
pub const MAX_RETYPES: usize = 3;

pub(super) const CAPTURE_NOTE_QUESTION: &str = "The coding work is done and its required checks passed. Answer one plain question in prose, no JSON structure beyond the single note field: what should a future engineer know about this change that the diff alone does not say? Name the decision you made and why, any rule you discovered, and anything that surprised you. Say \"nothing beyond the diff\" if there is nothing durable. Do not restate the objective.";

/// Durable derived state. Obligations are re-derived at every plan approval;
/// the counters bound autonomous recoveries for the whole task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolicState {
    /// Plan file -> direct dossier records of its resolved definitions.
    pub obligations: BTreeMap<String, Vec<String>>,
    pub obligations_digest: String,
    pub knowledge_revision: String,
    #[serde(default)]
    pub scope_escapes: usize,
    #[serde(default)]
    pub noop_continuations: usize,
    #[serde(default)]
    pub retypes: usize,
    /// Model replans answered by continuing the approved plan because nothing
    /// changed since approval. Unbounded; counted for the study.
    #[serde(default)]
    pub replan_continuations: usize,
    /// True from plan approval until the first applied edit, command, required
    /// check result or human answer. Reads do not end it. False by default, so
    /// an older journal never continues a replan it cannot prove unchanged.
    #[serde(default)]
    pub unchanged_since_approval: bool,
    /// The association derived for the current edit batch; cleared by each
    /// applied edit so a later batch is derived afresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association: Option<SymbolicAssociation>,
    /// Every required-check outcome in order, feeding the symbolic lesson.
    #[serde(default)]
    pub check_history: Vec<CheckOutcome>,
    /// Plans returned in the current planning cycle because their summary did
    /// not address a governing rule. Reset when a plan is stored.
    #[serde(default)]
    pub coverage_returns: usize,
    /// Digests of (file, grounding keys) whose grounding note was delivered;
    /// the same edit proposed again goes through.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub grounded: BTreeSet<String>,
    /// The one final capture note and its typing; cleared by each applied edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_note: Option<CaptureNoteState>,
}

/// `asked` (note journaled, typing not yet durable) -> `typed` (daemon typing
/// stored; the capture request is rebuilt from it on resume) -> `captured`
/// (its proposals are in the graph; never invalidated or submitted again).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureNoteState {
    pub operation_id: String,
    pub capture_operation_id: String,
    pub note_event: usize,
    pub note: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CaptureTypeResponse>,
}

#[derive(Debug, Deserialize)]
pub(super) struct NoteAnswer {
    pub(super) note: String,
}

/// One derived association batch: the daemon's page and the review it
/// entered. `derived` -> `awaiting_review` -> `resolved`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolicAssociation {
    pub page: AssociatePage,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_operation_id: Option<String>,
}
