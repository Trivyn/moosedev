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
    line.match_indices(marker).any(|(at, _)| {
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
    })
}

use super::super::Runner;

impl Runner {
    /// Stub markers left in the plan's files, as `(file, line, marker)`, read
    /// from disk.
    pub(in crate::harness::runner) fn planned_stubs(&self) -> Vec<(String, usize, &'static str)> {
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
                if let Some(marker) = syntax
                    .markers
                    .iter()
                    .find(|marker| is_code(line, marker, syntax))
                {
                    stubs.push((file.clone(), index + 1, *marker));
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
}

#[cfg(test)]
mod tests {
    use super::is_code;
    use crate::code::substrate::lang::stub_syntax_for;

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
