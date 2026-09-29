//! Where each governing rule stands for this step. A rule an accepted decision
//! already settles, an approved plan of this task fully implemented, or the
//! current plan says the existing code satisfies, needs no answer from the
//! planner, and a settled Requirement is shown as one line instead of its
//! claim (Constraint 979354f7, rule 4 as amended). Constraints are never
//! shortened by state: a decision that addresses a Constraint does not retire
//! it, since it binds every later change to what it governs (Lesson
//! f07aacbb). Their state still counts for plan coverage and the rules a plan
//! leaves open.
use super::task::{ApprovedPlan, PendingEdit, Plan};
use super::{Mode, Runner};
use crate::harness::protocol::GoverningRule;
use serde::Serialize;
use std::collections::BTreeMap;

/// Whether rules settle by state. `MOOSEDEV_HARNESS_RULES_BY_STATE=off` treats
/// every rule as open, as before: nothing is shortened, and coverage and the
/// open rules read every delivered rule. The satisfied claims of the current
/// plan are its own answer and still count ([`plan_satisfied_enabled`]
/// governs them).
pub(super) fn by_state_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_RULES_BY_STATE").map_or(true, |value| value.trim() != "off")
}

/// Whether plans may say which rules the existing code already satisfies.
/// `MOOSEDEV_HARNESS_PLAN_SATISFIED=off` removes `satisfied` from the schema,
/// the prompt and the rules header, drops any a model sends anyway, and
/// ignores any a resumed task's journal already holds: no claim settles a
/// rule, is journaled at approval or is shown at the gate.
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
    /// Approved plan n lists the rule in `addresses`, and every file of that
    /// plan was edited under it.
    AddressedByPlan(usize),
    /// The current plan says the existing code already satisfies the rule:
    /// the plan being proposed, or in Auto the approved plan being built.
    /// An earlier approved plan's claims settle nothing.
    ClaimedSatisfied,
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
            RuleState::ClaimedSatisfied => "plan says already satisfied".to_owned(),
        })
    }

    /// The state's name in receipts and journal counts.
    pub(super) fn name(&self) -> &'static str {
        match self {
            RuleState::Open => "open",
            RuleState::Decided(_) => "decided",
            RuleState::AddressedByPlan(_) => "addressed",
            RuleState::ClaimedSatisfied => "satisfied",
        }
    }
}

/// The settled states' names, in precedence order, for counted lines.
pub(super) const SETTLED_NAMES: [&str; 3] = ["decided", "addressed", "satisfied"];

/// What settles rules for one step.
pub(super) struct Settlement<'a> {
    by_state: bool,
    /// In Auto mode, the approved plan's `addresses`: the builder keeps the
    /// claims it implements, so these stay open whatever else settles them.
    implementing: &'a [String],
    approved: &'a [ApprovedPlan],
    /// `Task::edits` so far: an approved plan's `addresses` settle only once
    /// every file of the plan was edited under it. A plan replaced before any
    /// edit, or part-way through its files, did not implement what it
    /// addressed.
    edits: &'a [PendingEdit],
    /// The current plan's satisfied claims: the proposal's in Plan mode, and
    /// in Auto the approved plan's as well.
    claimed: Vec<&'a str>,
}

impl<'a> Settlement<'a> {
    /// `claimed` is the proposed plan's satisfied claims; in Auto the current
    /// plan's are added. With `satisfied` off no claim settles anything.
    pub(super) fn new(
        mode: Mode,
        current: Option<&'a Plan>,
        approved: &'a [ApprovedPlan],
        edits: &'a [PendingEdit],
        claimed: &'a [String],
        by_state: bool,
        satisfied: bool,
    ) -> Self {
        let current = match mode {
            Mode::Auto => current,
            Mode::Plan => None,
        };
        let implementing = current.map_or(&[][..], |plan| plan.addresses.as_slice());
        let claimed = if satisfied {
            claimed
                .iter()
                .chain(current.into_iter().flat_map(|plan| &plan.satisfied))
                .map(String::as_str)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            by_state,
            implementing,
            approved,
            edits,
            claimed,
        }
    }

    /// Precedence: Decided, AddressedByPlan, ClaimedSatisfied, Open; a
    /// proposed plan settles only through `claimed`, and an earlier approved
    /// plan only through what it fully implemented.
    pub(super) fn state(&self, rule: &GoverningRule) -> RuleState {
        if self.implementing.contains(&rule.iri) {
            return RuleState::Open;
        }
        if self.by_state {
            if let Some(decision) = rule.decided_by.first() {
                return RuleState::Decided(decision.clone());
            }
            if let Some((index, _)) = self.approved.iter().enumerate().rfind(|(index, plan)| {
                plan.addresses.contains(&rule.iri) && self.implemented(*index)
            }) {
                return RuleState::AddressedByPlan(index + 1);
            }
        }
        if self.claimed.contains(&rule.iri.as_str()) {
            return RuleState::ClaimedSatisfied;
        }
        RuleState::Open
    }

    pub(super) fn states(&self, rules: &[GoverningRule]) -> Vec<RuleState> {
        rules.iter().map(|rule| self.state(rule)).collect()
    }

    /// Whether approved plan `index` was carried out: at least one edit was
    /// made under it (from its `edit_start` to the next plan's, or to now),
    /// and those edits touched every file it lists.
    fn implemented(&self, index: usize) -> bool {
        let plan = &self.approved[index];
        let end = self
            .approved
            .get(index + 1)
            .map_or(self.edits.len(), |next| next.edit_start)
            .min(self.edits.len());
        let edits = &self.edits[plan.edit_start.min(end)..end];
        !edits.is_empty()
            && plan
                .files
                .iter()
                .all(|file| edits.iter().any(|edit| &edit.file == file))
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
    /// being proposed, empty when none is (in Auto the approved plan's own
    /// claims are added).
    pub(super) fn settlement<'a>(&'a self, claimed: &'a [String]) -> Settlement<'a> {
        Settlement::new(
            self.task.mode,
            self.task.plan.as_ref(),
            &self.task.approved_plans,
            &self.task.edits,
            claimed,
            by_state_enabled(),
            plan_satisfied_enabled(),
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
        let detail = SETTLED_NAMES
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

    fn approved_on(
        files: &[&str],
        addresses: &[&str],
        satisfied: &[&str],
        deferred: &[&str],
        edit_start: usize,
    ) -> ApprovedPlan {
        let strings = |list: &[&str]| list.iter().map(|item| item.to_string()).collect();
        ApprovedPlan {
            summary: "s".into(),
            files: strings(files),
            addresses: strings(addresses),
            satisfied: strings(satisfied),
            rules_in_view: Vec::new(),
            deferred: strings(deferred),
            edit_start,
        }
    }

    fn approved(
        addresses: &[&str],
        satisfied: &[&str],
        deferred: &[&str],
        edit_start: usize,
    ) -> ApprovedPlan {
        approved_on(&["a.rs"], addresses, satisfied, deferred, edit_start)
    }

    fn edits(files: &[&str]) -> Vec<PendingEdit> {
        files
            .iter()
            .map(|file| PendingEdit {
                file: file.to_string(),
                before: None,
                after: Some(String::new()),
                reason: String::new(),
                revision: String::new(),
            })
            .collect()
    }

    fn plan(addresses: &[&str], satisfied: &[&str]) -> Plan {
        let strings = |list: &[&str]| list.iter().map(|item| item.to_string()).collect();
        Plan {
            summary: "s".into(),
            files: vec!["a.rs".into()],
            checks: vec![],
            addresses: strings(addresses),
            satisfied: strings(satisfied),
            open_rules: vec![],
            open_choices: vec![],
        }
    }

    fn settle<'a>(
        mode: Mode,
        current: Option<&'a Plan>,
        plans: &'a [ApprovedPlan],
        edits: &'a [PendingEdit],
        claimed: &'a [String],
    ) -> Settlement<'a> {
        Settlement::new(mode, current, plans, edits, claimed, true, true)
    }

    #[test]
    fn precedence_is_decided_addressed_satisfied_open() {
        // Plan 1 addressed everything; its one file was edited under it.
        let everything = ["urn:a", "urn:b", "urn:c"];
        let plans = [approved(&everything, &[], &[], 0)];
        let edited = edits(&["a.rs"]);
        let claimed = vec!["urn:b".to_string(), "urn:c".to_string()];
        let state = settle(Mode::Plan, None, &plans, &edited, &claimed);
        assert_eq!(
            state.state(&rule("urn:a", &["urn:ad:1", "urn:ad:2"])),
            RuleState::Decided("urn:ad:1".into())
        );
        assert_eq!(
            state.state(&rule("urn:b", &[])),
            RuleState::AddressedByPlan(1)
        );
        let plans = [approved(&[], &[], &[], 0)];
        let state = settle(Mode::Plan, None, &plans, &edited, &claimed);
        assert_eq!(
            state.state(&rule("urn:c", &[])),
            RuleState::ClaimedSatisfied
        );
        assert_eq!(state.state(&rule("urn:e", &[])), RuleState::Open);
        // The latest approved plan that implemented the rule names it.
        let plans = [
            approved(&["urn:d"], &[], &[], 0),
            approved(&["urn:d"], &[], &[], 1),
        ];
        let edited = edits(&["a.rs", "a.rs"]);
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(
            state.state(&rule("urn:d", &[])),
            RuleState::AddressedByPlan(2)
        );
    }

    #[test]
    fn in_auto_the_rules_the_approved_plan_implements_stay_open() {
        let plans = [
            approved(&["urn:a"], &[], &[], 0),
            approved(&["urn:a"], &[], &[], 1),
        ];
        let edited = edits(&["a.rs", "a.rs"]);
        let current = plan(&["urn:a"], &[]);
        let decided = rule("urn:a", &["urn:ad"]);
        let auto = settle(Mode::Auto, Some(&current), &plans, &edited, &[]);
        assert_eq!(auto.state(&decided), RuleState::Open);
        let planning = settle(Mode::Plan, Some(&current), &plans, &edited, &[]);
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
        let proposed = plan(&["urn:a"], &[]);
        let state = settle(Mode::Plan, Some(&proposed), &[], &[], &[]);
        assert_eq!(state.state(&rule("urn:a", &[])), RuleState::Open);
        // Plan 1 was replaced by plan 2 before any edit: it implemented
        // nothing, and what it deferred is open.
        let plans = [
            approved(&["urn:a"], &[], &["urn:b"], 0),
            approved(&[], &[], &[], 0),
        ];
        let edited = edits(&["a.rs", "a.rs", "a.rs"]);
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:a", &[])), RuleState::Open);
        assert_eq!(state.state(&rule("urn:b", &[])), RuleState::Open);
        // The latest plan, with no edit yet, has implemented nothing either.
        let plans = [approved(&["urn:a"], &[], &[], 3)];
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:a", &[])), RuleState::Open);
        // A plan with no files still needs an edit made under it.
        let plans = [approved_on(&[], &["urn:a"], &[], &[], 3)];
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:a", &[])), RuleState::Open);
    }

    #[test]
    fn a_plan_replaced_part_way_through_its_files_settles_nothing() {
        // Plan 1 covers a.rs and b.rs; only a.rs was edited before plan 2
        // replaced it (b.rs was edited under plan 2, which does not count
        // for plan 1). Its addresses (the rule for b.rs among them) stay
        // open: nothing says the part it did implemented them.
        let plans = [
            approved_on(&["a.rs", "b.rs"], &["urn:a", "urn:b"], &[], &[], 0),
            approved_on(&["c.rs"], &[], &[], &[], 1),
        ];
        let edited = edits(&["a.rs", "b.rs", "c.rs"]);
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:b", &[])), RuleState::Open);
        assert_eq!(state.state(&rule("urn:a", &[])), RuleState::Open);
        // Every file edited under plan 1 settles what it addressed.
        let plans = [
            approved_on(&["a.rs", "b.rs"], &["urn:a", "urn:b"], &[], &[], 0),
            approved_on(&["c.rs"], &[], &[], &[], 2),
        ];
        let edited = edits(&["b.rs", "a.rs", "c.rs"]);
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(
            state.state(&rule("urn:b", &[])),
            RuleState::AddressedByPlan(1)
        );
    }

    #[test]
    fn only_the_current_plans_claims_settle() {
        // Plan 1 said urn:s already holds and deferred urn:f; a replan is
        // proposed. Neither settles anything for the new plan.
        let plans = [approved(&[], &["urn:s"], &["urn:f"], 0)];
        let edited = edits(&["a.rs"]);
        let state = settle(Mode::Plan, None, &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:s", &[])), RuleState::Open);
        assert_eq!(state.state(&rule("urn:f", &[])), RuleState::Open);
        // The proposal's own claim does.
        let claimed = vec!["urn:s".to_string()];
        let state = settle(Mode::Plan, None, &plans, &edited, &claimed);
        assert_eq!(
            state.state(&rule("urn:s", &[])),
            RuleState::ClaimedSatisfied
        );
        // In Auto, the approved plan being built claims for itself; in Plan
        // mode the same plan (replaced by the coming proposal) does not.
        let current = plan(&[], &["urn:s"]);
        let state = settle(Mode::Auto, Some(&current), &plans, &edited, &[]);
        assert_eq!(
            state.state(&rule("urn:s", &[])),
            RuleState::ClaimedSatisfied
        );
        let state = settle(Mode::Plan, Some(&current), &plans, &edited, &[]);
        assert_eq!(state.state(&rule("urn:s", &[])), RuleState::Open);
    }

    #[test]
    fn switched_off_every_rule_is_open_but_the_proposals_claim() {
        let plans = [approved(&["urn:a"], &["urn:b"], &["urn:c"], 0)];
        let edited = edits(&["a.rs"]);
        let claimed = vec!["urn:e".to_string()];
        let state = Settlement::new(Mode::Plan, None, &plans, &edited, &claimed, false, true);
        for iri in ["urn:a", "urn:b", "urn:c"] {
            assert_eq!(state.state(&rule(iri, &[])), RuleState::Open, "{iri}");
        }
        assert_eq!(state.state(&rule("urn:d", &["urn:ad"])), RuleState::Open);
        assert_eq!(
            state.state(&rule("urn:e", &[])),
            RuleState::ClaimedSatisfied
        );
    }

    #[test]
    fn with_satisfied_off_no_claim_settles() {
        // A resumed task's journal holds claims from when the field was on.
        let current = plan(&[], &["urn:s"]);
        let claimed = vec!["urn:p".to_string()];
        let state = Settlement::new(Mode::Auto, Some(&current), &[], &[], &claimed, true, false);
        assert_eq!(state.state(&rule("urn:s", &[])), RuleState::Open);
        assert_eq!(state.state(&rule("urn:p", &[])), RuleState::Open);
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
            RuleState::ClaimedSatisfied.note().unwrap(),
            "plan says already satisfied"
        );
    }
}
