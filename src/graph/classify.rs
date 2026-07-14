//! Evidence-based role/criticality classifier (judgment stratum).
//!
//! A **symbolic rule table** — no LLM (invariant #1) — over structural signals
//! (visibility, kind, paths, generated markers), the fan-in aggregation
//! (reference occurrences), and the churn sidecar (observation stratum). It
//! proposes judgments through the ratification queue; it never asserts an
//! edge. Discipline:
//!
//! * **Population**: the debt surface (public, non-module, non-test) ∩
//!   role-bearing kinds. Fields/constants/unknowns are reported as excluded
//!   groups — full accounting at group granularity, no wallpaper judgments.
//! * **Honest abstention**: no positive signal → unclassified-with-reason, no
//!   node. Criticality `standard` is the implicit default and never written.
//! * **Confidence gate** (Consequence `be097082`, never-nudge amendment):
//!   `escalated ⇔ confidence < 0.6 ∨ high-stakes`, where high-stakes =
//!   accepted `constrains` link ∨ proposed criticality high. Escalation is
//!   inbox prominence only — judgments never nudge.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::Path;

use chrono::Utc;

use crate::code::substrate::{DefinitionEntry, Substrate};

use super::code_entities::{entities_by_symbol, CodeTerms};
use super::debt::is_debt_surface;
use super::dossier::direct_records_for_entity;
use super::proposals::{list_proposals, propose_judgment, ProposalKind, AUTO_HELD, ESCALATED};
use super::state::AppState;
use super::taxonomy::{criticality_iri, ensure_taxonomy_individuals, role_iri};

/// Author stamped on classifier-proposed judgments.
pub const CLASSIFIER_AUTHOR: &str = "moosedev-classifier";

/// Kinds that can meaningfully play a role. Everything else is excluded from
/// the population and reported as a group.
const ROLE_BEARING_KINDS: &[&str] = &[
    "Function",
    "Method",
    "StaticMethod",
    "Struct",
    "Enum",
    "Trait",
    "Interface",
    "Class",
];

/// One judgment the plan would propose.
#[derive(Debug, Clone)]
pub struct PlannedJudgment {
    pub entity_iri: String,
    pub display_name: String,
    pub file: String,
    /// `playsRole` or `hasCriticality`.
    pub predicate_local: String,
    /// Target individual local name (e.g. `boundary`, `high`).
    pub target_local: String,
    pub confidence: f64,
    /// `escalated` or `auto-held`.
    pub escalation: &'static str,
    /// Human-readable rule trace.
    pub evidence: String,
}

/// Full-accounting classification plan: population = role judgments +
/// unclassified + skipped; criticality lists deviations only.
#[derive(Debug, Default)]
pub struct ClassifyPlan {
    pub role: Vec<PlannedJudgment>,
    pub criticality: Vec<PlannedJudgment>,
    /// (display name, reason) for population entities with no positive signal.
    pub unclassified: Vec<(String, &'static str)>,
    /// Population entities already judged (any status) on the role axis.
    pub skipped_existing: usize,
    /// Debt-surface definitions excluded by kind, tallied per kind.
    pub excluded_by_kind: BTreeMap<String, usize>,
    /// Debt-surface, role-bearing definitions with no minted CodeEntity.
    pub missing_entities: Vec<String>,
    /// Taxonomy individuals `apply` would still need to seed.
    pub taxonomy_to_seed: bool,
    /// Rough kg.nq growth: pending judgment nodes × quads per node.
    pub projected_quads: usize,
    /// True when the churn sidecar is absent — churn-dependent rules (R4
    /// core-algorithm) abstained rather than assume zero churn.
    pub churn_missing: bool,
}

/// What `apply_classify` wrote.
#[derive(Debug, Default)]
pub struct ClassifyOutcome {
    pub taxonomy_created: usize,
    pub proposed: usize,
}

/// Signals for one population entity, resolved once.
struct Evidence<'a> {
    entry: &'a DefinitionEntry,
    fan_in: u32,
    fan_in_p90: u32,
    /// `None` = no commits for the file within a PRESENT sidecar's window
    /// (verified zero); meaningless when `churn_available` is false.
    churn_commits: Option<u32>,
    churn_median: u32,
    /// Whether the churn sidecar exists at all. Absent sidecar ≠ zero churn:
    /// rules that need churn evidence must abstain, never fabricate stability.
    churn_available: bool,
    generated: bool,
    constrained: bool,
}

/// Build the classification plan. Pure read; writes nothing.
pub fn plan_classify(
    state: &AppState,
    substrate: &Substrate,
    repo_root: &Path,
) -> anyhow::Result<ClassifyPlan> {
    let terms = CodeTerms::resolve(state)?;
    let entities = entities_by_symbol(state, &terms)?;
    let reference_counts = substrate.reference_counts();

    // Judged (subject, predicate) pairs at ANY status — a rejected judgment is
    // a human decision, never re-proposed. Materialized edges ride along via
    // their accepted queue nodes.
    let judged: BTreeSet<(String, String)> = list_proposals(state, None)?
        .into_iter()
        .filter(|p| p.kind == ProposalKind::Judgment)
        .map(|p| (p.subject_iri, p.predicate_local))
        .collect();

    // Population: debt surface ∩ role-bearing kinds.
    let mut plan = ClassifyPlan::default();
    let mut population: Vec<&DefinitionEntry> = Vec::new();
    let definitions = substrate.definitions();
    for def in &definitions {
        if !is_debt_surface(def) {
            continue;
        }
        let kind = def.kind.as_deref().unwrap_or("Unknown");
        if !ROLE_BEARING_KINDS.contains(&kind) {
            *plan.excluded_by_kind.entry(kind.to_string()).or_insert(0) += 1;
            continue;
        }
        population.push(def);
    }

    // Percentile thresholds over the population.
    let mut fan_ins: Vec<u32> = population
        .iter()
        .map(|d| {
            reference_counts
                .get(&d.normalized_symbol)
                .copied()
                .unwrap_or(0)
        })
        .collect();
    fan_ins.sort_unstable();
    let fan_in_p90 = percentile(&fan_ins, 90).max(1);
    let mut churns: Vec<u32> = population
        .iter()
        .map(|d| substrate.churn_for_file(&d.file).map_or(0, |c| c.commits))
        .collect();
    churns.sort_unstable();
    let churn_median = percentile(&churns, 50);

    for def in &population {
        let Some(entity_iri) = entities.get(&def.normalized_symbol) else {
            plan.missing_entities.push(
                def.display_name
                    .clone()
                    .unwrap_or(def.normalized_symbol.clone()),
            );
            continue;
        };

        let evidence = Evidence {
            entry: def,
            fan_in: reference_counts
                .get(&def.normalized_symbol)
                .copied()
                .unwrap_or(0),
            fan_in_p90,
            churn_commits: substrate.churn_for_file(&def.file).map(|c| c.commits),
            churn_median,
            churn_available: substrate.churn_window_months().is_some(),
            generated: is_generated(repo_root, &def.file),
            constrained: has_accepted_constrains(state, entity_iri)?,
        };
        let display = def
            .display_name
            .clone()
            .unwrap_or_else(|| def.normalized_symbol.clone());

        let criticality = classify_criticality(&evidence);
        // High stakes: an accepted contract governs the entity, or the
        // classifier itself thinks it is critical.
        let high_stakes =
            evidence.constrained || matches!(&criticality, Some((local, ..)) if *local == "high");

        // Role axis — the full-accounting axis: every population entity lands
        // in exactly one of {role, unclassified, skipped_existing}.
        if judged.contains(&(entity_iri.clone(), "playsRole".to_string())) {
            plan.skipped_existing += 1;
        } else {
            match classify_role(&evidence) {
                Some((local, confidence, evidence_text)) => plan.role.push(PlannedJudgment {
                    entity_iri: entity_iri.clone(),
                    display_name: display.clone(),
                    file: def.file.clone(),
                    predicate_local: "playsRole".to_string(),
                    target_local: local.to_string(),
                    confidence,
                    escalation: disposition(confidence, high_stakes),
                    evidence: evidence_text,
                }),
                None => plan
                    .unclassified
                    .push((display.clone(), "no positive signal")),
            }
        }

        // Criticality axis — deviations from implicit `standard` only.
        if !judged.contains(&(entity_iri.clone(), "hasCriticality".to_string())) {
            if let Some((local, confidence, evidence_text)) = criticality {
                plan.criticality.push(PlannedJudgment {
                    entity_iri: entity_iri.clone(),
                    display_name: display,
                    file: def.file.clone(),
                    predicate_local: "hasCriticality".to_string(),
                    target_local: local.to_string(),
                    confidence,
                    escalation: disposition(confidence, high_stakes),
                    evidence: evidence_text,
                });
            }
        }
    }

    plan.unclassified.sort();
    plan.missing_entities.sort();
    plan.churn_missing = substrate.churn_window_months().is_none();
    plan.taxonomy_to_seed =
        state.resolve_code_class("CodeRole").is_ok() && !taxonomy_seeded(state)?;
    // ~11 quads per judgment node (type, label, 6 literals, author, status, ts).
    plan.projected_quads = (plan.role.len() + plan.criticality.len()) * 11;
    Ok(plan)
}

/// Seed the taxonomy then enqueue every planned judgment.
pub fn apply_classify(state: &AppState, plan: &ClassifyPlan) -> anyhow::Result<ClassifyOutcome> {
    let mut outcome = ClassifyOutcome {
        taxonomy_created: ensure_taxonomy_individuals(state)?,
        proposed: 0,
    };
    let now = Utc::now();
    for judgment in plan.role.iter().chain(plan.criticality.iter()) {
        let target_iri = match judgment.predicate_local.as_str() {
            "playsRole" => role_iri(&judgment.target_local),
            _ => criticality_iri(&judgment.target_local),
        };
        propose_judgment(
            state,
            &judgment.entity_iri,
            &judgment.predicate_local,
            &target_iri,
            judgment.confidence,
            judgment.escalation,
            &judgment.evidence,
            CLASSIFIER_AUTHOR,
            now,
        )?;
        outcome.proposed += 1;
    }
    Ok(outcome)
}

/// Role rules, first match wins. Returns (role local, confidence, evidence).
fn classify_role(evidence: &Evidence) -> Option<(&'static str, f64, String)> {
    let file = &evidence.entry.file;

    // R1 generated: path pattern or an explicit generated marker in the file.
    let generated_path =
        file.contains("/generated/") || file.contains(".pb.") || file.starts_with("dist/");
    if generated_path || evidence.generated {
        let via = if generated_path {
            "path"
        } else {
            "file marker"
        };
        return Some(("generated", 0.95, format!("R1 generated: {via} in {file}")));
    }

    // R2 boundary: public surface in adapter/entry-point territory.
    const BOUNDARY_PATTERNS: &[&str] = &[
        "api/handlers",
        "routes",
        "src/lsp/",
        "src/mcp/",
        "adapters",
        "clients/",
    ];
    if BOUNDARY_PATTERNS.iter().any(|p| file.contains(p)) || file == "src/main.rs" {
        return Some((
            "boundary",
            0.75,
            format!("R2 boundary: entry-point path {file}"),
        ));
    }

    // R3 glue: init/config/runtime plumbing.
    if file.starts_with("src/init") || file.starts_with("src/runtime") || file.contains("config") {
        return Some(("glue", 0.6, format!("R3 glue: plumbing path {file}")));
    }

    // R4 core-algorithm: heavily referenced and stable. Requires the churn
    // sidecar: an absent sidecar is missing evidence, not zero churn — abstain
    // rather than label a hot entity "stable" on fabricated data.
    if evidence.churn_available {
        // A file absent from a PRESENT sidecar had zero commits in the window.
        let churn = evidence.churn_commits.unwrap_or(0);
        if evidence.fan_in >= evidence.fan_in_p90 && churn <= evidence.churn_median {
            return Some((
                "core-algorithm",
                0.6,
                format!(
                    "R4 core-algorithm: fan-in {} (≥ P90 {}); churn {churn} commits/24mo (≤ median {})",
                    evidence.fan_in, evidence.fan_in_p90, evidence.churn_median
                ),
            ));
        }
    }

    None // honest abstention — never a low-confidence guess
}

/// Criticality rules — deviations from the implicit `standard` only.
fn classify_criticality(evidence: &Evidence) -> Option<(&'static str, f64, String)> {
    let file = &evidence.entry.file;
    if evidence.constrained || evidence.fan_in >= evidence.fan_in_p90 {
        let why = if evidence.constrained {
            "accepted constrains link".to_string()
        } else {
            format!("fan-in {} ≥ P90 {}", evidence.fan_in, evidence.fan_in_p90)
        };
        return Some(("high", 0.6, format!("criticality high: {why}")));
    }
    let generated_path =
        file.contains("/generated/") || file.contains(".pb.") || file.starts_with("dist/");
    if generated_path
        || evidence.generated
        || file.starts_with("bench/")
        || file.starts_with("examples/")
        || file.contains("fixtures")
    {
        return Some((
            "low",
            0.7,
            format!("criticality low: generated/bench/example path {file}"),
        ));
    }
    None // standard — implicit, never written
}

/// The confidence gate: escalate on low confidence or high stakes.
fn disposition(confidence: f64, high_stakes: bool) -> &'static str {
    if confidence < 0.6 || high_stakes {
        ESCALATED
    } else {
        AUTO_HELD
    }
}

/// Nearest-rank percentile over a sorted slice; 0 for empty input.
fn percentile(sorted: &[u32], pct: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Does the file's head carry an explicit generated marker?
fn is_generated(repo_root: &Path, file: &str) -> bool {
    let Ok(handle) = std::fs::File::open(repo_root.join(file)) else {
        return false;
    };
    std::io::BufReader::new(handle)
        .lines()
        .take(5)
        .map_while(Result::ok)
        .any(|line| line.contains("@generated") || line.contains("DO NOT EDIT"))
}

/// Does the entity carry an accepted `constrains` link?
fn has_accepted_constrains(state: &AppState, entity_iri: &str) -> anyhow::Result<bool> {
    Ok(direct_records_for_entity(state, entity_iri)?
        .iter()
        .any(|record| {
            record.predicate_local == "constrains"
                && record.kind == "Constraint"
                && record.status.eq_ignore_ascii_case("accepted")
        }))
}

/// Are all taxonomy individuals already present?
fn taxonomy_seeded(state: &AppState) -> anyhow::Result<bool> {
    // ensure_taxonomy_individuals is idempotent; a dry-run only needs to know
    // whether apply would create anything. Cheapest honest check: one probe.
    let role_class = state.resolve_code_class("CodeRole")?;
    let node = oxigraph::model::NamedNode::new(role_iri("core-algorithm"))?;
    let class = oxigraph::model::NamedNode::new(&role_class)?;
    let graph = oxigraph::model::NamedNodeRef::new(super::PROJECT_KG_GRAPH_IRI)?;
    Ok(state
        .store
        .quads_for_pattern(
            Some(node.as_ref().into()),
            Some(oxigraph::model::NamedNodeRef::new(moose::RDF_TYPE)?),
            Some(class.as_ref().into()),
            Some(oxigraph::model::GraphNameRef::NamedNode(graph)),
        )
        .next()
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, kind: &str) -> DefinitionEntry {
        DefinitionEntry {
            producer: "rust-analyzer".to_string(),
            symbol: format!("rust-analyzer cargo p 1.0.0 {file}/x()."),
            normalized_symbol: format!("rust-analyzer cargo p . {file}/x()."),
            display_name: Some("x".to_string()),
            kind: Some(kind.to_string()),
            signature: Some("pub fn x()".to_string()),
            file: file.to_string(),
            is_module: false,
            is_public: true,
        }
    }

    fn evidence<'a>(
        entry: &'a DefinitionEntry,
        fan_in: u32,
        churn: Option<u32>,
        constrained: bool,
    ) -> Evidence<'a> {
        Evidence {
            entry,
            fan_in,
            fan_in_p90: 10,
            churn_commits: churn,
            churn_median: 5,
            churn_available: true,
            generated: false,
            constrained,
        }
    }

    #[test]
    fn rule_precedence_and_abstention() {
        // R1 beats everything on a generated path.
        let generated = entry("src/generated/schema.rs", "Function");
        let e = evidence(&generated, 50, Some(0), false);
        assert_eq!(classify_role(&e).unwrap().0, "generated");

        // R2 boundary on adapter paths.
        let boundary = entry("src/api/handlers/policy.rs", "Function");
        let e = evidence(&boundary, 0, Some(9), false);
        assert_eq!(classify_role(&e).unwrap().0, "boundary");

        // R4 core: hot fan-in + cold churn.
        let core = entry("src/graph/dossier.rs", "Function");
        let e = evidence(&core, 12, Some(3), false);
        assert_eq!(classify_role(&e).unwrap().0, "core-algorithm");

        // Abstention: no signal → None, never a guess.
        let plain = entry("src/graph/dossier.rs", "Function");
        let e = evidence(&plain, 2, Some(9), false);
        assert!(classify_role(&e).is_none());

        // Missing sidecar ≠ zero churn: a hot entity must not be labeled a
        // stable core algorithm on fabricated evidence — abstain.
        let hot = entry("src/graph/dossier.rs", "Function");
        let mut e = evidence(&hot, 12, None, false);
        e.churn_available = false;
        assert!(
            classify_role(&e).is_none(),
            "R4 abstains without the churn sidecar"
        );
    }

    #[test]
    fn criticality_deviations_only() {
        let constrained = entry("src/graph/dossier.rs", "Function");
        let e = evidence(&constrained, 0, None, true);
        assert_eq!(classify_criticality(&e).unwrap().0, "high");

        let hot = entry("src/graph/dossier.rs", "Function");
        let e = evidence(&hot, 15, None, false);
        assert_eq!(classify_criticality(&e).unwrap().0, "high");

        let bench = entry("bench/harness.rs", "Function");
        let e = evidence(&bench, 0, None, false);
        assert_eq!(classify_criticality(&e).unwrap().0, "low");

        let plain = entry("src/graph/dossier.rs", "Function");
        let e = evidence(&plain, 2, None, false);
        assert!(classify_criticality(&e).is_none(), "standard is implicit");
    }

    #[test]
    fn confidence_gate_dispositions() {
        assert_eq!(disposition(0.75, false), AUTO_HELD);
        assert_eq!(disposition(0.75, true), ESCALATED, "high stakes escalates");
        assert_eq!(
            disposition(0.5, false),
            ESCALATED,
            "low confidence escalates"
        );
        assert_eq!(disposition(0.6, false), AUTO_HELD, "0.6 is the floor");
    }

    #[test]
    fn percentile_nearest_rank() {
        assert_eq!(percentile(&[], 90), 0);
        assert_eq!(percentile(&[3], 90), 3);
        let v: Vec<u32> = (1..=10).collect();
        assert_eq!(percentile(&v, 90), 9);
        assert_eq!(percentile(&v, 50), 5);
    }
}
