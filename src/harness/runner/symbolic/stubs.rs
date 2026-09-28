//! Code the model left unwritten: stub markers, and planned files that do
//! not exist yet. A small builder that meets a compile error
//! can make the crate build with `unimplemented!()` and call finish; with
//! only an ignored test the checks then pass (badciv be128e71). The markers
//! are per language, in the registry.

use crate::code::substrate::lang::{is_test_path, stub_syntax_for, StubSyntax};

/// Whether `marker` occurs in `line` as code, read with the language's own
/// syntax: not after one of its comment openers and not inside one of its
/// strings. Line-level and approximate (a string opened on an earlier line is
/// not seen); a false match only costs the one send-back the gate allows per
/// source.
fn is_code(line: &str, marker: &str, syntax: &StubSyntax) -> bool {
    line.match_indices(marker)
        .any(|(at, _)| code_at(line, at, syntax))
}

/// Whether byte `at` of `line` is code: not after a comment opener and not
/// inside a string.
pub(in crate::harness::runner) fn code_at(line: &str, at: usize, syntax: &StubSyntax) -> bool {
    let before = &line[..at];
    let opener = before.trim_start();
    let commented = syntax
        .line_comments
        .iter()
        .any(|comment| before.contains(comment))
        || syntax
            .block_comments
            .iter()
            .any(|comment| opener.starts_with(comment));
    let quoted = syntax
        .quotes
        .iter()
        .any(|quote| before.matches(*quote).count() % 2 == 1);
    !commented && !quoted
}

/// The stub `line` holds, as the report names it: a marker (`todo!(`), or a
/// failure construct in code followed, in the same statement (before the
/// next `;` in code), by a stub message inside a string
/// (`Err(MapError::Parse("Not implemented".to_string()))` is named `"Not
/// implemented"`). A failure with any other message (`"file not found"`) is
/// ordinary code, and so is a failure beside a statement that only holds the
/// message (`let r = Err(e); let s = "not implemented";`). A function that
/// returns the message without failing (`"Not implemented".to_string()`) is
/// not recognised.
fn line_stub(line: &str, syntax: &StubSyntax) -> Option<String> {
    if let Some(marker) = syntax
        .markers
        .iter()
        .find(|marker| is_code(line, marker, syntax))
    {
        return Some((*marker).to_owned());
    }
    // Each failure in code, as the span its statement's message may lie in:
    // from the construct's end to the next `;` in code, or the line's end.
    let failures: Vec<(usize, usize)> = syntax
        .failure_constructs
        .iter()
        .flat_map(|construct| {
            line.match_indices(construct)
                .filter(|(at, _)| code_at(line, *at, syntax))
                .map(|(at, construct)| {
                    let start = at + construct.len();
                    let end = line[start..]
                        .match_indices(';')
                        .map(|(offset, _)| start + offset)
                        .find(|&semicolon| code_at(line, semicolon, syntax))
                        .unwrap_or(line.len());
                    (start, end)
                })
        })
        .collect();
    if failures.is_empty() {
        return None;
    }
    // ASCII lowering keeps every byte offset.
    let lower = line.to_ascii_lowercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    syntax.stub_messages.iter().find_map(|message| {
        lower.match_indices(message).find_map(|(at, _)| {
            let end = at + message.len();
            let whole = !word(line[..at].chars().next_back()) && !word(line[end..].chars().next());
            let failing = failures
                .iter()
                .any(|&(start, stop)| (start..stop).contains(&at));
            (whole && failing && in_string(line, at, syntax))
                .then(|| format!("\"{}\"", &line[at..end]))
        })
    })
}

/// Whether byte `at` of `line` lies inside one of the language's strings and
/// not after a line comment opener: the inverse of [`is_code`]'s quote test.
fn in_string(line: &str, at: usize, syntax: &StubSyntax) -> bool {
    let before = &line[..at];
    let commented = syntax
        .line_comments
        .iter()
        .any(|comment| before.contains(comment));
    !commented
        && syntax
            .quotes
            .iter()
            .any(|quote| before.matches(*quote).count() % 2 == 1)
}

use super::super::Runner;
use anyhow::Result;

impl Runner {
    /// Stubs left in the plan's files, as `(file, line, stub)`, read from
    /// disk.
    pub(in crate::harness::runner) fn planned_stubs(&self) -> Vec<(String, usize, String)> {
        let Some(plan) = self.task.plan.as_ref() else {
            return Vec::new();
        };
        let mut stubs = Vec::new();
        for file in &plan.files {
            // A language without a stub idiom is not judged, and a test may
            // name a marker on purpose (asserting a message, a fixture).
            let Some(syntax) = stub_syntax_for(file).filter(|_| !is_test_path(file)) else {
                continue;
            };
            let Ok(Some(text)) = self.workspace.read(file) else {
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if let Some(stub) = line_stub(line, syntax) {
                    stubs.push((file.clone(), index + 1, stub));
                }
            }
        }
        stubs
    }

    /// The plan's files that do not exist on disk yet, in plan order.
    pub(in crate::harness::runner) fn unwritten_planned_files(&self) -> Vec<String> {
        self.task
            .plan
            .as_ref()
            .map(|plan| {
                plan.files
                    .iter()
                    .filter(|file| matches!(self.workspace.read(file), Ok(None)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The plan's files that exist but have no edit of the model's in this
    /// approval cycle (since `cycle_edit_start`), in plan order. A fix the
    /// harness applied is not the model's work on the file.
    pub(in crate::harness::runner) fn planned_files_unedited(&self) -> Vec<String> {
        let Some(plan) = self.task.plan.as_ref() else {
            return Vec::new();
        };
        let (start, harness_fixes) = self.task.symbolic.as_ref().map_or((0, None), |state| {
            (state.cycle_edit_start, Some(&state.auto_fixed_edits))
        });
        let edited_by_model = |file: &String| {
            self.task
                .edits
                .iter()
                .enumerate()
                .skip(start)
                .any(|(index, edit)| {
                    &edit.file == file && !harness_fixes.is_some_and(|fixes| fixes.contains(&index))
                })
        };
        plan.files
            .iter()
            .filter(|file| {
                matches!(self.workspace.read(file), Ok(Some(_))) && !edited_by_model(file)
            })
            .cloned()
            .collect()
    }

    /// Send a finish back while planned files hold stubs: once per source
    /// state, so a repeat finish goes on to the checks, which decide. True
    /// when the finish was refused.
    pub(in crate::harness::runner) fn refuse_stubbed_finish(&mut self) -> bool {
        let stubs = self.planned_stubs();
        if stubs.is_empty() {
            return false;
        }
        let at = self.task.edits.len();
        let state = self.symbolic_state_mut();
        if state.stub_refused_at == Some(at) {
            return false;
        }
        state.stub_refused_at = Some(at);
        let listing = stubs
            .iter()
            .take(8)
            .map(|(file, line, marker)| format!("{file}:{line} {marker}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.intent_event("finish_refused_stubs", &listing);
        self.event(format!(
            "Finish refused: planned files still hold stubs: {listing}."
        ));
        self.task.last_response = format!(
            "Not finished: planned files still hold code that is not written yet ({listing}). Write that code, then finish."
        );
        true
    }

    /// Send a finish back while planned files are missing or have no model
    /// edit this approval cycle: once per source state, naming each group.
    /// A repeat finish at the same source state asks the human, since only
    /// they can say whether the plan still wants the files: about the missing
    /// ones first (badciv P5: step 2 finished with 4 planned test files never
    /// written), else about the unedited ones, which a plan may list needing
    /// no change (badciv P5 attempt 3 spent finish after finish with planned
    /// files untouched). True when the finish was refused or parked.
    pub(in crate::harness::runner) fn refuse_unfinished_plan(&mut self) -> Result<bool> {
        let missing = self.unwritten_planned_files();
        let unedited = self.planned_files_unedited();
        if missing.is_empty() && unedited.is_empty() {
            return Ok(false);
        }
        let at = self.task.edits.len();
        let state = self.symbolic_state_mut();
        if state.unfinished_accepted_at == Some(at) {
            return Ok(false);
        }
        if state.unfinished_refused_at != Some(at) {
            state.unfinished_refused_at = Some(at);
            let mut facts = Vec::new();
            if !missing.is_empty() {
                facts.push(format!(
                    "planned file(s) {} do not exist yet",
                    missing.join(", ")
                ));
            }
            if !unedited.is_empty() {
                facts.push(format!(
                    "planned file(s) {} have no edit since the plan was approved",
                    unedited.join(", ")
                ));
            }
            let facts = facts.join("; ");
            self.intent_event(
                "finish_refused_unfinished",
                &format!(
                    "missing: [{}]; unedited: [{}]",
                    missing.join(", "),
                    unedited.join(", ")
                ),
            );
            self.event(format!("Finish refused: {facts}."));
            self.task.last_response = format!(
                "Not finished: {facts}. Write what the plan still needs, then finish. A planned file that needs no change can stay as it is: finish again."
            );
            return Ok(true);
        }
        if missing.is_empty() {
            self.ask_unedited_planned_files(unedited)?;
        } else {
            self.ask_missing_planned_files(missing)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_code, line_stub};
    use crate::code::substrate::lang::stub_syntax_for;

    #[test]
    fn a_failure_whose_message_says_not_written_is_a_stub() {
        let rust = stub_syntax_for("a.rs").unwrap();
        let python = stub_syntax_for("a.py").unwrap();
        let typescript = stub_syntax_for("a.ts").unwrap();
        let stub = |line: &str, syntax| line_stub(line, syntax);
        // badciv P5, verbatim.
        assert_eq!(
            stub(
                r#"    Err(MapError::Parse("Not implemented".to_string()))"#,
                rust
            )
            .as_deref(),
            Some("\"Not implemented\"")
        );
        assert_eq!(
            stub(r#"    panic!("not yet implemented: {name}")"#, rust).as_deref(),
            Some("\"not yet implemented\"")
        );
        assert_eq!(
            stub(r#"    anyhow::bail!("TODO")"#, rust).as_deref(),
            Some("\"TODO\"")
        );
        assert_eq!(stub("    todo!()", rust).as_deref(), Some("todo!("));
        // The statement ends at a `;` in code, not one inside the message.
        assert_eq!(
            stub(
                r#"    let r = Err(Error::new("a; not implemented"));"#,
                rust
            )
            .as_deref(),
            Some("\"not implemented\"")
        );
        assert_eq!(
            stub(r#"        raise ValueError("not implemented")"#, python).as_deref(),
            Some("\"not implemented\"")
        );
        assert_eq!(
            stub("        raise RuntimeError('Unimplemented')", python).as_deref(),
            Some("\"Unimplemented\"")
        );
        assert_eq!(
            stub("  throw new Error('Not yet implemented');", typescript).as_deref(),
            Some("\"Not yet implemented\"")
        );
        // Not stubs: another message, a comment, a message outside a string,
        // a failure in a comment, a word that merely contains one, and the
        // bare return badciv P5 also left (not recognised, by design).
        for line in [
            r#"    Err(MapError::Io("file not found".to_string()))"#,
            "    // Err(\"not implemented\") until the parser lands",
            "    return Err(e); // not implemented for v2",
            r#"    Err(MapError::Parse(format!("todos: {n}")))"#,
            r#"    "Not implemented".to_string()"#,
            r#"    let m = "not implemented";"#,
            // codex: a failure and a message in separate statements.
            r#"    let result = Err(io_error); let status = "not implemented";"#,
            r#"    let status = "not implemented"; return Err(io_error);"#,
        ] {
            assert_eq!(stub(line, rust), None, "{line}");
        }
        assert_eq!(
            stub("    # raise ValueError('not implemented')", python),
            None
        );
        assert_eq!(
            stub("    raise KeyError(f'file not found: {p}')", python),
            None
        );
        assert_eq!(
            stub("  // throw new Error('not implemented')", typescript),
            None
        );
    }

    #[test]
    fn a_marker_counts_only_as_code_in_its_own_language() {
        let rust = stub_syntax_for("a.rs").unwrap();
        let python = stub_syntax_for("a.py").unwrap();
        let typescript = stub_syntax_for("a.ts").unwrap();
        assert!(is_code("    unimplemented!()", "unimplemented!(", rust));
        assert!(is_code("pub fn f() { todo!() }", "todo!(", rust));
        assert!(is_code("#[inline] fn f() { todo!() }", "todo!(", rust));
        assert!(!is_code("    // todo!() later", "todo!(", rust));
        assert!(!is_code(
            "    /// Never unimplemented!() here.",
            "unimplemented!(",
            rust
        ));
        assert!(!is_code(
            "     * todo!() in a block comment",
            "todo!(",
            rust
        ));
        assert!(!is_code(
            r#"    let m = "todo!() is banned";"#,
            "todo!(",
            rust
        ));
        let raise = "raise NotImplementedError";
        assert!(is_code("        raise NotImplementedError", raise, python));
        assert!(!is_code(
            "    # raise NotImplementedError when unsupported",
            raise,
            python
        ));
        assert!(!is_code(
            "    msg = 'raise NotImplementedError'",
            raise,
            python
        ));
        let throw = "throw new Error(\"Not implemented\")";
        assert!(is_code(
            "  throw new Error(\"Not implemented\");",
            throw,
            typescript
        ));
        assert!(!is_code(
            "  const s = `throw new Error(\"Not implemented\")`;",
            throw,
            typescript
        ));
    }
}
