//! Operation journals: one file per harness operation id under
//! `harness/operations`, written atomically and replayed on retry so a lost
//! acknowledgment never repeats a graph write.
//!
//! `OPERATIONS` serializes journal transitions, including retried HTTP
//! requests. Graph lifecycle primitives separately serialize the shared
//! ratification queue. Capture typing deliberately runs its sensor outside the
//! lock: two concurrent retries of the same typing id may race to the first
//! save, and a non-deterministic sensor can then make the loser's replay fail
//! the same-request check. The runner retries with the stored answer.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::graph::AppState;

static OPERATIONS: Mutex<()> = Mutex::new(());

pub(super) fn lock_operations() -> anyhow::Result<MutexGuard<'static, ()>> {
    OPERATIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("harness operation lock poisoned"))
}

/// Operation and owner ids name journal files; they are bounded and
/// filesystem-safe by construction.
pub(super) fn validate_id(id: &str, what: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 160
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "invalid {what}"
    );
    Ok(())
}

/// `<data_dir>/harness/operations/<id>.<ext>`; each operation family uses its
/// own extension so a capture and a link operation may share an id.
pub(super) fn journal_path(state: &AppState, id: &str, ext: &str) -> anyhow::Result<PathBuf> {
    validate_id(id, "operation_id")?;
    Ok(state
        .data_dir
        .join("harness/operations")
        .join(format!("{id}.{ext}")))
}

/// The journal at `path`, or `None` when the operation was never stored.
pub(super) fn load<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

/// Replays the journal at `path` when it exists (after `check` refuses a
/// reused id with a different request), otherwise builds, saves and returns
/// the fresh operation. The flag reports whether the journal already existed.
pub(super) fn load_or_store<T: Serialize + DeserializeOwned>(
    path: &Path,
    check: impl FnOnce(&T) -> anyhow::Result<()>,
    build: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<(T, bool)> {
    if let Some(stored) = load::<T>(path)? {
        check(&stored)?;
        return Ok((stored, true));
    }
    let operation = build()?;
    save_operation(path, &operation)?;
    Ok((operation, false))
}

pub(super) fn save_operation(path: &Path, operation: &impl Serialize) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("operation path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(operation)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    // Also persist newly created harness/operations directory entries before
    // allowing a graph write whose retry identity lives in this journal.
    for directory in parent.ancestors().take(3) {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}
