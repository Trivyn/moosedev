//! Append-only fire-event telemetry (spec §4.4).
//!
//! Every push-fire and gate-fire is one JSON line in `.moosedev/fires.jsonl`.
//! Fires are operational telemetry, **not** knowledge — they never enter the
//! project graph, keeping `kg.nq` reviewable while making enforcement value
//! measurable (Consequence `6505ed72`). The file lives under the data dir,
//! which is gitignored except for the canonical `kg.nq`.
//!
//! Distinct from the in-graph `[trial-fire]` InformationRecords counted by
//! `bench/trial_report.py` — those belong to the in-anger trial protocol and
//! are untouched by this log.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;

/// File name of the fire log inside the data dir.
pub const FIRES_LOG_FILE_NAME: &str = "fires.jsonl";

/// One policy fire: an active-agency verb that actually delivered or blocked.
#[derive(Debug, Clone, Serialize)]
pub struct FireEvent {
    /// RFC3339 timestamp of the fire.
    pub ts: String,
    /// Active-agency verb: `push`, `gate`, or `capture`.
    pub verb: &'static str,
    /// Host adapter that reported the event (e.g. `claude-code`, `opencode`, `lsp`).
    pub host: String,
    /// Primary CodeEntity IRI the decision was about, when one resolved.
    pub entity: Option<String>,
    /// Enacted decision: `inject`, `deny`, `require_ratification`, `proposed`,
    /// or `journaled` for an automatic session checkpoint.
    pub decision: String,
    /// IRIs of the knowledge records the decision cited.
    pub records_cited: Vec<String>,
    /// Automatic-capture journal payload: the host's session-end summary.
    /// Journal entries live HERE, never in the graph — a session's final
    /// message is a status report, not a decision (Lesson `641c1811`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Automatic-capture journal payload: the changed files at the checkpoint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// Path of the fire log for a data dir (mirrors the `http.addr` path helper).
pub fn fires_log_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(FIRES_LOG_FILE_NAME)
}

/// Append one fire line, reporting serialization and IO failures to the caller.
pub fn append_fire(data_dir: &Path, event: &FireEvent) -> anyhow::Result<()> {
    let line = serde_json::to_string(event).context("serialize fire event")?;
    let path = fires_log_path_for(data_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open fire log {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("append fire log {}", path.display()))?;
    Ok(())
}

/// Best-effort append for operational telemetry that must not fail the policy
/// decision or deliberate graph capture it describes.
pub fn append_fire_best_effort(data_dir: &Path, event: &FireEvent) {
    if let Err(e) = append_fire(data_dir, event) {
        tracing::warn!("fires.jsonl: failed to append fire event: {e}");
    }
}
