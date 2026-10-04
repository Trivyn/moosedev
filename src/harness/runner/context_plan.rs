//! What one step prompt showed, section by section: the receipt of the
//! context plan (Requirement 6ffabf80, context plan item 1). Every model
//! request for a step action journals it twice: in full on its
//! `model_requests` entry, and as a compact `context_plan` intent event. It
//! only journals, like `source_delivery`, so it has no off switch.
use super::rule_state::RulesReceipt;
use super::source::{SourceView, Tier};
use crate::harness::session::EARLIER_TASKS_HEADER;
use serde::Serialize;
use std::collections::BTreeSet;

/// The receipt of one step prompt. The section byte counts (head, source,
/// navigation, history, state, observations), the output schema and the
/// repair note add up to `total`, the prompt text sent; `rules` is a part of
/// `head`.
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct ContextPlan {
    /// The step's scope files, in scope order.
    pub scope: Vec<String>,
    /// Working-set files preloaded from the scope, not read by the model.
    pub preloaded: Vec<String>,
    /// Scope files left out of the working set for space.
    pub preload_skipped: Vec<String>,
    pub rules: RulesPlan,
    pub source: SourcePlan,
    pub history: HistoryPlan,
    pub navigation_bytes: usize,
    pub observations_bytes: usize,
    /// Role, guidance, rules, instructions, knowledge and the plan; in the
    /// stable head (`stable_head_enabled`) also the output schema.
    pub head_bytes: usize,
    /// Guidance, harness state and the allowed actions.
    pub state_bytes: usize,
    /// The output schema appended to the prompt under the json_schema
    /// contract; 0 under tools, whose definitions travel beside the prompt.
    pub schema_bytes: usize,
    /// The note a repair attempt appends: why the last candidate was
    /// rejected.
    pub repair_bytes: usize,
    /// The prompt text sent: every section above, the schema and the note.
    pub total: usize,
    /// The prompt budget: what the prompt and output schema may take.
    pub budget: usize,
}

/// The rules section's receipt, and whether the daemon reports the accepted
/// decisions that settle a rule (context contract 3); without them, rules
/// settle only by this task's plans.
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct RulesPlan {
    #[serde(flatten)]
    pub receipt: RulesReceipt,
    pub decided_by_supported: bool,
}

/// The source section: its bytes (the full source, the outlines, the scope
/// note and the entity dossiers), how many working-set files each tier
/// took, the full-source budget, and the files shown in full in and out of
/// the scope.
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct SourcePlan {
    pub bytes: usize,
    pub full: usize,
    pub outline: usize,
    pub listed: usize,
    pub budget: usize,
    pub scope_full: usize,
    pub nonscope_full: usize,
}

impl SourcePlan {
    pub(super) fn new(bytes: usize, view: &SourceView, scope: &[String]) -> Self {
        let tier = |tier: Tier| view.placed.iter().filter(|p| p.tier == tier).count();
        let scope: BTreeSet<&str> = scope.iter().map(String::as_str).collect();
        let full = view.full();
        let scope_full = full.iter().filter(|f| scope.contains(f.as_str())).count();
        Self {
            bytes,
            full: full.len(),
            outline: tier(Tier::Outline),
            listed: tier(Tier::Listed),
            budget: view.budget,
            scope_full,
            nonscope_full: full.len() - scope_full,
        }
    }
}

/// The conversation history section, and how many earlier tasks it shows as
/// one line each.
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct HistoryPlan {
    pub bytes: usize,
    /// The one-line earlier tasks under their header. 0 also when the
    /// history's byte budget clipped that header off: the history is then
    /// cut mid-text, and what is left of the block cannot be told from the
    /// current task's turns.
    pub earlier_tasks: usize,
}

impl HistoryPlan {
    pub(super) fn new(history: &str) -> Self {
        Self {
            bytes: history.len(),
            earlier_tasks: earlier_task_lines(history),
        }
    }
}

/// The earlier-task lines under the earlier-tasks header in `history`; the
/// counted "omitted" line is not one.
fn earlier_task_lines(history: &str) -> usize {
    let Some(start) = history.find(EARLIER_TASKS_HEADER) else {
        return 0;
    };
    history[start + EARLIER_TASKS_HEADER.len()..]
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter(|line| line.starts_with("- "))
        .count()
}

/// Bytes as kilobytes with one decimal.
fn kb(bytes: usize) -> String {
    format!("{:.1}KB", bytes as f64 / 1000.0)
}

impl ContextPlan {
    /// One line for the `context_plan` intent event, well under its 2000-byte
    /// cap: counts, not file names.
    pub(super) fn compact(&self) -> String {
        let rules = &self.rules.receipt;
        let sum =
            |counts: &std::collections::BTreeMap<String, usize>| counts.values().sum::<usize>();
        let settled = |name: &str| rules.settled.get(name).copied().unwrap_or(0);
        let source = &self.source;
        format!(
            "rules {} full {} line {} title {} settled d{} p{} s{}{}; source {} budget {} full {} outline {} listed {} (in scope {}, out {}); scope {} pre {} skip {}; hist {} ({} earlier); nav {}; obs {}; head {}; state {}; schema {}; repair {}; total {}/{}",
            kb(rules.bytes),
            sum(&rules.full),
            sum(&rules.one_line),
            sum(&rules.title_only),
            settled("decided"),
            settled("addressed"),
            settled("satisfied"),
            if self.rules.decided_by_supported { "" } else { " (no decided_by)" },
            kb(source.bytes),
            kb(source.budget),
            source.full,
            source.outline,
            source.listed,
            source.scope_full,
            source.nonscope_full,
            self.scope.len(),
            self.preloaded.len(),
            self.preload_skipped.len(),
            kb(self.history.bytes),
            self.history.earlier_tasks,
            kb(self.navigation_bytes),
            kb(self.observations_bytes),
            kb(self.head_bytes),
            kb(self.state_bytes),
            kb(self.schema_bytes),
            kb(self.repair_bytes),
            kb(self.total).trim_end_matches("KB"),
            kb(self.budget),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compact_line_counts_every_section_within_its_bound() {
        let mut plan = ContextPlan {
            scope: vec!["a.rs".into(), "b.rs".into()],
            preloaded: vec!["b.rs".into()],
            preload_skipped: Vec::new(),
            navigation_bytes: 1_200,
            observations_bytes: 8_000,
            head_bytes: 40_100,
            state_bytes: 1_234,
            schema_bytes: 12_000,
            total: 58_300,
            budget: 98_976,
            ..Default::default()
        };
        plan.rules.receipt.bytes = 26_800;
        plan.rules.receipt.full.insert("Constraint".into(), 40);
        plan.rules.receipt.full.insert("Requirement".into(), 5);
        plan.rules.receipt.one_line.insert("Requirement".into(), 12);
        plan.rules.receipt.settled.insert("decided", 12);
        plan.rules.decided_by_supported = true;
        plan.source = SourcePlan {
            bytes: 30_100,
            full: 4,
            outline: 6,
            listed: 0,
            budget: 40_000,
            scope_full: 1,
            nonscope_full: 3,
        };
        plan.history = HistoryPlan {
            bytes: 2_100,
            earlier_tasks: 2,
        };
        assert_eq!(
            plan.compact(),
            "rules 26.8KB full 45 line 12 title 0 settled d12 p0 s0; source 30.1KB budget 40.0KB full 4 outline 6 listed 0 (in scope 1, out 3); scope 2 pre 1 skip 0; hist 2.1KB (2 earlier); nav 1.2KB; obs 8.0KB; head 40.1KB; state 1.2KB; schema 12.0KB; repair 0.0KB; total 58.3/99.0KB"
        );

        // Far past any real prompt (100 KB at most), the line stays small:
        // every byte count at a megabyte, every count at 99,999, with the
        // unsupported-decided_by note.
        let (bytes, count) = (999_999, 99_999);
        for kind in ["Constraint", "Requirement", "Lesson"] {
            plan.rules.receipt.full.insert(kind.into(), count);
            plan.rules.receipt.one_line.insert(kind.into(), count);
            plan.rules.receipt.title_only.insert(kind.into(), count);
        }
        for name in super::super::rule_state::SETTLED_NAMES {
            plan.rules.receipt.settled.insert(name, count);
        }
        plan.rules.receipt.bytes = bytes;
        plan.rules.decided_by_supported = false;
        plan.source = SourcePlan {
            bytes,
            full: count,
            outline: count,
            listed: count,
            budget: bytes,
            scope_full: count,
            nonscope_full: count,
        };
        plan.history = HistoryPlan {
            bytes,
            earlier_tasks: count,
        };
        plan.navigation_bytes = bytes;
        plan.observations_bytes = bytes;
        plan.head_bytes = bytes;
        plan.state_bytes = bytes;
        plan.schema_bytes = bytes;
        plan.repair_bytes = bytes;
        plan.total = bytes;
        plan.budget = bytes;
        // A 100-file scope names no file.
        plan.scope = (0..100).map(|i| format!("src/file_{i}.rs")).collect();
        plan.preloaded = plan.scope.clone();
        plan.preload_skipped = plan.scope.clone();
        let line = plan.compact();
        assert!(line.contains("(no decided_by)"), "{line}");
        assert!(line.len() <= 600, "{} bytes: {line}", line.len());
    }

    #[test]
    fn earlier_tasks_are_the_lines_under_their_header() {
        assert_eq!(earlier_task_lines(""), 0);
        assert_eq!(earlier_task_lines("user: - not a task\n"), 0);
        let history = format!(
            "Recent conversation (…):\n{EARLIER_TASKS_HEADER}\n1 earlier task(s) omitted.\n- fix a → fixed a\n- fix b → fixed b\n\nuser: - and now\nassistant: - ok\n"
        );
        assert_eq!(earlier_task_lines(&history), 2);
    }
}
