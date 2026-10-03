//! The durable derived state of a task and the bounds on its autonomous
//! recoveries.
use crate::harness::protocol::{AssociatePage, CaptureTypeResponse, CheckOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// With `MOOSEDEV_HARNESS_SCOPE_CHOICE=off`, edits outside the plan files
/// replan autonomously this many times per task; the next one parks for human
/// guidance. Otherwise each escape asks the human, who is the bound.
pub const MAX_SCOPE_ESCAPES: usize = 3;
/// A rejected typed capture is retyped under fresh ids this many times per
/// task; the next rejection parks for human guidance.
pub const MAX_RETYPES: usize = 3;
/// Consecutive searches matching nothing after which the harness states that
/// the channel is exhausted. Two: one empty search can be a poor choice of
/// words, a second on the same idea is evidence the knowledge is not recorded.
pub const FRUITLESS_SEARCH_LIMIT: usize = 2;

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
    /// The edit count at which a finish was sent back for stub markers: once
    /// per source state, like the language-server gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stub_refused_at: Option<usize>,
    /// The edit count at which a finish was sent back for planned files
    /// missing or not edited this approval cycle: once per source state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished_refused_at: Option<usize>,
    /// The edit count at which the human chose to verify with planned files
    /// still missing or unedited; a finish at that source state is not asked
    /// again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished_accepted_at: Option<usize>,
    /// The edit count at which the human answered "work" to the unedited
    /// planned files question; a finish at that source state parks instead
    /// of asking again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unedited_work_at: Option<usize>,
    /// The edit count the harness last steered at instead of parking a
    /// repeated look while a required check failed; the next refusal at that
    /// source state parks ([`super::stall`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steered_at: Option<usize>,
    /// Fingerprints of the replaces applied in this task, so an insertion is
    /// called re-applied only when this same replace was applied before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_replaces: Vec<String>,
    /// The edit count a clean, freshly checked edit armed auto-verify at; taken
    /// by the next advance whether it fires or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify_armed: Option<usize>,
    /// The edit count the harness last ran the required checks at by itself,
    /// so a failure there is told as the harness's run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verification: Option<usize>,
    /// Auto-verify runs in the current approval cycle; reset when a plan is
    /// approved.
    #[serde(default)]
    pub auto_verifications: usize,
    /// `auto_verify_exhausted` was journaled this approval cycle.
    #[serde(default)]
    pub auto_verify_exhausted: bool,
    /// `Task::edits` index where the current approval cycle began. Unlike an
    /// approved plan's `edit_start`, it moves on every approval, even of the
    /// same plan.
    #[serde(default)]
    pub cycle_edit_start: usize,
    /// Set when source or accepted knowledge withdrew the approval: the next
    /// approval starts counting edits afresh even if it only adds files to
    /// the plan before it. Consumed by that approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coverage_reset: bool,
    /// Set when a scope escape returned the task to Plan (the automatic
    /// replan, or the human's `replan` choice): the next approval keeps the
    /// earlier plan's edits if its files contain that plan's. Consumed by
    /// that approval; cleared when the human returns the task to Plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scope_replan: bool,
    /// (edits, required-check results) when a reply last continued the
    /// turn. Another reply may continue once either has grown since, so a
    /// model that works between replies keeps its turn and one that only
    /// replies still hands it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_continued_at: Option<(usize, usize)>,
    /// The edit count a fresh result with a fix the harness may apply armed
    /// auto-fix at; taken by the next advance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_fix_armed: Option<usize>,
    /// Fixes the harness applied in a row since the model's last edit.
    #[serde(default)]
    pub auto_fix_chain: usize,
    /// Fixes the harness applied in this task.
    #[serde(default)]
    pub auto_fixes: usize,
    /// Indices into `Task::edits` of the harness's own fixes: not the model's
    /// work, so auto-verify never counts them as a planned file edited.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub auto_fixed_edits: std::collections::BTreeSet<usize>,
    /// Files a missing-module question asked about in the current approval
    /// cycle: each is asked once; cleared when a plan is approved.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub asked_missing: BTreeSet<String>,
    /// Files the human declined to add to the approved plan (a scope or
    /// missing-module `refuse`) or removed from it (`drop` at a missing
    /// planned file): an earlier approved plan listing one never adds it by
    /// itself again in this task.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub scope_declined: BTreeSet<String>,
    /// The edit count at which a model replan was last held because every
    /// current error was in the approved files: a second replan at the same
    /// count goes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replan_held_at: Option<usize>,
    #[serde(default)]
    pub retypes: usize,
    /// Model replans answered by continuing the approved plan because nothing
    /// changed since approval. Unbounded; counted for the study.
    #[serde(default)]
    pub replan_continuations: usize,
    /// Replan continuations in the current approval cycle; reset when a plan is
    /// approved. The second one grounds the approved plan.
    #[serde(default)]
    pub cycle_replan_continuations: usize,
    /// True once this approval cycle's plan grounding was attempted; reset when a
    /// plan is approved.
    #[serde(default)]
    pub plan_grounded: bool,
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
    /// Consecutive `search` actions that matched no accepted knowledge and no
    /// repository text. Reset by any search that returns something.
    #[serde(default)]
    pub fruitless_searches: usize,
    /// Plans returned in the current planning cycle because their summary did
    /// not address a governing rule. Reset when a plan is stored.
    #[serde(default)]
    pub coverage_returns: usize,
    /// Plans returned this approval cycle for leaving rules of the spec in
    /// play open ([`Runner::spec_deferral_return`]).
    #[serde(default)]
    pub spec_deferral_returns: usize,
    /// Digests of (file, grounding keys) whose grounding note was delivered;
    /// the same edit proposed again goes through.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub grounded: BTreeSet<String>,
    /// Content fingerprint of each file the model read in this task, taken at
    /// the read. Storing a plan narrows the working set to its files; a file
    /// read before that whose content is unchanged has still been seen, so
    /// grounding does not hold an edit to show it again. Cleared only when a
    /// task stopped for context overflow is resumed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub read_snapshots: BTreeMap<String, Option<String>>,
    /// The one final capture note and its typing; cleared by each applied edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_note: Option<CaptureNoteState>,
    /// The most recent failed required check; cleared by a passing one, a
    /// human answer or a permission grant. A finish or no-op edit proposed
    /// with the same edit count asks to rerun it against source that has
    /// already failed, so it is repaired instead of run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<FailedRun>,
    /// The last failure and how many times in a row it came back with no
    /// edit between (`stall`); cleared by a pass of its command or a human
    /// answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_failure: Option<super::stall::StalledFailure>,
    /// Source states a command failed in, kept across human answers: an edit
    /// back to one is a sighting of that failure without a run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_states: Vec<super::stall::FailedState>,
    /// Source states applied edits produced, kept across human answers:
    /// while a failure stands, an edit back to one is a sighting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visited_states: Vec<super::stall::VisitedState>,
}

/// A required check that failed, how many edits the task had applied when
/// it ran, and whether the sandbox blocked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedRun {
    pub command: String,
    pub edits: usize,
    #[serde(default)]
    pub denied: bool,
    /// The denial named no path or network need, so no grant can help.
    #[serde(default)]
    pub ungrantable: bool,
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
    /// Facts the harness checked beside the note, shown to the human at
    /// review: what the checks proved, stubs left, planned files untouched,
    /// what the note claims that no edit shows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
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
