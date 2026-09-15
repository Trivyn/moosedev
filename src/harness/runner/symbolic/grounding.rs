//! Edit-time grounding in the runner: before a policy-checked edit applies, ask
//! the daemon where the code index defines what the edit compares. When a
//! compared literal is missing from a definition, or a compared name is defined
//! in a file the model has not read (or has changed since it read it), the edit
//! is held once: the defining files
//! join the working set and a note names the definitions. The same edit proposed
//! again goes through. Never a halt, a repair or a new decision.
use super::super::Runner;
use crate::harness::digest::sha256_hex;
use crate::harness::protocol::GroundRequest;

/// Defining files read into the working set per grounding note.
const MAX_GROUNDING_READS: usize = 2;

impl Runner {
    /// True when the edit was held for grounding; the caller applies nothing.
    pub(in crate::harness::runner) async fn ground_edit(
        &mut self,
        file: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> bool {
        let Some(after) = after else {
            return false;
        };
        let (ranges, ranges_coalesced) =
            super::super::scope::edit_ranges(before.unwrap_or_default(), after);
        let request = GroundRequest {
            file: file.to_string(),
            after: after.to_string(),
            ranges,
            ranges_coalesced,
        };
        let response = match self.ground(&request).await {
            Ok(response) => response,
            Err(error) => {
                self.intent_event(
                    "edit_grounding",
                    &format!("{file}: unavailable ({error:#}); edit continues"),
                );
                return false;
            }
        };
        if response.keys.is_empty() {
            return false;
        }
        let memo = sha256_hex(format!(
            "{file}\u{0}{}",
            serde_json::to_string(&response.keys).unwrap_or_default()
        ));
        if self
            .task
            .symbolic
            .as_ref()
            .is_some_and(|state| state.grounded.contains(&memo))
        {
            return false;
        }
        let unread: Vec<String> = response
            .definitions
            .iter()
            .map(|definition| definition.file.clone())
            .filter(|defining| {
                !self.task.read_files.contains(defining) && !self.read_is_current(defining)
            })
            .collect();
        if response.mismatches.is_empty() && unread.is_empty() {
            return false;
        }
        self.symbolic_state_mut().grounded.insert(memo);
        let mut read = Vec::new();
        for defining in response
            .mismatches
            .iter()
            .map(|mismatch| mismatch.file.clone())
            .chain(unread)
        {
            if read.len() >= MAX_GROUNDING_READS || read.contains(&defining) {
                continue;
            }
            if self.task.read_files.contains(&defining) {
                continue;
            }
            if self.read_into_working_set(&defining).await.is_ok() {
                read.push(defining);
            }
        }
        let mut note = format!(
            "Edit to {file} not applied yet. It compares values the code defines elsewhere:\n"
        );
        for key in &response.keys {
            note.push_str(&format!(
                "- {} compared with {}\n",
                key.attribute,
                key.literals
                    .iter()
                    .map(|literal| format!("'{literal}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for definition in &response.definitions {
            note.push_str(&format!(
                "\n{} in {}:\n{}\n",
                definition.name,
                definition.file,
                if definition.preview.is_empty() {
                    "[source not shown: the index cannot prove this file is current]"
                } else {
                    definition.preview.as_str()
                }
            ));
        }
        for mismatch in &response.mismatches {
            note.push_str(&format!(
                "\n'{}' does not appear among the values of {} in {}.\n",
                mismatch.literal, mismatch.definition, mismatch.file
            ));
        }
        note.push_str("\nThe defining files are now in your working set. Revise the edit to use a defined value, or propose the same edit again if it is intended.");
        self.intent_event(
            "edit_grounding",
            &format!(
                "{file}: keys {}; definitions {}; mismatches {}; read {}",
                response.keys.len(),
                response.definitions.len(),
                response.mismatches.len(),
                read.join(", ")
            ),
        );
        self.event(note.clone());
        self.task.last_response = note;
        true
    }

    /// True when the model read `file` earlier in this task and its content is
    /// unchanged since that read, even if a stored plan has since narrowed the
    /// working set.
    fn read_is_current(&self, file: &str) -> bool {
        let Some(read) = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.read_snapshots.get(file))
        else {
            return false;
        };
        self.snapshot(&[file.to_string()])
            .ok()
            .and_then(|now| now.get(file).cloned())
            .as_ref()
            == Some(read)
    }
}
