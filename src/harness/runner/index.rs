//! Code-index refresh at finish, so associations and capture anchors are
//! proven against the source the task produced rather than the index it
//! started from. The harness writes files itself, so nothing else (the
//! daemon's save scheduler listens to the editor) would rebuild the index
//! before capture.
use anyhow::Result;
use std::path::Path;

use super::Runner;
use crate::code::substrate::producer;
use crate::harness::config::IndexRefresh;

impl Runner {
    /// Rebuild the index with every producer that detects the project when the
    /// task changed files since the last rebuild. A failure costs links, never
    /// the task; each outcome is journaled as an intent event.
    pub(super) async fn refresh_code_index(&mut self) -> Result<()> {
        let edits = self.task.edits.len();
        if edits == 0
            || self.indexed_edits == Some(edits)
            || self.index_refresh != Some(IndexRefresh::Auto)
        {
            return Ok(());
        }
        self.indexed_edits = Some(edits);
        let root = self.workspace.root().to_path_buf();
        if !producer_detected(&root) {
            self.intent_event(
                "index_refresh_skipped",
                "no SCIP producer detects this project",
            );
            return Ok(());
        }
        self.event("Refreshing the code index for the changed source.");
        let data_dir = root.join(".moosedev");
        let report = tokio::task::spawn_blocking(move || producer::run_index(&root, &data_dir))
            .await
            .map_err(|error| anyhow::anyhow!("index refresh task failed: {error}"))?;
        match report {
            Ok(report) => {
                let detail = format!(
                    "{} documents, {} definitions, {:.1}s",
                    report.documents,
                    report.definitions,
                    report.duration.as_secs_f64()
                );
                self.intent_event("index_refreshed", &detail);
                self.event(format!("Code index refreshed: {detail}."));
            }
            Err(error) => {
                let detail = format!("{error:#}");
                self.intent_event("index_refresh_failed", &detail);
                self.event(format!(
                    "Code index refresh failed; links for this change may be missing: {detail}"
                ));
            }
        }
        self.persist()
    }
}

fn producer_detected(root: &Path) -> bool {
    producer::registry()
        .iter()
        .any(|spec| (spec.detect)(root).is_some())
}
