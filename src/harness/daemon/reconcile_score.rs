//! Pure symbolic reconciliation scoring for capture typing. A fresh
//! proposal is compared with same-kind accepted records by title, hybrid
//! recall rank and token overlap; the disposition is a threshold decision with
//! frozen defaults (Constraint "Reconciliation thresholds are frozen defaults,
//! overridable only by env, and recorded in receipts"). No writes, no model.
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::journal::{journal_path, load, load_or_store, validate_id};
use super::reconciliation::candidate_page;
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const CANDIDATE_LIMIT: usize = 20;
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "in", "is", "it", "its",
    "of", "on", "or", "that", "the", "this", "to", "was", "were", "with", "when", "which", "will",
    "not", "no", "we", "our", "so", "than", "then", "into", "over", "each", "every", "any", "all",
    "can", "must", "should", "never", "always", "only", "also",
];

impl ReconcileThresholds {
    pub fn from_env() -> anyhow::Result<Self> {
        let defaults = Self::default();
        Ok(Self {
            restates: threshold("MOOSEDEV_RECONCILE_RESTATES", defaults.restates)?,
            refines: threshold("MOOSEDEV_RECONCILE_REFINES", defaults.refines)?,
            refines_containment: threshold(
                "MOOSEDEV_RECONCILE_REFINES_CONTAINMENT",
                defaults.refines_containment,
            )?,
            tiebreak_band: threshold("MOOSEDEV_RECONCILE_TIEBREAK_BAND", defaults.tiebreak_band)?,
        })
    }
}

fn threshold(name: &str, default: f64) -> anyhow::Result<f64> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(default),
        Ok(value) => {
            let parsed: f64 = value
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} must be a decimal in 0.0..=1.0"))?;
            anyhow::ensure!(
                (0.0..=1.0).contains(&parsed),
                "{name} must be a decimal in 0.0..=1.0"
            );
            Ok(parsed)
        }
        Err(error) => anyhow::bail!("{name}: {error}"),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateScore {
    pub iri: String,
    pub kind: String,
    pub title: String,
    pub assertion_digest: String,
    pub title_score: f64,
    pub rank_score: f64,
    pub overlap: f64,
    /// Share of the candidate's tokens the proposal contains.
    pub containment: f64,
    /// The proposal says more than the candidate.
    pub longer: bool,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScoredDisposition {
    Restates {
        candidate_iri: String,
        score: f64,
    },
    Refines {
        candidate_iri: String,
        score: f64,
        containment: f64,
    },
    /// Within the tiebreak band of a threshold: the caller may ask a sensor,
    /// and otherwise treats it as distinct.
    Tiebreak {
        candidate_iri: String,
        score: f64,
        boundary: String,
    },
    Distinct {
        nearest: Option<CandidateScore>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredProposal {
    pub proposal_digest: String,
    pub candidate_revision: String,
    pub thresholds: ReconcileThresholds,
    pub candidates: Vec<CandidateScore>,
    pub disposition: ScoredDisposition,
}

/// Durable receipt for one symbolic reconciliation decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreReceipt {
    pub operation_id: String,
    pub owner_id: String,
    pub proposal_digest: String,
    pub candidate_revision: String,
    pub thresholds: ReconcileThresholds,
    pub disposition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_iri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_digest: Option<String>,
    pub score: f64,
    pub confidence: f64,
    /// `symbolic` or `llm_sensor` when a band tiebreak was resolved by a sensor.
    pub resolved_by: String,
}

pub fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|token| token.len() >= 2 && !STOPWORDS.contains(&token.as_str()))
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

fn normalize_title(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn round(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Score a fresh proposal against the current graph. Candidates come from the
/// reconciliation candidate page (exact titles first, then hybrid recall) and
/// are restricted to accepted records of the same kind.
pub fn score_proposal(
    state: &AppState,
    owner_id: &str,
    proposal: &KnowledgeProposal,
    thresholds: ReconcileThresholds,
) -> anyhow::Result<ScoredProposal> {
    validate_id(owner_id, "owner_id")?;
    let page = candidate_page(
        state,
        &CaptureCandidateRequest {
            owner_id: owner_id.into(),
            proposal: proposal.clone(),
            topic: None,
            cursor: None,
            limit: Some(CANDIDATE_LIMIT),
        },
    )?;
    let query = format!("{} {}", proposal.title, proposal.description);
    let ranked: Vec<String> =
        graph::relevant_context_snapshot(state, Some(&query), CANDIDATE_LIMIT, false)?
            .into_iter()
            .map(|item| item.iri)
            .collect();
    let proposal_title = tokens(&proposal.title);
    let proposal_text = tokens(&query);
    let normalized_title = normalize_title(&proposal.title);
    let title_local = graph::local_name(&state.capture.title).to_string();
    let description_local = graph::local_name(&state.capture.description).to_string();
    let mut candidates = Vec::new();
    for candidate in page
        .candidates
        .iter()
        .filter(|candidate| candidate.kind == proposal.kind && candidate.status == "accepted")
    {
        // Only the claim text takes part: the title (label, else the title
        // literal) and the description, never status or timestamps.
        let title = if candidate.title.trim().is_empty() {
            candidate
                .literals
                .iter()
                .find(|literal| literal.predicate == title_local)
                .map(|literal| literal.value.clone())
                .unwrap_or_default()
        } else {
            candidate.title.clone()
        };
        let candidate_title = tokens(&title);
        let description = candidate
            .literals
            .iter()
            .filter(|literal| literal.predicate == description_local)
            .map(|literal| literal.value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let candidate_text = tokens(&format!("{title} {description}"));
        let title_score = if normalize_title(&title) == normalized_title {
            1.0
        } else {
            jaccard(&proposal_title, &candidate_title)
        };
        let rank_score = ranked
            .iter()
            .position(|iri| iri == &candidate.iri)
            .map(|index| 1.0 / (1.0 + index as f64))
            .unwrap_or(0.0);
        let overlap = jaccard(&proposal_text, &candidate_text);
        let containment = if candidate_text.is_empty() {
            0.0
        } else {
            candidate_text.intersection(&proposal_text).count() as f64 / candidate_text.len() as f64
        };
        candidates.push(CandidateScore {
            iri: candidate.iri.clone(),
            kind: candidate.kind.clone(),
            title,
            assertion_digest: candidate.assertion_digest.clone(),
            title_score: round(title_score),
            rank_score: round(rank_score),
            overlap: round(overlap),
            containment: round(containment),
            longer: proposal_text.len() > candidate_text.len(),
            score: round(0.5 * title_score + 0.3 * rank_score + 0.2 * overlap),
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.iri.cmp(&b.iri))
    });
    let disposition = match candidates.first() {
        None => ScoredDisposition::Distinct { nearest: None },
        Some(best) if best.score >= thresholds.restates => ScoredDisposition::Restates {
            candidate_iri: best.iri.clone(),
            score: best.score,
        },
        Some(best)
            if best.score >= thresholds.refines
                && best.containment >= thresholds.refines_containment
                && best.longer =>
        {
            ScoredDisposition::Refines {
                candidate_iri: best.iri.clone(),
                score: best.score,
                containment: best.containment,
            }
        }
        Some(best) if (thresholds.restates - best.score).abs() <= thresholds.tiebreak_band => {
            ScoredDisposition::Tiebreak {
                candidate_iri: best.iri.clone(),
                score: best.score,
                boundary: "restates".into(),
            }
        }
        Some(best) if (thresholds.refines - best.score).abs() <= thresholds.tiebreak_band => {
            ScoredDisposition::Tiebreak {
                candidate_iri: best.iri.clone(),
                score: best.score,
                boundary: "refines".into(),
            }
        }
        Some(best) => ScoredDisposition::Distinct {
            nearest: Some(best.clone()),
        },
    };
    Ok(ScoredProposal {
        proposal_digest: page.proposal_digest,
        candidate_revision: page.revision,
        thresholds,
        candidates,
        disposition,
    })
}

fn receipt_path(state: &AppState, id: &str) -> anyhow::Result<PathBuf> {
    journal_path(state, id, "score.json")
}

/// Persist a receipt once; a replay with the same id must carry the same
/// receipt. Returns the stored receipt.
pub fn record_receipt(state: &AppState, receipt: ScoreReceipt) -> anyhow::Result<ScoreReceipt> {
    validate_id(&receipt.owner_id, "owner_id")?;
    let path = receipt_path(state, &receipt.operation_id)?;
    let check = |stored: &ScoreReceipt| {
        anyhow::ensure!(
            stored == &receipt,
            "operation_id was already used for a different reconciliation receipt"
        );
        Ok(())
    };
    let (stored, _) = load_or_store(&path, check, || Ok(receipt.clone()))?;
    Ok(stored)
}

pub fn load_receipt(state: &AppState, id: &str) -> anyhow::Result<Option<ScoreReceipt>> {
    load(&receipt_path(state, id)?)
}
