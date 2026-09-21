//! Validate sensor arguments and materialize edits before permission or execution.
use super::{model::Action, Mode, Runner, MAX_FILES, MAX_PLAN_SUMMARY};
use anyhow::{ensure, Context, Result};
use serde::Serialize;

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
    },
    Read {
        file: String,
    },
    Search {
        query: String,
    },
    Plan {
        summary: String,
        files: Vec<String>,
        checks: Vec<String>,
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
            Action::Reply { message } => {
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
                    "permission request must name at least one capability"
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
            Action::Reply { message } => return Ok(Step::Reply { message }),
            Action::Read { file } => return Ok(Step::Read { file }),
            Action::Search { query } => return Ok(Step::Search { query }),
            Action::Plan {
                summary,
                files,
                checks,
            } => {
                return Ok(Step::Plan {
                    summary,
                    files,
                    checks,
                })
            }
            Action::Command { command } => return Ok(Step::Command { command }),
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
        };
        if !self.task.read_files.contains(file) {
            // The next generation receives the source and dossier. Never apply a
            // proposal whose author did not see the governing source snapshot.
            self.event(format!("First-edit guard: requesting source and dossier for {file}; the unread edit proposal will not execute."));
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
                let (old_text, new_text) = match (count, repair_literal_span(source, &old_text)) {
                    (0, Some(repair)) => {
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
                    }
                    _ => {
                        ensure!(count == 1, "replace old_text must match exactly once; found {count}{}; use the supplied source to select a unique literal span", if count == 2 { " or more" } else { "" });
                        (old_text, new_text)
                    }
                };
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
    fn new_text_loses_only_the_same_junk_at_the_same_end() {
        let repair = repair_literal_span("original\n", "original\n}}").unwrap();
        assert_eq!(strip_same_junk("changed\n}}", &repair), "changed\n");
        assert_eq!(strip_same_junk("changed\n", &repair), "changed\n");
        let repair = repair_literal_span("fn x() {}\n", "\"$ fn x() {}").unwrap();
        assert_eq!(strip_same_junk("\"$ fn y() {}", &repair), "fn y() {}");
        assert_eq!(strip_same_junk("fn y() {}", &repair), "fn y() {}");
    }
}
