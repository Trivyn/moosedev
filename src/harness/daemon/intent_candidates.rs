//! Read-only, snapshot-bound projections of the changed definition scopes and
//! the current source they were proven against.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::validate_path;
use crate::code::substrate::{DefinitionScope, SourceRange, Substrate};
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const MAX_FILES: usize = 32;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// The definitions a change touches: a definition whose name token intersects
/// a changed range, or whose producer-provided enclosing range does.
pub(super) fn changed_scopes(
    substrate: &Substrate,
    changed: &ChangedFile,
) -> Vec<(DefinitionScope, IntentScopeBasis)> {
    substrate
        .definition_scopes_in_file(&changed.file)
        .into_iter()
        .filter_map(|scope| {
            let basis = if changed
                .changed_ranges
                .iter()
                .any(|r| intersects(scope.definition.range, (*r).into()))
            {
                IntentScopeBasis::ChangedDefinition
            } else if scope.enclosing_range.is_some_and(|enclosing| {
                changed
                    .changed_ranges
                    .iter()
                    .any(|r| intersects(enclosing, (*r).into()))
            }) {
                IntentScopeBasis::EnclosingDefinition
            } else {
                return None;
            };
            Some((scope, basis))
        })
        .collect()
}

/// Snapshot identity of the loaded index for the requested files.
pub(super) fn index_revision(
    substrate: &Substrate,
    files: &[ChangedFile],
) -> anyhow::Result<String> {
    let indexed_source_proofs = files
        .iter()
        .map(|file| {
            (
                file.file.as_str(),
                substrate
                    .read_indexed_source(&file.file)
                    .map(|source| format!("{:x}", Sha256::digest(source.as_bytes()))),
            )
        })
        .collect::<Vec<_>>();
    let meta = substrate.meta();
    digest(&(
        meta.schema_version,
        &meta.indexed_commit,
        meta.indexed_at,
        &meta.generation,
        &meta.producers,
        indexed_source_proofs,
    ))
}

pub(super) fn producer_label(substrate: &Substrate) -> Option<String> {
    Some(
        substrate
            .meta()
            .producers
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Current unless the index is stale or does not prove the requested current
/// source; deleted-file history is judged separately by the caller.
pub(super) fn index_status(substrate: &Substrate, files: &[ChangedFile]) -> IntentIndexStatus {
    let mut status = if substrate.is_stale() {
        IntentIndexStatus::Stale
    } else {
        IntentIndexStatus::Current
    };
    if files.iter().any(|changed| match &changed.after_digest {
        Some(expected) => {
            substrate
                .read_indexed_source(&changed.file)
                .map(|source| format!("{:x}", Sha256::digest(source.as_bytes())))
                .as_ref()
                != Some(expected)
        }
        None => substrate.covers_file(&changed.file),
    }) {
        status = IntentIndexStatus::Stale;
    }
    status
}

pub(super) fn entity_record_context(
    state: &AppState,
    symbol: &str,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut direct = Vec::new();
    let mut component = Vec::new();
    if let Some(dossier) =
        graph::get_entity_dossier(state, &graph::DossierTarget::Symbol(symbol.into()))?
    {
        for record in dossier.direct_records {
            if accepted_status(&record.status) && !direct.contains(&record.iri) {
                direct.push(record.iri)
            }
        }
        for record in dossier.component_records {
            if accepted_status(&record.status) && !component.contains(&record.iri) {
                component.push(record.iri)
            }
        }
    }
    direct.truncate(16);
    component.truncate(16);
    Ok((direct, component))
}

pub(super) fn accepted_status(status: &str) -> bool {
    status.is_empty() || status.eq_ignore_ascii_case("accepted")
}

pub(super) fn prove_changed_source(
    state: &AppState,
    changed: &ChangedFile,
) -> anyhow::Result<(String, Option<String>)> {
    match &changed.after_digest {
        Some(expected) => {
            let root = state.project_root().canonicalize()?;
            let path = root.join(&changed.file);
            let canonical = path.canonicalize()?;
            anyhow::ensure!(
                canonical.starts_with(&root) && canonical.is_file(),
                "current source is outside the project or not a file"
            );
            let before = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                before.len() <= MAX_SOURCE_BYTES,
                "current source exceeds the 16 MiB intent proof limit"
            );
            let mut bytes = Vec::with_capacity(before.len() as usize);
            std::fs::File::open(&canonical)?
                .take(MAX_SOURCE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            let after = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                bytes.len() as u64 == before.len()
                    && before.len() == after.len()
                    && before.modified()? == after.modified()?,
                "current source changed while proving its digest"
            );
            let actual = format!("{:x}", Sha256::digest(&bytes));
            anyhow::ensure!(
                &actual == expected,
                "after_digest does not match current source"
            );
            Ok((actual, Some(String::from_utf8(bytes)?)))
        }
        None => {
            anyhow::ensure!(
                !state.project_root().join(&changed.file).exists(),
                "deleted source still exists"
            );
            changed
                .before_digest
                .clone()
                .map(|digest| (digest, None))
                .ok_or_else(|| anyhow::anyhow!("deleted source needs before_digest"))
        }
    }
}

pub(super) fn validate_source_ranges(
    source: &str,
    ranges: &[HarnessSourceRange],
) -> anyhow::Result<()> {
    // Retain the final empty line: insertion at EOF after a trailing newline is
    // represented by its zero column and is a valid end-exclusive position.
    let lines = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    for range in ranges {
        for point in [range.start, range.end] {
            let line = lines
                .get(point.line as usize)
                .ok_or_else(|| anyhow::anyhow!("changed range is outside current source"))?;
            anyhow::ensure!(
                (point.col as usize) <= line.len() && line.is_char_boundary(point.col as usize),
                "changed range column is not a UTF-8 byte boundary in current source"
            );
        }
    }
    Ok(())
}

pub(super) fn maybe_refresh(state: &AppState, policy: &IntentRefreshPolicy) -> IntentRefreshAction {
    if matches!(policy, IntentRefreshPolicy::None) {
        return IntentRefreshAction::NotRequested;
    }
    let Some(substrate) = state.substrate() else {
        return IntentRefreshAction::Unsupported;
    };
    let supported = substrate.meta().producers.len() == 1
        && substrate.meta().producers[0].name == "scip-python"
        && std::env::var_os("MOOSEDEV_SCIP_PYTHON")
            .is_some_and(|p| Path::new(&p).is_absolute() && Path::new(&p).is_file());
    if !supported {
        return IntentRefreshAction::Unsupported;
    }
    let producers: Vec<_> = crate::code::substrate::registry()
        .iter()
        .filter(|p| p.name == "scip-python")
        .copied()
        .collect();
    match crate::code::substrate::producer::run_index_with(
        &producers,
        &state.project_root(),
        &state.data_dir,
    ) {
        Ok(_) => {
            state.load_substrate(&state.project_root());
            IntentRefreshAction::Refreshed
        }
        Err(_) => IntentRefreshAction::Failed,
    }
}

pub(super) fn validate_files(files: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(files.len() <= MAX_FILES, "at most 32 files");
    let mut unique = std::collections::BTreeSet::new();
    for f in files {
        validate_path(f)?;
        anyhow::ensure!(unique.insert(f), "duplicate intent file `{f}`");
    }
    Ok(())
}
pub(super) fn intersects(a: SourceRange, b: SourceRange) -> bool {
    a.start < b.end && b.start < a.end
}
pub(super) fn digest<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
impl From<HarnessSourceRange> for SourceRange {
    fn from(r: HarnessSourceRange) -> Self {
        Self {
            start: crate::code::substrate::Position {
                line: r.start.line,
                col: r.start.col,
            },
            end: crate::code::substrate::Position {
                line: r.end.line,
                col: r.end.col,
            },
        }
    }
}
impl From<SourceRange> for HarnessSourceRange {
    fn from(r: SourceRange) -> Self {
        Self {
            start: HarnessSourcePosition {
                line: r.start.line,
                col: r.start.col,
            },
            end: HarnessSourcePosition {
                line: r.end.line,
                col: r.end.col,
            },
        }
    }
}
