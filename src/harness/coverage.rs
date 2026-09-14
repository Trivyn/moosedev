//! Plan coverage: whether a proposed plan summary addresses each governing
//! Constraint the harness delivered. A pure, deterministic token test — a
//! delivery nudge, never evidence that the code complies (hidden and required
//! checks decide that). Thresholds are frozen defaults, overridable only by
//! env, and recorded in every receipt.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::harness::daemon::reconcile_score::tokens;

/// Distinctive label tokens a plan must mention (fewer when the label has fewer).
pub const DEFAULT_LABEL_MIN: usize = 2;
/// Distinctive claim tokens a plan must mention (fewer when the claim has fewer).
pub const DEFAULT_CLAIM_MIN: usize = 2;
/// Plans returned per planning cycle for unaddressed rules before one is kept.
pub const DEFAULT_RETURN_LIMIT: usize = 1;
/// The highest return limit the env override accepts.
pub const MAX_RETURN_LIMIT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageThresholds {
    pub label_min: usize,
    pub claim_min: usize,
    pub return_limit: usize,
}

impl Default for CoverageThresholds {
    fn default() -> Self {
        Self {
            label_min: DEFAULT_LABEL_MIN,
            claim_min: DEFAULT_CLAIM_MIN,
            return_limit: DEFAULT_RETURN_LIMIT,
        }
    }
}

impl CoverageThresholds {
    pub fn from_env() -> anyhow::Result<Self> {
        let defaults = Self::default();
        Ok(Self {
            label_min: count("MOOSEDEV_COVERAGE_LABEL_MIN", defaults.label_min, 1, 16)?,
            claim_min: count("MOOSEDEV_COVERAGE_CLAIM_MIN", defaults.claim_min, 1, 16)?,
            return_limit: count(
                "MOOSEDEV_COVERAGE_RETURN_LIMIT",
                defaults.return_limit,
                0,
                MAX_RETURN_LIMIT,
            )?,
        })
    }
}

fn count(name: &str, default: usize, min: usize, max: usize) -> anyhow::Result<usize> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(default),
        Ok(value) => {
            let parsed: usize = value
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} must be an integer in {min}..={max}"))?;
            anyhow::ensure!(
                (min..=max).contains(&parsed),
                "{name} must be an integer in {min}..={max}"
            );
            Ok(parsed)
        }
        Err(error) => anyhow::bail!("{name}: {error}"),
    }
}

/// One rule's coverage decision with the evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReceipt {
    pub iri: String,
    pub label: String,
    pub covered: bool,
    /// Distinctive label tokens the summary mentions, and how many there were.
    pub label_matched: Vec<String>,
    pub label_distinctive: usize,
    /// Distinctive claim tokens the summary mentions, and how many there were.
    pub claim_matched: Vec<String>,
    pub claim_distinctive: usize,
    pub thresholds: CoverageThresholds,
}

/// A frozen suffix fold so plural and tense variants of a word match.
pub fn stem(token: &str) -> String {
    let n = token.len();
    if n > 4 && token.ends_with("ies") {
        format!("{}y", &token[..n - 3])
    } else if n > 5 && token.ends_with("ing") {
        token[..n - 3].to_string()
    } else if n > 4 && (token.ends_with("ed") || token.ends_with("es")) {
        token[..n - 2].to_string()
    } else if n > 3 && token.ends_with('s') && !token.ends_with("ss") {
        token[..n - 1].to_string()
    } else {
        token.to_string()
    }
}

/// Stemmed content tokens of `text`, ignoring URLs (relationship lines carry IRIs).
pub fn stemmed_tokens(text: &str) -> BTreeSet<String> {
    let without_urls: String = text
        .split_whitespace()
        .filter(|word| !word.contains("://"))
        .collect::<Vec<_>>()
        .join(" ");
    tokens(&without_urls)
        .iter()
        .map(|token| stem(token))
        .collect()
}

/// The values of a rendered claim body: each `key: value` line without its
/// predicate name, which is renderer vocabulary, not the rule's words.
fn claim_values(claim: &str) -> String {
    claim
        .lines()
        .map(|line| match line.split_once(": ") {
            Some((key, value))
                if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_') =>
            {
                value
            }
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `summary` addresses the rule `label`/`claim` beyond what `background`
/// (the objective and human guidance) already says.
pub fn assess(
    summary: &str,
    background: &str,
    iri: &str,
    label: &str,
    claim: &str,
    thresholds: &CoverageThresholds,
) -> CoverageReceipt {
    let background = stemmed_tokens(background);
    let plan = stemmed_tokens(summary);
    let label_distinctive: BTreeSet<String> = stemmed_tokens(label)
        .difference(&background)
        .cloned()
        .collect();
    let claim_distinctive: BTreeSet<String> = stemmed_tokens(&claim_values(claim))
        .difference(&background)
        .cloned()
        .collect();
    let label_matched: Vec<String> = label_distinctive.intersection(&plan).cloned().collect();
    let claim_matched: Vec<String> = claim_distinctive.intersection(&plan).cloned().collect();
    // A rule with nothing beyond the objective's words is addressed by the
    // objective itself; otherwise a few distinctive words must appear.
    let covered = (label_distinctive.is_empty() && claim_distinctive.is_empty())
        || (!label_distinctive.is_empty()
            && label_matched.len() >= thresholds.label_min.min(label_distinctive.len()))
        || (!claim_distinctive.is_empty()
            && claim_matched.len() >= thresholds.claim_min.min(claim_distinctive.len()));
    CoverageReceipt {
        iri: iri.into(),
        label: label.into(),
        covered,
        label_matched,
        label_distinctive: label_distinctive.len(),
        claim_matched,
        claim_distinctive: claim_distinctive.len(),
        thresholds: *thresholds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECTIVE: &str =
        "Add a retry limit to the upload client so a failing upload stops after five attempts.";
    const LABEL: &str = "Uploads resume from the last acknowledged chunk";
    const CLAIM: &str = "hasDescription: An interrupted upload resumes from the last chunk the server acknowledged and never restarts from zero.\nconcerns: https://example.org/kg/component-1234\n";

    fn check(summary: &str) -> CoverageReceipt {
        assess(
            summary,
            OBJECTIVE,
            "urn:rule",
            LABEL,
            CLAIM,
            &CoverageThresholds::default(),
        )
    }

    #[test]
    fn stems_fold_plural_and_tense_suffixes() {
        assert_eq!(stem("uploads"), "upload");
        assert_eq!(stem("retries"), "retry");
        assert_eq!(stem("acknowledged"), "acknowledg");
        assert_eq!(stem("resuming"), "resum");
        assert_eq!(stem("class"), "class");
        assert_eq!(stem("is"), "is");
    }

    #[test]
    fn objective_tokens_are_not_distinctive() {
        // "upload" appears in the objective, so mentioning it proves nothing.
        let receipt =
            check("Add a retry counter to the upload client; uploads stop after five tries.");
        assert!(!receipt.covered, "{receipt:?}");
        assert!(!receipt.label_matched.contains(&"upload".to_string()));
    }

    #[test]
    fn a_plan_naming_the_rule_passes_on_label_tokens() {
        let receipt =
            check("Retry failed uploads; each retry resumes from the last acknowledged chunk.");
        assert!(receipt.covered, "{receipt:?}");
        assert!(receipt.label_matched.len() >= 2);
    }

    #[test]
    fn a_plan_in_claim_vocabulary_passes_and_urls_are_ignored() {
        let receipt =
            check("Never restart an interrupted transfer from zero; the server state wins.");
        assert!(receipt.covered, "{receipt:?}");
        assert!(receipt.claim_matched.iter().all(|t| !t.contains("example")));
    }

    #[test]
    fn a_plan_saying_the_rule_does_not_apply_passes() {
        let receipt =
            check("Retry limit only. The acknowledged-chunk resume rule does not apply here.");
        assert!(receipt.covered, "{receipt:?}");
    }

    #[test]
    fn a_plan_in_unrelated_code_vocabulary_still_returns() {
        let receipt =
            check("Wrap send() in a loop with attempt counter max_attempts = 5 and backoff.");
        assert!(!receipt.covered, "{receipt:?}");
        assert!(receipt.label_distinctive >= 2 && receipt.claim_distinctive >= 2);
    }

    #[test]
    fn a_rule_that_only_restates_the_objective_is_covered() {
        let receipt = assess(
            "anything",
            "retry the upload",
            "urn:r",
            "Retry upload",
            "hasDescription: Retry the upload.\n",
            &CoverageThresholds::default(),
        );
        assert!(receipt.covered);
        assert_eq!(
            (receipt.label_distinctive, receipt.claim_distinctive),
            (0, 0)
        );
    }

    #[test]
    fn env_overrides_are_bounded_and_invalid_values_error() {
        // One test owns these variables so parallel tests never observe them.
        std::env::set_var("MOOSEDEV_COVERAGE_LABEL_MIN", "3");
        std::env::set_var("MOOSEDEV_COVERAGE_RETURN_LIMIT", "2");
        let thresholds = CoverageThresholds::from_env().unwrap();
        assert_eq!(
            (
                thresholds.label_min,
                thresholds.claim_min,
                thresholds.return_limit
            ),
            (3, DEFAULT_CLAIM_MIN, 2)
        );
        std::env::set_var("MOOSEDEV_COVERAGE_RETURN_LIMIT", "3");
        assert!(CoverageThresholds::from_env().is_err());
        std::env::set_var("MOOSEDEV_COVERAGE_RETURN_LIMIT", "x");
        assert!(CoverageThresholds::from_env().is_err());
        std::env::set_var("MOOSEDEV_COVERAGE_LABEL_MIN", "0");
        std::env::remove_var("MOOSEDEV_COVERAGE_RETURN_LIMIT");
        assert!(CoverageThresholds::from_env().is_err());
        std::env::remove_var("MOOSEDEV_COVERAGE_LABEL_MIN");
        assert_eq!(
            CoverageThresholds::from_env().unwrap(),
            CoverageThresholds::default()
        );
    }
}
