//! Plan coverage: before a proposed plan is stored, check that its summary
//! addresses each governing rule the daemon delivered for the plan files. A plan
//! that skips one goes back with a note naming every unmet rule, at most the
//! configured number of times per planning cycle; then the plan is kept and the
//! gap is journaled. A delivery nudge on wording, never a compliance verdict,
//! and never a halt or a repair.
use super::super::{ContextResponse, Runner};
use crate::harness::coverage::{assess, CoverageThresholds};
use crate::harness::protocol::{spec_title_key, GoverningRule};

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
                "\n[{}] {} ({})\n{}",
                rule.kind, rule.label, rule.iri, rule.claim
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

    /// Resolve the plan's `addresses` to the IRIs of rules delivered for its
    /// files. An entry naming no such rule is dropped and journaled, never
    /// returned to the model: a missing entry only leaves that rule open.
    pub(in crate::harness::runner) fn resolve_plan_addresses(
        &mut self,
        addresses: &[String],
        context: &ContextResponse,
    ) -> Vec<String> {
        let mut resolved: Vec<String> = Vec::new();
        let mut unknown: Vec<&str> = Vec::new();
        for entry in addresses {
            match resolve_address(entry, &context.governing_rules) {
                Some(rule) if !resolved.contains(&rule.iri) => resolved.push(rule.iri.clone()),
                Some(_) => {}
                None if entry.trim().is_empty() => {}
                None => unknown.push(entry.trim()),
            }
        }
        if !unknown.is_empty() {
            self.intent_event("plan_addresses_unresolved", &unknown.join("; "));
            self.event(format!(
                "Plan addresses ignored: {} named no project rule of the plan files.",
                unknown.join("; ")
            ));
        }
        if !context.governing_rules.is_empty() {
            self.intent_event(
                "plan_addresses",
                &format!(
                    "{} of {} rule(s): {}",
                    resolved.len(),
                    context.governing_rules.len(),
                    resolved.join(" ")
                ),
            );
        }
        resolved
    }
}

/// One `addresses` entry to a delivered rule: its IRI (exactly, else the one
/// rule whose IRI the entry contains), else its label compared as a spec
/// title. The entry may name the kind first, as the rules block does
/// (`[Requirement] X`) or as prose does (`Requirement X`, `Constraint: X`);
/// the kind then narrows the match. The whole entry is tried as a label
/// first, so a rule whose title starts with a kind word still resolves. An
/// entry that fits more than one rule resolves to none: a wrong edge would
/// count work that was not done.
fn resolve_address<'a>(entry: &str, rules: &'a [GoverningRule]) -> Option<&'a GoverningRule> {
    let entry = entry.trim();
    if let Some(rule) = rules.iter().find(|rule| rule.iri == entry) {
        return Some(rule);
    }
    let unique = |mut found: Vec<&'a GoverningRule>| (found.len() == 1).then(|| found.remove(0));
    let by_iri: Vec<_> = rules
        .iter()
        .filter(|rule| entry.contains(rule.iri.as_str()))
        .collect();
    if !by_iri.is_empty() {
        return unique(by_iri);
    }
    // The tail of an IRI: `Constraint/b19f73d3-…` or the bare id (gemma in
    // badciv dc6a7586 named every rule this way).
    if !entry.is_empty() && !entry.contains(char::is_whitespace) {
        let by_tail: Vec<_> = rules
            .iter()
            .filter(|rule| {
                rule.iri
                    .strip_suffix(entry)
                    .is_some_and(|head| head.ends_with('/'))
            })
            .collect();
        if !by_tail.is_empty() {
            return unique(by_tail);
        }
    }
    let matching = |kind: Option<&str>, label: &str| -> Vec<&'a GoverningRule> {
        let key = spec_title_key(label);
        rules
            .iter()
            .filter(|rule| spec_title_key(&rule.label) == key)
            .filter(|rule| kind.is_none_or(|kind| rule.kind.eq_ignore_ascii_case(kind)))
            .collect()
    };
    let whole = matching(None, entry);
    if !whole.is_empty() {
        return unique(whole);
    }
    let unbracketed = entry.strip_prefix('[').unwrap_or(entry);
    let (kind, rest) = unbracketed
        .split_once(|c: char| c == ']' || c == ':' || c.is_whitespace())
        .unwrap_or((unbracketed, ""));
    let rest = rest.trim_start_matches([']', ':']).trim();
    if rest.is_empty()
        || !rules
            .iter()
            .any(|rule| rule.kind.eq_ignore_ascii_case(kind))
    {
        return None;
    }
    unique(matching(Some(kind), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(iri: &str, label: &str) -> GoverningRule {
        GoverningRule {
            iri: iri.into(),
            label: label.into(),
            kind: "Requirement".into(),
            claim: String::new(),
            via: String::new(),
        }
    }

    #[test]
    fn an_address_resolves_by_iri_label_or_kind_prefixed_label() {
        let rules = [
            rule("urn:a", "Game Factions"),
            rule("urn:b", "TUI  Graphics"),
        ];
        let iri = |entry: &str| resolve_address(entry, &rules).map(|rule| rule.iri.as_str());
        assert_eq!(iri("urn:b"), Some("urn:b"));
        assert_eq!(iri("TUI Graphics (urn:b)"), Some("urn:b"));
        assert_eq!(iri("game factions"), Some("urn:a"));
        assert_eq!(iri("[Requirement] Game Factions"), Some("urn:a"));
        // The tail of a rule's IRI, with or without its kind segment.
        let iris = [
            rule(
                "https://moosedev.dev/kg/Constraint/b19f73d3",
                "Implementation Language",
            ),
            rule(
                "https://moosedev.dev/kg/Requirement/ab19f73d3",
                "Game Factions",
            ),
        ];
        let tail = |entry: &str| resolve_address(entry, &iris).map(|rule| rule.label.as_str());
        assert_eq!(tail("Constraint/b19f73d3"), Some("Implementation Language"));
        assert_eq!(
            tail("b19f73d3"),
            Some("Implementation Language"),
            "whole segment only"
        );
        assert_eq!(tail("Requirement/ab19f73d3"), Some("Game Factions"));
        assert_eq!(tail("9f73d3"), None, "a partial segment names nothing");
        // Gemma's form in the badciv run: the kind as a bare leading word.
        assert_eq!(iri("Requirement Game Factions"), Some("urn:a"));
        assert_eq!(iri("Requirement: TUI Graphics"), Some("urn:b"));
        assert_eq!(iri("[Requirement]: Game Factions"), Some("urn:a"));
        assert_eq!(
            iri("Constraint Game Factions"),
            None,
            "no Constraint by that name"
        );
        assert_eq!(iri("Feature Game Factions"), None, "not a kind");
        assert_eq!(iri("tui graphics"), Some("urn:b"), "whitespace collapses");
        assert_eq!(iri("Game"), None);

        // The same label under two kinds: the kind decides, and without one
        // the entry is ambiguous and resolves to nothing.
        let mut twins = rules.to_vec();
        twins.push(GoverningRule {
            kind: "Constraint".into(),
            ..rule("urn:c", "Game Factions")
        });
        let twin = |entry: &str| resolve_address(entry, &twins).map(|rule| rule.iri.as_str());
        assert_eq!(twin("[Constraint] Game Factions"), Some("urn:c"));
        assert_eq!(twin("Constraint Game Factions"), Some("urn:c"));
        // A title that itself starts with a kind word matches whole first.
        let titled = [rule("urn:d", "Constraint Solver Limits")];
        assert_eq!(
            resolve_address("Constraint Solver Limits", &titled).map(|rule| rule.iri.as_str()),
            Some("urn:d")
        );
        assert_eq!(twin("[Requirement] Game Factions"), Some("urn:a"));
        assert_eq!(twin("Game Factions"), None);
    }
}
