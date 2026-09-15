//! Grounding in the runner. Edit-time: before a policy-checked edit applies, ask
//! the daemon where the code index defines what the edit compares. When a
//! compared literal is missing from a definition, or a compared name is defined
//! in a file the model has not read (or has changed since it read it), the edit
//! is held once: the defining files
//! join the working set and a note names the definitions. The same edit proposed
//! again goes through. Plan-time: the second replan continued in one approval
//! cycle grounds the approved plan once in the same way. Never a halt, a repair
//! or a new decision.
use super::super::Runner;
use crate::harness::digest::sha256_hex;
use crate::harness::protocol::{GroundRequest, GroundResponse, PlanGroundRequest};

/// Defining files read into the working set per grounding note.
const MAX_GROUNDING_READS: usize = 2;
/// Replan continuations in one approval cycle before its plan is grounded.
const DISPUTES_BEFORE_PLAN_GROUNDING: usize = 2;
/// Bound on a `plan_grounding` journal detail.
const PLAN_GROUNDING_DETAIL_BYTES: usize = 2048;

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
        let read = self
            .read_for_grounding(
                response
                    .mismatches
                    .iter()
                    .map(|mismatch| mismatch.file.clone())
                    .chain(unread)
                    .collect(),
            )
            .await;
        let mut note = format!(
            "Edit to {file} not applied yet. It compares values the code defines elsewhere:\n"
        );
        note.push_str(&grounding_details(&response));
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

    /// The second replan continued in one approval cycle is a dispute the
    /// continuation note did not settle: ground the approved plan once. The
    /// definitions its summary and the replan reason compare against join the
    /// working set with a note, and the unchanged window ends, so a further replan
    /// reopens planning. Nothing defined, or the route failing, leaves the
    /// continuation as it was.
    pub(in crate::harness::runner) async fn ground_disputed_plan(&mut self, reason: &str) {
        let due = self.task.symbolic.as_ref().is_some_and(|state| {
            !state.plan_grounded
                && state.cycle_replan_continuations >= DISPUTES_BEFORE_PLAN_GROUNDING
        });
        let Some(plan) = self.task.plan.clone().filter(|_| due) else {
            return;
        };
        self.symbolic_state_mut().plan_grounded = true;
        let request = PlanGroundRequest {
            text: format!("{}\n{reason}", plan.summary),
            files: plan.files,
        };
        let response = match self.ground_plan(&request).await {
            Ok(response) => response,
            Err(error) => {
                self.plan_grounding_event(format!("unavailable ({error:#}); continuation stands"));
                return;
            }
        };
        if response.definitions.is_empty() {
            self.plan_grounding_event(format!(
                "keys {}; no definitions; continuation stands",
                response.keys.len()
            ));
            return;
        }
        let read = self
            .read_for_grounding(
                response
                    .mismatches
                    .iter()
                    .map(|mismatch| mismatch.file.clone())
                    .chain(
                        response
                            .definitions
                            .iter()
                            .map(|definition| definition.file.clone()),
                    )
                    .collect(),
            )
            .await;
        let mut note =
            "Replan not taken yet: the approved plan names values the code defines elsewhere:\n"
                .to_string();
        note.push_str(&grounding_details(&response));
        note.push_str("\nThe defining files are now in your working set. If the approved plan relies on a value the code does not define, replan with a corrected plan; otherwise make the change it describes.");
        self.plan_grounding_event(format!(
            "keys {}; definitions {}; mismatches {}; read {}",
            response.keys.len(),
            response.definitions.len(),
            response.mismatches.len(),
            read.join(", ")
        ));
        self.event(note.clone());
        self.task.last_response = note;
        self.end_unchanged_window();
    }

    /// Read defining files into the working set, at most
    /// [`MAX_GROUNDING_READS`], skipping files already in it.
    async fn read_for_grounding(&mut self, candidates: Vec<String>) -> Vec<String> {
        let mut read = Vec::new();
        for defining in candidates {
            if read.len() >= MAX_GROUNDING_READS
                || read.contains(&defining)
                || self.task.read_files.contains(&defining)
            {
                continue;
            }
            if self.read_into_working_set(&defining).await.is_ok() {
                read.push(defining);
            }
        }
        read
    }

    fn plan_grounding_event(&mut self, mut detail: String) {
        if detail.len() > PLAN_GROUNDING_DETAIL_BYTES {
            let mut end = PLAN_GROUNDING_DETAIL_BYTES;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
        }
        self.intent_event("plan_grounding", &detail);
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

/// The keys, definitions (with proven previews) and mismatches of a grounding
/// answer, one note section each.
fn grounding_details(response: &GroundResponse) -> String {
    let mut note = String::new();
    for key in &response.keys {
        if key.literals.is_empty() {
            note.push_str(&format!("- {}\n", key.attribute));
            continue;
        }
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
    note
}
