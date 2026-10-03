//! The same failure coming back with nothing edited in between. In badciv run
//! 12 qwen spent about twenty steps reading parse.rs, paging the output and
//! rerunning `cargo test` while `grid_too_few_rows` failed each time, with no
//! edit between the failures. No repeat guard fired, because each action
//! differed (`| tail -30`, `| tail -40`, reads, inspects). The harness can see
//! that the failure is the same and the source is not, so it decides
//! (Constraint cd9f1a96): the second time it shows the failing test's source
//! and the code it calls; the fourth time it parks for the human.
//!
//! A required check is the plan's own measure of done, so its failure shows
//! the focus block the first time (badciv orE: the model paged a failed
//! check's output until the inspect guard parked, in 3 of 6 replicates,
//! before any failure came back). And while a required check fails, a
//! repeated look that would park is steered once instead: the failing test,
//! its values and its source, then a park only if the model keeps looking
//! (the offloading plan's "steer before a park").

use serde::{Deserialize, Serialize};

use super::super::dispatch::error_lines;
use super::super::{Phase, Runner};
use crate::code::substrate::lang::{failed_tests, is_test_path, FailedTest};
use crate::code::substrate::outline;

/// Bytes the focus block may take: about a test and three short functions.
const FOCUS_BYTES: usize = 4_000;
/// Of which the failing test itself, in full up to this.
const TEST_BYTES: usize = 1_500;
/// Definitions the test calls, shown after it.
const MAX_CALLEES: usize = 3;
/// The failure seen this many times with no edit shows the focus block.
const FOCUS_AT: usize = 2;
/// And this many times parks the task for the human.
const PARK_AT: usize = 4;
/// Room kept for the line naming definitions left out.
const OMITTED_RESERVE: usize = 96;
/// A definition given less room than this is named instead of shown.
const MIN_SECTION_BYTES: usize = 160;
/// A failure naming no test and no error line is known by this much output.
const SIGNATURE_OUTPUT_BYTES: usize = 200;
/// How a signature from the output alone begins: the weakest identity.
const OUTPUT_SIGNATURE: &str = "output: ";

/// The failure last seen, how many edits the task had applied then, and how
/// many times in a row it has been seen at that edit count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalledFailure {
    pub signature: String,
    pub edits: usize,
    pub count: usize,
    /// The command that last failed this way. A pass of that command clears
    /// the record; a pass of another (`cargo check` between two `cargo test`
    /// runs) says nothing about this failure.
    #[serde(default)]
    pub command: String,
    /// The source the failure was last seen in: a fingerprint of the
    /// task's code ([`stall_by_state_enabled`]).
    #[serde(default)]
    pub state: String,
    /// The event from which this failure has stood: a source an edit
    /// produced before it is not a return to one this failure was seen
    /// through.
    #[serde(default)]
    pub since: usize,
}

/// A source state a command failed in: its fingerprint, the failure's
/// signature, the command and the event that journaled it. A fact about that
/// source, so a human answer, which restarts the count, keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedState {
    pub state: String,
    pub signature: String,
    pub command: String,
    pub event: usize,
}

/// Failed source states remembered; the oldest go first.
const MAX_FAILED_STATES: usize = 64;

/// A source state an applied edit produced, the event of that edit, and
/// the commands that later passed in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitedState {
    pub state: String,
    pub event: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passed: Vec<String>,
}

/// Visited source states remembered; the oldest go first.
const MAX_VISITED_STATES: usize = 256;

/// A sighting in a source the task held before: the event that showed it,
/// and whether a command failed there (else nothing was run on it).
#[derive(Debug, Clone, Copy)]
struct Revisit {
    at: usize,
    failed_there: bool,
}

/// Lines of a failure's expected and actual values the steer quotes.
const MAX_VALUE_LINES: usize = 6;
/// Bytes those lines may take.
const VALUE_BYTES: usize = 600;

/// `MOOSEDEV_HARNESS_LOOP_DETECTOR=off` switches the detector off for study
/// variants: nothing is tracked, shown or parked.
fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_LOOP_DETECTOR").map_or(true, |value| value.trim() != "off")
}

/// `MOOSEDEV_HARNESS_STALL_BY_STATE=off` counts progress by edits, as before:
/// any edit starts the count again.
fn stall_by_state_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_STALL_BY_STATE").map_or(true, |value| value.trim() != "off")
}

/// `MOOSEDEV_HARNESS_FOCUS_FIRST=off` keeps a required check's first failure
/// to its output, as any command's.
fn focus_first_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_FOCUS_FIRST").map_or(true, |value| value.trim() != "off")
}

/// `MOOSEDEV_HARNESS_STEER=off` parks a repeated look while a required check
/// fails, as before the steer.
fn steer_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_STEER").map_or(true, |value| value.trim() != "off")
}

/// The lines of a failure's output that state its expected and actual
/// values: libtest's `assertion`, `left:` and `right:`, pytest's `E ` lines,
/// and any line naming expected or actual, at most [`MAX_VALUE_LINES`].
fn value_lines(output: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut bytes = 0;
    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        let states_values = line.starts_with("E ")
            || ["assertion", "left:", "right:", "expected", "actual"]
                .iter()
                .any(|start| lower.starts_with(start));
        if !states_values || trimmed.is_empty() {
            continue;
        }
        if lines.len() == MAX_VALUE_LINES || bytes + trimmed.len() > VALUE_BYTES {
            break;
        }
        bytes += trimmed.len();
        lines.push(trimmed.to_string());
    }
    lines
}

/// What makes two failures the same: the failed tests' names, sorted (a
/// `| tail` keeps them while cutting the rest); else the compiler's error
/// lines; else the start of the output.
fn signature(output: &str, tests: &[FailedTest]) -> String {
    if !tests.is_empty() {
        let mut names: Vec<&str> = tests.iter().map(|test| test.name.as_str()).collect();
        names.sort_unstable();
        return format!("tests: {}", names.join(", "));
    }
    let errors = error_lines(output);
    if !errors.is_empty() {
        return format!(
            "errors: {}",
            errors.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    let output = output.trim();
    format!(
        "{OUTPUT_SIGNATURE}{}",
        &output[..floor_boundary(output, SIGNATURE_OUTPUT_BYTES)]
    )
}

/// The failure as the model is told it.
fn described(tests: &[FailedTest], signature: &str) -> String {
    match tests {
        [] if signature.starts_with(OUTPUT_SIGNATURE) => "the same output".into(),
        [] => format!(
            "the same {} error line(s)",
            signature.lines().count().max(1)
        ),
        [test] => format!("test `{}`", test.name),
        [test, rest @ ..] => format!("test `{}` and {} more", test.name, rest.len()),
    }
}

/// The largest char boundary at or below `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// A function's current text, located by the tree-sitter outline of the
/// file's current text: the SCIP index is stale for files just written.
#[derive(Debug, Clone)]
struct Definition {
    file: String,
    name: String,
    start: usize,
    end: usize,
    text: String,
}

/// The named functions of a file's current text.
fn functions(file: &str, text: &str) -> Vec<Definition> {
    let lines: Vec<&str> = text.lines().collect();
    outline(file, text)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.kind == "fn")
        .filter_map(|entry| {
            let name = entry.name?;
            let body = lines.get(entry.line - 1..entry.end_line.min(lines.len()))?;
            Some(Definition {
                file: file.to_string(),
                name,
                start: entry.line,
                end: entry.end_line,
                text: body.join("\n"),
            })
        })
        .collect()
}

/// Where `word` first occurs in `text` as a whole identifier.
fn word_at(text: &str, word: &str) -> Option<usize> {
    let identifier = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).map(|(at, _)| at).find(|&at| {
        !text[..at].chars().next_back().is_some_and(identifier)
            && !text[at + word.len()..]
                .chars()
                .next()
                .is_some_and(identifier)
    })
}

/// One definition as the focus block shows it, cut to `budget` bytes with
/// what was cut named.
fn section(definition: &Definition, role: &str, budget: usize) -> String {
    let header = format!(
        "{}:{}-{} `{}`{role}:\n",
        definition.file, definition.start, definition.end, definition.name
    );
    let text = &definition.text;
    if header.len() + text.len() < budget {
        return format!("{header}{text}\n");
    }
    let notice = |cut: usize| {
        format!(
            "\n[… `{}` continues; {} bytes not shown]\n",
            definition.name,
            text.len() - cut
        )
    };
    // The notice for a cut at 0 is the longest one.
    let cut = floor_boundary(text, budget.saturating_sub(header.len() + notice(0).len()));
    format!("{header}{}{}", &text[..cut], notice(cut))
}

const FOCUS_CLOSING: &str = "Rerunning or rereading without an edit shows this same failure again: edit the code it points at.]\n";
const STEER_CLOSING: &str = "Edit the code the test exercises, or the test if it is wrong; a further look parks for the human.]\n";

impl Runner {
    /// Track a failed command or required check against the edits applied so
    /// far, and return the harness text to put before its output: the focus
    /// block on the second sighting with no edit between, the park message on
    /// the fourth, which also parks the task.
    pub(in crate::harness::runner) fn note_failure(
        &mut self,
        command: &str,
        output: &str,
    ) -> Option<String> {
        if !enabled() {
            return None;
        }
        let tests = failed_tests(output);
        let signature = signature(output, &tests);
        let edits = self.task.edits.len();
        let required = self.is_required_check(command);
        // Progress is a source the failure has not been seen in, not an
        // edit: badciv orH, 3 of 6 local replicates, alternated one test
        // assertion between two values for 2 h, each edit resetting the count.
        let by_state = stall_by_state_enabled();
        let source = if by_state {
            self.source_state()
        } else {
            String::new()
        };
        // The failing command's own event, journaled before it is noted.
        let event = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with("Command: "))
            .unwrap_or_default();
        let unchanged = |stall: &StalledFailure| {
            if by_state {
                stall.state == source
            } else {
                stall.edits == edits
            }
        };
        let state = self.symbolic_state_mut();
        // The event that first showed this failure in exactly this source.
        let known = by_state
            .then(|| {
                state
                    .failed_states
                    .iter()
                    .find(|failed| failed.state == source && failed.signature == signature)
                    .map(|failed| failed.event)
            })
            .flatten();
        let mut revisited = None;
        let count = match state.stalled_failure.as_mut() {
            Some(stall) if unchanged(stall) && stall.signature == signature => {
                stall.count += 1;
                stall.edits = edits;
                stall.command = command.to_string();
                stall.count
            }
            // A failure that names no test and no error (a `grep` that
            // matched nothing) says nothing about the one being tracked.
            Some(stall)
                if unchanged(stall)
                    && signature.starts_with(OUTPUT_SIGNATURE)
                    && !stall.signature.starts_with(OUTPUT_SIGNATURE) =>
            {
                return None;
            }
            // The same failure in a source it was seen in before: the edits
            // since went back and forth, which is not progress.
            Some(stall) if stall.signature == signature && known.is_some() => {
                revisited = known;
                stall.count += 1;
                stall.state = source.clone();
                stall.edits = edits;
                stall.command = command.to_string();
                stall.count
            }
            // The same failure in a new source: progress, so the count starts
            // again.
            Some(stall) if by_state && stall.signature == signature => {
                stall.count = 1;
                stall.state = source.clone();
                stall.edits = edits;
                stall.command = command.to_string();
                1
            }
            _ => {
                revisited = known;
                state.stalled_failure = Some(StalledFailure {
                    signature: signature.clone(),
                    edits,
                    count: 1,
                    command: command.to_string(),
                    state: source.clone(),
                    since: event,
                });
                1
            }
        };
        if by_state && known.is_none() {
            remember_failed_state(
                state,
                FailedState {
                    state: source,
                    signature: signature.clone(),
                    command: command.to_string(),
                    event,
                },
            );
        }
        let what = described(&tests, &signature);
        let first = count == 1 && required && !tests.is_empty() && focus_first_enabled();
        let revisit = revisited.map(|at| Revisit {
            at,
            failed_there: true,
        });
        self.stall_response(&what, &tests, count, command, revisit, first, false)
    }

    /// The harness text for the `count`th sighting of a failure: the park
    /// message from the fourth, which also parks the task; the focus block on
    /// the second, or on a required check's `first` failure; otherwise
    /// nothing. `revisit` names the source the task held before. `no_run`
    /// is set when nothing ran: an edit returned the source to that state,
    /// and every such sighting says so ([`Self::note_source_revisit`]).
    #[allow(clippy::too_many_arguments)]
    fn stall_response(
        &mut self,
        what: &str,
        tests: &[FailedTest],
        count: usize,
        command: &str,
        revisit: Option<Revisit>,
        first: bool,
        no_run: bool,
    ) -> Option<String> {
        let revisited = revisit.map(|revisit| revisit.at);
        let edits = self.task.edits.len();
        if count >= PARK_AT {
            self.intent_event(
                "stalled_failure_parked",
                &format!("{what}, {count} times at edit {edits}: {command}"),
            );
            self.event(if revisited.is_some() {
                format!(
                    "Stalled failure: {what} came back {count} times as the source went back and forth; parked for guidance."
                )
            } else {
                format!(
                    "Stalled failure: {what} came back {count} times with no edit between; parked for guidance."
                )
            });
            self.task.phase = Phase::AwaitingInput;
            self.task.turn_finished = true;
            self.park_under_approved_plan();
            return Some(match revisited {
                Some(at) => format!(
                    "[Harness: the same failure ({what}) has come back {count} times, the last with the source exactly as it was at event {at}: the edits since have gone back and forth without changing the result. Guidance is needed: say which side is wrong, the test or the code it exercises, or /plan to change the approach.]\n"
                ),
                None => format!(
                    "[Harness: the same failure ({what}) has come back {count} times with no edit in between; rerunning, reading and paging have not changed it. Guidance is needed: say what to change, or /plan to change the approach.]\n"
                ),
            });
        }
        // A required check that names its failing test shows where to look
        // the first time it fails.
        if count != FOCUS_AT && !first {
            // A source returned to with no run says what is known of it.
            return revisit.filter(|_| no_run).map(|Revisit { at, failed_there }| {
                if failed_there {
                    format!(
                        "[Harness: this edit returns the source to exactly what it was at event {at}, where `{command}` failed: {what}. That result stands without a rerun; edit the code the test exercises, or the test if it is wrong.]\n"
                    )
                } else {
                    format!(
                        "[Harness: this edit returns the source to exactly what it was after the edit at event {at}; `{command}` has passed in no version since it failed ({what}), so going back is not progress. Edit the code the test exercises, or the test if it is wrong.]\n"
                    )
                }
            });
        }
        self.intent_event(
            "stalled_failure_focus",
            &format!(
                "{what} at edit {edits}{}: {command}",
                if first { ", first failure" } else { "" }
            ),
        );
        let opening = if let Some(Revisit { at, failed_there }) = revisit {
            if failed_there {
                format!(
                    "the failure is back with the source exactly as it was at event {at}, when this check failed the same way: the edits since went back and forth without changing the result, so look at the code the test exercises: {what}"
                )
            } else {
                format!(
                    "the source is back exactly as it was after the edit at event {at}, and `{command}` has passed in no version since it failed: the edits went back and forth without changing the result, so look at the code the test exercises: {what}"
                )
            }
        } else if first {
            format!("a required check failed: {what}")
        } else {
            format!("the same failure again with no edit since: {what}")
        };
        Some(match tests.first() {
            Some(test) => self.focus_block(test, &opening, "", FOCUS_CLOSING),
            None => format!("[Harness: {opening}. {FOCUS_CLOSING}"),
        })
    }

    /// After an applied edit: an edit that returns the task's code to a source
    /// a command already failed in is another sighting of that failure,
    /// without running anything, since the result of exactly that source is
    /// known. Past the auto-verify limit no check runs at all, so a model
    /// alternating a file between two versions was never told its edits went
    /// back and forth (badciv orH1: 104 edits, two states, no check after the
    /// third). Counts toward the focus block and the park like a run; returns
    /// the text to put before the Last result.
    pub(in crate::harness::runner) fn note_source_revisit(&mut self) -> Option<String> {
        if !enabled() || !stall_by_state_enabled() {
            return None;
        }
        // An applied edit always changes the source, so the source it lands
        // in is never the one already counted: A, then B unchecked, then A
        // again is a return.
        let source = self.source_state();
        let edit_event = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with("Applied edit"))
            .unwrap_or_default();
        let edits = self.task.edits.len();
        let symbolic = self.task.symbolic.as_ref()?;
        let known = symbolic
            .failed_states
            .iter()
            .rev()
            .find(|failed| failed.state == source)
            .cloned();
        let visited = symbolic
            .visited_states
            .iter()
            .find(|visited| visited.state == source)
            .cloned();
        let (signature, command, revisit, output) = match (known, visited) {
            (Some(known), _) => {
                let output = self
                    .task
                    .events
                    .get(known.event)
                    .and_then(|event| event.message.splitn(4, '\n').nth(3))
                    .unwrap_or_default()
                    .to_string();
                (
                    known.signature,
                    known.command,
                    Revisit {
                        at: known.event,
                        failed_there: true,
                    },
                    output,
                )
            }
            // A source held before, while a failure stands that no version
            // since has passed (badciv orHA1 cycled through three versions of
            // a test no check ran on).
            // Only a source produced while this failure stood, and one its
            // command never passed in: going back to a version from before
            // the failure is a plausible repair.
            (None, Some(visited)) => {
                let stall = symbolic.stalled_failure.as_ref().filter(|stall| {
                    visited.event > stall.since && !visited.passed.contains(&stall.command)
                })?;
                let output = self.command_output(&stall.command).unwrap_or_default();
                (
                    stall.signature.clone(),
                    stall.command.clone(),
                    Revisit {
                        at: visited.event,
                        failed_there: false,
                    },
                    output,
                )
            }
            (None, None) => {
                remember_visited_state(
                    self.symbolic_state_mut(),
                    VisitedState {
                        state: source,
                        event: edit_event,
                        passed: Vec::new(),
                    },
                );
                return None;
            }
        };
        let tests = failed_tests(&output);
        let state = self.symbolic_state_mut();
        // The result is known, or no version since passed: the harness does
        // not run the checks on it.
        state.auto_verify_armed = None;
        let count = match state.stalled_failure.as_mut() {
            Some(stall) if stall.signature == signature => {
                stall.count += 1;
                stall.state = source;
                stall.edits = edits;
                stall.count
            }
            _ => {
                state.stalled_failure = Some(StalledFailure {
                    signature: signature.clone(),
                    edits,
                    count: 1,
                    command: command.clone(),
                    state: source,
                    since: revisit.at,
                });
                1
            }
        };
        let what = described(&tests, &signature);
        let at = revisit.at;
        self.intent_event(
            "failed_source_revisited",
            &format!(
                "{what}: source as at event {at}{}, sighting {count}",
                if revisit.failed_there {
                    ""
                } else {
                    " (unchecked)"
                }
            ),
        );
        self.event(if revisit.failed_there {
            format!(
                "Source revisited: the code is exactly as it was at event {at}, where `{command}` failed ({what}); not rerun."
            )
        } else {
            format!(
                "Source revisited: the code is exactly as it was after the edit at event {at}; `{command}` has passed in no version since it failed ({what}); not rerun."
            )
        });
        self.stall_response(&what, &tests, count, &command, Some(revisit), false, true)
    }

    /// A fingerprint of the code the task works on: the current text, on
    /// disk, of the plan's files and of every file it edited. Reading or
    /// preloading another file is not a change of the code, so it is left
    /// out.
    fn source_state(&self) -> String {
        let mut files: Vec<String> = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        files.extend(self.task.edits.iter().map(|edit| edit.file.clone()));
        files.sort();
        files.dedup();
        let state: Vec<(String, Option<String>)> = files
            .into_iter()
            .map(|file| {
                let text = self.workspace.read(&file).ok().flatten();
                (file, text)
            })
            .collect();
        let text = serde_json::to_string(&state).unwrap_or_default();
        super::super::fingerprint(&Some(text)).unwrap_or_default()
    }

    /// Whether `command` is one of the approved plan's required checks.
    fn is_required_check(&self, command: &str) -> bool {
        self.task
            .plan
            .as_ref()
            .is_some_and(|plan| plan.checks.iter().any(|check| check == command))
    }

    /// The steer before a park: while a required check is failing at this
    /// source state, a repeated read or inspect that would park gets the
    /// failing test, its expected and actual values and its source once
    /// instead, with the next step that fits. `None` when no steer is due
    /// (switched off, no required check failing here, no named test, or
    /// already steered at this source state): the refusal parks as before.
    pub(in crate::harness::runner) fn steer_before_park(&mut self) -> Option<String> {
        if !enabled() || !steer_enabled() {
            return None;
        }
        let edits = self.task.edits.len();
        let state = self.task.symbolic.as_ref()?;
        let stall = state.stalled_failure.as_ref()?;
        if stall.edits != edits || state.steered_at == Some(edits) {
            return None;
        }
        let command = stall.command.clone();
        let signature = stall.signature.clone();
        if !self.is_required_check(&command) {
            return None;
        }
        let output = self.command_output(&command)?;
        let tests = failed_tests(&output);
        let test = tests.first()?;
        let what = described(&tests, &signature);
        let values = value_lines(&output);
        let values = if values.is_empty() {
            String::new()
        } else {
            format!("\nIts values:\n{}\n", values.join("\n"))
        };
        let steer = self.focus_block(
            test,
            &format!(
                "the required check is still failing, and looking again will not change it: {what}"
            ),
            &values,
            STEER_CLOSING,
        );
        self.symbolic_state_mut().steered_at = Some(edits);
        self.intent_event(
            "steer_before_park",
            &format!("{what} at edit {edits}: {command}"),
        );
        self.event(format!(
            "Steer before park: the required check is still failing ({what}); showed its source instead of parking."
        ));
        Some(steer)
    }

    /// The output of the last run of `command`, from its journaled event.
    fn command_output(&self, command: &str) -> Option<String> {
        let prefix = format!("Command: {command}\n");
        self.task.events.iter().rev().find_map(|event| {
            let rest = event.message.strip_prefix(&prefix)?;
            // Permission grants and Success lines precede the output.
            rest.splitn(3, '\n').nth(2).map(str::to_string)
        })
    }

    /// A pass of the command that last failed clears the record: the failure
    /// it tracked did not come back. A pass in a source remembered as failing
    /// that command (after a grant, say) means that failure no longer stands
    /// there.
    pub(in crate::harness::runner) fn note_pass(&mut self, command: &str) {
        let source = (enabled() && stall_by_state_enabled()).then(|| self.source_state());
        if let Some(state) = self.task.symbolic.as_mut() {
            if let Some(source) = source {
                state
                    .failed_states
                    .retain(|failed| !(failed.state == source && failed.command == command));
                // Going back to a source this command passed in is not a step
                // back for it.
                match state
                    .visited_states
                    .iter_mut()
                    .find(|visited| visited.state == source)
                {
                    Some(visited) => {
                        if !visited.passed.iter().any(|passed| passed == command) {
                            visited.passed.push(command.to_string());
                        }
                    }
                    None => remember_visited_state(
                        state,
                        VisitedState {
                            state: source,
                            event: 0,
                            passed: vec![command.to_string()],
                        },
                    ),
                }
            }
            if state
                .stalled_failure
                .as_ref()
                .is_some_and(|stall| stall.command == command)
            {
                state.stalled_failure = None;
            }
        }
    }

    /// The failing test's source and up to three definitions it calls, from
    /// the files' current text, in at most [`FOCUS_BYTES`]: `opening`, the
    /// test's place, `detail`, the sources, then `closing`.
    fn focus_block(&self, test: &FailedTest, opening: &str, detail: &str, closing: &str) -> String {
        let place = test
            .location
            .as_ref()
            .map(|(file, line)| format!(" ({file}:{line})"))
            .unwrap_or_default();
        let mut block = format!("[Harness: {opening}{place}.{detail}");
        let (found, panicked_in) = self.find_test(test);
        if found.is_none() && panicked_in.is_none() {
            block.push_str(&format!(
                " The test `{}` was not found in the plan's files or the working set. {closing}",
                test.function()
            ));
            return block;
        }
        block.push_str(if block.ends_with('\n') {
            "Its source and the code it calls:\n"
        } else {
            " Its source and the code it calls:\n"
        });
        let mut shown: Vec<(Definition, &str)> = Vec::new();
        if let Some(panicked) = panicked_in {
            shown.push((panicked, " (where it panicked)"));
        }
        if let Some(found) = found {
            block.push_str(&section(&found, " (the test)", TEST_BYTES));
            for callee in self.callees(&found) {
                if shown.len() >= MAX_CALLEES {
                    break;
                }
                if !shown
                    .iter()
                    .any(|(definition, _)| definition.name == callee.name)
                {
                    shown.push((callee, " (called by the test)"));
                }
            }
        }
        let mut omitted = Vec::new();
        let total = shown.len();
        for (index, (definition, role)) in shown.into_iter().enumerate() {
            let room = FOCUS_BYTES.saturating_sub(block.len() + closing.len() + OMITTED_RESERVE);
            let budget = room / (total - index);
            if budget < MIN_SECTION_BYTES {
                omitted.push(format!("`{}`", definition.name));
                continue;
            }
            block.push_str(&section(&definition, role, budget));
        }
        if !omitted.is_empty() {
            block.push_str(&format!("[Not shown for room: {}]\n", omitted.join(", ")));
        }
        block.push_str(closing);
        block
    }

    /// A file's current text: the working set's copy, else the workspace's.
    fn current_text(&self, file: &str) -> Option<String> {
        match self.task.source.get(file) {
            Some(text) => text.clone(),
            None => self.workspace.read(file).ok().flatten(),
        }
    }

    /// The plan's files, then the working set's, once each.
    fn known_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        for file in self.task.source.keys() {
            if !files.contains(file) {
                files.push(file.clone());
            }
        }
        files
    }

    /// A path from a runner's output as the workspace knows it: as printed,
    /// else the known file it names relative to another directory (a crate's
    /// `tests/x.rs` under `badciv-map/`, or an absolute path).
    fn resolve_path(&self, path: &str) -> Option<(String, String)> {
        if let Some(text) = self.current_text(path) {
            return Some((path.to_string(), text));
        }
        self.known_files()
            .into_iter()
            .filter(|file| {
                file.ends_with(&format!("/{path}")) || path.ends_with(&format!("/{file}"))
            })
            .find_map(|file| self.current_text(&file).map(|text| (file, text)))
    }

    /// The failing test's function, and the function it panicked in when that
    /// is another. The panic location names the function when the test's own
    /// assertion failed; otherwise the test is found by name in the plan's
    /// test files, then in the working set.
    fn find_test(&self, test: &FailedTest) -> (Option<Definition>, Option<Definition>) {
        let function = test.function();
        let mut panicked_in = None;
        if let Some((path, line)) = &test.location {
            if let Some((file, text)) = self.resolve_path(path) {
                let line = *line as usize;
                let containing = functions(&file, &text)
                    .into_iter()
                    .filter(|definition| definition.start <= line && line <= definition.end)
                    .max_by_key(|definition| definition.start);
                match containing {
                    Some(definition) if definition.name == function => {
                        return (Some(definition), None);
                    }
                    other => panicked_in = other,
                }
            }
        }
        let files = self.known_files();
        let (tests, others): (Vec<String>, Vec<String>) =
            files.into_iter().partition(|file| is_test_path(file));
        let found = tests.iter().chain(&others).find_map(|file| {
            let text = self.current_text(file)?;
            functions(file, &text)
                .into_iter()
                .find(|definition| definition.name == function)
        });
        (found, panicked_in)
    }

    /// Functions declared in the plan's non-test files whose names the test
    /// uses, in the order the test first uses them.
    fn callees(&self, test: &Definition) -> Vec<Definition> {
        let files: Vec<String> = self
            .task
            .plan
            .as_ref()
            .map(|plan| plan.files.clone())
            .unwrap_or_default();
        let mut used: Vec<(usize, Definition)> = files
            .iter()
            .filter(|file| !is_test_path(file))
            .filter_map(|file| Some((file, self.current_text(file)?)))
            .flat_map(|(file, text)| functions(file, &text))
            .filter(|definition| definition.name != test.name)
            .filter_map(|definition| Some((word_at(&test.text, &definition.name)?, definition)))
            .collect();
        used.sort_by_key(|(at, _)| *at);
        let mut callees: Vec<Definition> = Vec::new();
        for (_, definition) in used {
            if !callees.iter().any(|seen| seen.name == definition.name) {
                callees.push(definition);
            }
        }
        callees
    }
}

/// Remember `visited`, dropping the oldest past [`MAX_VISITED_STATES`].
fn remember_visited_state(state: &mut super::state::SymbolicState, visited: VisitedState) {
    state.visited_states.push(visited);
    let excess = state
        .visited_states
        .len()
        .saturating_sub(MAX_VISITED_STATES);
    state.visited_states.drain(..excess);
}

/// Remember `failed`, dropping the oldest past [`MAX_FAILED_STATES`].
fn remember_failed_state(state: &mut super::state::SymbolicState, failed: FailedState) {
    state.failed_states.push(failed);
    let excess = state.failed_states.len().saturating_sub(MAX_FAILED_STATES);
    state.failed_states.drain(..excess);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(name: &str) -> FailedTest {
        FailedTest {
            name: name.into(),
            location: None,
        }
    }

    #[test]
    fn a_signature_is_the_failed_tests_else_the_errors_else_the_output() {
        let tests = [failed("b"), failed("a")];
        assert_eq!(signature("", &tests), "tests: a, b");
        let errors = "   Compiling x\nerror[E0308]: mismatched types\n  --> src/a.rs:1:1\nerror: could not compile `x`\n";
        assert_eq!(
            signature(errors, &[]),
            "errors: --> src/a.rs:1:1\nerror[E0308]: mismatched types"
        );
        let long = "é".repeat(150);
        let output = signature(&long, &[]);
        assert!(output.starts_with(OUTPUT_SIGNATURE));
        assert_eq!(output.len(), OUTPUT_SIGNATURE.len() + 200);
    }

    #[test]
    fn a_failure_is_described_by_what_identifies_it() {
        assert_eq!(described(&[failed("grid")], "tests: grid"), "test `grid`");
        assert_eq!(
            described(&[failed("a"), failed("b")], "tests: a, b"),
            "test `a` and 1 more"
        );
        assert_eq!(described(&[], "errors: a\nb"), "the same 2 error line(s)");
        assert_eq!(described(&[], "output: x"), "the same output");
    }

    #[test]
    fn identifiers_are_found_whole() {
        assert_eq!(
            word_at("parse_map_all(x); parse_map(y)", "parse_map"),
            Some(18)
        );
        assert_eq!(word_at("reparse_map(x)", "parse_map"), None);
    }

    #[test]
    fn functions_carry_their_whole_current_text() {
        let text = "fn helper() -> u32 {\n    1\n}\n\n#[test]\nfn grid() {\n    assert_eq!(helper(), 2);\n}\n";
        let found = functions("tests/t.rs", text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].name, "grid");
        assert_eq!((found[1].start, found[1].end), (6, 8));
        assert_eq!(
            found[1].text,
            "fn grid() {\n    assert_eq!(helper(), 2);\n}"
        );
    }

    #[test]
    fn a_section_over_budget_is_cut_on_a_char_boundary_and_says_so() {
        let definition = Definition {
            file: "src/a.rs".into(),
            name: "long".into(),
            start: 1,
            end: 2,
            text: "é".repeat(1_000),
        };
        let shown = section(&definition, " (the test)", 400);
        assert!(shown.len() <= 400, "{}", shown.len());
        assert!(shown.starts_with("src/a.rs:1-2 `long` (the test):\n"));
        assert!(shown.contains("`long` continues;"));
        let short = Definition {
            text: "fn long() {}".into(),
            ..definition
        };
        assert_eq!(
            section(&short, "", 400),
            "src/a.rs:1-2 `long`:\nfn long() {}\n"
        );
    }
}
