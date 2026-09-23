//! Plan coverage: before a proposed plan is stored, check that its summary
//! addresses each governing rule the daemon delivered for the plan files. A plan
//! that skips one goes back with a note naming every unmet rule, at most the
//! configured number of times per planning cycle; then the plan is kept and the
//! gap is journaled. A delivery nudge on wording, never a compliance verdict,
//! and never a halt or a repair.
use super::super::{ContextResponse, Runner};
use crate::harness::coverage::{assess, CoverageThresholds};

impl Runner {
    /// True when the plan was returned; the caller stores nothing.
    pub(in crate::harness::runner) fn plan_coverage_return(
        &mut self,
        summary: &str,
        context: &ContextResponse,
    ) -> bool {
        let rules = &context.governing_rules;
        if rules.is_empty() {
            self.symbolic_state_mut().coverage_returns = 0;
            return false;
        }
        let thresholds = match CoverageThresholds::from_env() {
            Ok(thresholds) => thresholds,
            Err(error) => {
                self.intent_event(
                    "constraint_coverage",
                    &format!("thresholds invalid ({error:#}); using frozen defaults"),
                );
                CoverageThresholds::default()
            }
        };
        let background = format!("{} {}", self.task.objective, self.task.guidance);
        let mut unmet = Vec::new();
        for rule in rules {
            let receipt = assess(
                summary,
                &background,
                &rule.iri,
                &rule.label,
                &rule.claim,
                &thresholds,
            );
            let detail = serde_json::to_string(&receipt).unwrap_or_default();
            self.intent_event("constraint_coverage", &detail);
            if !receipt.covered {
                unmet.push(rule);
            }
        }
        if unmet.is_empty() {
            self.symbolic_state_mut().coverage_returns = 0;
            return false;
        }
        let returns = self.symbolic_state_mut().coverage_returns;
        if returns >= thresholds.return_limit {
            let names: Vec<String> = unmet
                .iter()
                .map(|rule| format!("{} ({})", rule.label, rule.iri))
                .collect();
            self.intent_event(
                "constraint_coverage_unmet",
                &format!(
                    "plan kept after {returns} coverage return(s); its summary does not mention: {}",
                    names.join("; ")
                ),
            );
            self.symbolic_state_mut().coverage_returns = 0;
            return false;
        }
        self.symbolic_state_mut().coverage_returns = returns + 1;
        let mut note = String::from(
            "Plan not stored: its summary does not say how the change satisfies these project rules, or why they do not apply. Propose the plan again with a summary that addresses each one (this checks the summary\'s wording only; required checks judge the code).\n",
        );
        for rule in &unmet {
            note.push_str(&format!(
                "\n[Constraint] {} ({})\n{}",
                rule.label, rule.iri, rule.claim
            ));
        }
        self.event(format!(
            "Plan returned for project rules ({} of {}): {}",
            returns + 1,
            thresholds.return_limit,
            unmet
                .iter()
                .map(|rule| rule.label.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ));
        self.task.last_response = note;
        true
    }
}
