//! Look requests: while planning, a `command` is a question about the project
//! or its environment, and the harness answers it ([`executor::look`]
//! classifies). Nothing it does changes the project, raises a permission
//! gate, feeds a check result or a failure memory, or counts as progress.
//!
//! A whole-file view is the read the model would have asked for, with its
//! governing knowledge; everything else is answered here and journaled as a
//! `Look answer` event the model can inspect.

use std::path::Path;

use anyhow::Result;

use super::dispatch::empty_looks_enabled;
use super::model::Action;
use super::Runner;
use crate::harness::executor::{
    self,
    look::{self, Look},
    CommandPermissions,
};

/// How the journal marks a look the harness answered; [`Runner::looking_run`]
/// reads a command followed by it as looking, not progress.
pub(super) const LOOK_REQUEST: &str = "Look request: ";
/// The journaled answer of a look.
const LOOK_ANSWER: &str = "Look answer: ";

/// `MOOSEDEV_HARNESS_PLAN_LOOKS=off` restores the Plan-mode action set
/// without `command`.
pub(super) fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PLAN_LOOKS").map_or(true, |value| value.trim() != "off")
}

/// The Last result's part of a journaled look answer `text` (event
/// `event`), with where the rest is: the shown text is the event's own, so
/// the offset is exact.
fn clipped(text: &str, budget: usize, event: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut end = budget.saturating_sub(96).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[{} more bytes: inspect journal event {event} from offset {end}]",
        &text[..end],
        text.len() - end
    )
}

impl Runner {
    /// Answers the look `command`. A whole-file view of a project file
    /// returns the read to dispatch instead.
    pub(super) async fn answer_look(
        &mut self,
        command: &str,
        files: &[String],
        budget: usize,
    ) -> Result<Option<Action>> {
        let root = self.workspace.root().to_path_buf();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let look = look::classify(command);
        // A path outside the project is answered only under a language's
        // dependency roots, and only in the sandbox, which can read them.
        let place = |path: &str| -> Place {
            let candidate = Path::new(path);
            if !candidate.is_absolute() {
                return match project_relative(path) {
                    Some(relative) => Place::Project(relative),
                    None => Place::Outside,
                };
            }
            if let Ok(rest) = candidate.strip_prefix(&root) {
                return match project_relative(&rest.to_string_lossy()) {
                    Some(relative) => Place::Project(relative),
                    None => Place::Outside,
                };
            }
            match &home {
                Some(home) if look::in_dependency_root(path, home) => Place::Dependency,
                _ => Place::Outside,
            }
        };
        let outside = "it names a path outside the project; looks read the project and its dependencies' source (the cargo registry), nothing else.";
        let (how, answer) = match look {
            Look::FileView { path, lines } => match place(&path) {
                Place::Project(file) if lines.is_none() && files.contains(&file) => {
                    self.event(format!(
                        "{LOOK_REQUEST}{command}\nAnswered by: a read of {file}"
                    ));
                    self.intent_event("look_answered", &format!("a read: {command}"));
                    return Ok(Some(Action::Read { file }));
                }
                // A directory is listed; a file the harness cannot show
                // (protected, binary, too large) says so.
                Place::Project(dir)
                    if dir.is_empty()
                        || files
                            .iter()
                            .any(|file| file.starts_with(&format!("{dir}/"))) =>
                {
                    (
                        "the harness's file list".to_owned(),
                        look::listing(files, &dir, None, false),
                    )
                }
                Place::Project(file) => match self.workspace.read(&file) {
                    Ok(Some(text)) => (
                        format!("the harness, from {file}"),
                        slice(&file, &text, lines.as_deref()),
                    ),
                    Ok(None) => (
                        "the harness".to_owned(),
                        format!("`{file}` does not exist in the project."),
                    ),
                    Err(error) => (
                        "nothing: declined".to_owned(),
                        format!("Declined: `{file}` cannot be shown ({error:#})."),
                    ),
                },
                Place::Dependency => self.sandbox_look(command).await?,
                Place::Outside => (
                    "nothing: declined".to_owned(),
                    format!("Declined: {outside}"),
                ),
            },
            Look::Listing {
                dir,
                name,
                recursive,
            } => match place(&dir) {
                Place::Project(dir) => (
                    "the harness's file list".to_owned(),
                    look::listing(files, &dir, name.as_deref(), recursive),
                ),
                Place::Dependency => self.sandbox_look(command).await?,
                Place::Outside => (
                    "nothing: declined".to_owned(),
                    format!("Declined: {outside}"),
                ),
            },
            Look::Grep {
                pattern,
                paths,
                ignore_case,
                files_only,
                word,
            } => {
                let places: Vec<Place> = paths.iter().map(|path| place(path)).collect();
                if places.iter().any(|place| matches!(place, Place::Outside)) {
                    (
                        "nothing: declined".to_owned(),
                        format!("Declined: {outside}"),
                    )
                } else if places
                    .iter()
                    .any(|place| matches!(place, Place::Dependency))
                {
                    self.sandbox_look(command).await?
                } else {
                    let paths: Vec<String> = places
                        .into_iter()
                        .filter_map(|place| match place {
                            Place::Project(path) => Some(path),
                            _ => None,
                        })
                        .collect();
                    let workspace = &self.workspace;
                    (
                        "the harness, over the project's files".to_owned(),
                        look::grep_files(
                            files,
                            |file| workspace.read(file).ok().flatten(),
                            &pattern,
                            &paths,
                            ignore_case,
                            files_only,
                            word,
                            budget.max(1_000),
                        ),
                    )
                }
            }
            Look::Toolchain { tool, args } => {
                let words: Vec<&str> = args.iter().map(String::as_str).collect();
                match (tool.answer, &home) {
                    (Some(answer), Some(home)) => match answer(&words, home) {
                        Some(text) => ("the harness, by intent".to_owned(), text),
                        None => self.sandbox_look(command).await?,
                    },
                    _ => self.sandbox_look(command).await?,
                }
            }
            Look::Git { args } => match look::run_git(&root, &args).await {
                Ok((success, output)) => (
                    "read-only git, run by the harness".to_owned(),
                    format!(
                        "Success: {success}\n{}",
                        if output.trim().is_empty() {
                            "(no output)"
                        } else {
                            output.as_str()
                        }
                    ),
                ),
                Err(error) => ("nothing: git failed".to_owned(), error),
            },
            Look::Sandbox => self.sandbox_look(command).await?,
            Look::Decline(reason) => (
                "nothing: declined".to_owned(),
                format!("Declined: {reason}"),
            ),
        };
        self.event(format!("{LOOK_REQUEST}{command}\nAnswered by: {how}"));
        let event = self.task.events.len();
        let text = format!("{LOOK_ANSWER}`{command}`, answered by {how}:\n{answer}");
        self.event(text.clone());
        self.intent_event("look_answered", &format!("{how}: {command}"));
        self.task.last_response = clipped(&text, budget, event);
        Ok(None)
    }

    /// Runs a read-only look as asked in the sandbox: no network and no
    /// task write grant, whatever the task holds.
    async fn sandbox_look(&mut self, command: &str) -> Result<(String, String)> {
        let standing = self.command_permissions()?;
        let permissions = CommandPermissions {
            read_paths: standing.read_paths,
            write_paths: Vec::new(),
            network: false,
        };
        let scratch = self.scratch_path();
        let result = executor::command_with_permissions(
            self.workspace.root(),
            &scratch,
            command,
            &permissions,
            self.progress.clone(),
        )
        .await;
        Ok(match result {
            Ok(result) => (
                "the read-only sandbox (no network)".to_owned(),
                format!("Success: {}\n{}", result.success, result.output),
            ),
            Err(error) => (
                "nothing: the sandbox could not run it".to_owned(),
                format!("{error:#}"),
            ),
        })
    }

    /// A look asked again since the last progress, with nothing changed: its
    /// answer is served again and counted as an empty look; the second empty
    /// look since the last progress is the model stuck. True when the step
    /// was answered here.
    pub(super) fn repeated_look(&mut self, command: &str, budget: usize) -> bool {
        if !empty_looks_enabled() {
            return false;
        }
        let at = self.task.events.len() - 1;
        let start = self.progress_window_start();
        let request = format!("{LOOK_REQUEST}{command}\n");
        let Some(earlier) = (start..at)
            .rev()
            .find(|&index| self.task.events[index].message.starts_with(&request))
        else {
            return false;
        };
        // A whole-file view was the read itself, and a read has its own
        // guards (already shown, outlined, refused twice).
        if self.task.events[earlier]
            .message
            .contains("\nAnswered by: a read of ")
        {
            return false;
        }
        // Journaled as a look, so the looking run goes on through it.
        self.event(format!(
            "{LOOK_REQUEST}{command}\nAnswered by: the answer at event {earlier}"
        ));
        let why = format!("look `{command}` already answered at event {earlier}");
        if self.empty_look_repeated(at) {
            self.intent_event("empty_look_repeated", &why);
            self.looking_parked = true;
            self.stop_stuck(
                "look loop",
                format!("Look loop: the model keeps asking what it was already answered ({why}); parked for guidance."),
                format!("The model keeps asking looks it was already answered ({why}), without planning. Guidance is needed: say what to plan, or /plan to change the approach."),
            );
            return true;
        }
        self.intent_event("empty_look", &why);
        self.task.empty_looks.push(at);
        let note = format!(
            "You asked this look at event {earlier} and nothing has changed since; its answer stands. Propose the plan, or ask the human with question.\n"
        );
        let answered = format!("{LOOK_ANSWER}`{command}`,");
        let answer = (start..at)
            .rev()
            .find(|&index| self.task.events[index].message.starts_with(&answered))
            .map(|index| {
                clipped(
                    &self.task.events[index].message,
                    budget.saturating_sub(note.len()),
                    index,
                )
            })
            .unwrap_or_default();
        self.task.last_response = format!("{note}{answer}");
        true
    }
}

enum Place {
    Project(String),
    Dependency,
    Outside,
}

/// A relative path normalized to the project's form (`./a/b` → `a/b`); None
/// when it climbs out (`../x`).
fn project_relative(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The lines of `text` a `head`, `tail` or `sed -n 'a,bp'` view asks for,
/// numbered, under a header naming them.
fn slice(file: &str, text: &str, lines: Option<&str>) -> String {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let (from, to) = match lines.map(|spec| spec.split_once(' ').unwrap_or((spec, ""))) {
        Some(("head", count)) => (1, count.parse().unwrap_or(10).min(total)),
        // `tail -n +N` starts at line N.
        Some(("tail", count)) => match count.strip_prefix('+') {
            Some(from) => (from.parse::<usize>().unwrap_or(1).max(1), total),
            None => {
                let count: usize = count.parse().unwrap_or(10);
                (total.saturating_sub(count) + 1, total)
            }
        },
        Some(("lines", range)) => {
            let (a, b) = range.split_once(',').unwrap_or((range, range));
            let a: usize = a.trim().parse().unwrap_or(1).max(1);
            let b = if b.trim() == "$" {
                total
            } else {
                b.trim().parse().unwrap_or(a)
            };
            (a, b.min(total))
        }
        _ => (1, total),
    };
    if from > to {
        return format!("`{file}` has {total} lines; that range is empty.");
    }
    let body: Vec<String> = (from..=to)
        .map(|number| format!("{number:>5}  {}", all[number - 1]))
        .collect();
    format!("{file}, lines {from}-{to} of {total}:\n{}", body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slice_names_its_lines() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(
            slice("f", text, Some("head 2")),
            "f, lines 1-2 of 4:\n    1  a\n    2  b"
        );
        assert_eq!(
            slice("f", text, Some("tail 1")),
            "f, lines 4-4 of 4:\n    4  d"
        );
        assert_eq!(
            slice("f", text, Some("tail +3")),
            "f, lines 3-4 of 4:\n    3  c\n    4  d"
        );
        assert_eq!(
            slice("f", text, Some("lines 2,3")),
            "f, lines 2-3 of 4:\n    2  b\n    3  c"
        );
        assert!(slice("f", text, Some("lines 9,12")).contains("empty"));
    }

    #[test]
    fn project_paths_normalize_and_never_climb_out() {
        assert_eq!(
            project_relative("./src/../src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(project_relative(".").as_deref(), Some(""));
        assert_eq!(project_relative("../other"), None);
    }

    #[test]
    fn a_long_answer_is_clipped_with_where_the_rest_is() {
        let answer = "x".repeat(500);
        let shown = clipped(&answer, 200, 7);
        assert!(shown.len() <= 200, "{}", shown.len());
        assert!(shown.contains("inspect journal event 7"), "{shown}");
        assert_eq!(clipped("short", 200, 7), "short");
    }
}
