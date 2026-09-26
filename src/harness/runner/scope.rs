//! The approved change scope and the hunk geometry that feeds association
//! derivation and capture anchoring. Daemon contracts are validated here at task creation.
use super::*;
use crate::harness::protocol::{ChangedFile, HarnessSourcePosition, HarnessSourceRange};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedChangeScope {
    pub version: u8,
    pub knowledge_revision: String,
    pub files: BTreeMap<String, Option<String>>,
    pub obligation_iris: Vec<String>,
    #[serde(default)]
    pub definition_scopes: Vec<ApprovedDefinitionScope>,
    pub checks: Vec<String>,
    pub approval_cycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedDefinitionScope {
    pub file: String,
    pub symbol: String,
    pub source_digest: String,
}

impl Runner {
    pub(super) fn validate_daemon_contracts(context: &ContextResponse) -> Result<()> {
        anyhow::ensure!(
            context.capture_contracts.contains(&3),
            "project daemon does not advertise capture contract v3; upgrade the daemon before creating a new harness task"
        );
        anyhow::ensure!(
            context.intent_contracts.contains(&2),
            "project daemon does not advertise intent contract v2; upgrade the daemon before creating a new harness task"
        );
        anyhow::ensure!(
            context.context_contracts.contains(&1),
            "project daemon does not advertise context contract v1; upgrade the daemon before creating a new harness task"
        );
        Ok(())
    }
}

/// Per-file hunk bound: past it the change coalesces to one range per side.
const MAX_HUNKS: usize = 64;
/// Changed-middle line bound on either side; the line diff's table grows with
/// the product of both, so past it the change coalesces too.
const MAX_DIFF_LINES: usize = 2000;

pub(super) fn changed_files(edits: &[PendingEdit]) -> Result<Vec<ChangedFile>> {
    let mut chains: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for edit in edits {
        chains
            .entry(edit.file.clone())
            .and_modify(|(_, after)| {
                *after = edit.after.clone();
            })
            .or_insert_with(|| (edit.before.clone(), edit.after.clone()));
    }
    Ok(chains
        .into_iter()
        .map(|(file, (before, after))| {
            let hunks = hunks(
                before.as_deref().unwrap_or_default(),
                after.as_deref().unwrap_or_default(),
            );
            ChangedFile {
                file,
                before_digest: fingerprint(&before),
                after_digest: fingerprint(&after),
                changed_ranges: hunks.after,
                before_ranges: hunks.before,
                ranges_coalesced: hunks.coalesced,
            }
        })
        .collect())
}

/// The after-side hunk ranges of one edit and whether they coalesced.
pub(super) fn edit_ranges(before: &str, after: &str) -> (Vec<HarnessSourceRange>, bool) {
    let hunks = hunks(before, after);
    (hunks.after, hunks.coalesced)
}

/// One change's hunk ranges on each side, pairwise, or one prefix/suffix range
/// per side when the change exceeds the hunk or line bound.
struct Hunks {
    after: Vec<HarnessSourceRange>,
    before: Vec<HarnessSourceRange>,
    coalesced: bool,
}

/// Line hunks of an original-to-final change. Lines keep their terminators, so
/// a line-ending change is a hunk; an insertion or a deletion is zero-width on
/// the side without it. A missing side (a created or deleted file) is empty.
fn hunks(before: &str, after: &str) -> Hunks {
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let coalesced = || Hunks {
        after: vec![changed_range_between(after, before)],
        before: vec![changed_range_between(before, after)],
        coalesced: true,
    };
    if old.len() - prefix - suffix > MAX_DIFF_LINES || new.len() - prefix - suffix > MAX_DIFF_LINES
    {
        return coalesced();
    }
    let mut spans: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let (mut old_line, mut new_line) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    for step in diff::slice(&old, &new) {
        match step {
            diff::Result::Both(..) => {
                if let Some((old_start, new_start)) = open.take() {
                    spans.push(((old_start, old_line), (new_start, new_line)));
                }
                old_line += 1;
                new_line += 1;
            }
            diff::Result::Left(_) => {
                open.get_or_insert((old_line, new_line));
                old_line += 1;
            }
            diff::Result::Right(_) => {
                open.get_or_insert((old_line, new_line));
                new_line += 1;
            }
        }
    }
    if let Some((old_start, new_start)) = open {
        spans.push(((old_start, old_line), (new_start, new_line)));
    }
    if spans.len() > MAX_HUNKS {
        return coalesced();
    }
    Hunks {
        after: spans
            .iter()
            .map(|(_, (start, end))| line_range(after, *start, *end))
            .collect(),
        before: spans
            .iter()
            .map(|((start, end), _)| line_range(before, *start, *end))
            .collect(),
        coalesced: false,
    }
}

/// Lines `[start, end)` as an end-exclusive range. A line past the last newline
/// ends at that final line's column, the daemon validator's convention.
fn line_range(source: &str, start: usize, end: usize) -> HarnessSourceRange {
    let newlines = source.bytes().filter(|byte| *byte == b'\n').count();
    let position = |line: usize| {
        if line <= newlines {
            HarnessSourcePosition {
                line: line as u32,
                col: 0,
            }
        } else {
            let last = source.rsplit('\n').next().unwrap_or_default();
            HarnessSourcePosition {
                line: newlines as u32,
                col: last.strip_suffix('\r').unwrap_or(last).len() as u32,
            }
        }
    };
    HarnessSourceRange {
        start: position(start),
        end: position(end),
    }
}

fn changed_range_between(source: &str, other: &str) -> HarnessSourceRange {
    let mut prefix = source
        .bytes()
        .zip(other.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !source.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = source[prefix..]
        .bytes()
        .rev()
        .zip(
            other.as_bytes()[prefix.min(other.len())..]
                .iter()
                .rev()
                .copied(),
        )
        .take_while(|(a, b)| a == b)
        .count();
    while suffix > 0 && !source.is_char_boundary(source.len() - suffix) {
        suffix -= 1;
    }
    let position = |offset: usize| {
        let head = &source[..offset.min(source.len())];
        let line = head.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let col = head
            .rsplit_once('\n')
            .map_or(head.len(), |(_, tail)| tail.len()) as u32;
        HarnessSourcePosition { line, col }
    };
    HarnessSourceRange {
        start: position(prefix),
        end: position(source.len().saturating_sub(suffix)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(file: &str, before: Option<&str>, after: Option<&str>) -> PendingEdit {
        PendingEdit {
            file: file.into(),
            before: before.map(Into::into),
            after: after.map(Into::into),
            reason: String::new(),
            revision: "r1".into(),
        }
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> HarnessSourceRange {
        HarnessSourceRange {
            start: HarnessSourcePosition {
                line: start.0,
                col: start.1,
            },
            end: HarnessSourcePosition {
                line: end.0,
                col: end.1,
            },
        }
    }

    fn one(before: Option<&str>, after: Option<&str>) -> ChangedFile {
        let mut changed = changed_files(&[edit("code.py", before, after)]).unwrap();
        assert_eq!(changed.len(), 1);
        changed.remove(0)
    }

    #[test]
    fn sequential_edits_are_rebased_to_one_original_to_final_change() {
        let edits = vec![
            edit("code.py", Some("abc\n"), Some("abXc\n")),
            edit("code.py", Some("abXc\n"), Some("QabXc\n")),
        ];
        let changed = changed_files(&edits).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].before_digest, fingerprint(&Some("abc\n".into())));
        assert_eq!(
            changed[0].after_digest,
            fingerprint(&Some("QabXc\n".into()))
        );
        assert_eq!(changed[0].changed_ranges, vec![range((0, 0), (1, 0))]);
        assert_eq!(changed[0].before_ranges, vec![range((0, 0), (1, 0))]);
    }

    #[test]
    fn separated_changes_are_separate_hunks_in_both_coordinates() {
        let changed = one(Some("a\nb\nc\nd\ne\n"), Some("a\nB\nc\nd\nE\nf\n"));
        assert_eq!(
            changed.changed_ranges,
            vec![range((1, 0), (2, 0)), range((4, 0), (6, 0))]
        );
        assert_eq!(
            changed.before_ranges,
            vec![range((1, 0), (2, 0)), range((4, 0), (5, 0))]
        );
        assert!(!changed.ranges_coalesced);
    }

    #[test]
    fn insertions_and_deletions_are_zero_width_on_the_other_side() {
        let inserted = one(Some("a\nc\n"), Some("a\nb\nc\n"));
        assert_eq!(inserted.changed_ranges, vec![range((1, 0), (2, 0))]);
        assert_eq!(inserted.before_ranges, vec![range((1, 0), (1, 0))]);
        let deleted = one(Some("a\nb\nc\n"), Some("a\nc\n"));
        assert_eq!(deleted.changed_ranges, vec![range((1, 0), (1, 0))]);
        assert_eq!(deleted.before_ranges, vec![range((1, 0), (2, 0))]);
        let created = one(None, Some("a\n"));
        assert_eq!(created.changed_ranges, vec![range((0, 0), (1, 0))]);
        assert_eq!(created.before_ranges, vec![range((0, 0), (0, 0))]);
        let removed = one(Some("a\n"), None);
        assert_eq!(removed.changed_ranges, vec![range((0, 0), (0, 0))]);
        assert_eq!(removed.before_ranges, vec![range((0, 0), (1, 0))]);
    }

    #[test]
    fn line_ending_changes_are_hunks_and_unterminated_lines_end_at_their_column() {
        let crlf = one(Some("a\r\nb\r\n"), Some("a\nb\r\n"));
        assert_eq!(crlf.changed_ranges, vec![range((0, 0), (1, 0))]);
        assert_eq!(crlf.before_ranges, vec![range((0, 0), (1, 0))]);
        let unterminated = one(Some("a\nb"), Some("a\nbb"));
        assert_eq!(unterminated.changed_ranges, vec![range((1, 0), (1, 2))]);
        assert_eq!(unterminated.before_ranges, vec![range((1, 0), (1, 1))]);
    }

    #[test]
    fn past_the_hunk_or_line_bound_the_change_coalesces_to_one_range() {
        let alternating = |lines: usize| -> (String, String) {
            let before = (0..lines).map(|i| format!("line {i}\n")).collect();
            let after = (0..lines)
                .map(|i| {
                    if i % 2 == 0 {
                        format!("changed {i}\n")
                    } else {
                        format!("line {i}\n")
                    }
                })
                .collect();
            (before, after)
        };
        let (before, after) = alternating(128);
        let bounded = one(Some(&before), Some(&after));
        assert!(!bounded.ranges_coalesced);
        assert_eq!(bounded.changed_ranges.len(), MAX_HUNKS);
        let (before, after) = alternating(130);
        let coalesced = one(Some(&before), Some(&after));
        assert!(coalesced.ranges_coalesced);
        assert_eq!(
            coalesced.changed_ranges,
            vec![changed_range_between(&after, &before)]
        );
        assert_eq!(
            coalesced.before_ranges,
            vec![changed_range_between(&before, &after)]
        );
        let before: String = (0..=MAX_DIFF_LINES).map(|i| format!("old {i}\n")).collect();
        let after: String = (0..=MAX_DIFF_LINES).map(|i| format!("new {i}\n")).collect();
        let long = one(Some(&before), Some(&after));
        assert!(long.ranges_coalesced);
        assert_eq!(long.changed_ranges.len(), 1);
    }

    #[test]
    fn a_daemon_without_capture_contract_3_is_refused() {
        let context = |capture_contracts: Vec<u32>| ContextResponse {
            project_root: String::new(),
            revision: String::new(),
            context: String::new(),
            files: vec![],
            records: vec![],
            evidence_iris: vec![],
            delivery_receipt: None,
            capture_contracts,
            intent_contracts: vec![2],
            context_contracts: vec![1],
            governing_rules: vec![],
            approved_specs: vec![],
        };
        let error = Runner::validate_daemon_contracts(&context(vec![2]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("capture contract v3"), "{error}");
        Runner::validate_daemon_contracts(&context(vec![2, 3])).unwrap();
        // A daemon from before `rule_files` is refused before any task starts.
        let mut old = context(vec![2, 3]);
        old.context_contracts.clear();
        let error = Runner::validate_daemon_contracts(&old)
            .unwrap_err()
            .to_string();
        assert!(error.contains("context contract v1"), "{error}");
    }
}
