//! The harness applies an unambiguous quick fix itself, without a model step
//! (offloading change 2, AD d1195c3d; Constraint cd9f1a96). Choosing a fix
//! cost small builders whole steps (badciv run 11: 11 fix steps, 7.5 min, 595
//! tokens) and invited line numbers sent as fix ids (badciv e3c533b4).
//!
//! Only what the server itself marks preferred, for an error or a lint whose
//! offered list is complete, and never a fix that only deletes
//! ([`DiagnosticsSnapshot::auto_fix`]) nor one that adds a panicking call
//! (held when applied, against the file's text). Armed on a fresh result like
//! auto-verify, one fix per advance, at most three in a row after a model edit
//! and twenty per task. A policy gate holds it: the harness never makes a
//! pending edit the human did not see coming. `MOOSEDEV_HARNESS_AUTO_FIX=off`
//! removes the lever for study variants (Requirement e9166711).
//!
//! [`DiagnosticsSnapshot::auto_fix`]: crate::harness::runner::DiagnosticsSnapshot

use anyhow::Result;

use super::super::{PendingEdit, Runner};
use crate::policy::PolicyDecision;

/// Fixes the harness applies in a row after one model edit.
const AUTO_FIX_CHAIN: usize = 3;
/// Fixes the harness applies in one task: a runaway guard.
const AUTO_FIX_LIMIT: usize = 20;

/// Calls that turn a failure into a panic. rustc's preferred `u32`/`usize`
/// conversion `(…).try_into().unwrap()` was applied twice in the same run
/// (badciv P5), making code panic where the compiler asked for a conversion
/// or an error path.
const PANICKING_CALLS: &[&str] = &[".unwrap()", ".expect("];

/// How many [`PANICKING_CALLS`] `text` holds. A fix is held when the file
/// has more after it than before, so one that rewrites a line keeping its
/// `.unwrap()` is still applied.
fn panicking_calls(text: &str) -> usize {
    PANICKING_CALLS
        .iter()
        .map(|call| text.matches(call).count())
        .sum()
}

fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_AUTO_FIX").map_or(true, |value| value.trim() != "off")
}

impl Runner {
    /// After an applied edit: arm when the servers reported on it with a fix
    /// the harness may apply.
    pub(in crate::harness::runner) fn arm_auto_fix(&mut self, fresh: bool) {
        let selectable = fresh
            && self
                .task
                .diagnostics
                .as_ref()
                .is_some_and(|diagnostics| diagnostics.auto_fix().is_some());
        let at = self.task.edits.len();
        self.symbolic_state_mut().auto_fix_armed = selectable.then_some(at);
    }

    /// Whether the harness applies a fix now. Takes the arm.
    pub(in crate::harness::runner) fn auto_fix_due(&mut self) -> bool {
        let at = self.task.edits.len();
        let armed = self
            .task
            .symbolic
            .as_mut()
            .and_then(|state| state.auto_fix_armed.take());
        if armed != Some(at) || !enabled() || !self.harness_may_act() {
            return false;
        }
        if self
            .task
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.auto_fix())
            .is_none()
        {
            return false;
        }
        let state = self.symbolic_state_mut();
        if state.auto_fix_chain >= AUTO_FIX_CHAIN {
            return false;
        }
        if state.auto_fixes >= AUTO_FIX_LIMIT {
            if state.auto_fixes == AUTO_FIX_LIMIT {
                // Counted past the limit so this is journaled once.
                state.auto_fixes += 1;
                self.intent_event(
                    "auto_fix_exhausted",
                    &format!("{AUTO_FIX_LIMIT} fixes this task; the model applies the rest"),
                );
            }
            return false;
        }
        true
    }

    /// Apply the selected fix as an ordinary edit, then check it again.
    pub(in crate::harness::runner) async fn auto_apply_fix(&mut self) -> Result<()> {
        let Some((finding, fix)) = self
            .task
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.auto_fix())
            .map(|(finding, fix)| (finding.clone(), fix.clone()))
        else {
            return Ok(());
        };
        let place = format!(
            "{}:{}: {}",
            finding.file,
            finding.line,
            finding.message.lines().next().unwrap_or_default()
        );
        let planned = self
            .task
            .plan
            .as_ref()
            .is_some_and(|plan| plan.files.contains(&fix.file));
        if !planned || !self.task.read_files.contains(&fix.file) {
            return self.hold_fix(&fix.file, "not a planned file the model has read");
        }
        let Some(before) = self.workspace.read(&fix.file)? else {
            return self.hold_fix(&fix.file, "the file no longer exists");
        };
        let Some(after) = fix.apply(&before) else {
            return self.hold_fix(&fix.file, "the file changed since the fix was offered");
        };
        if panicking_calls(&after) > panicking_calls(&before) {
            return self.hold_fix(&fix.file, "the fix adds a panicking call");
        }
        // Policy decides as for any edit; a gate holds the fix for the model,
        // whose own apply_fix would bring it to the human.
        let context = self.refresh(std::slice::from_ref(&fix.file)).await?;
        if matches!(context.files[0].policy, PolicyDecision::Gate { .. }) {
            return self.hold_fix(&fix.file, "policy gates edits to this file");
        }
        let chain = {
            let state = self.symbolic_state_mut();
            state.auto_fix_chain += 1;
            state.auto_fixes += 1;
            state.auto_fix_chain
        };
        self.intent_event(
            "fix_auto_applied",
            &format!(
                "{}: {} for {place}; chain {chain}/{AUTO_FIX_CHAIN}",
                fix.file, fix.title
            ),
        );
        self.event(format!(
            "Harness applied fix: {} to {} (the language server's preferred fix for {place}).",
            fix.title, fix.file
        ));
        // Marked before the edit persists, so a restart never counts it as
        // the model's. Should the edit not land, the mark falls on the model's
        // next edit instead, which then only delays auto-verify.
        let index = self.task.edits.len();
        self.symbolic_state_mut().auto_fixed_edits.insert(index);
        if let Err(error) = self.apply_edit(PendingEdit {
            file: fix.file.clone(),
            before: Some(before),
            after: Some(after.clone()),
            reason: String::new(),
            revision: context.revision,
        }) {
            self.symbolic_state_mut().auto_fixed_edits.remove(&index);
            return Err(error);
        }
        let note = format!(
            "The harness applied the language server's preferred fix to {}: {} (for {place}). The source above includes it.",
            fix.file, fix.title
        );
        self.task.last_response = if chain > 1 && self.task.last_response_observation {
            format!("{}\n{note}", self.task.last_response)
        } else {
            note
        };
        self.task.last_response_observation = true;
        self.settle_applied_edit(&fix.file, true, Some(&after), true)
            .await;
        self.persist()
    }

    fn hold_fix(&mut self, file: &str, reason: &str) -> Result<()> {
        self.intent_event("fix_auto_held", &format!("{file}: {reason}"));
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn panicking_calls_are_counted_in_the_whole_text() {
        assert_eq!(super::panicking_calls("let x = y;"), 0);
        assert_eq!(
            super::panicking_calls("let x: usize = y.try_into().unwrap();\nz.expect(\"z\");"),
            2
        );
    }
}
