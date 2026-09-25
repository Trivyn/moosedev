//! Source in the prompt, bounded by scope rather than truncation (Requirement
//! 6ffabf80). `task.source` keeps the full text of every working-set file.
//! Each prompt shows as many of them in full as the source budget holds, in a
//! fixed order the harness decides (Constraint cd9f1a96), and every other file
//! as an outline of its declarations. No file is cut partway and none is left
//! out: the working set is the inventory, as a record list is (AD 6187309b).
use super::Runner;
use crate::code::substrate::outline;
use std::collections::{BTreeMap, BTreeSet};

/// Full source may take this share of the prompt budget, which follows the
/// role's configured context window. It keeps the prompt from growing step
/// after step and holds prefill time down (Lesson af16b95e).
const SOURCE_SHARE_NUMERATOR: usize = 2;
const SOURCE_SHARE_DENOMINATOR: usize = 5;
/// An outline line longer than this is cut at a character boundary.
const OUTLINE_LINE_BYTES: usize = 160;
pub(super) const OUTLINES_HEADER: &str = "Source outlines (these files are in the working set but not shown in full; read one to see it in full before editing it):\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tier {
    Full,
    Outline,
    /// No grammar for the file type: path, size and line count only.
    Listed,
}

impl Tier {
    fn as_str(self) -> &'static str {
        match self {
            Tier::Full => "full",
            Tier::Outline => "outline",
            Tier::Listed => "listed",
        }
    }
}

/// One working-set file as the prompt could show it.
pub(super) struct SourceBlock {
    pub file: String,
    /// Bytes this file adds to the `Current source` JSON object when shown in
    /// full.
    pub full_cost: usize,
    /// The outline (or listing) rendered for the outlines section.
    pub outline: String,
    /// The tier the file takes when it is not shown in full.
    pub short_tier: Tier,
}

#[derive(Debug)]
pub(super) struct Placed {
    pub file: String,
    pub tier: Tier,
    pub bytes: usize,
    pub reason: &'static str,
}

/// What one prompt shows of the working set.
#[derive(Debug)]
pub(super) struct SourceView {
    /// The files shown in full as a JSON object, the `Current source` line.
    pub full_json: String,
    /// The outlines section; empty when every file is shown in full.
    pub outlines: String,
    pub placed: Vec<Placed>,
    /// Bytes of full source this prompt could show.
    pub budget: usize,
    /// Files outlined here that the previous prompt showed in full; the
    /// outlines section names them (see [`swap_notice`]).
    pub swapped: Vec<String>,
}

impl SourceView {
    /// Files this prompt did not show in full.
    pub fn outlined(&self) -> BTreeSet<String> {
        self.placed
            .iter()
            .filter(|placed| placed.tier != Tier::Full)
            .map(|placed| placed.file.clone())
            .collect()
    }

    /// The `source_delivery` receipt: every file's tier, size and reason.
    /// `None` when everything was shown in full, so nothing was shortened.
    pub fn receipt(&self) -> Option<String> {
        if self.placed.iter().all(|placed| placed.tier == Tier::Full) {
            return None;
        }
        Some(
            self.placed
                .iter()
                .map(|placed| {
                    format!(
                        "{} {} ({} bytes, {})",
                        placed.tier.as_str(),
                        placed.file,
                        placed.bytes,
                        placed.reason
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

/// Bytes of full source a prompt may show: its share of the budget, and never
/// more than the budget leaves after the protected part (everything never cut,
/// with every outline) and the observation floor.
pub(super) fn source_budget(limit: usize, protected: usize, observation_floor: usize) -> usize {
    (limit * SOURCE_SHARE_NUMERATOR / SOURCE_SHARE_DENOMINATOR).min(
        limit
            .saturating_sub(protected)
            .saturating_sub(observation_floor),
    )
}

/// Source bytes every prompt carries whatever the budget: each file that could
/// be outlined as its outline (and the section header), each absent file as
/// its `null`.
pub(super) fn protected_source(blocks: &[SourceBlock]) -> usize {
    let outlines: usize = blocks.iter().map(|block| block.outline.len()).sum();
    let absent: usize = blocks
        .iter()
        .filter(|block| block.short_tier == Tier::Full)
        .map(|block| block.full_cost)
        .sum();
    let header = if outlines > 0 {
        OUTLINES_HEADER.len() + swap_notice_bound(blocks)
    } else {
        0
    };
    header + outlines + absent
}

/// The line that names files a prompt shows as outlines although the previous
/// prompt showed them in full. When the working set is larger than the source
/// budget, a model re-reading the files it needs rotates them through the
/// budget without knowing it (badciv 7e0c50eb); the harness knows, so the
/// prompt says so (AD 6166bb16). It is part of the source section, built by
/// the same pure step that picks the tiers, so it never touches the last
/// result and a retry renders it identically.
fn swap_notice(swapped: &[String], files: usize, total: usize, budget: usize) -> String {
    format!(
        "Newly shown only as {} to fit the source budget: {}. The {files} files in the working set hold {total} bytes; {budget} bytes of source fit in full. Read a file again only when you need its full text to edit it.\n",
        if swapped.len() == 1 { "an outline" } else { "outlines" },
        swapped.join(", ")
    )
}

/// The largest the swap notice can be for these files: every file named, and
/// the widest numbers.
fn swap_notice_bound(blocks: &[SourceBlock]) -> usize {
    let names: Vec<String> = blocks.iter().map(|block| block.file.clone()).collect();
    swap_notice(&names, usize::MAX, usize::MAX, usize::MAX).len()
}

/// The file too large to show in full, when the one the model just read does
/// not fit the source budget: showing its outline would only have it read
/// again.
pub(super) struct Oversized {
    pub file: String,
    pub bytes: usize,
    pub budget: usize,
}

/// The output of a journaled `Command:` event that failed. The status is the
/// line after the grants header, never text the command printed.
pub(super) fn failed_command_output(message: &str) -> Option<&str> {
    let (_, rest) = message
        .strip_prefix("Command: ")?
        .split_once("\nPermission grants: ")?;
    let (_grants, rest) = rest.split_once('\n')?;
    match rest.strip_prefix("Success: false") {
        Some(output) if output.is_empty() || output.starts_with('\n') => {
            Some(output.strip_prefix('\n').unwrap_or(output))
        }
        _ => None,
    }
}

/// Working-set files the output names, in the order it first names them. A
/// longer path is matched first and masks the shorter paths inside it, so
/// `src/lib.rs` is not found inside `crates/map/src/lib.rs`.
fn files_named_in(output: &str, files: &[&String]) -> Vec<String> {
    let mut longest_first = files.to_vec();
    longest_first.sort_by_key(|file| std::cmp::Reverse(file.len()));
    let mut taken: Vec<std::ops::Range<usize>> = Vec::new();
    let mut found: Vec<(usize, String)> = Vec::new();
    for file in longest_first {
        let mut first = None;
        for (at, _) in output.match_indices(file.as_str()) {
            let range = at..at + file.len();
            let bounded = output[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')));
            let inside = taken
                .iter()
                .any(|t| t.start <= range.start && range.end <= t.end);
            if bounded && !inside {
                first.get_or_insert(at);
                taken.push(range);
            }
        }
        if let Some(at) = first {
            found.push((at, file.clone()));
        }
    }
    found.sort();
    found.into_iter().map(|(_, file)| file).collect()
}

fn outline_block(file: &str, text: &str) -> (String, Tier) {
    let lines = text.lines().count();
    let Some(entries) = outline(file, text) else {
        return (
            format!(
                "{file} ({} bytes, {lines} lines; no outline for this file type)\n",
                text.len()
            ),
            Tier::Listed,
        );
    };
    let mut block = format!("{file} ({} bytes, {lines} lines):\n", text.len());
    if entries.is_empty() {
        block.push_str("  (no declarations)\n");
    }
    for entry in entries {
        let mut end = entry.text.len().min(OUTLINE_LINE_BYTES);
        while !entry.text.is_char_boundary(end) {
            end -= 1;
        }
        block.push_str(&format!(
            "{}{}: {}\n",
            "  ".repeat(entry.depth + 1),
            entry.line,
            &entry.text[..end]
        ));
    }
    (block, Tier::Outline)
}

impl Runner {
    /// Record that `file` was just read or edited, for the order in which
    /// the next prompt shows source in full. A read and an edit count alike:
    /// ranking an old edit above later reads kept an unrelated file in full
    /// while the files being read rotated out (badciv c75d5d20).
    pub(super) fn touch_source(&mut self, file: &str) {
        self.task.source_recency.retain(|seen| seen != file);
        self.task.source_recency.push(file.to_string());
    }

    /// Forget the working set's order, when the working set itself is
    /// cleared.
    pub(super) fn clear_source_order(&mut self) {
        self.task.source_recency.clear();
        self.task.source_outlined.clear();
        self.task.source_outlined_seen.clear();
    }

    /// The model acted on the last prompt, so it has seen that prompt's
    /// outlines; the swap notice stops naming them.
    pub(super) fn source_outlines_seen(&mut self) {
        self.task.source_outlined_seen = self.task.source_outlined.clone();
    }

    /// Every working-set file, each with the cost of showing it in full and
    /// its outline. A file that does not exist is always shown (`null`), so
    /// its cost belongs to the protected part ([`protected_source`]).
    pub(super) fn source_blocks(&self) -> Vec<SourceBlock> {
        self.task
            .source
            .iter()
            .map(|(file, text)| {
                let key = serde_json::to_string(file).unwrap_or_default().len();
                let value = serde_json::to_string(text).unwrap_or_default().len();
                let (outline, short_tier) = match text {
                    Some(text) => outline_block(file, text),
                    // An absent file costs `null`; it is always shown.
                    None => (String::new(), Tier::Full),
                };
                SourceBlock {
                    file: file.clone(),
                    full_cost: key + 1 + value + 1,
                    outline,
                    short_tier,
                }
            })
            .collect()
    }

    /// The order in which files are shown in full, each with why: the file
    /// the model read or edited last, the files the latest failed command
    /// names, then the rest, most recently read or edited first.
    fn source_ranking(&self) -> Vec<(String, &'static str)> {
        fn push(ranked: &mut Vec<(String, &'static str)>, file: &str, reason: &'static str) {
            if !ranked.iter().any(|(seen, _)| seen == file) {
                ranked.push((file.to_string(), reason));
            }
        }
        let source = &self.task.source;
        let mut ranked = Vec::new();
        let present = |file: &&String| source.contains_key(file.as_str());
        if let Some(file) = self.task.source_recency.iter().rev().find(present) {
            push(&mut ranked, file, "latest");
        }
        // The latest failed command, even when a later command (a listing, a
        // search) succeeded: those do not fix what the failure names.
        let last_failure = self
            .task
            .events
            .iter()
            .rev()
            .find_map(|event| failed_command_output(&event.message));
        if let Some(output) = last_failure {
            let files: Vec<&String> = source.keys().collect();
            for file in files_named_in(output, &files) {
                push(&mut ranked, &file, "failed_output");
            }
        }
        for file in self.task.source_recency.iter().rev().filter(present) {
            push(&mut ranked, file, "recency");
        }
        for file in self.task.read_files.iter().rev().filter(present) {
            push(&mut ranked, file, "recency");
        }
        for file in source.keys() {
            push(&mut ranked, file, "recency");
        }
        ranked
    }

    /// Where a working-set file is displayed: files never edited first, in
    /// the order they were read, then edited files, least recently edited
    /// first. A model server reuses its prefix cache up to the first changed
    /// byte, and this puts the source that changes least before the source
    /// that changes most.
    fn source_stability(&self, file: &str) -> (usize, usize, String) {
        let edited = self
            .task
            .edits
            .iter()
            .rposition(|edit| edit.file == file)
            .map_or(0, |index| index + 1);
        let read = self
            .task
            .read_files
            .iter()
            .position(|read| read == file)
            .unwrap_or(usize::MAX);
        (edited, read, file.to_owned())
    }

    /// Choose each file's tier within `budget` bytes of full source. A file
    /// that does not fit is shown as its outline and the next one is tried.
    /// Errs when the file the model just read or edited cannot fit even
    /// alone.
    pub(super) fn source_view(
        &self,
        blocks: &[SourceBlock],
        budget: usize,
    ) -> Result<SourceView, Oversized> {
        let by_file: BTreeMap<&str, &SourceBlock> = blocks
            .iter()
            .map(|block| (block.file.as_str(), block))
            .collect();
        let mut left = budget;
        let mut full: Vec<(&str, &Option<String>)> = Vec::new();
        let mut placed = Vec::new();
        let mut outlined: Vec<&SourceBlock> = Vec::new();
        for (file, reason) in self.source_ranking() {
            let Some(block) = by_file.get(file.as_str()) else {
                continue;
            };
            let text = &self.task.source[&file];
            let bytes = text.as_ref().map_or(0, String::len);
            // An absent file is always shown, and its `null` is counted with
            // the protected part, not against the source budget.
            if block.short_tier == Tier::Full {
                full.push((block.file.as_str(), text));
                placed.push(Placed {
                    file,
                    tier: Tier::Full,
                    bytes,
                    reason,
                });
                continue;
            }
            if block.full_cost <= left {
                left -= block.full_cost;
                full.push((block.file.as_str(), text));
                placed.push(Placed {
                    file,
                    tier: Tier::Full,
                    bytes,
                    reason,
                });
                continue;
            }
            if reason == "latest" {
                return Err(Oversized {
                    file,
                    bytes,
                    budget,
                });
            }
            outlined.push(block);
            placed.push(Placed {
                file,
                tier: block.short_tier,
                bytes,
                reason: "over_budget",
            });
        }
        let swapped: Vec<String> = placed
            .iter()
            .filter(|placed| {
                placed.tier != Tier::Full && !self.task.source_outlined_seen.contains(&placed.file)
            })
            .map(|placed| placed.file.clone())
            .collect();
        // Ranking decides tiers; display order is stability order, so an
        // edit changes the prompt only from the edited file onward.
        full.sort_by_key(|(file, _)| self.source_stability(file));
        outlined.sort_by_key(|block| self.source_stability(&block.file));
        let mut outlines: String = outlined
            .iter()
            .map(|block| block.outline.as_str())
            .collect();
        if !outlines.is_empty() {
            if !swapped.is_empty() {
                let total: usize = self.task.source.values().flatten().map(String::len).sum();
                outlines.insert_str(
                    0,
                    &swap_notice(&swapped, self.task.source.len(), total, budget),
                );
            }
            outlines.insert_str(0, OUTLINES_HEADER);
        }
        let entries: Vec<String> = full
            .iter()
            .map(|(file, text)| {
                format!(
                    "{}:{}",
                    serde_json::to_string(file).unwrap_or_default(),
                    serde_json::to_string(text).unwrap_or_default()
                )
            })
            .collect();
        Ok(SourceView {
            full_json: format!("{{{}}}", entries.join(",")),
            outlines,
            placed,
            budget,
            swapped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::actions::Step;
    use super::super::model::{Action, PromptOverflow};
    use super::super::task::PendingEdit;
    use super::super::test_support::{context_router, serve, test_config, Project};
    use super::super::Phase;
    use super::*;

    /// Rust source of about `functions` × 200 bytes, with bodies, so its
    /// outline is a small fraction of it.
    fn module(functions: usize) -> String {
        (0..functions)
            .map(|i| {
                format!(
                    "fn item_{i}() -> u32 {{\n    let value = {i};\n    let doubled = value * 2;\n    let tripled = value * 3;\n    let total = value + doubled + tripled;\n    total - {i} * 5 + 7 * 11 - 77\n}}\n\n"
                )
            })
            .collect()
    }

    async fn runner_with(
        project: &Project,
        files: &[(&str, String)],
    ) -> (Runner, tokio::task::JoinHandle<()>) {
        let (daemon, server) = serve(context_router(), project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Fix the build".into())
            .await
            .unwrap();
        runner.configure(test_config(), None);
        for (file, text) in files {
            runner
                .task
                .source
                .insert((*file).into(), Some(text.clone()));
            runner.task.read_files.push((*file).into());
        }
        (runner, server)
    }

    fn full_files(prompt: &str) -> Vec<String> {
        let line = prompt
            .split_once("Current source, refreshed before this action:\n")
            .unwrap()
            .1
            .lines()
            .next()
            .unwrap();
        let full: BTreeMap<String, Option<String>> = serde_json::from_str(line).unwrap();
        full.into_keys().collect()
    }

    fn shared_prefix(left: &str, right: &str) -> usize {
        left.bytes()
            .zip(right.bytes())
            .take_while(|(a, b)| a == b)
            .count()
    }

    #[tokio::test]
    async fn consecutive_prompts_share_everything_before_what_changed() {
        let project = Project::new("prefix-order");
        let (mut runner, server) = runner_with(
            &project,
            &[
                ("a.rs", module(5)),
                ("b.rs", module(6)),
                ("c.rs", module(7)),
            ],
        )
        .await;
        let context = runner.context.clone().unwrap();
        let (first, _) = runner.prompt(&context, &[]).unwrap();

        // A new observation changes nothing before the state block.
        runner.task.last_response = "Read c.rs with its governing knowledge.".into();
        let (second, _) = runner.prompt(&context, &[]).unwrap();
        let state = second.find("\nCurrent human guidance:").unwrap();
        assert!(shared_prefix(&first, &second) >= state);
        assert!(second.find("Current source, refreshed").unwrap() < state);
        assert!(second
            .find("Project rules")
            .is_none_or(|rules| rules < state));

        // An edit moves the edited file to the end of the full source, so the
        // prompt is unchanged up to that file.
        let before = runner.task.source["a.rs"].clone();
        let after = Some(format!("{}// edited\n", before.clone().unwrap()));
        runner.task.source.insert("a.rs".into(), after.clone());
        runner.task.edits.push(PendingEdit {
            file: "a.rs".into(),
            before,
            after,
            reason: String::new(),
            revision: String::new(),
        });
        let (third, _) = runner.prompt(&context, &[]).unwrap();
        let order = |prompt: &str| {
            let at = |file: &str| prompt.find(&format!("\"{file}\":")).unwrap();
            (at("a.rs"), at("b.rs"), at("c.rs"))
        };
        let (a, b, c) = order(&second);
        assert!(a < b && b < c, "files never edited keep their read order");
        let (a, b, c) = order(&third);
        assert!(b < c && c < a, "the edited file is shown last");

        // Editing it again changes nothing before it.
        let before = runner.task.source["a.rs"].clone();
        let after = Some(format!("{}// again\n", before.clone().unwrap()));
        runner.task.source.insert("a.rs".into(), after.clone());
        runner.task.edits.push(PendingEdit {
            file: "a.rs".into(),
            before,
            after,
            reason: String::new(),
            revision: String::new(),
        });
        let (fourth, _) = runner.prompt(&context, &[]).unwrap();
        assert_eq!(order(&fourth), (a, b, c));
        assert!(shared_prefix(&third, &fourth) > a);
        server.abort();
    }

    #[test]
    fn the_full_source_cap_is_two_fifths_of_the_window_budget() {
        // prompt_budget(): min(100,000, (window - 4096) * 3) - 1024.
        for (window_budget, cap) in [(35_840, 14_336), (84_992, 33_996), (98_976, 39_590)] {
            assert_eq!(source_budget(window_budget, 0, 0), cap);
        }
        // Never more than the budget leaves after the protected part and the
        // observation floor.
        assert_eq!(source_budget(84_992, 70_000, 8_000), 6_992);
        assert_eq!(source_budget(84_992, 84_000, 8_000), 0);
    }

    #[tokio::test]
    async fn source_beyond_its_budget_is_outlined_in_a_fixed_order() {
        let project = Project::new("source-tiers");
        let big = module(100);
        assert!(big.len() > 15_000 && big.len() < 20_000, "{}", big.len());
        let (mut runner, server) = runner_with(
            &project,
            &[
                ("a.rs", big.clone()),
                ("b.rs", big.clone()),
                ("c.rs", module(10)),
            ],
        )
        .await;
        let context = runner.context.clone().unwrap();
        runner.touch_source("a.rs");
        runner.touch_source("b.rs");
        runner.touch_source("c.rs");

        // The latest touch, then the next most recent; the third does not
        // fit.
        let (prompt, view) = runner.prompt(&context, &[]).unwrap();
        assert_eq!(full_files(&prompt), ["b.rs", "c.rs"]);
        assert!(prompt.contains(OUTLINES_HEADER), "{prompt}");
        assert!(prompt.contains("a.rs ("), "{prompt}");
        assert!(prompt.contains("  1: fn item_0() -> u32 {"), "{prompt}");
        assert_eq!(view.outlined(), BTreeSet::from(["a.rs".to_string()]));
        let receipt = view.receipt().unwrap();
        assert!(
            receipt.contains("full c.rs (") && receipt.contains("latest"),
            "{receipt}"
        );
        assert!(
            receipt.contains("outline a.rs (") && receipt.contains("over_budget"),
            "{receipt}"
        );

        // The file a failed command names goes right after the latest touch,
        // ahead of a more recently touched one.
        runner.event("Command: cargo build\nPermission grants: none\nSuccess: false\nerror[E0308]: mismatched types\n --> a.rs:12:5".to_string());
        // A later command that succeeded fixes nothing the failure names.
        runner.event("Command: ls\nPermission grants: none\nSuccess: true\nb.rs\n".to_string());
        let (prompt, view) = runner.prompt(&context, &[]).unwrap();
        assert_eq!(full_files(&prompt), ["a.rs", "c.rs"]);
        assert!(view.receipt().unwrap().contains("failed_output"));

        // Nothing is shortened when everything fits: no receipt, no section.
        runner.task.source.remove("a.rs");
        runner.task.source.remove("b.rs");
        let (prompt, view) = runner.prompt(&context, &[]).unwrap();
        assert!(!prompt.contains(OUTLINES_HEADER));
        assert!(view.receipt().is_none());
        server.abort();
    }

    /// badciv c75d5d20: the model read three files in turn while an old
    /// edit held a slot, so each read pushed another of the three back to
    /// an outline and the model read it again, for 70 events.
    #[tokio::test]
    async fn files_read_in_turn_all_stay_in_full() {
        let project = Project::new("source-rotation");
        let file = module(50);
        assert!(file.len() > 8_000 && file.len() < 10_000, "{}", file.len());
        let (mut runner, server) = runner_with(
            &project,
            &[
                ("a.rs", file.clone()),
                ("b.rs", file.clone()),
                ("c.rs", file.clone()),
                ("d.rs", file.clone()),
            ],
        )
        .await;
        let context = runner.context.clone().unwrap();
        runner.touch_source("d.rs"); // edited once, long ago
        for round in 0..2 {
            for read in ["a.rs", "b.rs", "c.rs"] {
                runner.touch_source(read);
                let (prompt, view) = runner.prompt(&context, &[]).unwrap();
                if round == 1 || read == "c.rs" {
                    assert_eq!(
                        full_files(&prompt),
                        ["a.rs", "b.rs", "c.rs"],
                        "after reading {read} in round {round}"
                    );
                    assert_eq!(view.outlined(), BTreeSet::from(["d.rs".to_string()]));
                }
            }
        }
        server.abort();
    }

    /// Source gives way to the observations the step will show, not to a
    /// fixed floor: a one-line read result leaves source more room than a
    /// long command output does.
    #[tokio::test]
    async fn source_reserves_only_the_observations_the_step_shows() {
        let project = Project::new("source-reserve");
        let (mut runner, server) = runner_with(&project, &[("a.rs", module(100))]).await;
        let mut context = runner.context.clone().unwrap();
        // Knowledge large enough that what is left, not the share, bounds source.
        context.context = "k".repeat(45_000);
        runner.task.last_response = "Read a.rs with its governing knowledge.".into();
        let short = runner.source_view_for(&context).unwrap().budget;
        runner.task.last_response = "x".repeat(20_000);
        let long = runner.source_view_for(&context).unwrap().budget;
        assert!(short > long, "{short} vs {long}");
        assert!(
            short - long < 8_000,
            "never more than the floor: {short} vs {long}"
        );
        server.abort();
    }

    /// badciv 7e0c50eb: five files one short of the budget, read in a cycle.
    /// Each read says which file it pushed to an outline, and the sizes.
    #[tokio::test]
    async fn a_file_pushed_to_an_outline_is_named_in_the_source_section() {
        let project = Project::new("source-swap");
        let file = module(40);
        let names = ["a.rs", "b.rs", "c.rs", "d.rs"];
        let files: Vec<(&str, String)> = names.iter().map(|name| (*name, file.clone())).collect();
        let (mut runner, server) = runner_with(&project, &files).await;
        let mut context = runner.context.clone().unwrap();
        for name in names {
            runner.touch_source(name);
        }
        // Pad knowledge until exactly one of the four no longer fits.
        let mut pad = 0;
        while runner
            .source_view_for(&context)
            .unwrap()
            .outlined()
            .is_empty()
        {
            pad += 500;
            context.context = "k".repeat(pad);
        }
        for round in 0..2 {
            for name in names {
                let (_, view) = runner.prompt(&context, &[]).unwrap();
                runner.task.source_outlined = view.outlined();
                assert_eq!(
                    runner.task.source_outlined.len(),
                    1,
                    "round {round}: one file outlined"
                );
                let outlined = runner.task.source_outlined.iter().next().unwrap().clone();
                if outlined != name {
                    continue;
                }
                // The model reads the outlined file: the next prompt shows it in
                // full and names the file that took its place as an outline.
                runner.touch_source(name);
                let (prompt, view) = runner.prompt(&context, &[]).unwrap();
                assert_eq!(view.swapped.len(), 1, "{view:?}");
                assert_ne!(view.swapped[0], name);
                let notice = format!(
                    "Newly shown only as an outline to fit the source budget: {}. The 4 files in the working set hold",
                    view.swapped[0]
                );
                assert!(prompt.contains(&notice), "{prompt}");
                // The same prompt built again (a retry) says the same thing.
                assert_eq!(runner.prompt(&context, &[]).unwrap().0, prompt);
            }
        }
        // A repair prompt, rebuilt after a rejected action, names it again;
        // once an action on that prompt is accepted, it is not named again.
        let (_, view) = runner.prompt(&context, &[]).unwrap();
        runner.task.source_outlined = view.outlined();
        let (repair, _) = runner.prompt(&context, &[]).unwrap();
        assert!(repair.contains("Newly shown only as"), "{repair}");
        runner.source_outlines_seen();
        let (prompt, view) = runner.prompt(&context, &[]).unwrap();
        assert!(view.swapped.is_empty());
        assert!(!prompt.contains("Newly shown only as"));
        server.abort();
    }

    #[tokio::test]
    async fn an_edit_to_an_outlined_file_reads_it_first() {
        let project = Project::new("source-guard");
        let big = module(100);
        let (mut runner, server) =
            runner_with(&project, &[("a.rs", big.clone()), ("b.rs", big.clone())]).await;
        let context = runner.context.clone().unwrap();
        runner.touch_source("a.rs");
        runner.touch_source("b.rs");
        let (_, view) = runner.prompt(&context, &[]).unwrap();
        runner.task.source_outlined = view.outlined();
        assert!(runner.task.source_outlined.contains("a.rs"));
        let edit = || Action::Replace {
            file: "a.rs".into(),
            old_text: "let value = 0;".into(),
            new_text: "let value = 1;".into(),
        };
        let step = runner.validate_action(edit()).unwrap();
        assert!(matches!(&step, Step::Read { file } if file == "a.rs"));
        assert!(runner
            .task
            .events
            .last()
            .unwrap()
            .message
            .starts_with("Edit guard: a.rs was shown only as an outline"));

        // The read makes it the latest read: the next prompt shows it in full
        // and the edit is no longer turned into a read.
        runner.touch_source("a.rs");
        let (prompt, view) = runner.prompt(&context, &[]).unwrap();
        assert!(full_files(&prompt).contains(&"a.rs".to_string()));
        runner.task.source_outlined = view.outlined();
        let step = runner.validate_action(edit());
        assert!(!matches!(step, Ok(Step::Read { .. })));
        server.abort();
    }

    #[tokio::test]
    async fn a_prompt_that_cannot_fit_stops_for_the_human_before_any_request() {
        let project = Project::new("source-overflow");
        // The file the model just read is too large to show in full.
        let (mut runner, server) = runner_with(&project, &[("huge.rs", module(300))]).await;
        let context = runner.context.clone().unwrap();
        runner.touch_source("huge.rs");
        let error = runner.prompt(&context, &[]).unwrap_err();
        let overflow = error.downcast_ref::<PromptOverflow>().unwrap();
        assert_eq!(overflow.file.as_ref().unwrap().0, "huge.rs");

        // The protected part alone is over the budget: advance stops the task
        // with the sizes and what to do, and asks no model.
        runner.task.source.clear();
        runner.task.read_files.clear();
        runner.task.objective = "x".repeat(120_000);
        runner.advance().await.unwrap();
        assert_eq!(runner.task.phase, Phase::AwaitingInput);
        assert_eq!(
            runner.task.last_error_kind.as_deref(),
            Some("context_overflow")
        );
        let guidance = &runner.task.last_response;
        assert!(
            guidance.starts_with("Stopped before asking the model"),
            "{guidance}"
        );
        assert!(
            guidance.contains("context_window_tokens 32768"),
            "{guidance}"
        );
        assert!(
            guidance.contains("instructions and task state"),
            "{guidance}"
        );
        assert!(runner.task.model_requests.is_empty());

        // The answer returns the task to Plan with an empty working set, so it
        // does not build the same prompt again, even from Auto.
        runner.task.mode = super::super::Mode::Auto;
        runner.task.read_files.push("a.rs".into());
        runner.task.source.insert("a.rs".into(), Some(module(1)));
        runner.touch_source("a.rs");
        runner
            .answer("Only the parser, please.".into())
            .await
            .unwrap();
        assert_eq!(runner.task.mode, super::super::Mode::Plan);
        assert_eq!(runner.task.phase, Phase::Planning);
        assert!(runner.task.read_files.is_empty() && runner.task.source.is_empty());
        assert!(runner.task.source_recency.is_empty());
        server.abort();
    }

    #[test]
    fn a_failed_command_output_is_read_from_its_header_only() {
        let failed = "Command: cargo build\nPermission grants: none\nSuccess: false\nerror[E0425]: src/lib.rs:3";
        assert_eq!(
            failed_command_output(failed),
            Some("error[E0425]: src/lib.rs:3")
        );
        let passed = "Command: echo 'Success: false'\nPermission grants: none\nSuccess: true\nSuccess: false";
        assert_eq!(failed_command_output(passed), None);
        assert_eq!(failed_command_output("Read src/lib.rs: x"), None);
    }

    #[test]
    fn named_files_follow_the_output_and_longer_paths_mask_shorter() {
        let (lib, map_lib, parse) = (
            "src/lib.rs".to_string(),
            "map/src/lib.rs".to_string(),
            "map/src/parse.rs".to_string(),
        );
        let output = "error: --> map/src/parse.rs:4:1\nerror: --> map/src/lib.rs:9:2\n";
        assert_eq!(
            files_named_in(output, &[&lib, &map_lib, &parse]),
            vec![parse.clone(), map_lib.clone()],
            "src/lib.rs appears only inside map/src/lib.rs"
        );
        assert_eq!(
            files_named_in("see xsrc/lib.rs", &[&lib]),
            Vec::<String>::new(),
            "a path must start at a boundary"
        );
    }
}
