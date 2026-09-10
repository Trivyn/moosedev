//! Validate sensor arguments and materialize edits before permission or execution.
use super::{model::Action, Mode, Runner, MAX_FILES, MAX_PLAN_SUMMARY};
use anyhow::{ensure, Context, Result};

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
            Action::Command { .. } | Action::Finish { .. } => ensure!(
                self.task.mode == Mode::Auto,
                "Plan mode cannot execute or finish code work; human approval is required"
            ),
            Action::Plan { .. } => ensure!(
                self.task.mode == Mode::Plan,
                "switch to Plan before changing the approved approach"
            ),
            _ => {}
        }
        Ok(())
    }

    pub(super) fn validate_action(&mut self, action: Action) -> Result<Action> {
        match &action {
            Action::Plan {
                summary,
                files,
                checks,
            } => {
                ensure!(!summary.trim().is_empty() && summary.len() <= MAX_PLAN_SUMMARY && !files.is_empty() && files.len() <= MAX_FILES,
                    "plan requires a nonempty summary of at most 4000 bytes and 1..100 explicit file paths");
                ensure!(
                    !checks.is_empty()
                        && checks.len() <= 20
                        && checks
                            .iter()
                            .all(|c| !c.trim().is_empty() && c.len() <= 4000),
                    "plan requires 1..20 nonempty verification commands of at most 4000 bytes each"
                );
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
            Action::Reply { message } => {
                ensure!(!message.trim().is_empty(), "reply message cannot be empty")
            }
            Action::Command { command } => {
                ensure!(
                    !command.trim().is_empty() && command.len() <= 4000,
                    "command must contain 1..4000 bytes"
                );
            }
            _ => {}
        }
        let file = match &action {
            Action::Replace { file, .. }
            | Action::Write { file, .. }
            | Action::Edit { file, .. } => file,
            _ => return Ok(action),
        };
        if !self.task.read_files.contains(file) {
            // The next generation receives the source and dossier. Never apply a
            // proposal whose author did not see the governing source snapshot.
            self.event(format!("First-edit guard: requesting source and dossier for {file}; the unread edit proposal will not execute."));
            return Ok(Action::Read { file: file.clone() });
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
                // Count overlapping occurrences too: ambiguous matching must never
                // choose an arbitrary location even when str::matches skips one.
                let count = source
                    .char_indices()
                    .filter(|(i, _)| source[*i..].starts_with(&old_text))
                    .take(2)
                    .count();
                ensure!(count == 1, "replace old_text must match exactly once; found {count}{}; use the supplied source to select a unique literal span", if count == 2 { " or more" } else { "" });
                (file, Some(source.replacen(&old_text, &new_text, 1)))
            }
            Action::Write { file, content } => (file, content),
            Action::Edit {
                file,
                before: supplied,
                after,
            } => {
                ensure!(supplied == before, "legacy edit.before must equal the entire supplied file; use replace for a unique fragment or write for full content");
                (file, after)
            }
            _ => unreachable!(),
        };
        ensure!(before != after, "edit makes no change; choose a different edit or finish if the objective is already satisfied");
        Ok(Action::Edit {
            file,
            before,
            after,
        })
    }
}
