//! Where each governing rule stands for this step. A rule an accepted decision
//! already settles, or an approved plan of this task addressed, deferred or
//! said the existing code satisfies, needs no answer from the planner, and a
//! settled Requirement is shown as one line instead of its claim (Constraint
//! 979354f7, rule 4 as amended). Constraints are never shortened by state: a
//! decision that addresses a Constraint does not retire it, since it binds
//! every later change to what it governs (Lesson f07aacbb). Their state still
//! counts for plan coverage and the rules a plan leaves open.
use super::task::{ApprovedPlan, Plan};
use super::{Mode, Runner};
use crate::harness::protocol::GoverningRule;
use serde::Serialize;
use std::collections::BTreeMap;

/// Whether rules settle by state. `MOOSEDEV_HARNESS_RULES_BY_STATE=off` treats
/// every rule as open, as before: nothing is shortened, and coverage and the
/// open rules read every delivered rule. The satisfied claims of the plan
/// being proposed are its own answer and still count
/// ([`plan_satisfied_enabled`] governs them).
pub(super) fn by_state_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_RULES_BY_STATE").map_or(true, |value| value.trim() != "off")
}

/// Whether plans may say which rules the existing code already satisfies.
/// `MOOSEDEV_HARNESS_PLAN_SATISFIED=off` removes `satisfied` from the schema,
/// the prompt and the rules header, and drops any a model sends anyway.
pub(super) fn plan_satisfied_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PLAN_SATISFIED").map_or(true, |value| value.trim() != "off")
}

/// A governing rule's state for one step. Plan numbers are 1-based positions
/// in `Task::approved_plans`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuleState {
    Open,
    /// An accepted decision `isMotivatedBy` the rule: the first of its
    /// `decided_by`.
    Decided(String),
    /// Approved plan n lists the rule in `addresses`, and an edit was made
    /// under it.
    AddressedByPlan(usize),
    /// A plan says the existing code already satisfies the rule: approved
    /// plan n, or (`None`) the plan being proposed.
    ClaimedSatisfied(Option<usize>),
    /// Approved plan n left the rule open, and its approval deferred it.
    Deferred(usize),
}

impl RuleState {
    pub(super) fn is_settled(&self) -> bool {
        *self != RuleState::Open
    }

    /// How the rule was settled, for its one line; `None` while open.
    pub(super) fn note(&self) -> Option<String> {
        Some(match self {
            RuleState::Open => return None,
            RuleState::Decided(decision) => format!("decided by {decision}"),
            RuleState::AddressedByPlan(n) => format!("addressed by approved plan {n}"),
            RuleState::ClaimedSatisfied(_) => "plan says already satisfied".to_owned(),
            RuleState::Deferred(n) => format!("deferred by approved plan {n}"),
        })
    }

    /// The state's name in receipts and journal counts.
    pub(super) fn name(&self) -> &'static str {
        match self {
            RuleState::Open => "open",
            RuleState::Decided(_) => "decided",
            RuleState::AddressedByPlan(_) => "addressed",
            RuleState::ClaimedSatisfied(_) => "satisfied",
            RuleState::Deferred(_) => "deferred",
        }
    }
}

/// What settles rules for one step.
pub(super) struct Settlement<'a> {
    by_state: bool,
    /// In Auto mode, the approved plan's `addresses`: the builder keeps the
    /// claims it implements, so these stay open whatever else settles them.
    implementing: &'a [String],
    approved: &'a [ApprovedPlan],
    /// `Task::edits` so far: a plan replaced before any edit implemented
    /// nothing, so its `addresses` settle nothing (as capture reads them).
    edits: usize,
    /// The satisfied claims of the plan being proposed.
    claimed: &'a [String],
}

impl<'a> Settlement<'a> {
    pub(super) fn new(
        mode: Mode,
        current: Option<&'a Plan>,
        approved: &'a [ApprovedPlan],
        edits: usize,
        claimed: &'a [String],
        by_state: bool,
    ) -> Self {
        let implementing = match (mode, current) {
            (Mode::Auto, Some(plan)) => plan.addresses.as_slice(),
            _ => &[],
        };
        Self {
            by_state,
            implementing,
            approved,
            edits,
            claimed,
        }
    }

    /// Precedence: Decided, AddressedByPlan, ClaimedSatisfied, Deferred,
    /// Open; a proposed plan settles only through `claimed`.
    pub(super) fn state(&self, rule: &GoverningRule) -> RuleState {
        if self.implementing.contains(&rule.iri) {
            return RuleState::Open;
        }
        let in_plan = |field: fn(&ApprovedPlan) -> &[String], edited: bool| {
            self.approved
                .iter()
                .enumerate()
                .rfind(|(index, plan)| {
                    field(plan).contains(&rule.iri) && (!edited || self.edited_under(*index))
                })
                .map(|(index, _)| index + 1)
        };
        if self.by_state {
            if let Some(decision) = rule.decided_by.first() {
                return RuleState::Decided(decision.clone());
            }
            if let Some(n) = in_plan(|plan| &plan.addresses, true) {
                return RuleState::AddressedByPlan(n);
            }
        }
        if self.claimed.contains(&rule.iri) {
            return RuleState::ClaimedSatisfied(None);
        }
        if self.by_state {
            if let Some(n) = in_plan(|plan| &plan.satisfied, false) {
                return RuleState::ClaimedSatisfied(Some(n));
            }
            if let Some(n) = in_plan(|plan| &plan.deferred, false) {
                return RuleState::Deferred(n);
            }
        }
        RuleState::Open
    }

    pub(super) fn states(&self, rules: &[GoverningRule]) -> Vec<RuleState> {
        rules.iter().map(|rule| self.state(rule)).collect()
    }

    /// Whether an edit was made under approved plan `index`.
    fn edited_under(&self, index: usize) -> bool {
        let end = self
            .approved
            .get(index + 1)
            .map_or(self.edits, |next| next.edit_start)
            .min(self.edits);
        self.approved[index].edit_start < end
    }
}

/// What the rules section delivered: its bytes, how each kind was shown, and
/// the settled rules by state (Constraints included, though shown in full).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct RulesReceipt {
    pub bytes: usize,
    pub full: BTreeMap<String, usize>,
    pub one_line: BTreeMap<String, usize>,
    pub title_only: BTreeMap<String, usize>,
    pub settled: BTreeMap<&'static str, usize>,
}

impl Runner {
    /// What settles rules now; `claimed` is the satisfied claims of the plan
    /// being proposed, empty when none is.
    pub(super) fn settlement<'a>(&'a self, claimed: &'a [String]) -> Settlement<'a> {
        Settlement::new(
            self.task.mode,
            self.task.plan.as_ref(),
            &self.task.approved_plans,
            self.task.edits.len(),
            claimed,
            by_state_enabled(),
        )
    }

    /// The rules among `rules` nothing settles, by the plan being proposed's
    /// `claimed` and everything else ([`Self::settlement`]).
    pub(super) fn unsettled_rules(
        &self,
        rules: Vec<GoverningRule>,
        claimed: &[String],
    ) -> Vec<GoverningRule> {
        let settlement = self.settlement(claimed);
        rules
            .into_iter()
            .filter(|rule| !settlement.state(rule).is_settled())
            .collect()
    }

    /// Journal `rules_settled`: how many of `rules` are settled, and how.
    pub(super) fn journal_rules_settled(&mut self, rules: &[GoverningRule], claimed: &[String]) {
        let mut counts = BTreeMap::<&str, usize>::new();
        for state in self.settlement(claimed).states(rules) {
            if state.is_settled() {
                *counts.entry(state.name()).or_default() += 1;
            }
        }
        if counts.is_empty() {
            return;
        }
        let detail = ["decided", "addressed", "satisfied", "deferred"]
            .iter()
            .map(|name| format!("{name} {}", counts.get(name).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(", ");
        self.intent_event(
            "rules_settled",
            &format!("{detail} of {} rule(s)", rules.len()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(iri: &str, decided_by: &[&str]) -> GoverningRule {
        GoverningRule {
            iri: iri.into(),
            label: iri.into(),
            kind: "Requirement".into(),
            claim: String::new(),
            via: String::new(),
            decided_by: decided_by.iter().map(|iri| iri.to_string()).collect(),
        }
    }

    fn approved(
        addresses: &[&str],
        satisfied: &[&str],
        deferred: &[&str],
        edit_start: usize,
    ) -> ApprovedPlan {
        let iris = |list: &[&str]| list.iter().map(|iri| iri.to_string()).collect();
        ApprovedPlan {
            summary: "s".into(),
            files: vec!["a.rs".into()],
            addresses: iris(addresses),
            satisfied: iris(satisfied),
            rules_in_view: Vec::new(),
            deferred: iris(deferred),
            edit_start,
        }
    }

    fn plan(addresses: &[&str]) -> Plan {
        Plan {
            summary: "s".into(),
            files: vec!["a.rs".into()],
            checks: vec![],
            addresses: addresses.iter().map(|iri| iri.to_string()).collect(),
            satisfied: vec![],
            open_rules: vec![],
            open_choices: vec![],
        }
    }

    #[test]
    fn precedence_is_decided_addressed_satisfied_deferred_open() {
        // Plan 1 addressed, said satisfied and deferred everything; an edit
        // was made under it.
        let everything = ["urn:a", "urn:b", "urn:c", "urn:d"];
        let plans = [approved(&everything, &everything, &everything, 0)];
        let claimed = vec!["urn:c".to_string()];
        let settle = Settlement::new(Mode::Plan, None, &plans, 1, &claimed, true);
        assert_eq!(
            settle.state(&rule("urn:a", &["urn:ad:1", "urn:ad:2"])),
            RuleState::Decided("urn:ad:1".into())
        );
        assert_eq!(
            settle.state(&rule("urn:b", &[])),
            RuleState::AddressedByPlan(1)
        );

        let plans = [approved(&[], &["urn:c"], &["urn:c", "urn:d"], 0)];
        let settle = Settlement::new(Mode::Plan, None, &plans, 1, &claimed, true);
        assert_eq!(
            settle.state(&rule("urn:c", &[])),
            RuleState::ClaimedSatisfied(None),
            "the proposal's claim outranks an approved plan's"
        );
        let settle = Settlement::new(Mode::Plan, None, &plans, 1, &[], true);
        assert_eq!(
            settle.state(&rule("urn:c", &[])),
            RuleState::ClaimedSatisfied(Some(1))
        );
        assert_eq!(settle.state(&rule("urn:d", &[])), RuleState::Deferred(1));
        assert_eq!(settle.state(&rule("urn:e", &[])), RuleState::Open);
        // The latest approved plan names the rule.
        let plans = [
            approved(&[], &[], &["urn:d"], 0),
            approved(&[], &[], &["urn:d"], 0),
        ];
        let settle = Settlement::new(Mode::Plan, None, &plans, 0, &[], true);
        assert_eq!(settle.state(&rule("urn:d", &[])), RuleState::Deferred(2));
    }

    #[test]
    fn in_auto_the_rules_the_approved_plan_implements_stay_open() {
        let plans = [
            approved(&["urn:a"], &[], &[], 0),
            approved(&["urn:a"], &[], &[], 1),
        ];
        let current = plan(&["urn:a"]);
        let decided = rule("urn:a", &["urn:ad"]);
        let auto = Settlement::new(Mode::Auto, Some(&current), &plans, 2, &[], true);
        assert_eq!(auto.state(&decided), RuleState::Open);
        let planning = Settlement::new(Mode::Plan, Some(&current), &plans, 2, &[], true);
        assert_eq!(
            planning.state(&decided),
            RuleState::Decided("urn:ad".into())
        );
        assert_eq!(
            planning.state(&rule("urn:a", &[])),
            RuleState::AddressedByPlan(2)
        );
    }

    #[test]
    fn a_proposed_plan_or_one_replaced_before_any_edit_settles_nothing() {
        // A proposed plan is `Task::plan`, never an approved plan: in Plan
        // mode its addresses settle nothing.
        let proposed = plan(&["urn:a"]);
        let settle = Settlement::new(Mode::Plan, Some(&proposed), &[], 0, &[], true);
        assert_eq!(settle.state(&rule("urn:a", &[])), RuleState::Open);
        // Plan 1 was replaced by plan 2 before any edit: it implemented
        // nothing, though what it deferred stays deferred.
        let plans = [
            approved(&["urn:a"], &[], &["urn:b"], 0),
            approved(&[], &[], &[], 0),
        ];
        let settle = Settlement::new(Mode::Plan, None, &plans, 3, &[], true);
        assert_eq!(settle.state(&rule("urn:a", &[])), RuleState::Open);
        assert_eq!(settle.state(&rule("urn:b", &[])), RuleState::Deferred(1));
        // The latest plan, with no edit yet, has implemented nothing either.
        let plans = [approved(&["urn:a"], &[], &[], 3)];
        let settle = Settlement::new(Mode::Plan, None, &plans, 3, &[], true);
        assert_eq!(settle.state(&rule("urn:a", &[])), RuleState::Open);
    }

    #[test]
    fn switched_off_every_rule_is_open_but_the_proposals_claim() {
        let plans = [approved(&["urn:a"], &["urn:b"], &["urn:c"], 0)];
        let claimed = vec!["urn:e".to_string()];
        let settle = Settlement::new(Mode::Plan, None, &plans, 1, &claimed, false);
        for iri in ["urn:a", "urn:b", "urn:c"] {
            assert_eq!(settle.state(&rule(iri, &[])), RuleState::Open, "{iri}");
        }
        assert_eq!(settle.state(&rule("urn:d", &["urn:ad"])), RuleState::Open);
        assert_eq!(
            settle.state(&rule("urn:e", &[])),
            RuleState::ClaimedSatisfied(None)
        );
    }

    #[test]
    fn each_settled_state_names_how_it_was_settled() {
        assert_eq!(RuleState::Open.note(), None);
        assert!(!RuleState::Open.is_settled());
        assert_eq!(
            RuleState::Decided("urn:ad".into()).note().unwrap(),
            "decided by urn:ad"
        );
        assert_eq!(
            RuleState::AddressedByPlan(2).note().unwrap(),
            "addressed by approved plan 2"
        );
        assert_eq!(
            RuleState::ClaimedSatisfied(Some(1)).note().unwrap(),
            "plan says already satisfied"
        );
        assert_eq!(
            RuleState::Deferred(3).note().unwrap(),
            "deferred by approved plan 3"
        );
    }
}
