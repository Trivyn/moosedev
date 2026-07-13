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
    /// Enacted decision: `inject`, `deny`, `require_ratification`, `proposed`.
    pub decision: String,
    /// IRIs of the knowledge records the decision cited.
    pub records_cited: Vec<String>,
}

/// Path of the fire log for a data dir (mirrors the `http.addr` path helper).
pub fn fires_log_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(FIRES_LOG_FILE_NAME)
}

/// Best-effort append of one fire line. Telemetry must never fail the policy
/// evaluation it describes (invariant #1: the symbolic decision is primary),
/// so IO or serialization errors are logged and swallowed.
pub fn append_fire(data_dir: &Path, event: &FireEvent) {
    let line = match serde_json::to_string(event) {
        Ok(line) => line,
        Err(e) => {
            tracing::warn!("fires.jsonl: failed to serialize fire event: {e}");
            return;
        }
    };
    let path = fires_log_path_for(data_dir);
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(e) = result {
        tracing::warn!("fires.jsonl: failed to append to {}: {e}", path.display());
    }
}
