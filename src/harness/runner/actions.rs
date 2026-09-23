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
                let decoded = (count == 0)
                    .then(|| decode_json_escapes(&old_text))
                    .flatten();
                let (old_text, new_text) = if count == 1 {
                    (old_text, new_text)
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

    /// badciv-map's shape: nine replaces whose old_text was the whole file,
    /// the last six changing 12 to 308 bytes of about 16 KB.
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
}
