//! What a proposed plan leaves to the human. The rules it leaves open are
//! named at the approval gate and `/approve` records them as deferred. The
//! questions it asks (open choices) are answered with `/choose <n> <option>`
//! while the plan awaits approval; approval settles the rest by their
//! defaults, and the builder is shown each decision.
use super::task::{OpenChoice, OpenRule, Phase};
use super::{ContextResponse, Runner};
use crate::harness::protocol::GoverningRule;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Open choices one plan may carry.
const MAX_OPEN_CHOICES: usize = 3;
const MAX_QUESTION_BYTES: usize = 300;
const MAX_OPTION_BYTES: usize = 120;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

/// Whether plans may carry open choices. `MOOSEDEV_HARNESS_PLAN_CHOICES=off`
/// removes the field from the schema and the prompt, and drops any a model
/// sends anyway.
pub(super) fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PLAN_CHOICES").map_or(true, |value| value.trim() != "off")
}

/// The plan action's `open_choices` field: required by the strict schema,
/// and empty when the plan asks nothing.
pub(super) fn schema() -> Value {
    let text = |bytes: usize| json!({"type":"string","maxLength":bytes});
    json!({"type":"array","maxItems":MAX_OPEN_CHOICES,"items":{
        "type":"object",
        "properties":{
            "question":text(MAX_QUESTION_BYTES),
            "options":{"type":"array","minItems":MIN_OPTIONS,"maxItems":MAX_OPTIONS,"items":text(MAX_OPTION_BYTES)},
            "default":text(MAX_OPTION_BYTES)
        },
        "required":["question","options","default"],
        "additionalProperties":false
    }})
}

/// An open choice as the model proposes it: no answer, which is the human's.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProposedChoice {
    pub question: String,
    pub options: Vec<String>,
    pub default: String,
}

/// The index of the option `text` names: its text, ignoring case and
/// surrounding space.
fn option_named(options: &[String], text: &str) -> Option<usize> {
    let text = text.trim().to_lowercase();
    options
        .iter()
        .position(|option| option.trim().to_lowercase() == text)
}

/// The bounds a plan's open choices must meet; a violation is the model's to
/// repair.
pub(super) fn validate(choices: &[ProposedChoice]) -> Result<()> {
    ensure!(
        choices.len() <= MAX_OPEN_CHOICES,
        "plan lists {} open choices; at most {MAX_OPEN_CHOICES} are allowed",
        choices.len()
    );
    for (index, choice) in choices.iter().enumerate() {
        let number = index + 1;
        ensure!(
            !choice.question.trim().is_empty() && choice.question.len() <= MAX_QUESTION_BYTES,
            "open choice {number}: its question must contain 1..{MAX_QUESTION_BYTES} bytes"
        );
        ensure!(
            (MIN_OPTIONS..=MAX_OPTIONS).contains(&choice.options.len()),
            "open choice {number} lists {} options; it requires {MIN_OPTIONS}..{MAX_OPTIONS}",
            choice.options.len()
        );
        for (at, option) in choice.options.iter().enumerate() {
            ensure!(
                !option.trim().is_empty() && option.len() <= MAX_OPTION_BYTES,
                "open choice {number}: each option must contain 1..{MAX_OPTION_BYTES} bytes"
            );
            // Distinct as `/choose` reads them, so every option can be named.
            ensure!(
                option_named(&choice.options[..at], option).is_none(),
                "open choice {number}: option {option:?} is listed twice"
            );
        }
        ensure!(
            option_named(&choice.options, &choice.default).is_some(),
            "open choice {number}: its default {:?} is not one of its options",
            choice.default
        );
    }
    Ok(())
}

/// Validated proposals as the plan stores them, each default spelled as its
/// option is.
pub(super) fn open_choices(proposed: Vec<ProposedChoice>) -> Vec<OpenChoice> {
    proposed
        .into_iter()
        .map(|choice| {
            let default = option_named(&choice.options, &choice.default)
                .map_or(choice.default, |index| choice.options[index].clone());
            OpenChoice {
                question: choice.question,
                options: choice.options,
                default,
                answer: None,
            }
        })
        .collect()
}

/// One line per answered choice, for the plan the model is shown.
pub(super) fn decided(choices: &[OpenChoice]) -> String {
    choices
        .iter()
        .filter_map(|choice| {
            let answer = choice.answer.as_ref()?;
            Some(format!("Decided: {} → {answer}", choice.question))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Runner {
    /// The rules the just-stored plan leaves open among those delivered for
    /// its files, as the plan keeps them: every one not in its `addresses`,
    /// the structural record of what it implements, and not settled: not
    /// among its `satisfied` claims, decided, or settled by an earlier
    /// approved plan of this task. A summary that says a rule is deferred or
    /// does not apply leaves it open too; such a rule is marked as mentioned.
    pub(super) fn open_rules(&self, context: &ContextResponse) -> Vec<OpenRule> {
        let Some(plan) = self.task.plan.as_ref() else {
            return Vec::new();
        };
        let open: Vec<GoverningRule> = self.unsettled_rules(
            context
                .governing_rules
                .iter()
                .filter(|rule| !plan.addresses.contains(&rule.iri))
                .cloned()
                .collect(),
            &plan.satisfied,
        );
        let unmentioned: Vec<String> = self
            .unaddressed_rules(&open)
            .into_iter()
            .map(|rule| rule.iri)
            .collect();
        open.into_iter()
            .map(|rule| OpenRule {
                mentioned: !unmentioned.contains(&rule.iri),
                iri: rule.iri,
                label: rule.label,
                kind: rule.kind,
            })
            .collect()
    }

    /// `/choose <n> <option>` while the plan awaits approval: answer open
    /// choice `n` with an option named by its text (any case) or its 1-based
    /// number. A later answer replaces an earlier one.
    pub fn choose_plan_option(&mut self, argument: &str) -> Result<()> {
        ensure!(
            self.task.phase == Phase::AwaitingPlan,
            "no plan awaiting approval"
        );
        let plan = self.task.plan.as_mut().context("no plan")?;
        let count = plan.open_choices.len();
        ensure!(count > 0, "the displayed plan leaves no open choice");
        let (number, option) = argument
            .trim()
            .split_once(char::is_whitespace)
            .context("Use /choose <n> <option>.")?;
        let number = number
            .parse::<usize>()
            .ok()
            .filter(|number| (1..=count).contains(number))
            .with_context(|| format!("no open choice {number}; the plan has {count}"))?;
        let choice = &mut plan.open_choices[number - 1];
        let index = option_named(&choice.options, option)
            .or_else(|| {
                option
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|at| (1..=choice.options.len()).contains(at))
                    .map(|at| at - 1)
            })
            .with_context(|| {
                format!(
                    "unknown option {:?} for open choice {number}; choose one of: {}",
                    option.trim(),
                    choice.options.join(" / ")
                )
            })?;
        let chosen = choice.options[index].clone();
        choice.answer = Some(chosen.clone());
        self.intent_event("plan_choice", &format!("{number}: {chosen}"));
        self.event(format!("Human chose for open choice {number}: {chosen}"));
        self.persist()
    }

    /// On approval: every unanswered open choice takes its default, the
    /// rules the plan leaves open are journaled as deferred, and those it
    /// says already hold as claimed satisfied.
    pub(super) fn settle_plan_approval(&mut self) {
        let claimed: Vec<String> = self.task.plan.as_ref().map_or_else(Vec::new, |plan| {
            plan.satisfied
                .iter()
                .map(|iri| {
                    self.context
                        .as_ref()
                        .and_then(|context| {
                            context.governing_rules.iter().find(|rule| &rule.iri == iri)
                        })
                        .map_or_else(|| iri.clone(), |rule| rule.label.clone())
                })
                .collect()
        });
        let Some(plan) = self.task.plan.as_mut() else {
            return;
        };
        let mut defaulted = Vec::new();
        for (index, choice) in plan.open_choices.iter_mut().enumerate() {
            if choice.answer.is_none() {
                choice.answer = Some(choice.default.clone());
                defaulted.push(format!("{}: {} (default)", index + 1, choice.default));
            }
        }
        let deferred: Vec<String> = plan
            .open_rules
            .iter()
            .map(|rule| rule.label.clone())
            .collect();
        for detail in defaulted {
            self.intent_event("plan_choice", &detail);
        }
        if !deferred.is_empty() {
            self.intent_event(
                "rules_deferred",
                &format!("{} rule(s): {}", deferred.len(), deferred.join("; ")),
            );
        }
        if !claimed.is_empty() {
            self.intent_event(
                "rules_claimed_satisfied",
                &format!("{} rule(s): {}", claimed.len(), claimed.join("; ")),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposed(options: &[&str], default: &str) -> ProposedChoice {
        ProposedChoice {
            question: "Which store?".into(),
            options: options.iter().map(|option| option.to_string()).collect(),
            default: default.into(),
        }
    }

    #[test]
    fn open_choices_are_bounded_and_their_default_is_an_option() {
        assert!(validate(&[]).is_ok());
        assert!(validate(&[proposed(&["a", "b"], "a")]).is_ok());
        let error = validate(&[proposed(&["a", "b"], "c")]).unwrap_err();
        assert!(
            error.to_string().contains("not one of its options"),
            "{error}"
        );
        assert!(validate(&[proposed(&["a"], "a")]).is_err());
        assert!(validate(&[proposed(&["a", "b", "c", "d", "e"], "a")]).is_err());
        assert!(validate(&[proposed(&["a", "A"], "a")]).is_err());
        assert!(validate(&[proposed(&["a", &"x".repeat(121)], "a")]).is_err());
        let four = vec![proposed(&["a", "b"], "a"); 4];
        let error = validate(&four).unwrap_err();
        assert!(error.to_string().contains("at most 3"), "{error}");
        let mut long = proposed(&["a", "b"], "a");
        long.question = "q".repeat(301);
        assert!(validate(&[long]).is_err());
    }

    #[test]
    fn a_plan_journaled_before_open_rules_and_choices_loads_and_saves_alike() {
        use super::super::task::{ApprovedPlan, Plan};
        let old = json!({"summary":"s","files":["a.rs"],"checks":["true"]});
        let plan: Plan = serde_json::from_value(old.clone()).unwrap();
        assert!(plan.open_rules.is_empty() && plan.open_choices.is_empty());
        assert!(plan.satisfied.is_empty());
        assert_eq!(serde_json::to_value(&plan).unwrap(), old);
        let old = json!({"summary":"s","files":["a.rs"],"edit_start":0});
        let approved: ApprovedPlan = serde_json::from_value(old.clone()).unwrap();
        assert!(approved.deferred.is_empty() && approved.satisfied.is_empty());
        assert_eq!(serde_json::to_value(&approved).unwrap(), old);
        // A plan that claims a rule already holds keeps the claim.
        let new = json!({"summary":"s","files":["a.rs"],"checks":["true"],"satisfied":["urn:r"]});
        let plan: Plan = serde_json::from_value(new.clone()).unwrap();
        assert_eq!(plan.satisfied, ["urn:r"]);
        assert_eq!(serde_json::to_value(&plan).unwrap(), new);
    }

    #[test]
    fn a_default_is_stored_as_its_option_is_spelled() {
        let stored = open_choices(vec![proposed(&["SQLite", "Postgres"], " sqlite ")]);
        assert_eq!(stored[0].default, "SQLite");
        assert_eq!(stored[0].answer, None);
    }

    #[test]
    fn only_answered_choices_are_decided() {
        let mut choices =
            open_choices(vec![proposed(&["a", "b"], "a"), proposed(&["c", "d"], "d")]);
        assert_eq!(decided(&choices), "");
        choices[1].answer = Some("c".into());
        assert_eq!(decided(&choices), "Decided: Which store? → c");
    }
}
