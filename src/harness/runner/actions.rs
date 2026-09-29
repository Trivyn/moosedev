//! Validate sensor arguments and materialize edits before permission or execution.
use super::dispatch::READ_REFUSED;
use super::symbolic::{code_at, quote_open};
use super::{
    model::{Action, ReplyThen},
    plan_choices, Mode, OpenChoice, Runner, MAX_FILES, MAX_PLAN_SUMMARY,
};
use crate::code::substrate::lang::stub_syntax_for;
use crate::code::substrate::outline;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};

/// A validated action the dispatcher can execute without re-checking its
/// arguments: bounds hold, the target was read, and `replace`/`write` are
/// already materialized as one whole-file `Edit`. Serializes exactly like the
/// corresponding `Action` so the journal records what the model proposed.
#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(super) enum Step {
    Inspect {
        event: usize,
        offset: usize,
    },
    Reply {
        message: String,
        #[serde(skip_serializing_if = "ReplyThen::is_wait")]
        then: ReplyThen,
    },
    Read {
        file: String,
    },
    /// A model read of a file whose current text the prompt already covers.
    /// It journals as the read the model proposed; dispatch refuses it
    /// instead of rotating the source tiers (badciv 40cef4a5).
    #[serde(rename = "read")]
    ReadRefused {
        file: String,
        #[serde(skip)]
        reason: String,
    },
    /// A model read of a file outlined only for space whose earlier read is
    /// still current. It journals as the read the model proposed; dispatch
    /// serves the file's current text as the Last result, leaving the source
    /// tiers and the working set as they are.
    #[serde(rename = "read")]
    ReadOutlined {
        file: String,
    },
    /// A model read of an existing file outside the step's non-empty scope
    /// that the model has not read. It journals as the read the model
    /// proposed; dispatch serves the file's current text as the Last result
    /// without adding it to the working set.
    #[serde(rename = "read")]
    ReadOutsideScope {
        file: String,
    },
    Search {
        query: String,
    },
    Plan {
        summary: String,
        files: Vec<String>,
        checks: Vec<String>,
        addresses: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        satisfied: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        open_choices: Vec<OpenChoice>,
    },
    Edit {
        file: String,
        before: Option<String>,
        after: Option<String>,
    },
    Command {
        command: String,
    },
    RequestPermission {
        command: String,
        justification: String,
        read_paths: Vec<String>,
        write_paths: Vec<String>,
        network: bool,
    },
    Question {
        question: String,
    },
    Replan {
        reason: String,
    },
    Finish {
        summary: String,
    },
}

impl Runner {
    pub(super) fn validate_permission(&self, action: &Action) -> Result<()> {
        match action {
            Action::Edit { file, .. }
            | Action::Replace { file, .. }
            | Action::Write { file, .. } => {
                ensure!(
                    self.task.mode == Mode::Auto,
                    "Plan mode cannot edit code; human plan approval is required"
                );
                ensure!(
                    self.task
                        .plan
                        .as_ref()
                        .is_some_and(|p| p.files.contains(file)),
                    "edit is outside approved file scope; return to Plan"
                );
            }
            Action::ApplyFix { fix } => {
                ensure!(
                    self.task.mode == Mode::Auto,
                    "Plan mode cannot edit code; human plan approval is required"
                );
                // An unknown number is the model's mistake, refused as it
                // materializes. A fix outside the plan became a scope-escape
                // replan before this, as any edit does; this is the backstop.
                if let Some(offered) = self
                    .task
                    .diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.fix(*fix))
                {
                    ensure!(
                        self.task
                            .plan
                            .as_ref()
                            .is_some_and(|p| p.files.contains(&offered.file)),
                        "fix {fix} edits {}, outside approved file scope; return to Plan",
                        offered.file
                    );
                }
            }
            Action::Command { .. } | Action::RequestPermission { .. } | Action::Finish { .. } => {
                ensure!(
                    self.task.mode == Mode::Auto,
                    "Plan mode cannot execute or finish code work; human approval is required"
                )
            }
            Action::Plan { .. } => ensure!(
                self.task.mode == Mode::Plan,
                "switch to Plan before changing the approved approach"
            ),
            _ => {}
        }
        Ok(())
    }

    pub(super) fn validate_action(&mut self, action: Action) -> Result<Step> {
        // A narrowed repair (the repair lever) offers only these; argument
        // values are not schema-checked, so the file list is enforced here.
        if let Some(files) = self.narrowed_files() {
            match &action {
                Action::Write { file, .. } => ensure!(
                    files.contains(file),
                    "write is offered now only for the planned files that do not exist yet: {}",
                    files.join(", ")
                ),
                Action::Read { .. } | Action::Question { .. } => {}
                _ => anyhow::bail!(
                    "only read, question, or a write to {} is offered now",
                    files.join(", ")
                ),
            }
        }
        match &action {
            Action::Plan {
                summary,
                files,
                checks,
                open_choices,
                ..
            } => {
                if plan_choices::enabled() {
                    plan_choices::validate(open_choices)?;
                }
                ensure!(
                    !summary.trim().is_empty(),
                    "plan requires a nonempty summary"
                );
                ensure!(
                    summary.len() <= MAX_PLAN_SUMMARY,
                    "plan summary is {} bytes; the bound is {MAX_PLAN_SUMMARY}",
                    summary.len()
                );
                ensure!(
                    !files.is_empty() && files.len() <= MAX_FILES,
                    "plan lists {} file paths; it requires 1..{MAX_FILES} explicit file paths",
                    files.len()
                );
                ensure!(
                    !checks.is_empty()
                        && checks.len() <= 20
                        && checks
                            .iter()
                            .all(|c| !c.trim().is_empty() && c.len() <= 4000),
                    "plan requires 1..20 nonempty verification commands of at most 4000 bytes each"
                );
                // A check runs verbatim through /bin/sh. A description written in
                // its place fails with exit 127 only after approval and work,
                // and small models read that failure as the environment's.
                for (index, check) in checks.iter().enumerate() {
                    if let Some(reason) =
                        crate::harness::executor::unrunnable_reason(self.workspace.root(), check)
                    {
                        self.intent_event(
                            "plan_check_rejected",
                            &format!("check {}: {reason}", index + 1),
                        );
                        anyhow::bail!(
                            "plan check {} is not a runnable shell command: {reason}. Each check runs verbatim through /bin/sh in the project root, so write a command line (for example the task's verification commands), not a description of what should be verified",
                            index + 1
                        );
                    }
                    // A check whose tool finds its project by a manifest the
                    // project lacks and the plan does not create fails at
                    // finish, where the fix is outside the approved files.
                    if let Some(reason) = crate::harness::executor::missing_project_reason(
                        self.workspace.root(),
                        check,
                        files,
                    ) {
                        self.intent_event(
                            "plan_check_rejected",
                            &format!("check {}: {reason}", index + 1),
                        );
                        anyhow::bail!("plan check {} cannot find its project: {reason}", index + 1);
                    }
                }
            }
            Action::Inspect { event, offset } => {
                let observation = self
                    .task
                    .events
                    .get(*event)
                    .context("unknown journal event; choose a supplied event index")?;
                ensure!(
                    *offset <= observation.message.len()
                        && observation.message.is_char_boundary(*offset),
                    "inspect offset must be a UTF-8 boundary within the selected event"
                );
            }
            Action::Search { query } => ensure!(!query.is_empty(), "search query cannot be empty"),
            Action::Reply { message, .. } => {
                ensure!(!message.trim().is_empty(), "reply message cannot be empty")
            }
            Action::Command { command } => {
                ensure!(
                    !command.trim().is_empty() && command.len() <= 4000,
                    "command must contain 1..4000 bytes"
                );
            }
            Action::RequestPermission {
                command,
                justification,
                read_paths,
                write_paths,
                network,
            } => {
                ensure!(
                    !command.trim().is_empty() && command.len() <= 4000,
                    "permission request command must contain 1..4000 bytes"
                );
                ensure!(
                    !justification.trim().is_empty() && justification.len() <= 2000,
                    "permission request justification must contain 1..2000 bytes"
                );
                ensure!(
                    !read_paths.is_empty() || !write_paths.is_empty() || *network,
                    "permission request must name at least one capability (read_paths, write_paths or network). When the sandbox denial named no path outside the project and no network need, no grant can help: ask the human with question, or choose a command that runs without a terminal"
                );
                ensure!(
                    read_paths.len() <= 32 && write_paths.len() <= 32,
                    "permission request supports at most 32 read and 32 write paths"
                );
            }
            _ => {}
        }
        let file = match action {
            Action::Replace { ref file, .. }
            | Action::Write { ref file, .. }
            | Action::Edit { ref file, .. } => file,
            Action::Inspect { event, offset } => return Ok(Step::Inspect { event, offset }),
            Action::Reply { message, then } => return Ok(Step::Reply { message, then }),
            Action::Read { file } => return Ok(self.read_step(file)),
            Action::Search { query } => return Ok(Step::Search { query }),
            Action::Plan {
                summary,
                files,
                checks,
                addresses,
                satisfied,
                open_choices,
            } => {
                // Switched off, a field is not offered; any sent anyway are
                // dropped rather than judged.
                let open_choices = if plan_choices::enabled() {
                    plan_choices::open_choices(open_choices)
                } else {
                    Vec::new()
                };
                let satisfied = if super::rule_state::plan_satisfied_enabled() {
                    satisfied
                } else {
                    Vec::new()
                };
                return Ok(Step::Plan {
                    summary,
                    files,
                    checks,
                    addresses,
                    satisfied,
                    open_choices,
                });
            }
            Action::Command { command } => {
                return Ok(Step::Command {
                    command: self.without_missing_cd(command),
                })
            }
            Action::RequestPermission {
                command,
                justification,
                read_paths,
                write_paths,
                network,
            } => {
                return Ok(Step::RequestPermission {
                    command,
                    justification,
                    read_paths,
                    write_paths,
                    network,
                })
            }
            Action::Question { question } => return Ok(Step::Question { question }),
            Action::Replan { reason } => return Ok(Step::Replan { reason }),
            Action::Finish { summary } => return Ok(Step::Finish { summary }),
            Action::ApplyFix { fix } => return self.materialize_fix(fix),
        };
        if !self.task.read_files.contains(file) {
            // The next generation receives the source and dossier. Never apply a
            // proposal whose author did not see the governing source snapshot.
            self.event(format!("First-edit guard: requesting source and dossier for {file}; the unread edit proposal will not execute."));
            return Ok(Step::Read { file: file.clone() });
        }
        if self.task.source_outlined.contains(file) && !self.served_in_full(file) {
            // An outline is not the source: an edit written from it would
            // guess the text it replaces. Read it, which shows it in full next.
            // A Last result that is the file's whole current text, served for
            // a read of it, is the source.
            self.event(format!("Edit guard: {file} was shown only as an outline; showing its full source. The edit proposal will not execute."));
            return Ok(Step::Read { file: file.clone() });
        }
        let before = self
            .task
            .source
            .get(file)
            .context("source snapshot missing; read the target before editing")?
            .clone();
        let (file, after) = match action {
            Action::Replace {
                file,
                old_text,
                new_text,
            } => {
                let source = before
                    .as_deref()
                    .context("replace requires an existing file; use write to create one")?;
                ensure!(!old_text.is_empty(), "replace old_text must not be empty");
                let count = occurrences(source, &old_text);
                let decoded = (count == 0)
                    .then(|| decode_json_escapes(&old_text))
                    .flatten();
                let (old_text, new_text) = if count == 1 {
                    (old_text, new_text)
                } else if count == 0 && already_applied(source, &old_text, &new_text) {
                    // The model is proposing an edit it applied earlier (badciv
                    // e3c533b4 re-sent an `#[ignore]` it had added, three times,
                    // and parked). That is a no-op, which runs the checks, not a
                    // failed match.
                    return Err(anyhow::Error::new(super::model::NoopEdit));
                } else if let (0, Some(repair)) = (count, repair_literal_span(source, &old_text)) {
                    let trimmed_new = strip_same_junk(&new_text, &repair);
                    let detail = super::bounded(
                        &format!(
                            "{file}: trimmed old_text prefix {:?} suffix {:?}; new_text {}",
                            repair.prefix,
                            repair.suffix,
                            if trimmed_new == new_text {
                                "unchanged"
                            } else {
                                "lost the same junk"
                            }
                        ),
                        2000,
                    );
                    self.intent_event("replace_text_repair", &detail);
                    self.event(format!("Replace text repair: {detail}. The trimmed old_text matches the supplied source exactly once."));
                    (repair.span, trimmed_new)
                } else if let Some(span) = decoded
                    .as_deref()
                    .filter(|span| occurrences(source, span) == 1)
                {
                    // The prompt shows source JSON-encoded; a model that copies that
                    // rendering sends `\"` for every `"`. Decode new_text only when
                    // it is escaped the same way throughout, so authored text with a
                    // real `\"` inside a string literal is never altered.
                    let (new_text, decoded_new) = match consistently_escaped(&new_text)
                        .then(|| decode_json_escapes(&new_text))
                        .flatten()
                    {
                        Some(decoded) => (decoded, ", new_text"),
                        None => (new_text, ""),
                    };
                    // new_text left as sent is written as sent: literal `\n`
                    // escapes in it land in code (badciv P5 attempts 2 and 3
                    // mixed real line breaks with `\n` in one new_text).
                    if decoded_new.is_empty() {
                        if let Some(line) = escaped_newline_in_code(&file, &new_text) {
                            self.intent_event(
                                "replace_escapes_refused",
                                &format!("{file}: new_text line {line}"),
                            );
                            anyhow::bail!("new_text contains literal \\n escapes outside string literals on line {line}; send the replacement with real line breaks");
                        }
                    }
                    let detail =
                        format!("{file}: decoded JSON string escapes in old_text{decoded_new}");
                    self.intent_event("replace_text_repair", &detail);
                    self.event(format!("Replace text repair: {detail}. The decoded old_text matches the supplied source exactly once."));
                    (span.to_owned(), new_text)
                } else {
                    let hint = match decoded {
                        Some(span) if occurrences(source, &span) > 1 => "; old_text matches only after decoding JSON escapes, and then more than once; widen it".to_owned(),
                        _ => first_absent_line(source, &old_text)
                            .map(|line| format!("; first old_text line absent from the file: {line:?}"))
                            .unwrap_or_default(),
                    };
                    anyhow::bail!("replace old_text must match exactly once; found {count}{}; use the supplied source to select a unique literal span{hint}", if count == 2 { " or more" } else { "" });
                };
                if let Some(detail) = whole_file_rewrite(source, &old_text, &new_text) {
                    let detail = format!("{file}: {detail}");
                    self.intent_event("edit_whole_file", &detail);
                    self.event(format!(
                        "Whole-file rewrite: {detail}. The edit was applied. Select the span you are changing: a replace that resends the file costs the window it needs for the rest of the task, and repeating it is how an edit loop starts."
                    ));
                }
                (file, Some(source.replacen(&old_text, &new_text, 1)))
            }
            Action::Write { file, content } => {
                ensure!(
                    before.is_some() || content.is_some(),
                    "`{file}` does not exist, so there is nothing to delete. To create it, send its whole text in `content`."
                );
                if let (Some(before), Some(after), true) =
                    (before.as_deref(), content.as_deref(), write_guard_enabled())
                {
                    if let Some(deleted) = deleted_declarations(&file, before, after) {
                        let names = listed_names(&deleted, DELETED_NAMES_SHOWN);
                        self.intent_event(
                            "destructive_write_refused",
                            &format!("{file}: {}", deleted.join(", ")),
                        );
                        anyhow::bail!("This write deletes {names} from {file}. To add to a file use replace on a span, or write the whole file including what it already declares.");
                    }
                }
                (file, content)
            }
            Action::Edit {
                file,
                before: supplied,
                after,
            } => {
                ensure!(supplied == before, "legacy edit.before must equal the entire supplied file; use replace for a unique fragment or write for full content");
                (file, after)
            }
            _ => unreachable!("only edit-shaped actions reach materialization"),
        };
        if before == after {
            return Err(anyhow::Error::new(super::model::NoopEdit));
        }
        Ok(Step::Edit {
            file,
            before,
            after,
        })
    }
}

/// Whether a `write` to an existing file that deletes most of what it
/// declares is refused. `MOOSEDEV_HARNESS_WRITE_GUARD=off` applies it, as
/// before.
fn write_guard_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_WRITE_GUARD").map_or(true, |value| value.trim() != "off")
}

/// Declarations a refused write's message names at most.
const DELETED_NAMES_SHOWN: usize = 8;

/// The top-level named declarations of `text` as `(kind, name)`, in source
/// order; None for a language with no grammar.
fn top_level_declarations(file: &str, text: &str) -> Option<Vec<(&'static str, String)>> {
    Some(
        outline(file, text)?
            .into_iter()
            .filter(|entry| entry.depth == 0)
            .filter_map(|entry| Some((entry.kind, entry.name?)))
            .collect(),
    )
}

/// The top-level declarations of `before` a whole-file write of `after`
/// deletes, when that is at least half of them and at least two: a4b
/// answered "add a test" with a `write` of `lib.rs` holding only the test
/// module, deleting every type and `mod` declaration (badciv P5 attempt 3).
/// Deletions are net per kind: a name of a kind that is gone counts only
/// beyond the new names of that kind the write adds, so a rewrite renaming
/// functions deletes none. The names listed are the gone names of each kind
/// with a net deletion. None for a language with no grammar, or a write that
/// keeps most of them.
fn deleted_declarations(file: &str, before: &str, after: &str) -> Option<Vec<String>> {
    let declared = top_level_declarations(file, before)?;
    let written = top_level_declarations(file, after)?;
    let gone: Vec<&(&str, String)> = declared
        .iter()
        .filter(|declaration| !written.contains(declaration))
        .collect();
    let added: Vec<&(&str, String)> = written
        .iter()
        .filter(|declaration| !declared.contains(declaration))
        .collect();
    let net_deleted = |kind: &str| {
        let count = |list: &[&(&str, String)]| list.iter().filter(|(k, _)| *k == kind).count();
        count(&gone).saturating_sub(count(&added))
    };
    let kinds: BTreeSet<&str> = gone.iter().map(|(kind, _)| *kind).collect();
    let deleted: usize = kinds.into_iter().map(net_deleted).sum();
    if deleted < 2 || deleted * 2 < declared.len() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for (kind, name) in &gone {
        if net_deleted(kind) > 0 && !names.contains(name) {
            names.push(name.clone());
        }
    }
    Some(names)
}

/// `names` in backticks, comma-separated, at most `shown` then an ellipsis.
fn listed_names(names: &[String], shown: usize) -> String {
    let mut listed: Vec<String> = names
        .iter()
        .take(shown)
        .map(|name| format!("`{name}`"))
        .collect();
    if names.len() > shown {
        listed.push("…".into());
    }
    listed.join(", ")
}

/// The 1-based line of `text` that uses literal `\n` escapes as line breaks:
/// one physical line holding at least two `\n` escapes (a backslash then
/// `n`, not after another backslash) each followed by indentation (two spaces or a tab), at least one of them in
/// code, outside the string literals and line comments of `file`'s language
/// (a `"` quote parity when the language is unknown). badciv P5 attempts 2
/// and 3 broke a replacement's later lines that way. A single escape, or
/// escapes not followed by indentation, is left alone: a line-level reading
/// cannot tell a raw string, a triple-quoted string or a regex from code.
fn escaped_newline_in_code(file: &str, text: &str) -> Option<usize> {
    let syntax = stub_syntax_for(file);
    let in_code = |line: &str, at: usize| match syntax {
        Some(syntax) => code_at(line, at, syntax),
        None => !quote_open(&line[..at], '"'),
    };
    text.lines().enumerate().find_map(|(index, line)| {
        let breaks: Vec<usize> = line
            .match_indices("\\n")
            .map(|(at, _)| at)
            .filter(|&at| {
                let backslashes = line[..at].chars().rev().take_while(|c| *c == '\\').count();
                let indented = line[at + 2..].starts_with("  ") || line[at + 2..].starts_with('\t');
                backslashes.is_multiple_of(2) && indented
            })
            .collect();
        (breaks.len() >= 2 && breaks.iter().any(|&at| in_code(line, at))).then_some(index + 1)
    })
}

/// Whether an outlined file's re-read is served as the Last result.
/// `MOOSEDEV_HARNESS_SERVE_OUTLINED=off` refuses it instead, as before.
fn serve_outlined_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_SERVE_OUTLINED").map_or(true, |value| value.trim() != "off")
}

/// The journal prefix of an outlined read the harness served.
pub(super) const OUTLINED_SERVED: &str = "Served outlined read:";

/// The Last result serving the whole of `text`, the current text of the
/// outlined `file`.
pub(super) fn outlined_text_response(file: &str, text: &str) -> String {
    format!(
        "Current text of `{file}` (shown as an outline in Source; not added back to the working set):\n{text}"
    )
}

/// The journal prefix of a read outside the step's scope the harness served.
pub(super) const OUTSIDE_SCOPE_SERVED: &str = "Served read outside scope:";

/// The Last result serving `text`, the current text of `file`, which is
/// outside the step's scope.
pub(super) fn outside_scope_text_response(file: &str, text: &str) -> String {
    format!(
        "Current text of `{file}` (outside this step's scope; not added to the working set):\n{text}"
    )
}

impl Runner {
    /// What a model read of `file` becomes: a read into the working set; a
    /// refusal when it would add nothing ([`Self::redundant_read`]); or, for a
    /// file outlined only for space whose earlier read is still current, its
    /// current text served as the Last result without rotating the source
    /// tiers: an outlined file read again was refused, and small models
    /// re-read it anyway or edited blind. A serve is repeated only once the
    /// Last result has moved on from it ([`Self::served_repeat`]).
    fn read_step(&self, file: String) -> Step {
        // Outside the step's scope a read shows the file as the Last result,
        // and the working set stays the scope's.
        if self.outside_scope(&file) {
            let next = if self.task.mode == Mode::Plan {
                "Propose the plan with what it showed"
            } else {
                "Take the plan's next step with what it showed"
            };
            return match self.served_repeat(&file, next) {
                Some(reason) => Step::ReadRefused { file, reason },
                None => Step::ReadOutsideScope { file },
            };
        }
        let reason = match self.redundant_read(&file) {
            None => return Step::Read { file },
            Some(reason) => reason,
        };
        if !serve_outlined_enabled()
            || !self.task.source_outlined.contains(&file)
            || self.task.source_full.contains(&file)
        {
            return Step::ReadRefused { file, reason };
        }
        let next = if self.task.mode == Mode::Plan {
            "Propose the plan from it"
        } else {
            "Edit it"
        };
        match self.served_repeat(&file, next) {
            Some(reason) => Step::ReadRefused { file, reason },
            None => Step::ReadOutlined { file },
        }
    }

    /// Why a re-read of `file`, served (outlined, or outside the scope) since
    /// the model last did anything but look (reads, inspects and searches),
    /// is refused, `next` naming the step to take instead; `None` when it is
    /// served. Never refused once the file on disk changed since the serve
    /// (its read snapshot): that is not a repeat. Refused while the Last
    /// result, which the prompt shows, is still the served text or a page of
    /// its journal event. Once the Last result has moved on, the text is no
    /// longer in the prompt and is served again (badciv run 14 re-read the
    /// spec it was building and was sent to page the journal instead), but
    /// only once per looking run and never after a refusal of it in that
    /// run: a model re-reading in a loop meets a refusal, then parks as any
    /// repeated refusal does.
    fn served_repeat(&self, file: &str, next: &str) -> Option<String> {
        let served = [
            format!("{OUTLINED_SERVED} {file} "),
            format!("{OUTSIDE_SCOPE_SERVED} {file} "),
        ];
        let refused = format!("{READ_REFUSED} `{file}` ");
        let mut serves = Vec::new();
        let mut refused_before = false;
        for (index, event) in self.looking_run(self.task.events.len()) {
            if served.iter().any(|at| event.message.starts_with(at)) {
                serves.push(index);
            }
            refused_before |= event.message.starts_with(&refused);
        }
        let latest = *serves.first()?;
        if !self.read_is_current(file) {
            return None;
        }
        // The served text, or a page of a serve's journal event.
        let last = &self.task.last_response;
        let shown = [
            outlined_text_response(file, ""),
            outside_scope_text_response(file, ""),
        ]
        .iter()
        .any(|header| last.starts_with(header))
            || serves
                .iter()
                .any(|event| last.starts_with(&format!("Journal event {event}, bytes ")));
        if shown {
            return Some(format!(
                "`{file}` is unchanged and its current text is the Last result (served at event {latest}). {next}, or inspect event {latest}."
            ));
        }
        if refused_before || serves.len() > 1 {
            return Some(format!(
                "`{file}` is unchanged and was already served {} time(s), with only reads, inspects and searches since, most recently at event {latest}. {next}, or inspect event {latest}.",
                serves.len()
            ));
        }
        None
    }

    /// Whether the Last result is the whole current text of `file`, served
    /// for a read of it: what the model proposing an edit now has seen.
    fn served_in_full(&self, file: &str) -> bool {
        match self.task.source.get(file) {
            Some(Some(text)) => self.task.last_response == outlined_text_response(file, text),
            _ => false,
        }
    }

    /// Why a model read of `file` would add nothing, or `None` when it
    /// should run. The prompt that produced the read already showed the
    /// file's current text in full, or it outlined the file only for space
    /// and the model's earlier read of it is still current. Reading it
    /// again would push the next file out of the budget: with more source
    /// than fits, a model reading each file in turn evicts exactly the file
    /// it reads next (badciv 40cef4a5, Lesson f5d2b5f9). Reads the guards
    /// make are never judged here.
    fn redundant_read(&self, file: &str) -> Option<String> {
        // A preloaded file shown in full was seen as a read one is.
        let read =
            self.task.read_files.iter().any(|known| known == file) || self.preloaded_in_full(file);
        if read && self.task.source_full.contains(file) {
            // badciv 1e6cd3e7: without a next step, a planner that wanted to
            // "verify" read the same file again and parked.
            let next = if self.task.mode == Mode::Plan {
                "In Plan mode the next action is plan: propose it from the source shown."
            } else {
                "Edit it, run a check, or finish if the work is done."
            };
            return Some(format!(
                "{file} is shown in full under Source and is current, so it was not read again. {next}"
            ));
        }
        if !self.task.source_outlined.contains(file) || !self.read_is_current(file) {
            return None;
        }
        let prefix = format!("Read {file}: ");
        let event = self
            .task
            .events
            .iter()
            .rposition(|event| event.message.starts_with(&prefix))?;
        let budget = self
            .source_budget
            .map(|bytes| format!(" of {bytes} bytes"))
            .unwrap_or_default();
        let next = if self.task.mode == Mode::Plan {
            format!("Plan from its outline, which lists its declarations with line numbers, or inspect event {event}.")
        } else {
            format!("Inspect event {event}, or propose the edit and the harness shows its full source first.")
        };
        Some(format!(
            "{file} is outlined only because the working set is larger than the source budget{budget}; its full text at event {event} is unchanged, so it was not read again. {next}"
        ))
    }
}

impl Runner {
    /// `command` without a leading `cd` into an absolute path that does not
    /// exist. Commands already run in the project root, and a small model
    /// invents one (`cd /home/user/project && cargo test`: four times over
    /// badciv runs 7 and 8), which only fails and costs a step.
    fn without_missing_cd(&mut self, command: String) -> String {
        let Some((path, rest)) = missing_cd(&command, |path| path.exists()) else {
            return command;
        };
        let detail = format!(
            "dropped `cd {path}` (no such directory); `{rest}` runs in the project root instead"
        );
        self.intent_event("command_cd_repair", &detail);
        self.event(format!(
            "Command repair: {detail}. Commands already run in the project root."
        ));
        rest
    }

    /// The edit a numbered quick fix makes to the current source: an ordinary
    /// whole-file edit, so it goes through the same approval, grounding,
    /// policy and checking as one the model wrote.
    fn materialize_fix(&mut self, id: usize) -> Result<Step> {
        let diagnostics = self.task.diagnostics.as_ref();
        let fix = diagnostics
            .and_then(|diagnostics| diagnostics.fix(id))
            .cloned()
            .with_context(|| match diagnostics {
                Some(diagnostics) => diagnostics.unknown_fix(id),
                None => format!("no fix {id} is offered: no finding has a quick fix now; make the change with replace or write"),
            })?;
        let file = fix.file.clone();
        if !self.task.read_files.contains(&file)
            || (self.task.source_outlined.contains(&file) && !self.served_in_full(&file))
        {
            // As for any edit: never change a file whose source and dossier
            // the model has not been shown in full.
            self.event(format!(
                "Fix guard: showing {file} in full first; fix {id} will not execute."
            ));
            return Ok(Step::Read { file });
        }
        let before = self
            .task
            .source
            .get(&file)
            .cloned()
            .flatten()
            .context("source snapshot missing; read the target before applying a fix")?;
        let after = fix.apply(&before).with_context(|| {
            format!("fix {id} no longer applies: {file} has changed since it was offered; make the change with replace")
        })?;
        self.event(format!("Applying fix {id} to {file}: {}", fix.title));
        Ok(Step::Edit {
            file,
            before: Some(before),
            after: Some(after),
        })
    }
}

/// The path of a leading `cd` into an absolute path that does not exist, and
/// the command it guards: `cd /home/user/project && cargo test` gives
/// `("/home/user/project", "cargo test")`. None for anything else, so the
/// shell decides: a relative or existing path; one the shell would expand
/// (`$`, `~`, globs, escapes), whose real meaning only it knows; a `cd` alone,
/// or one followed by `;`, after which the rest runs in the project root
/// anyway; a rest with `||`, whose fallback dropping the `cd` would skip; and
/// `&&` on another line, which the shell rejects.
fn missing_cd(
    command: &str,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<(String, String)> {
    let rest = command.trim_start().strip_prefix("cd")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start();
    let (path, after) = match rest.chars().next()? {
        quote @ ('\'' | '"') => {
            let end = rest[1..].find(quote)? + 1;
            (&rest[1..end], &rest[end + 1..])
        }
        _ => {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ';' || c == '&')
                .unwrap_or(rest.len());
            rest.split_at(end)
        }
    };
    if !path.starts_with('/')
        || path.contains(['$', '`', '~', '*', '?', '[', '{', '\\'])
        || exists(std::path::Path::new(path))
    {
        return None;
    }
    let remainder = after
        .trim_start_matches([' ', '\t'])
        .strip_prefix("&&")?
        .trim_start();
    (!remainder.is_empty() && !remainder.contains("||"))
        .then(|| (path.to_owned(), remainder.to_owned()))
}

/// Files below this are small enough that resending one costs nothing worth
/// naming, and a genuine small-file rewrite is ordinary work.
const WHOLE_FILE_MIN_BYTES: usize = 1024;

/// A `replace` that resent the file to change a fraction of it, described for
/// the journal; `None` when the span is targeted or the rewrite is real.
///
/// `ACTION_MEANINGS` already tells the model not to reproduce the whole source
/// merely as a precondition. badciv-map ignored it nine times: each `old_text`
/// was the entire file, and the last six changed 12 to 308 bytes of about
/// 16 KB. That is what spends the context window (Lesson af16b95e) and what an
/// edit loop looks like from the outside, so the harness names it rather than
/// leaving it to be reconstructed from archived prompts.
///
/// Both conditions are needed. A span covering the file is only waste when
/// little of it actually changed: restructuring a file really does rewrite it,
/// and that must not be reported as a mistake.
fn whole_file_rewrite(source: &str, old_text: &str, new_text: &str) -> Option<String> {
    if source.len() < WHOLE_FILE_MIN_BYTES || old_text.len() * 10 < source.len() * 9 {
        return None;
    }
    // Bytes throughout: a common prefix can end mid-character, so slicing the
    // &str at it would panic on any source that is not ASCII.
    let (old, new) = (old_text.as_bytes(), new_text.as_bytes());
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| a == b)
        .count()
        .min(old.len().min(new.len()));
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let changed = (old.len() - prefix - suffix).max(new.len() - prefix - suffix);
    (changed * 4 <= old_text.len()).then(|| {
        format!(
            "old_text spans {} of {} bytes to change {changed}",
            old_text.len(),
            source.len()
        )
    })
}

/// Whether a replace whose `old_text` is gone has already been made. The
/// evidence is structural, never a lone `new_text` match: `new_text` is in the
/// file exactly once, `old_text` is nowhere, the two are related (they share a
/// kept line that anchors them, or every line of `old_text` lies within
/// `new_text`), every line the edit removes is gone from the file, and
/// anything a junk trim would match of `old_text` lies within that change.
/// That covers an added line (badciv e3c533b4's `#[ignore]`), an extended one
/// (`return name` → `return name.strip()`) and a changed one (badciv P5's
/// derive gaining `Hash, PartialOrd`). A `new_text` that merely occurs
/// elsewhere is unrelated to `old_text`, and a removed line still in the file
/// means the change was not made: both stay a miss. A kept line anchors only
/// when it says something (not bare brackets or punctuation, at least four
/// characters) and is a whole line of the file exactly once: codex found two
/// `if enabled {` blocks, where the untouched one made the other's needed
/// replace look made.
fn already_applied(source: &str, old_text: &str, new_text: &str) -> bool {
    fn lines(text: &str) -> impl Iterator<Item = &str> {
        text.lines().map(str::trim).filter(|line| !line.is_empty())
    }
    let new_lines: HashSet<&str> = lines(new_text).collect();
    let source_lines: HashSet<&str> = lines(source).collect();
    let (kept, removed): (Vec<&str>, Vec<&str>) =
        lines(old_text).partition(|line| new_lines.contains(line));
    let anchors = |line: &&str| {
        line.len() >= 4
            && line.chars().any(char::is_alphanumeric)
            && lines(source).filter(|known| known == line).count() == 1
    };
    let related = kept.iter().any(anchors) || removed.iter().all(|line| new_text.contains(line));
    !new_text.trim().is_empty()
        && occurrences(source, new_text) == 1
        && occurrences(source, old_text) == 0
        && related
        && removed.iter().all(|line| !source_lines.contains(line))
        && repair_literal_span(source, old_text)
            .is_none_or(|repair| new_text.contains(&repair.span))
}

/// Occurrences of `literal` in `source`, counting overlaps, capped at two:
/// ambiguous matching must never choose an arbitrary location even when
/// str::matches would skip one.
fn occurrences(source: &str, literal: &str) -> usize {
    source
        .char_indices()
        .filter(|(i, _)| source[*i..].starts_with(literal))
        .take(2)
        .count()
}

/// The most stray bytes trimmed from either end of a literal span.
const MAX_JUNK_BYTES: usize = 16;

/// A literal span recovered by trimming stray junk from its ends.
#[derive(Debug)]
pub(super) struct SpanRepair {
    pub(super) span: String,
    pub(super) prefix: String,
    pub(super) suffix: String,
}

/// Characters a model leaks into a string from its JSON envelope or template.
fn is_junk_char(c: char) -> bool {
    matches!(c, '}' | ']' | '"' | '\'' | '`' | '$') || c.is_whitespace()
}

fn token_fragment_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Byte lengths of trimmable junk prefixes, smallest first (always starting at 0).
fn prefix_cuts(text: &str) -> Vec<usize> {
    let mut cuts = vec![0];
    let mut start = 0;
    loop {
        let rest = &text[start..];
        let token = rest.strip_prefix("<|").and_then(|inner| {
            let end = inner.find("|>")?;
            inner[..end]
                .chars()
                .all(token_fragment_char)
                .then_some(end + 4)
        });
        let unit = token.or_else(|| {
            rest.chars()
                .next()
                .filter(|c| is_junk_char(*c))
                .map(char::len_utf8)
        });
        match unit {
            Some(unit) if start + unit <= MAX_JUNK_BYTES && start + unit < text.len() => {
                start += unit;
                cuts.push(start);
            }
            _ => return cuts,
        }
    }
}

/// Byte lengths of trimmable junk suffixes, smallest first (always starting at 0).
/// A special-token fragment such as `<|im_end|>` or a dangling `<|` is one unit.
fn suffix_cuts(text: &str) -> Vec<usize> {
    let mut cuts = vec![0];
    let mut end = text.len();
    loop {
        let rest = &text[..end];
        let token = rest.rfind("<|").and_then(|start| {
            rest[start + 2..]
                .chars()
                .all(|c| token_fragment_char(c) || matches!(c, '|' | '>'))
                .then_some(rest.len() - start)
        });
        let unit = token.or_else(|| {
            rest.chars()
                .next_back()
                .filter(|c| is_junk_char(*c))
                .map(char::len_utf8)
        });
        match unit {
            Some(unit) if text.len() - end + unit <= MAX_JUNK_BYTES && unit < end => {
                end -= unit;
                cuts.push(text.len() - end);
            }
            _ => return cuts,
        }
    }
}

/// Recover a literal that matches nowhere only because stray junk (closing
/// braces or brackets, quotes, `$`, whitespace, special-token fragments) was
/// leaked onto its ends. The smallest trim whose remainder occurs exactly once
/// wins; two different remainders at that size are ambiguous and refused, as is
/// any literal that already occurs or would need non-junk characters removed.
pub(super) fn repair_literal_span(source: &str, literal: &str) -> Option<SpanRepair> {
    if occurrences(source, literal) != 0 {
        return None;
    }
    let prefixes = prefix_cuts(literal);
    let suffixes = suffix_cuts(literal);
    let mut candidates: Vec<(usize, usize, usize)> = prefixes
        .iter()
        .flat_map(|&p| suffixes.iter().map(move |&s| (p + s, p, s)))
        .filter(|&(total, p, s)| total > 0 && p + s < literal.len())
        .collect();
    candidates.sort_unstable();
    let mut found: Option<(usize, SpanRepair)> = None;
    for (total, p, s) in candidates {
        if found.as_ref().is_some_and(|(size, _)| total > *size) {
            break;
        }
        let span = &literal[p..literal.len() - s];
        if span.trim().is_empty() || occurrences(source, span) != 1 {
            continue;
        }
        match &found {
            Some((_, repair)) if repair.span != span => return None,
            Some(_) => {}
            None => {
                found = Some((
                    total,
                    SpanRepair {
                        span: span.to_owned(),
                        prefix: literal[..p].to_owned(),
                        suffix: literal[literal.len() - s..].to_owned(),
                    },
                ))
            }
        }
    }
    found.map(|(_, repair)| repair)
}

/// Decode JSON string escapes (`\"`, `\\`, `\/`, `\n`, `\t`, `\r`, `\b`, `\f`,
/// `\uXXXX`) that a model copied from the JSON-encoded source in its prompt.
/// `None` when nothing was escaped or any escape is not JSON's, so an ordinary
/// backslash in code is never reinterpreted.
fn decode_json_escapes(text: &str) -> Option<String> {
    let mut decoded = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        escaped = true;
        decoded.push(match chars.next()? {
            '"' => '"',
            '\\' => '\\',
            '/' => '/',
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                let code = (hex.len() == 4)
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()?;
                char::from_u32(code)?
            }
            _ => return None,
        });
    }
    escaped.then_some(decoded)
}

/// True when every double quote in `text` is written as `\"`: the whole value
/// was copied from a JSON rendering rather than authored.
fn consistently_escaped(text: &str) -> bool {
    text.contains("\\\"") && !text.replace("\\\"", "").contains('"')
}

/// The first non-blank line of `literal` that occurs nowhere in `source`,
/// bounded for a diagnostic.
fn first_absent_line(source: &str, literal: &str) -> Option<String> {
    literal
        .lines()
        .filter(|line| !line.trim().is_empty())
        .find(|line| !source.contains(line))
        .map(|line| super::bounded(line, 160))
}

/// Remove from replacement text only the junk a repair removed from the old
/// span, and only where it carries that exact junk at the same end.
pub(super) fn strip_same_junk(text: &str, repair: &SpanRepair) -> String {
    let mut text = text;
    if !repair.suffix.is_empty() {
        text = text.strip_suffix(repair.suffix.as_str()).unwrap_or(text);
    }
    if !repair.prefix.is_empty() {
        text = text.strip_prefix(repair.prefix.as_str()).unwrap_or(text);
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// badciv b443836f and edc914f6: invented project roots before `&&`.
    #[test]
    fn a_cd_into_a_missing_absolute_path_is_dropped() {
        let missing = |_: &std::path::Path| false;
        assert_eq!(
            missing_cd(
                "cd /home/ryan/src/moosedev && cargo test --workspace 2>&1 | tail -30",
                missing
            ),
            Some((
                "/home/ryan/src/moosedev".into(),
                "cargo test --workspace 2>&1 | tail -30".into()
            ))
        );
        assert_eq!(
            missing_cd("  cd '/home/my project'&&cargo build", missing),
            Some(("/home/my project".into(), "cargo build".into()))
        );
        // Everything else is left to the shell.
        for command in [
            "cd badciv-map && cargo test",
            "cd /home/x; cargo test",
            "cd /home/x",
            "cd /home/x &&",
            "cdx /home/x && ls",
            "echo cd /home/x && ls",
            "cd /home/my\\ dir && ls",
            "cd \"/home/$USER/project\" && ls",
            "cd ~/project && ls",
            "cd /home/x && true || echo fallback",
            "cd /home/x\n&& cargo clean",
        ] {
            assert_eq!(missing_cd(command, missing), None, "{command}");
        }
        assert_eq!(
            missing_cd("cd /tmp && ls", |_: &std::path::Path| true),
            None,
            "an existing directory is the model's choice"
        );
    }

    /// badciv-map's shape: nine replaces whose old_text was the whole file,
    /// the last six changing 12 to 308 bytes of about 16 KB.
    #[test]
    fn a_replace_whose_change_is_already_in_the_file_is_recognised() {
        // badciv e3c533b4, verbatim.
        let source = "use badciv_map::Map;\n\n#[test]\n#[ignore]\nfn test_tiny_fixture() {\n    let input = 1;\n}\n";
        assert!(already_applied(
            source,
            "#[test]\nfn test_tiny_fixture() {",
            "#[test]\n#[ignore]\nfn test_tiny_fixture() {"
        ));
        // Junk around a real old_text elsewhere: the trim, not a no-op.
        assert!(!already_applied(
            "let a = foo();\nlet b = bar();\n",
            "foo()}}",
            "bar()"
        ));
        // codex: new_text found elsewhere, old_text a different line entirely.
        assert!(!already_applied(
            "let a = 1;\nlet b = 2;\n",
            "let a = 3;",
            "let b = 2;"
        ));
        // A new_text that is not in the file is an ordinary miss.
        assert!(!already_applied(
            source,
            "#[test]\nfn gone() {",
            "#[test]\nfn other() {"
        ));

        // badciv P5: a changed line, not an added one. The derive gained
        // `Hash, PartialOrd`; the enum body is the kept context.
        let old = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum Faction {\n    Terrans,\n    Saurids,\n    Greys,\n}";
        let new = "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd)]\npub enum Faction {\n    Terrans,\n    Saurids,\n    Greys,\n}";
        let applied = format!("use std::fmt;\n\n{new}\n\nimpl Faction {{}}\n");
        assert!(already_applied(&applied, old, new));
        // The removed derive line is still in the file (another type keeps
        // it): the change is not evidently made, so it stays a miss.
        let still = format!(
            "{applied}\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum Climate {{}}\n"
        );
        assert!(!already_applied(&still, old, new));
        // An extended line: the old line lies within the new one.
        assert!(already_applied(
            "def label(name):\n    return name.strip()\n",
            "    return name\n",
            "    return name.strip()\n"
        ));
        // codex: two `if enabled {` blocks. The replace meant for `stop`
        // (whose body the model misremembered) has a new_text that is the
        // untouched block in `start`; the shared lines are a line the file
        // holds twice and a bare brace, so nothing anchors it: a miss.
        let blocks = "fn start() {\n    if enabled {\n        flush();\n    }\n}\n\nfn stop() {\n    if enabled {\n        close();\n    }\n}\n";
        assert!(!already_applied(
            blocks,
            "    if enabled {\n        drain();\n    }",
            "    if enabled {\n        flush();\n    }"
        ));
        // The same edit with the block written once is recognised.
        assert!(already_applied(
            "fn start() {\n    if enabled {\n        flush();\n    }\n}\n",
            "    if enabled {\n        drain();\n    }",
            "    if enabled {\n        flush();\n    }"
        ));
    }

    #[test]
    fn a_replace_that_resends_the_file_to_change_little_is_named() {
        let body = "fn item() -> u32 { 0 }\n".repeat(200);
        let source = format!("//! A module.\n{body}");
        let changed = source.replace("fn item() -> u32 { 0 }\n", "fn item() -> u32 { 1 }\n");

        // The whole file resent for a small change: both conditions hold.
        let detail = whole_file_rewrite(&source, &source, &format!("{source}\nfn extra() {{}}\n"))
            .expect("whole file resent for a 16-byte addition");
        assert!(detail.contains("old_text spans"), "{detail}");

        // A genuine rewrite spans the file and changes it: not a mistake.
        assert_eq!(whole_file_rewrite(&source, &source, &changed), None);

        // A targeted span is never reported, however small the file's change.
        assert_eq!(
            whole_file_rewrite(
                &source,
                "fn item() -> u32 { 0 }\n",
                "fn item() -> u32 { 1 }\n"
            ),
            None
        );

        // A small file is ordinary work either way.
        let small = "fn main() {}\n";
        assert_eq!(whole_file_rewrite(small, small, "fn main() { }\n"), None);

        // A common prefix ending mid-character must not slice a &str.
        let utf8 = format!("//! Ωmega διαμόρφωση\n{body}");
        assert!(whole_file_rewrite(&utf8, &utf8, &format!("{utf8}ω")).is_some());
    }

    #[test]
    fn trailing_envelope_braces_are_trimmed_to_the_unique_source_span() {
        let source = "class FeePolicy:\n    def late_fee(self):\n        return 0\n";
        let repair = repair_literal_span(
            source,
            "class FeePolicy:\n    def late_fee(self):\n        return 0\n}}",
        )
        .unwrap();
        assert_eq!(repair.span, source);
        assert_eq!(repair.prefix, "");
        assert_eq!(repair.suffix, "}}");
    }

    #[test]
    fn leading_junk_and_special_token_fragments_are_trimmed() {
        let repair = repair_literal_span("fn x() {}\n", "\"$ fn x() {}").unwrap();
        assert_eq!(repair.span, "fn x() {}");
        assert_eq!(repair.prefix, "\"$ ");
        assert_eq!(repair.suffix, "");
        let repair = repair_literal_span("value = 1\n", "value = 1<|im_end|>").unwrap();
        assert_eq!(repair.span, "value = 1");
        assert_eq!(repair.suffix, "<|im_end|>");
        // The smallest trim that matches wins: the stray newline stays in the span.
        let repair = repair_literal_span("a\nreturn 0\nb", "return 0\n$}}} <|").unwrap();
        assert_eq!(repair.span, "return 0\n");
        assert_eq!(repair.suffix, "$}}} <|");
    }

    #[test]
    fn ambiguous_unjunked_matching_or_oversized_trims_are_refused() {
        // Two different trims of the same size each match once.
        assert!(repair_literal_span("x} and }x", "}x}").is_none());
        // No junk to remove: an ordinary mismatch stays a mismatch.
        assert!(repair_literal_span("abc", "abd").is_none());
        // Removing non-junk characters is never a repair.
        assert!(repair_literal_span("abc", "abcdef").is_none());
        // A literal that already matches needs no repair.
        assert!(repair_literal_span("a}}", "a}}").is_none());
        // A literal that matches more than once after trimming stays ambiguous.
        assert!(repair_literal_span("ok ok", "ok}}").is_none());
        // Junk runs are bounded.
        assert!(repair_literal_span("ok\n", &format!("ok{}", "}".repeat(20))).is_none());
    }

    #[test]
    fn json_string_escapes_copied_from_the_prompt_are_decoded() {
        assert_eq!(
            decode_json_escapes("let s = Some(\\\"a\\\".to_string());\\n\\tx\\\\y \\u0041")
                .unwrap(),
            "let s = Some(\"a\".to_string());\n\tx\\y A"
        );
        // Nothing escaped: not a repair.
        assert!(decode_json_escapes("plain \"quoted\" text").is_none());
        // Escapes that are not JSON's belong to the code itself.
        assert!(decode_json_escapes("regex \\d+ here").is_none());
        assert!(decode_json_escapes("trailing \\").is_none());
        assert!(decode_json_escapes("\\u12").is_none());
        assert!(decode_json_escapes("\\ud800").is_none());
    }

    #[test]
    fn only_a_wholly_escaped_value_counts_as_copied() {
        assert!(consistently_escaped("a = \\\"x\\\";"));
        assert!(!consistently_escaped("a = \"x\"; b = \\\"y\\\";"));
        assert!(!consistently_escaped("no quotes"));
        assert!(!consistently_escaped("a = \"x\";"));
    }

    #[test]
    fn the_first_absent_line_names_where_old_text_diverges() {
        let source = "fn a() {}\nfn b() {\n    1\n}\n";
        assert_eq!(
            first_absent_line(source, "fn a() {}\n\nfn b() {\n    2\n}\n").as_deref(),
            Some("    2")
        );
        assert!(first_absent_line(source, "fn a() {}\n").is_none());
    }

    #[test]
    fn new_text_loses_only_the_same_junk_at_the_same_end() {
        let repair = repair_literal_span("original\n", "original\n}}").unwrap();
        assert_eq!(strip_same_junk("changed\n}}", &repair), "changed\n");
        assert_eq!(strip_same_junk("changed\n", &repair), "changed\n");
        let repair = repair_literal_span("fn x() {}\n", "\"$ fn x() {}").unwrap();
        assert_eq!(strip_same_junk("\"$ fn y() {}", &repair), "fn y() {}");
        assert_eq!(strip_same_junk("fn y() {}", &repair), "fn y() {}");
    }

    /// badciv P5 attempt 3, from the archived journal (variants trimmed):
    /// a4b answered "add a test" by writing lib.rs as the test module alone.
    #[test]
    fn a_write_that_deletes_most_top_level_declarations_is_named() {
        let before = "#[derive(Debug, Clone, PartialEq, Eq)]\npub enum Terrain {\n    Ocean,\n    Plains,\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum Climate {\n    Arctic,\n}\n\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct Tile {\n    pub terrain: Terrain,\n}\n\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct Map {\n    pub width: u32,\n    pub tiles: Vec<Tile>,\n}\n\nimpl Map {\n    pub fn tile(&self, x: u32) -> Option<&Tile> {\n        self.tiles.get(x as usize)\n    }\n}\n\npub mod codes;\npub mod error;\npub mod parse;\n";
        let after = "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn test_map_tile_access() {\n        let map = Map { width: 1, tiles: vec![] };\n        assert!(map.tile(0).is_none());\n    }\n}\n";
        let deleted = deleted_declarations("badciv-map/src/lib.rs", before, after).unwrap();
        assert_eq!(
            deleted,
            ["Terrain", "Climate", "Tile", "Map", "codes", "error", "parse"]
        );
        assert_eq!(listed_names(&deleted, 3), "`Terrain`, `Climate`, `Tile`, …");
        // The whole file written again with a test added keeps everything.
        let whole = format!("{before}\n{after}");
        assert_eq!(
            deleted_declarations("badciv-map/src/lib.rs", before, &whole),
            None
        );
        // Removing one of seven, or the one of two that is left: not most.
        let fewer = before.replace("pub mod parse;\n", "");
        assert_eq!(
            deleted_declarations("badciv-map/src/lib.rs", before, &fewer),
            None
        );
        assert_eq!(
            deleted_declarations("a.rs", "fn a() {}\nfn b() {}\n", "fn a() {}\n"),
            None
        );
        // Half, and at least two: named. A language with no grammar: never.
        assert_eq!(
            deleted_declarations(
                "a.py",
                "def a():\n    pass\n\ndef b():\n    pass\n\nclass C:\n    pass\n\ndef d():\n    pass\n",
                "def a():\n    pass\n\ndef d():\n    pass\n"
            ),
            Some(vec!["b".to_string(), "C".to_string()])
        );
        assert_eq!(deleted_declarations("notes.txt", before, after), None);
    }

    /// A rewrite that renames declarations deletes none of them: deletions
    /// are net per kind. The P5 write, which added only a test module, still
    /// deletes most of lib.rs.
    #[test]
    fn a_write_that_renames_declarations_deletes_none() {
        let before = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let renamed = "fn a() {}\nfn b() {}\nfn parse_c() {}\nfn parse_d() {}\n";
        assert_eq!(deleted_declarations("a.rs", before, renamed), None);
        // Renaming two and dropping the other two still deletes two of four.
        assert_eq!(
            deleted_declarations("a.rs", before, "fn x() {}\nfn y() {}\n"),
            Some(vec!["a".into(), "b".into(), "c".into(), "d".into()])
        );
        // A new name of another kind does not offset a deleted function.
        assert_eq!(
            deleted_declarations(
                "a.rs",
                before,
                "fn a() {}\nfn b() {}\nstruct C;\nstruct D;\n"
            ),
            Some(vec!["c".into(), "d".into()])
        );
        let lib = "pub struct Map {\n    pub width: u32,\n}\n\npub struct Tile;\n\npub mod codes;\npub mod error;\npub mod parse;\n";
        let test_only = "#[cfg(test)]\nmod tests {\n    use super::*;\n}\n";
        assert_eq!(
            deleted_declarations("badciv-map/src/lib.rs", lib, test_only),
            Some(vec![
                "Map".into(),
                "Tile".into(),
                "codes".into(),
                "error".into(),
                "parse".into()
            ])
        );
    }

    /// badciv P5 attempt 3, from the archived journal: the replace's new_text
    /// (decoded from the model's JSON) broke its first lines with real line
    /// breaks and the rest with literal `\n`.
    #[test]
    fn literal_newline_escapes_in_code_are_found_by_line() {
        let new_text = r#"            let terrain = codes::char_to_terrain(*terrain_char).ok_or_else(|| MapError::UnknownChar {
                section: "terrain".to_string(), x, y, c: *terrain_char, terrain: crate::Terrain::Ocean 
            })?;\n\n            let climate_char = climate_grid.get(idx).ok_or_else(|| MapError::UnknownSection("climate grid incomplete".to_string()))?;\n            let climate = codes::char_to_climate(*climate_char).ok_or_else(|| MapError::UnknownChar {\n                section: "climate".to_string(), x, y, c: *climate_char, terrain: crate::Terrain::Ocean \n            })?;"#;
        assert_eq!(
            escaped_newline_in_code("badciv-map/src/parse.rs", new_text),
            Some(3)
        );
        // Escapes used as line breaks inside a string, a comment, or after
        // an escaped backslash: not code.
        for text in [
            "let s = \"a\\n    b\\n    c\";\nlet t = 1;",
            "let x = 1; // split on \\n    x\\n    y\n",
            "let s = r\"C:\\\\n    \\\\n    \";",
        ] {
            assert_eq!(escaped_newline_in_code("src/a.rs", text), None, "{text}");
        }
        assert_eq!(
            escaped_newline_in_code("a.py", "x = 1\\n    y = 2\\n    z = 3"),
            Some(1)
        );
        // An unknown language: a `"` string is still read.
        assert_eq!(
            escaped_newline_in_code("notes.txt", "a\\n  b\\n  c"),
            Some(1)
        );
        assert_eq!(
            escaped_newline_in_code("notes.txt", "say \"a\\n  b\\n  c\""),
            None
        );
    }

    /// Only `\n` escapes used as line breaks, two or more on a line each
    /// followed by indentation, are the P5 corruption; the string shapes a
    /// line-level reading misjudges are left alone.
    #[test]
    fn a_newline_escape_is_refused_only_as_indented_line_breaks() {
        for (file, text) in [
            // One escape followed by indentation.
            ("src/a.rs", "println!(\"a\\n    b\");"),
            // A string with an escaped quote before its escapes.
            ("src/a.rs", "let s = \"a\\\"b\\n\";"),
            ("src/a.rs", "let s = \"a\\\"b\\n    c\\n    d\";"),
            // Python triple-quoted text.
            ("a.py", "HELP = \"\"\"usage:\\n    run\\n    stop\"\"\""),
            // A TypeScript regex.
            ("a.ts", "const lines = text.split(/\\r?\\n/);"),
            // Escapes not followed by indentation.
            ("src/a.rs", "let b = 2;\\n\\nlet c = 3;"),
            ("a.py", "x = 1\\ny = 2"),
        ] {
            assert_eq!(escaped_newline_in_code(file, text), None, "{text}");
        }
    }
}
