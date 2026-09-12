//! The accepted-knowledge revision guard shared by every daemon operation:
//! work computed against one revision is refused when the graph moved.
use super::accepted_revision;
use crate::graph::AppState;

pub(super) fn ensure_unchanged(
    state: &AppState,
    expected: &str,
    message: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(accepted_revision(state)? == expected, "{message}");
    Ok(())
}
