//! Capture anchors: the code entities a captured proposal's links target. A
//! changed file anchors to the leaf definitions its hunks touch, proven
//! against the loaded index; a file with no resolved definition anchor falls
//! back to its module. A projection only: the caller queues the links.
use super::scope::{
    changed_leaf_definitions, intersects, validate_files, validate_source_ranges, ChangedLeaf,
};
use crate::code::substrate::{DefinitionEntry, SourceRange, Substrate};
use crate::harness::protocol::*;

/// Definition anchors per file and per proposal. Past either bound the first
/// ones in file, range and symbol order are kept and the module is added.
const MAX_FILE_ANCHORS: usize = 8;
const MAX_PROPOSAL_ANCHORS: usize = 16;
/// The runner's per-file hunk bound across the most files a capture names.
const MAX_CHANGED_RANGES: usize = 32 * 64;

#[derive(Default)]
pub(super) struct ProposalAnchors {
    /// `(raw symbol, file)` in link order, frozen into the operation journal.
    pub(super) links: Vec<(String, String)>,
    pub(super) anchors: Vec<CaptureAnchor>,
    pub(super) notes: Vec<AnchorNote>,
    pub(super) unanchored: Vec<String>,
}

impl ProposalAnchors {
    /// Keep the anchors `keep` accepts, with their links in lockstep.
    pub(super) fn retain(
        &mut self,
        mut keep: impl FnMut(&CaptureAnchor) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        let links = std::mem::take(&mut self.links);
        let anchors = std::mem::take(&mut self.anchors);
        for (link, anchor) in links.into_iter().zip(anchors) {
            if keep(&anchor)? {
                self.links.push(link);
                self.anchors.push(anchor);
            }
        }
        Ok(())
    }
}

/// The runner's hunk geometry: unique repository paths, ordered ranges, and
/// original-source ranges pairwise with the changed ones when present.
pub(super) fn validate_changed(changed: &[ChangedFile]) -> anyhow::Result<()> {
    validate_files(
        &changed
            .iter()
            .map(|change| change.file.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut ranges = 0usize;
    for change in changed {
        anyhow::ensure!(
            change.before_ranges.is_empty()
                || change.before_ranges.len() == change.changed_ranges.len(),
            "before ranges must pair with the changed ranges of `{}`",
            change.file
        );
        for range in change.changed_ranges.iter().chain(&change.before_ranges) {
            anyhow::ensure!(
                range.start <= range.end,
                "changed range ends before it starts"
            );
        }
        ranges += change.changed_ranges.len();
    }
    anyhow::ensure!(ranges <= MAX_CHANGED_RANGES, "too many changed ranges");
    Ok(())
}

/// Anchors for one proposal's files. `substrate` is `None` when no index is
/// loaded or the record kind has no legal code predicate: every file is then
/// unanchored.
pub(super) fn proposal_anchors(
    substrate: Option<&Substrate>,
    changed: &[ChangedFile],
    files: &[String],
) -> anyhow::Result<ProposalAnchors> {
    let mut result = ProposalAnchors::default();
    let Some(substrate) = substrate else {
        result.unanchored = files.to_vec();
        return Ok(result);
    };
    let mut ordered: Vec<&String> = files.iter().collect();
    ordered.sort();
    ordered.dedup();
    let mut definitions = 0usize;
    for file in ordered {
        let mut leaves: Vec<DefinitionEntry> = Vec::new();
        let mut with_module = false;
        if let Some(change) = changed.iter().find(|change| &change.file == file) {
            if change.ranges_coalesced {
                result
                    .notes
                    .push(note(file, AnchorNoteKind::RangesCoalesced));
            }
            match proven_leaves(substrate, change)? {
                Proof::Leaves(proven) => {
                    if proven.iter().any(|leaf| leaf.ambiguous) {
                        result
                            .notes
                            .push(note(file, AnchorNoteKind::AnchorAmbiguous));
                        with_module = true;
                    }
                    leaves.extend(
                        proven
                            .into_iter()
                            .filter(|leaf| !leaf.ambiguous)
                            .map(|leaf| leaf.scope.definition.entry),
                    );
                }
                Proof::Unproven => result.notes.push(note(file, AnchorNoteKind::IndexUnproven)),
                Proof::NotIndexed => {}
            }
        }
        let capacity = MAX_FILE_ANCHORS.min(MAX_PROPOSAL_ANCHORS - definitions);
        if leaves.len() > capacity {
            leaves.truncate(capacity);
            result
                .notes
                .push(note(file, AnchorNoteKind::AnchorOverflow));
            with_module = true;
        }
        definitions += leaves.len();
        for entry in &leaves {
            push(&mut result, file, entry, AnchorBasis::Definition);
        }
        if leaves.is_empty() || with_module {
            match module_anchor(substrate, file) {
                Some(entry) => push(&mut result, file, &entry, AnchorBasis::Module),
                None if leaves.is_empty() => result.unanchored.push(file.clone()),
                None => {}
            }
        }
    }
    Ok(result)
}

enum Proof {
    Leaves(Vec<ChangedLeaf>),
    /// The index covers the file but proves neither side of the change.
    Unproven,
    /// The index does not cover the file at all.
    NotIndexed,
}

/// Leaves proven against the loaded index. An index of the changed source
/// takes the changed ranges. An index of the original source (Rust's, which
/// is not refreshed after an edit) takes the original ranges and keeps only
/// definitions whose name token no hunk touches: a rewritten name may not
/// exist any more.
fn proven_leaves(substrate: &Substrate, change: &ChangedFile) -> anyhow::Result<Proof> {
    let Some(indexed) = substrate.indexed_source_digest(&change.file) else {
        return Ok(if substrate.covers_file(&change.file) {
            Proof::Unproven
        } else {
            Proof::NotIndexed
        });
    };
    if change.after_digest.as_deref() == Some(indexed.as_str()) {
        if let Some(source) = substrate.read_indexed_source(&change.file) {
            validate_source_ranges(&source, &change.changed_ranges)?;
        }
        return Ok(Proof::Leaves(
            changed_leaf_definitions(substrate, &change.file, &change.changed_ranges).leaves,
        ));
    }
    if change.before_digest.as_deref() == Some(indexed.as_str())
        && change.before_ranges.len() == change.changed_ranges.len()
    {
        let hunks: Vec<SourceRange> = change
            .before_ranges
            .iter()
            .map(|range| (*range).into())
            .collect();
        let mut leaves =
            changed_leaf_definitions(substrate, &change.file, &change.before_ranges).leaves;
        leaves.retain(|leaf| {
            hunks
                .iter()
                .all(|hunk| !intersects(leaf.scope.definition.range, *hunk))
        });
        return Ok(Proof::Leaves(leaves));
    }
    Ok(Proof::Unproven)
}

/// The file's module entity: a producer's module definition, else the
/// synthetic whole-file module (rust-analyzer's), which anchors only.
fn module_anchor(substrate: &Substrate, file: &str) -> Option<DefinitionEntry> {
    substrate
        .definitions_in_file(file)
        .into_iter()
        .filter(|definition| {
            definition.entry.is_module && definition.entry.display_name.as_deref() != Some("tests")
        })
        .min_by_key(|definition| definition.entry.symbol.matches('/').count())
        .map(|definition| definition.entry)
        .or_else(|| substrate.file_module_symbol(file))
}

/// Append an anchor unless its normalized symbol is already anchored.
fn push(result: &mut ProposalAnchors, file: &str, entry: &DefinitionEntry, basis: AnchorBasis) {
    if result
        .anchors
        .iter()
        .any(|anchor| anchor.symbol == entry.normalized_symbol)
    {
        return;
    }
    result.links.push((entry.symbol.clone(), file.to_string()));
    result.anchors.push(CaptureAnchor {
        file: file.to_string(),
        symbol: entry.normalized_symbol.clone(),
        name: entry.display_name.clone(),
        basis,
    });
}

fn note(file: &str, note: AnchorNoteKind) -> AnchorNote {
    AnchorNote {
        file: file.to_string(),
        note,
    }
}
