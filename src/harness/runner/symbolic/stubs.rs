//! Code the model left unwritten: stub markers, and planned files that do
//! not exist yet. A small builder that meets a compile error
//! can make the crate build with `unimplemented!()` and call finish; with
//! only an ignored test the checks then pass (badciv be128e71). The markers
//! are per language, in the registry.

use crate::code::substrate::lang::{is_test_path, stub_syntax_for, StubSyntax};

/// Whether `marker` occurs in `line` as code, read with the language's own
/// syntax: not after one of its comment openers and not inside one of its
/// strings. Line-level and approximate (a whole file is read through
/// [`code_lines`] first, which blanks comments and strings that span lines);
/// a false match only costs the one send-back the gate allows per source.
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
    let quoted = syntax.quotes.iter().any(|quote| quote_open(before, *quote));
    !commented && !quoted
}

/// Whether `before` leaves a `quote` string open: an odd count of `quote`s
/// not escaped by a backslash (`"a\"b` is still open).
pub(in crate::harness::runner) fn quote_open(before: &str, quote: char) -> bool {
    let mut open = false;
    let mut escaped = false;
    for c in before.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            open = !open;
        }
    }
    open
}

/// `text`'s lines with every comment or string that spans lines (the
/// language's `multiline` delimiters: a Rust `/* */` block, a Python
/// triple-quoted string) blanked out, delimiters included, byte offsets
/// kept: a `todo!()` in a block comment or `raise NotImplementedError` in a
/// docstring is not code, and a line read alone cannot see where the span
/// opened. A span closed on the line it opens on is left to the line-level
/// reading, which judges a stub message inside a string. Approximate: an
/// ordinary string ends with its line, and block comments do not nest.
fn code_lines(text: &str, syntax: &StubSyntax) -> Vec<String> {
    // The open span: its closer, whether escapes apply in it (a string), and
    // the line and byte it opened at.
    let mut open: Option<(&str, bool, usize, usize)> = None;
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut blank: Vec<(usize, usize)> = Vec::new();
        let mut quote: Option<char> = None;
        let mut at = 0;
        while at < line.len() {
            let rest = &line[at..];
            if let Some((closer, string, opened_line, opened_at)) = open {
                let from = if opened_line == index { opened_at } else { 0 };
                match span_end(rest, closer, string) {
                    Some(end) => {
                        at += end;
                        if opened_line != index {
                            blank.push((from, at));
                        }
                        open = None;
                    }
                    None => {
                        blank.push((from, line.len()));
                        at = line.len();
                    }
                }
                continue;
            }
            let c = rest.chars().next().unwrap_or_default();
            if let Some(open_quote) = quote {
                if c == '\\' {
                    at += 1;
                    at += line[at..].chars().next().map_or(0, char::len_utf8);
                    continue;
                }
                if c == open_quote {
                    quote = None;
                }
            } else if syntax
                .line_comments
                .iter()
                .any(|comment| rest.starts_with(comment))
            {
                break;
            } else if let Some((opener, closer)) = syntax
                .multiline
                .iter()
                .find(|(opener, _)| rest.starts_with(opener))
            {
                let string = syntax.quotes.iter().any(|quote| opener.starts_with(*quote));
                open = Some((closer, string, index, at));
                at += opener.len();
                continue;
            } else if syntax.quotes.contains(&c) {
                quote = Some(c);
            }
            at += c.len_utf8();
        }
        let mut code = line.to_owned();
        for (from, to) in blank {
            code.replace_range(from..to, &" ".repeat(to - from));
        }
        lines.push(code);
    }
    lines
}

/// The byte just past `closer` in `rest`, skipping a backslash-escaped
/// character inside a string.
fn span_end(rest: &str, closer: &str, string: bool) -> Option<usize> {
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        if rest[at..].starts_with(closer) {
            return Some(at + closer.len());
        }
        if string && c == '\\' {
            chars.next();
        }
    }
    None
}

/// The stubs of a file's `text`, as `(line, stub)` with 1-based lines.
fn text_stubs(text: &str, syntax: &StubSyntax) -> Vec<(usize, String)> {
    code_lines(text, syntax)
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line_stub(line, syntax).map(|stub| (index + 1, stub)))
        .collect()
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
    !commented && syntax.quotes.iter().any(|quote| quote_open(before, *quote))
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
        plan.files
            .iter()
            .flat_map(|file| {
                self.file_stubs(file)
                    .into_iter()
                    .map(move |(line, stub)| (file.clone(), line, stub))
            })
            .collect()
    }

    /// Stubs left in `file`, as `(line, stub)`, read from disk. A file that
    /// does not exist holds none, a language without a stub idiom is not
    /// judged, and a test may name a marker on purpose (asserting a message,
    /// a fixture).
    pub(in crate::harness::runner) fn file_stubs(&self, file: &str) -> Vec<(usize, String)> {
        let Some(syntax) = stub_syntax_for(file).filter(|_| !is_test_path(file)) else {
            return Vec::new();
        };
        match self.workspace.read(file) {
            Ok(Some(text)) => text_stubs(&text, syntax),
            _ => Vec::new(),
        }
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
    use super::{is_code, line_stub, text_stubs};
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
        // A quote escaped inside a string does not close it.
        assert!(!is_code(r#"    let m = "a \" todo!()";"#, "todo!(", rust));
    }

    /// A marker inside a comment or string that spans lines is not code: a
    /// line read alone cannot see the `/*` or `"""` that opened it.
    #[test]
    fn a_marker_in_a_multiline_comment_or_docstring_is_not_a_stub() {
        let rust = stub_syntax_for("a.rs").unwrap();
        let python = stub_syntax_for("a.py").unwrap();
        let typescript = stub_syntax_for("a.ts").unwrap();
        let stubs = |text: &str, syntax| text_stubs(text, syntax);
        let found = |pairs: &[(usize, &str)]| {
            pairs
                .iter()
                .map(|(line, stub)| (*line, (*stub).to_owned()))
                .collect::<Vec<_>>()
        };
        let block = "fn f() -> u32 {\n    /* Before this lands:\n    todo!()\n    */\n    1\n}\n";
        assert!(stubs(block, rust).is_empty());
        // Code after the closer on its line, and after the block: stubs.
        let after = "/* a\n b */ todo!();\nfn g() {\n    unimplemented!()\n}\n";
        assert_eq!(
            stubs(after, rust),
            found(&[(2, "todo!("), (4, "unimplemented!(")])
        );
        // Code before an opener on its line is still read.
        assert_eq!(
            stubs("fn f() { todo!() } /* note\n todo!()\n*/\n", rust),
            found(&[(1, "todo!(")])
        );
        // An opener inside a string or after a line comment opens nothing.
        assert_eq!(
            stubs("let s = \"/*\";\ntodo!()\n// /*\nunimplemented!()\n", rust),
            found(&[(2, "todo!("), (4, "unimplemented!(")])
        );
        let docstring = "def parse(text):\n    \"\"\"Parse the map.\n\n    raise NotImplementedError for unknown sections.\n    \"\"\"\n    return text\n";
        assert!(stubs(docstring, python).is_empty());
        let single =
            "HELP = '''\nraise NotImplementedError\n'''\ndef f():\n    raise NotImplementedError\n";
        assert_eq!(
            stubs(single, python),
            found(&[(5, "raise NotImplementedError")])
        );
        // A one-line docstring stays with the line-level reading.
        assert_eq!(
            stubs(
                "def f():\n    \"\"\"Doc.\"\"\"\n    raise NotImplementedError\n",
                python
            ),
            found(&[(3, "raise NotImplementedError")])
        );
        let ts = "/*\n throw new Error(\"Not implemented\");\n*/\nexport const x = 1;\n";
        assert!(stubs(ts, typescript).is_empty());
    }
}
