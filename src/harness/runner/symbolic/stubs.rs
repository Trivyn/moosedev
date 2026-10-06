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

/// `entries` resolved to the planned `files`, once each in the order first
/// named, and the entries naming none. An entry names a file by its path
/// (`./` and surrounding space ignored) or by a suffix of whole path
/// components only one planned file ends with (`src/lib.rs`). Blank entries are neither.
fn stub_entries(entries: &[String], files: &[String]) -> (Vec<String>, Vec<String>) {
    let mut resolved: Vec<String> = Vec::new();
    let mut unknown = Vec::new();
    for entry in entries {
        let name = entry.trim().trim_start_matches("./");
        if name.is_empty() {
            continue;
        }
        let suffix = format!("/{name}");
        let exact = files.iter().find(|file| file.as_str() == name);
        let mut by_suffix = files.iter().filter(|file| file.ends_with(&suffix));
        let file = exact.or_else(|| match (by_suffix.next(), by_suffix.next()) {
            (Some(file), None) => Some(file),
            _ => None,
        });
        match file {
            Some(file) if !resolved.contains(file) => resolved.push(file.clone()),
            Some(_) => {}
            None => unknown.push(entry.trim().to_owned()),
        }
    }
    (resolved, unknown)
}

/// Whether plans may name the planned files that need no edit (`unchanged`).
/// `MOOSEDEV_HARNESS_PLAN_UNCHANGED=off` removes the field from the schema,
/// and every planned file must be edited again.
pub(in crate::harness::runner) fn plan_unchanged_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PLAN_UNCHANGED").map_or(true, |value| value.trim() != "off")
}

/// Whether plans may name the files they leave as stubs (`stubs`).
/// `MOOSEDEV_HARNESS_PLAN_STUBS=off` removes the field from the schema, and
/// every consumer then reads none, so every stub is judged.
pub(in crate::harness::runner) fn plan_stubs_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PLAN_STUBS").map_or(true, |value| value.trim() != "off")
}

use super::super::Runner;
use anyhow::Result;

/// A stub left in a planned file: `(file, line, stub)`, 1-based line.
pub(in crate::harness::runner) type Stub = (String, usize, String);

impl Runner {
    /// Stubs left in the plan's files, as `(file, line, stub)`, read from
    /// disk.
    pub(in crate::harness::runner) fn planned_stubs(&self) -> Vec<Stub> {
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

    /// [`Self::planned_stubs`] split into those in files the plan says it
    /// leaves as stubs, and the rest. The finish gate and auto-verify judge
    /// only the rest: a scaffold plan that asks for stubs is done once they
    /// are written (badciv run 15, where the refusal had the builder write
    /// the whole parser in the scaffold step).
    pub(in crate::harness::runner) fn planned_stubs_split(&self) -> (Vec<Stub>, Vec<Stub>) {
        let left = self
            .task
            .plan
            .as_ref()
            .map_or(&[][..], |plan| plan.stub_files());
        self.planned_stubs()
            .into_iter()
            .partition(|(file, _, _)| left.contains(file))
    }

    /// A proposed plan's `stubs` entries resolved to its `files`, once each
    /// in the order first named ([`stub_entries`]). An entry naming no
    /// planned file is journaled and dropped, never returned to the model.
    pub(in crate::harness::runner) fn resolve_plan_stubs(
        &mut self,
        stubs: &[String],
        files: &[String],
    ) -> Vec<String> {
        self.resolve_plan_files("stubs", stubs, files)
    }

    /// A proposed plan's `unchanged` entries resolved to its files, the way
    /// [`Self::resolve_plan_stubs`] resolves `stubs`.
    pub(in crate::harness::runner) fn resolve_plan_unchanged(
        &mut self,
        unchanged: &[String],
        files: &[String],
    ) -> Vec<String> {
        self.resolve_plan_files("unchanged", unchanged, files)
    }

    /// `entries` of the plan field `field` resolved to the planned `files`
    /// ([`stub_entries`]), journaled as `plan_{field}` and, for entries
    /// naming no planned file, `plan_{field}_unresolved`.
    fn resolve_plan_files(
        &mut self,
        field: &str,
        entries: &[String],
        files: &[String],
    ) -> Vec<String> {
        let (resolved, unknown) = stub_entries(entries, files);
        if !unknown.is_empty() {
            self.intent_event(&format!("plan_{field}_unresolved"), &unknown.join("; "));
            self.event(format!(
                "Plan {field} entries ignored: {} named no planned file.",
                unknown.join("; ")
            ));
        }
        if !resolved.is_empty() {
            self.intent_event(&format!("plan_{field}"), &resolved.join(" "));
        }
        resolved
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
        // A file the plan lists for reference needs no edit, once the model
        // has edited some other planned file this cycle: a plan that marks
        // every file unchanged cannot finish having changed nothing.
        let unchanged = plan.unchanged_files();
        let other_work = plan
            .files
            .iter()
            .any(|file| !unchanged.contains(file) && edited_by_model(file));
        plan.files
            .iter()
            .filter(|file| {
                !(other_work && unchanged.contains(file))
                    && matches!(self.workspace.read(file), Ok(Some(_)))
                    && !edited_by_model(file)
            })
            .cloned()
            .collect()
    }

    /// Send a finish back while planned files hold stubs the plan does not
    /// leave: once per source state, so a repeat finish goes on to the
    /// checks, which decide. True when the finish was refused.
    pub(in crate::harness::runner) fn refuse_stubbed_finish(&mut self) -> bool {
        let (_, stubs) = self.planned_stubs_split();
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
            // The human already said these files need work, and the model
            // finished again without editing anything: asking once more would
            // loop on the same answer (badciv run 17 asked 40 times), so stop
            // for the human with the files named.
            if self.symbolic_state_mut().unedited_work_at == Some(at) {
                // Parked once: after the human's answer, a finish asks again,
                // where the human can choose to verify as it stands.
                self.symbolic_state_mut().unedited_work_at = None;
                let files = unedited.join(", ");
                self.intent_event("unedited_work_parked", &files);
                self.stop_stuck(
                    "unedited work",
                    format!(
                        "Finish refused: {files} still unedited after the human chose work; parked for guidance."
                    ),
                    format!(
                        "The model finished again without editing {files}, after the human said the plan still needs them. Guidance is needed: say what each file needs; the next finish asks again, where finish verifies as it stands."
                    ),
                );
                return Ok(true);
            }
            self.ask_unedited_planned_files(unedited)?;
        } else {
            self.ask_missing_planned_files(missing)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_code, line_stub, stub_entries, text_stubs};
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

    #[test]
    fn plan_stub_entries_resolve_to_planned_files_by_path_or_unique_suffix() {
        let files: Vec<String> = [
            "badciv-map/src/lib.rs",
            "badciv-sim/src/lib.rs",
            "Cargo.toml",
        ]
        .map(String::from)
        .to_vec();
        let entries: Vec<String> = [
            " ./badciv-map/src/lib.rs ",
            "badciv-map/src/lib.rs",
            "src/lib.rs",
            "Cargo.toml",
            "",
            "tui.rs",
        ]
        .map(String::from)
        .to_vec();
        let (resolved, unknown) = stub_entries(&entries, &files);
        assert_eq!(resolved, ["badciv-map/src/lib.rs", "Cargo.toml"]);
        // Two planned files end with src/lib.rs, so that entry names neither.
        assert_eq!(unknown, ["src/lib.rs", "tui.rs"]);
        // A suffix matches whole path components only.
        let (resolved, unknown) = stub_entries(
            &[
                "sim/src/lib.rs".to_owned(),
                "badciv-sim/src/lib.rs".to_owned(),
            ],
            &files,
        );
        assert_eq!(resolved, ["badciv-sim/src/lib.rs"]);
        assert_eq!(unknown, ["sim/src/lib.rs"]);
        let nested = ["crates/sim/src/lib.rs".to_owned()];
        let (resolved, _) = stub_entries(&["sim/src/lib.rs".to_owned()], &nested);
        assert_eq!(resolved, nested);
    }
}
