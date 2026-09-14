//! Read-only, snapshot-bound projections of the changed definition scopes and
//! the current source they were proven against.

use std::io::Read;
use std::path::Path;

use crate::harness::digest::{sha256_hex, sha256_json};

use super::validate_path;
use crate::code::substrate::symbols::{descriptor_role, is_top_level_declaration, DescriptorRole};
use crate::code::substrate::{
    is_test_path, DefinitionEntry, DefinitionScope, SourceRange, Substrate,
};
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const MAX_FILES: usize = 32;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// One changed leaf definition: kept by the kind filter, intersecting a hunk,
/// and enclosing no other kept definition that intersects the same hunk.
#[derive(Debug, Clone)]
pub(super) struct ChangedLeaf {
    pub(super) scope: DefinitionScope,
    pub(super) basis: IntentScopeBasis,
    /// Another leaf of a different symbol has exactly this span, so the
    /// change cannot be attributed to either one.
    pub(super) ambiguous: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ChangedLeaves {
    /// In definition order: range start, then normalized symbol.
    pub(super) leaves: Vec<ChangedLeaf>,
    /// Intersecting definitions that are not leaves, with the reason.
    pub(super) skipped: Vec<(DefinitionScope, SkipReason)>,
}

/// The definitions a change is about, shared by association and capture. A
/// definition intersects a hunk through its name token (a changed definition)
/// or its producer-provided enclosing range (an enclosing one). The kind filter
/// drops test code, parameters, type members and locals; of the kept
/// definitions, each hunk contributes those enclosing no other kept definition
/// that intersects the same hunk.
pub(super) fn changed_leaf_definitions(
    substrate: &Substrate,
    file: &str,
    ranges: &[HarnessSourceRange],
) -> ChangedLeaves {
    let ranges: Vec<SourceRange> = ranges.iter().map(|range| (*range).into()).collect();
    let touches = |scope: &DefinitionScope, range: SourceRange| {
        intersects(scope.definition.range, range) || intersects(span(scope), range)
    };
    let mut result = ChangedLeaves::default();
    let mut kept: Vec<(DefinitionScope, IntentScopeBasis)> = Vec::new();
    for scope in substrate.definition_scopes_in_file(file) {
        let basis = if ranges
            .iter()
            .any(|range| intersects(scope.definition.range, *range))
        {
            IntentScopeBasis::ChangedDefinition
        } else if ranges.iter().any(|range| intersects(span(&scope), *range)) {
            IntentScopeBasis::EnclosingDefinition
        } else {
            continue;
        };
        match skip_reason(&scope.definition.entry) {
            Some(reason) => result.skipped.push((scope, reason)),
            None => kept.push((scope, basis)),
        }
    }
    let is_leaf = |index: usize| {
        let outer = span(&kept[index].0);
        ranges.iter().any(|range| {
            touches(&kept[index].0, *range)
                && !kept.iter().enumerate().any(|(other, (scope, _))| {
                    let inner = span(scope);
                    other != index
                        && inner != outer
                        && outer.start <= inner.start
                        && inner.end <= outer.end
                        && touches(scope, *range)
                })
        })
    };
    let leaf_flags: Vec<bool> = (0..kept.len()).map(is_leaf).collect();
    for ((scope, basis), leaf) in kept.into_iter().zip(leaf_flags) {
        if !leaf {
            result.skipped.push((scope, SkipReason::Enclosing));
        } else if !result.leaves.iter().any(|known| {
            known.scope.definition.entry.normalized_symbol
                == scope.definition.entry.normalized_symbol
        }) {
            result.leaves.push(ChangedLeaf {
                scope,
                basis,
                ambiguous: false,
            });
        }
    }
    let spans: Vec<(SourceRange, String)> = result
        .leaves
        .iter()
        .map(|leaf| {
            (
                span(&leaf.scope),
                leaf.scope.definition.entry.normalized_symbol.clone(),
            )
        })
        .collect();
    for leaf in &mut result.leaves {
        let own = span(&leaf.scope);
        leaf.ambiguous = spans.iter().any(|(other, symbol)| {
            *other == own && symbol != &leaf.scope.definition.entry.normalized_symbol
        });
    }
    result
}

/// Definitions that never carry a knowledge anchor of their own: test code,
/// parameters, type members and locals. A module-level constant or table is a
/// declaration. A producer that leaves the kind unspecified (scip-python) is
/// read by the symbol grammar; syntactic anchors without one are kept.
fn skip_reason(entry: &DefinitionEntry) -> Option<SkipReason> {
    if is_test_path(&entry.file) {
        return Some(SkipReason::TestPath);
    }
    if graph::is_type_member(entry) {
        return Some(SkipReason::TypeMember);
    }
    match entry.kind.as_deref() {
        Some("Parameter" | "TypeParameter") => Some(SkipReason::Parameter),
        Some("Variable" | "Constant") if is_top_level_declaration(&entry.symbol) => None,
        Some("Variable" | "Local" | "Constant" | "Property") => Some(SkipReason::Local),
        Some(_) => None,
        None => match descriptor_role(&entry.symbol) {
            Some(DescriptorRole::Parameter) => Some(SkipReason::Parameter),
            Some(DescriptorRole::TypeMember) => Some(SkipReason::TypeMember),
            Some(DescriptorRole::Local) => Some(SkipReason::Local),
            Some(DescriptorRole::Declaration) | None => None,
        },
    }
}

/// A definition's extent: its enclosing range when the producer gives one.
pub(super) fn span(scope: &DefinitionScope) -> SourceRange {
    scope.enclosing_range.unwrap_or(scope.definition.range)
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
                substrate.read_indexed_source(&file.file).map(sha256_hex),
            )
        })
        .collect::<Vec<_>>();
    let meta = substrate.meta();
    sha256_json(&(
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
                .map(sha256_hex)
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
            if graph::is_accepted(&record.status) && !direct.contains(&record.iri) {
                direct.push(record.iri)
            }
        }
        for record in dossier.component_records {
            if graph::is_accepted(&record.status) && !component.contains(&record.iri) {
                component.push(record.iri)
            }
        }
    }
    direct.truncate(16);
    component.truncate(16);
    Ok((direct, component))
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
            let actual = sha256_hex(&bytes);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::substrate::SubstrateMeta;
    use protobuf::EnumOrUnknown;
    use scip::types::symbol_information::Kind;
    use scip::types::{Document, Index, Occurrence, SymbolInformation};

    const CLASS: &str = "scip-python python sample 1 labels/Labels#";
    const RENDER: &str = "scip-python python sample 1 labels/Labels#render().";
    const STRIP: &str = "scip-python python sample 1 labels/Labels#strip().";

    type Definition<'a> = (&'a str, Kind, Vec<i32>, Vec<i32>);

    fn substrate(files: &[(&str, &[Definition])]) -> Substrate {
        let mut index = Index::new();
        for (file, definitions) in files {
            let mut document = Document::new();
            document.relative_path = (*file).into();
            for (symbol, kind, range, enclosing) in definitions.iter() {
                let mut info = SymbolInformation::new();
                info.symbol = (*symbol).into();
                info.display_name = crate::code::substrate::symbols::last_descriptor_name(symbol)
                    .unwrap_or_default();
                info.kind = EnumOrUnknown::new(*kind);
                document.symbols.push(info);
                let mut occurrence = Occurrence::new();
                occurrence.symbol = (*symbol).into();
                occurrence.symbol_roles = 1;
                occurrence.range = range.clone();
                occurrence.enclosing_range = enclosing.clone();
                document.occurrences.push(occurrence);
            }
            index.documents.push(document);
        }
        Substrate::from_index(
            index,
            SubstrateMeta::single("scip-python", "test", chrono::Utc::now(), 1, 1),
            false,
        )
        .unwrap()
    }

    fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> HarnessSourceRange {
        HarnessSourceRange {
            start: HarnessSourcePosition { line: l1, col: c1 },
            end: HarnessSourcePosition { line: l2, col: c2 },
        }
    }

    fn symbols(leaves: &ChangedLeaves) -> Vec<&str> {
        leaves
            .leaves
            .iter()
            .map(|leaf| leaf.scope.definition.entry.symbol.as_str())
            .collect()
    }

    fn skipped_of(leaves: &ChangedLeaves) -> Vec<(&str, SkipReason)> {
        skipped(leaves)
    }

    fn skipped(leaves: &ChangedLeaves) -> Vec<(&str, SkipReason)> {
        leaves
            .skipped
            .iter()
            .map(|(scope, reason)| (scope.definition.entry.symbol.as_str(), *reason))
            .collect()
    }

    #[test]
    fn every_hunk_anchors_its_innermost_definitions_and_skips_the_enclosing_class() {
        let definitions = [
            (CLASS, Kind::Class, vec![0, 6, 12], vec![0, 0, 9, 0]),
            (RENDER, Kind::Method, vec![1, 8, 14], vec![1, 4, 3, 0]),
            (STRIP, Kind::Method, vec![4, 8, 13], vec![4, 4, 6, 0]),
        ];
        let substrate = substrate(&[("labels.py", &definitions)]);
        let leaves = changed_leaf_definitions(
            &substrate,
            "labels.py",
            &[range(2, 0, 3, 0), range(5, 0, 6, 0)],
        );
        assert_eq!(symbols(&leaves), [RENDER, STRIP]);
        assert!(leaves.leaves.iter().all(|leaf| !leaf.ambiguous));
        assert!(leaves
            .leaves
            .iter()
            .all(|leaf| leaf.basis == IntentScopeBasis::EnclosingDefinition));
        assert_eq!(skipped(&leaves), [(CLASS, SkipReason::Enclosing)]);
        // A hunk in the class body outside every method is about the class.
        let leaves = changed_leaf_definitions(
            &substrate,
            "labels.py",
            &[range(2, 0, 3, 0), range(7, 0, 8, 0)],
        );
        assert_eq!(symbols(&leaves), [CLASS, RENDER]);
        assert!(leaves.skipped.is_empty());
    }

    #[test]
    fn kind_filter_keeps_top_level_tables_and_skips_parameters_members_locals_and_tests() {
        const RATE: &str = "scip-python python sample 1 fees/LATE_FEE_RATE.";
        const LIMIT: &str = "scip-python python sample 1 fees/LIMIT.";
        const POLICY: &str = "scip-python python sample 1 fees/FeePolicy#";
        const ATTRIBUTE: &str = "scip-python python sample 1 fees/FeePolicy#rate.";
        const FIELD: &str = "scip-python python sample 1 fees/FeePolicy#cap.";
        const LATE_FEE: &str = "scip-python python sample 1 fees/FeePolicy#late_fee().";
        const TODAY: &str = "scip-python python sample 1 fees/FeePolicy#late_fee().(today)";
        const DAYS: &str = "scip-python python sample 1 fees/FeePolicy#late_fee().days.";
        const TEST: &str = "scip-python python sample 1 tests/test_fees/test_late().";
        // scip-python leaves every kind unspecified; the explicit kinds are
        // what other producers emit for the same shapes.
        let fees = [
            (RATE, Kind::UnspecifiedKind, vec![0, 0, 13], vec![]),
            (LIMIT, Kind::Constant, vec![1, 0, 5], vec![]),
            (
                POLICY,
                Kind::UnspecifiedKind,
                vec![3, 6, 15],
                vec![3, 0, 8, 0],
            ),
            (ATTRIBUTE, Kind::UnspecifiedKind, vec![4, 4, 8], vec![]),
            (FIELD, Kind::Field, vec![5, 4, 7], vec![]),
            (
                LATE_FEE,
                Kind::UnspecifiedKind,
                vec![6, 8, 16],
                vec![6, 4, 8, 0],
            ),
            (TODAY, Kind::UnspecifiedKind, vec![6, 23, 28], vec![]),
            (DAYS, Kind::UnspecifiedKind, vec![7, 8, 12], vec![]),
        ];
        let tests = [(TEST, Kind::Function, vec![0, 4, 13], vec![0, 0, 1, 0])];
        let substrate = substrate(&[("fees.py", &fees), ("tests/test_fees.py", &tests)]);
        let leaves = changed_leaf_definitions(
            &substrate,
            "fees.py",
            &[range(0, 0, 2, 0), range(4, 0, 8, 0)],
        );
        assert_eq!(symbols(&leaves), [RATE, LIMIT, LATE_FEE]);
        let mut skipped = skipped(&leaves);
        skipped.sort();
        let mut expected = vec![
            (POLICY, SkipReason::Enclosing),
            (ATTRIBUTE, SkipReason::TypeMember),
            (FIELD, SkipReason::TypeMember),
            (TODAY, SkipReason::Parameter),
            (DAYS, SkipReason::Local),
        ];
        expected.sort();
        assert_eq!(skipped, expected);
        let leaves =
            changed_leaf_definitions(&substrate, "tests/test_fees.py", &[range(0, 0, 1, 0)]);
        assert!(leaves.leaves.is_empty());
        assert_eq!(skipped_of(&leaves), [(TEST, SkipReason::TestPath)]);
    }

    #[test]
    fn identical_span_leaves_of_different_symbols_are_ambiguous() {
        let first = "scip-python python sample 1 labels/render().";
        let second = "scip-python python sample 1 labels/render_alias().";
        let definitions = [
            (first, Kind::Function, vec![0, 4, 10], vec![0, 0, 2, 0]),
            (second, Kind::Function, vec![0, 4, 10], vec![0, 0, 2, 0]),
        ];
        let substrate = substrate(&[("labels.py", &definitions)]);
        let leaves = changed_leaf_definitions(&substrate, "labels.py", &[range(1, 0, 2, 0)]);
        assert_eq!(symbols(&leaves), [first, second]);
        assert!(leaves.leaves.iter().all(|leaf| leaf.ambiguous));
    }
}
