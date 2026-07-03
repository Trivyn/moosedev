//! Committed canonical text serialization of the project KG.
//!
//! Requirement `d459cac2`: the committed source of truth for the project
//! knowledge graph is a deterministic text serialization — `<data_dir>/kg.nq`,
//! canonical N-Quads — while the RocksDB store (and the vector DBs) are a
//! derived, gitignored local cache. Capture write-through keeps the text in
//! step with the store; a hash stamp lets startup detect which side changed
//! (a `git pull` vs. a write that never reached the text) and reconcile.
//!
//! The canonical text excludes reasoner-materialized quads — see
//! [`crate::export::export_canonical_project`] — so it carries only asserted
//! knowledge; inferred edges re-derive locally via lazy enrichment.

use std::path::{Path, PathBuf};

use oxigraph::store::Store;
use sha2::{Digest, Sha256};

use crate::export::{export_canonical_project, GraphDump};
use crate::graph_import::{import_graph, GraphImport, ImportFormat, ImportMode};

/// The committed canonical serialization, relative to the data dir.
pub const CANONICAL_FILE_NAME: &str = "kg.nq";
/// Local (gitignored) sync stamp: hex SHA-256 of `kg.nq` at the last sync.
pub const STAMP_FILE_NAME: &str = "kg.nq.stamp";

pub fn canonical_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CANONICAL_FILE_NAME)
}

pub fn stamp_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STAMP_FILE_NAME)
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// What startup reconciliation decided to do (see [`decide`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Text and store agree with the stamp — or there is nothing anywhere.
    Nothing,
    /// Store content matches the text byte-for-byte; only (re)write the stamp.
    StampOnly,
    /// Store has content the text lacks (adoption, or a write that never
    /// reached the text): re-export `kg.nq` + stamp.
    ExportText,
    /// The text changed externally (fresh clone, `git pull`, checkout) and the
    /// store did not: the text is authoritative — replace-import it.
    HydrateReplace,
    /// Both sides changed since the last sync: patch-import the text (loses
    /// nothing), re-export the union, and warn.
    MergeDiverged,
}

/// Pure reconciliation decision. `text_hash` is `None` when `kg.nq` is absent;
/// `stamp` is `None` when no stamp file exists; `store_hash`/`store_empty`
/// describe the canonical export of the current store.
pub fn decide(
    text_hash: Option<&str>,
    store_hash: &str,
    store_empty: bool,
    stamp: Option<&str>,
) -> StartupAction {
    let Some(text_hash) = text_hash else {
        // No committed text yet: adopt a non-empty store; stay quiet otherwise
        // (the file appears on the first capture, not as an empty artifact).
        return if store_empty {
            StartupAction::Nothing
        } else {
            StartupAction::ExportText
        };
    };
    if store_empty {
        // Fresh clone: only the text has content.
        return StartupAction::HydrateReplace;
    }
    if store_hash == text_hash {
        // Same content on both sides; ensure the stamp records it.
        return if stamp == Some(text_hash) {
            StartupAction::Nothing
        } else {
            StartupAction::StampOnly
        };
    }
    match stamp {
        Some(stamp) if text_hash != stamp && store_hash == stamp => StartupAction::HydrateReplace,
        Some(stamp) if text_hash == stamp && store_hash != stamp => StartupAction::ExportText,
        // Both moved since the last sync — or we have no stamp to arbitrate.
        _ => StartupAction::MergeDiverged,
    }
}

/// Outcome of [`sync_on_startup`], for logging.
#[derive(Debug)]
pub struct StartupSync {
    pub action: StartupAction,
    /// Quads in the canonical text after reconciliation.
    pub quad_count: usize,
}

/// Reconcile `<data_dir>/kg.nq` with the local store when a data dir is opened.
///
/// Errors here mean the committed text could not be honored (unreadable or
/// unparseable — e.g. unresolved merge-conflict markers) and are FATAL to the
/// caller by design: continuing would let the next write-through clobber the
/// file and silently lose the other side's records. Plain write failures while
/// *exporting* are non-fatal (warn; retried by the next write-through).
pub fn sync_on_startup(store: &Store, data_dir: &Path) -> anyhow::Result<StartupSync> {
    let dump = export_canonical_project(store)?;
    let store_hash = sha256_hex(&dump.text);
    let store_empty = dump.quad_count == 0;

    let file = canonical_path(data_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => anyhow::bail!("read canonical {}: {e}", file.display()),
    };
    let text_hash = text.as_deref().map(sha256_hex);
    let stamp = read_stamp(data_dir);

    let action = decide(
        text_hash.as_deref(),
        &store_hash,
        store_empty,
        stamp.as_deref(),
    );

    let quad_count = match action {
        StartupAction::Nothing => dump.quad_count,
        StartupAction::StampOnly => {
            write_stamp_best_effort(data_dir, text_hash.as_deref().unwrap_or(&store_hash));
            dump.quad_count
        }
        StartupAction::ExportText => {
            write_canonical_best_effort(data_dir, &dump.text, &store_hash);
            dump.quad_count
        }
        StartupAction::HydrateReplace => {
            let text = text.as_deref().expect("hydrate requires canonical text");
            let outcome = hydrate(store, text, ImportMode::Replace).map_err(|e| {
                anyhow::anyhow!(
                    "canonical {} could not be loaded (resolve the file — e.g. leftover merge \
                     conflict markers — and restart): {e}",
                    file.display()
                )
            })?;
            write_stamp_best_effort(data_dir, text_hash.as_deref().unwrap_or_default());
            outcome.inserted_quad_count
        }
        StartupAction::MergeDiverged => {
            let text = text.as_deref().expect("merge requires canonical text");
            let outcome = hydrate(store, text, ImportMode::Patch).map_err(|e| {
                anyhow::anyhow!(
                    "canonical {} could not be merged (resolve the file — e.g. leftover merge \
                     conflict markers — and restart): {e}",
                    file.display()
                )
            })?;
            let union = export_canonical_project(store)?;
            let union_hash = sha256_hex(&union.text);
            write_canonical_best_effort(data_dir, &union.text, &union_hash);
            tracing::warn!(
                "canonical {} and the local store BOTH changed since the last sync; merged as a \
                 union ({} quad(s) added from the text). Review `git diff` — a record whose \
                 lifecycle status changed on one side may briefly carry two status literals.",
                file.display(),
                outcome.inserted_quad_count
            );
            union.quad_count
        }
    };

    Ok(StartupSync { action, quad_count })
}

/// Re-export the canonical text after a successful project-graph write.
/// Best-effort by contract (mirrors the post-write provenance/dense-index
/// steps): the caller warns on error and the next write retries.
pub fn write_through(store: &Store, data_dir: &Path) -> anyhow::Result<()> {
    let dump: GraphDump = export_canonical_project(store)?;
    let hash = sha256_hex(&dump.text);
    write_canonical(data_dir, &dump.text, &hash)
}

fn hydrate(store: &Store, text: &str, mode: ImportMode) -> anyhow::Result<GraphImport> {
    import_graph(
        store,
        crate::export::ExportScope::Project,
        ImportFormat::NQuads,
        mode,
        text,
    )
}

fn read_stamp(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(stamp_path(data_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write `kg.nq` atomically (temp file + rename) so a crash mid-write can never
/// leave a torn committed artifact, then record its hash in the stamp.
fn write_canonical(data_dir: &Path, text: &str, hash: &str) -> anyhow::Result<()> {
    let target = canonical_path(data_dir);
    let tmp = data_dir.join(format!("{CANONICAL_FILE_NAME}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| anyhow::anyhow!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .map_err(|e| anyhow::anyhow!("rename {} -> {}: {e}", tmp.display(), target.display()))?;
    std::fs::write(stamp_path(data_dir), hash)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", stamp_path(data_dir).display()))?;
    Ok(())
}

fn write_canonical_best_effort(data_dir: &Path, text: &str, hash: &str) {
    if let Err(e) = write_canonical(data_dir, text, hash) {
        tracing::warn!("canonical kg.nq export failed (retried on next write): {e}");
    }
}

fn write_stamp_best_effort(data_dir: &Path, hash: &str) {
    if let Err(e) = std::fs::write(stamp_path(data_dir), hash) {
        tracing::warn!(
            "canonical stamp write failed ({}): {e}",
            stamp_path(data_dir).display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "hash-a";
    const B: &str = "hash-b";
    const C: &str = "hash-c";

    #[test]
    fn nothing_when_no_text_and_empty_store() {
        assert_eq!(decide(None, A, true, None), StartupAction::Nothing);
    }

    #[test]
    fn adoption_exports_a_nonempty_store() {
        assert_eq!(decide(None, A, false, None), StartupAction::ExportText);
        // A stale stamp with no file behaves the same.
        assert_eq!(decide(None, A, false, Some(B)), StartupAction::ExportText);
    }

    #[test]
    fn fresh_clone_hydrates_from_text() {
        assert_eq!(
            decide(Some(A), B, true, None),
            StartupAction::HydrateReplace
        );
        assert_eq!(
            decide(Some(A), B, true, Some(A)),
            StartupAction::HydrateReplace
        );
    }

    #[test]
    fn in_sync_is_a_noop_and_missing_stamp_is_backfilled() {
        assert_eq!(decide(Some(A), A, false, Some(A)), StartupAction::Nothing);
        assert_eq!(decide(Some(A), A, false, None), StartupAction::StampOnly);
        assert_eq!(decide(Some(A), A, false, Some(B)), StartupAction::StampOnly);
    }

    #[test]
    fn git_pull_replaces_when_only_text_moved() {
        assert_eq!(
            decide(Some(B), A, false, Some(A)),
            StartupAction::HydrateReplace
        );
    }

    #[test]
    fn missed_export_rewrites_when_only_store_moved() {
        assert_eq!(
            decide(Some(A), B, false, Some(A)),
            StartupAction::ExportText
        );
    }

    #[test]
    fn divergence_merges_when_both_moved_or_stamp_missing() {
        assert_eq!(
            decide(Some(B), C, false, Some(A)),
            StartupAction::MergeDiverged
        );
        assert_eq!(
            decide(Some(B), C, false, None),
            StartupAction::MergeDiverged
        );
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(sha256_hex(""), sha256_hex(""));
        assert_ne!(sha256_hex("a"), sha256_hex("b"));
        assert_eq!(sha256_hex("abc").len(), 64);
    }
}
