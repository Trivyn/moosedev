//! Facts the harness checks beside the capture note, for the human who
//! reviews it. badciv be128e71 finished with `parse_map` and `write_map`
//! stubbed and only an ignored test, and a note describing validation code
//! that did not exist was accepted: the review showed the note and nothing
//! against it. Each fact here is read from the task and the disk, never from
//! the model.

use std::collections::BTreeSet;

use anyhow::Result;

use super::super::Runner;
use crate::code::substrate::lang::{file_name, is_language_file};
use crate::code::substrate::outline;

/// Identifiers from the note listed at most, so the line stays readable.
const NOTE_NAMES: usize = 6;

impl Runner {
    /// Compute the evidence for the current capture note and keep it on the
    /// note's state; each fact is journaled once, when it first appears.
    pub(in crate::harness::runner) fn record_review_evidence(&mut self) -> Result<()> {
        let evidence = self.review_evidence();
        let Some(state) = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.capture_note.as_mut())
        else {
            return Ok(());
        };
        if state.evidence == evidence {
            return Ok(());
        }
        let new: Vec<String> = evidence
            .iter()
            .filter(|fact| !state.evidence.contains(fact))
            .cloned()
            .collect();
        state.evidence = evidence;
        for fact in new {
            self.intent_event("review_evidence", &fact);
        }
        self.persist()
    }

    fn review_evidence(&self) -> Vec<String> {
        let mut facts = Vec::new();
        let Some(plan) = self.task.plan.as_ref() else {
            return facts;
        };
        if !self.task.check_results.is_empty() {
            let passed: u64 = self
                .task
                .check_results
                .iter()
                .filter(|result| result.success)
                .map(|result| super::super::dispatch::tests_passed(&result.output))
                .sum();
            facts.push(if passed == 0 {
                "Required checks passed, but no test passed: a test listed as ignored or skipped verified nothing.".to_string()
            } else {
                format!("Required checks passed {passed} test(s).")
            });
        }
        // Stubs the plan says it leaves are still stubs: the review names
        // them apart, so a scaffold's are not read as unfinished work.
        let listing = |stubs: &[(String, usize, String)]| {
            stubs
                .iter()
                .take(8)
                .map(|(file, line, marker)| format!("{file}:{line} {marker}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (left, stubs) = self.planned_stubs_split();
        if !stubs.is_empty() {
            facts.push(format!("Stubs left in planned files: {}.", listing(&stubs)));
        }
        if !left.is_empty() {
            facts.push(format!("Stubs the plan leaves: {}.", listing(&left)));
        }
        let (kept, withheld, _) = self.addressed_split();
        if !withheld.is_empty() {
            facts.push(format!(
                "Motivated-by edges withheld: stubs left in planned files ({} of {} addressed rules).",
                withheld.len(),
                withheld.len() + kept.len()
            ));
        }
        let edited: BTreeSet<&str> = self
            .task
            .edits
            .iter()
            .map(|edit| edit.file.as_str())
            .collect();
        let planned: BTreeSet<&str> = self
            .task
            .approved_plans
            .iter()
            .flat_map(|approved| approved.files.iter())
            .chain(plan.files.iter())
            .map(String::as_str)
            .collect();
        let untouched: Vec<&str> = planned.difference(&edited).copied().collect();
        if !untouched.is_empty() {
            facts.push(format!(
                "Planned files not edited: {}.",
                untouched.join(", ")
            ));
        }
        if let Some(note) = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.capture_note.as_ref())
        {
            let changed = self.changed_code();
            let files: BTreeSet<&str> = edited
                .iter()
                .chain(planned.iter())
                .copied()
                .chain(self.task.read_files.iter().map(String::as_str))
                .collect();
            let absent: Vec<String> = note_names(&note.note)
                .into_iter()
                .filter(|name| !names_a_file(name, &files))
                .filter(|name| !changed.touches(name))
                .take(NOTE_NAMES)
                .collect();
            if !absent.is_empty() {
                facts.push(format!(
                    "The note names {}, which no edit in this task added or changed.",
                    absent
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        facts
    }

    /// What the task's edits changed: each edited file's final text against
    /// its text before the first edit.
    fn changed_code(&self) -> ChangedCode {
        let mut first_before = std::collections::BTreeMap::new();
        let mut last_after = std::collections::BTreeMap::new();
        for edit in &self.task.edits {
            first_before
                .entry(edit.file.as_str())
                .or_insert(edit.before.as_deref());
            last_after.insert(edit.file.as_str(), edit.after.as_deref());
        }
        let mut changed = ChangedCode::default();
        for (file, after) in last_after {
            let before = first_before.get(file).copied().flatten();
            changed.add(file, before.unwrap_or_default(), after.unwrap_or_default());
        }
        changed
    }
}

/// The added or changed lines of a task's edits, and the declarations whose
/// span holds one.
#[derive(Default)]
struct ChangedCode {
    lines: Vec<String>,
    declarations: BTreeSet<String>,
}

impl ChangedCode {
    fn add(&mut self, file: &str, before: &str, after: &str) {
        let before: BTreeSet<&str> = before.lines().collect();
        let mut numbers = Vec::new();
        for (index, line) in after.lines().enumerate() {
            if !before.contains(line) {
                numbers.push(index + 1);
                self.lines.push(line.to_owned());
            }
        }
        // A declaration whose body changed was changed, though the line
        // naming it was not.
        for entry in outline(file, after).unwrap_or_default() {
            if let Some(name) = entry.name {
                if numbers
                    .iter()
                    .any(|number| (entry.line..=entry.end_line).contains(number))
                {
                    self.declarations.insert(name);
                }
            }
        }
    }

    /// Whether an edit added or changed code `name` names: a changed line
    /// holding it, or a changed declaration of that name.
    fn touches(&self, name: &str) -> bool {
        let last = name.rsplit([':', '.']).next().unwrap_or(name);
        self.declarations.contains(last) || self.lines.iter().any(|line| contains_word(line, name))
    }
}

/// Code names the note mentions: text in backticks that reads as one
/// identifier or path (`parse_map`, `MapError::GridWrongRowCount`), and
/// words written as a call (`validate_grid(`).
fn note_names(note: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let identifier = |text: &str| {
        text.len() >= 3
            && text.chars().any(|c| c.is_alphabetic())
            && text
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.')
            && !text.starts_with('.')
    };
    for (index, part) in note.split('`').enumerate() {
        let part = part.trim().trim_end_matches("()");
        if index % 2 == 1 && identifier(part) {
            names.push(part.to_string());
        }
    }
    let bytes = note.as_bytes();
    for (at, _) in note.match_indices('(') {
        let start = note[..at]
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        let word = &note[start..at];
        if at > 0 && bytes[at - 1] != b' ' && identifier(word) && word.contains('_') {
            names.push(word.to_string());
        }
    }
    let mut seen = BTreeSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

/// Whether a note name is a file rather than code: one of the task's files,
/// or its base name, or anything with a registered language's extension.
/// badciv P5's review said "The note names `lib.rs`, which no edit in this
/// task added or changed" of a file the task had edited.
fn names_a_file(name: &str, files: &BTreeSet<&str>) -> bool {
    files
        .iter()
        .any(|file| *file == name || file_name(file) == name)
        || is_language_file(name)
}

/// Whether `line` holds `name` as a whole word (a path's last segment
/// counts: `Map::tile` is found by `tile`).
fn contains_word(line: &str, name: &str) -> bool {
    let last = name.rsplit([':', '.']).next().unwrap_or(name);
    [name, last].iter().any(|needle| {
        line.match_indices(needle).any(|(at, found)| {
            let before = line[..at].chars().next_back();
            let after = line[at + found.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            boundary(before) && boundary(after)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{contains_word, names_a_file, note_names, ChangedCode};
    use std::collections::BTreeSet;

    #[test]
    fn a_note_name_that_is_a_file_is_not_code() {
        let files: BTreeSet<&str> = ["badciv-map/src/lib.rs", "badciv-map/Makefile"].into();
        // badciv P5: `lib.rs` is the base name of an edited file.
        assert!(names_a_file("lib.rs", &files));
        assert!(names_a_file("badciv-map/src/lib.rs", &files));
        assert!(names_a_file("Makefile", &files));
        // A registered language's extension is a file whatever the task touched.
        assert!(names_a_file("codes.rs", &files));
        assert!(names_a_file("labels.py", &files));
        // Code stays code, dotted paths included.
        for name in ["parse_map", "MapError::Parse", "self.grid", "os.path"] {
            assert!(!names_a_file(name, &files), "{name}");
        }
    }

    #[test]
    fn a_note_names_code_in_backticks_and_calls() {
        let note = "I decided to implement the validation logic as a separate pass (`validate_map`) that calls check_grid(map), and the `SCRATCHMAP 1` magic is enforced by `parse_map()`; see `MapError::GridWrongRowCount`.";
        assert_eq!(
            note_names(note),
            [
                "validate_map",
                "parse_map",
                "MapError::GridWrongRowCount",
                "check_grid"
            ]
        );
        assert!(contains_word(
            "pub fn validate_map(map: &Map) {",
            "validate_map"
        ));
        assert!(!contains_word("pub fn validate_maps() {", "validate_map"));
        assert!(contains_word(
            "    GridWrongRowCount { expected: u32 },",
            "MapError::GridWrongRowCount"
        ));
    }

    #[test]
    fn a_declaration_whose_body_changed_is_touched() {
        let before = "pub fn parse_map(text: &str) -> u32 {\n    unimplemented!()\n}\n\npub fn write_map() {}\n";
        let after = "pub fn parse_map(text: &str) -> u32 {\n    text.len() as u32\n}\n\npub fn write_map() {}\n";
        let mut changed = ChangedCode::default();
        changed.add("src/parse.rs", before, after);
        assert!(changed.touches("parse_map"));
        assert!(changed.touches("crate::parse::parse_map"));
        assert!(!changed.touches("write_map"));
    }
}
