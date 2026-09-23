//! Capture typing: one plain-prose note from the coding model,
//! plus the plan, the diff and the check history, become typed proposals.
//! Two proposals are purely symbolic (a decision for the change, a lesson for
//! a check that failed then passed); a bounded LLM sensor may add more when
//! the daemon has one. Every proposal is then reconciled symbolically against
//! the graph and receives a durable receipt. No graph writes happen here.
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use moose::types::LlmAssistLevel;
use serde::Deserialize;
use serde_json::json;

use super::current_status;
use super::journal::{journal_path, load, save_operation, validate_id};
use super::reconcile_score::{
    approved_plan_line, record_receipt, score_proposal, ScoreReceipt, ScoredDisposition,
};
use super::revision::ensure_unchanged;
use crate::api::error::ApiError;
use crate::graph::{self, AppState};
use crate::harness::protocol::*;

const MAX_SENSOR_PROPOSALS: usize = 5;
const MAX_PROPOSALS: usize = 16;
const MAX_TITLE_CHARS: usize = 100;

pub async fn capture_type(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureTypeRequest>,
) -> Result<Json<CaptureTypeResponse>, ApiError> {
    Ok(Json(capture_type_operation(&state, request).await?))
}

#[derive(Debug, Deserialize)]
struct SensorTyping {
    proposals: Vec<SensorProposal>,
    #[allow(dead_code)]
    reason: String,
}
#[derive(Debug, Deserialize)]
struct SensorProposal {
    kind: String,
    title: String,
    description: String,
}

fn cap_title(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_TITLE_CHARS {
        return text;
    }
    // Cut where a phrase ends, not mid-word. A title is a name, and a record
    // named "…is a deliberate design choice to bridge the gap be…" names
    // nothing: the badciv-map capture produced exactly that. Prefer the last
    // clause boundary inside the cap, else the last word boundary.
    //
    // Only the cut point changes. Where the title comes from is AD dec341e4's
    // decision, and Lesson 04d8c3f7 records that retitling naively disabled
    // dedup, because the title is half the reconciliation score.
    let capped: String = text.chars().take(MAX_TITLE_CHARS - 1).collect();
    let clause = capped
        .rfind([',', ';', ':'])
        .filter(|at| *at * 2 >= capped.len());
    let cut = clause.or_else(|| capped.rfind(' ')).unwrap_or(capped.len());
    let kept = capped[..cut].trim_end().trim_end_matches([',', ';', ':']);
    format!("{kept}…")
}

/// The answer the capture question invites when nothing durable happened.
const NOTHING_DURABLE: &str = "nothing beyond the diff";

/// Whether the note declares that the change carries nothing durable: empty,
/// or opening with the phrase the question offers for that case. A note that
/// states a claim and only ends with the phrase still carries the claim.
pub(crate) fn declares_nothing(note: &str) -> bool {
    let normalized: String = note
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    normalized.is_empty() || normalized.starts_with(NOTHING_DURABLE)
}

/// Openers a model puts before the claim itself; the title keeps the claim.
const CLAIM_OPENERS: [&str; 7] = [
    "i decided that",
    "i decided to",
    "i chose to",
    "we decided to",
    "we chose to",
    "decision:",
    "decided to",
];

/// The note's first sentence, without a decision opener, as the record title.
/// `None` when the note has no sentence to name.
pub(crate) fn claim_title(note: &str) -> Option<String> {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentence = note.as_str();
    for (index, c) in note.char_indices() {
        let closes = match c {
            '!' | '?' => true,
            '.' => note[index + 1..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace),
            _ => false,
        };
        if closes && index > 0 {
            sentence = &note[..index];
            break;
        }
    }
    let lowered = sentence.to_lowercase();
    let stripped = CLAIM_OPENERS
        .iter()
        .find(|opener| lowered.starts_with(*opener))
        .map(|opener| sentence[opener.len()..].trim_start())
        .unwrap_or(sentence)
        .trim();
    let mut chars = stripped.chars();
    let first = chars.next()?;
    Some(cap_title(&format!(
        "{}{}",
        first.to_uppercase(),
        chars.as_str()
    )))
}

/// `title (qualifier)` within the title cap. The base is shortened, never the
/// qualifier: re-capping the joined text once cut the qualifier off a title
/// already at the cap, so every retype repeated the colliding title.
fn qualified_title(title: &str, qualifier: &str) -> String {
    let suffix = format!(" ({qualifier})");
    let room = MAX_TITLE_CHARS.saturating_sub(suffix.chars().count());
    if room < 2 {
        return cap_title(&format!("{title}{suffix}"));
    }
    let base = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if base.chars().count() <= room {
        base
    } else {
        let kept: String = base.chars().take(room - 1).collect();
        format!("{}…", kept.trim_end())
    };
    format!("{base}{suffix}")
}

/// Whether a current or proposed record already carries this exact title.
fn title_in_use(state: &AppState, title: &str) -> bool {
    graph::resolve_record_exact_all(state, title)
        .into_iter()
        .any(|(iri, _)| {
            current_status(state, &iri).is_some_and(|status| graph::is_current_or_proposed(&status))
        })
}

fn normalized(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Legal `isMotivatedBy` targets for a captured ArchitecturalDecision, per the
/// ontology's range (`software-architecture.ttl`). Picking an illegal target
/// would fail SHACL in `plan_relation_args` and take the whole capture with it.
const MOTIVATION_KINDS: [&str; 2] = ["Requirement", "Constraint"];

/// Legal `learnedFrom` sources for a captured Lesson. Requirements and
/// Constraints are deliberately absent: the ontology range excludes them.
const LESSON_SOURCE_KINDS: [&str; 2] = ["ArchitecturalDecision", "AntiPattern"];

/// The one obligation that legally motivates this record, or nothing.
///
/// The approved plan's obligations are every record governing its files, of
/// every kind and in no priority order (`symbolic/scope.rs` flattens the
/// dossier's kind ranking away). The ontology range is what narrows them: an
/// ArchitecturalDecision may only be motivated by a Requirement or Constraint,
/// a Lesson may only be learned from an ArchitecturalDecision or AntiPattern.
///
/// After that filter, exactly one candidate is not a judgement call — the human
/// approved the plan against that obligation, and the approval is the
/// ratification. Two or more IS a judgement call, so nothing is drawn: the
/// record keeps its SHACL `minCount` warning, which `under_linked_records`
/// already reports and `suggest_links` already offers candidates for, at
/// leisure and without paging anyone.
/// The rule itself, over a caller-supplied kind lookup so it can be tested
/// without a live graph. `kind_of` returns None for an IRI that is not a
/// current, ratified InformationRecord — that record is not a candidate.
fn derive_relation(
    predicate: &'static str,
    legal_kinds: &[&str],
    obligations: &[String],
    kind_of: impl Fn(&str) -> Option<String>,
) -> DerivedRelation {
    let mut legal: Vec<String> = Vec::new();
    for iri in obligations {
        let Some(kind) = kind_of(iri) else { continue };
        if legal_kinds.contains(&kind.as_str()) && !legal.contains(iri) {
            legal.push(iri.clone());
        }
    }
    match legal.len() {
        1 => DerivedRelation {
            predicate: predicate.into(),
            chosen: legal.pop(),
            candidates_considered: 1,
            reason: "asserted".into(),
        },
        0 => DerivedRelation {
            predicate: predicate.into(),
            chosen: None,
            candidates_considered: 0,
            reason: "none_legal".into(),
        },
        n => DerivedRelation {
            predicate: predicate.into(),
            chosen: None,
            candidates_considered: n,
            reason: "ambiguous".into(),
        },
    }
}

/// Draw the relations the approved plan's obligations support for one proposal.
/// Nothing here can make a proposal governing: both predicates travel the
/// ordinary relation path, never the lifecycle fields.
pub(super) fn derive_relations(
    proposal: &mut KnowledgeProposal,
    obligations: &[String],
    kind_of: impl Fn(&str) -> Option<String>,
) -> Vec<DerivedRelation> {
    // Nothing to decide, so nothing to journal. `none_legal` means the plan
    // carried obligations and none were legal for this predicate — a real
    // signal. An empty list means no obligations were supplied at all (a plan
    // over ungoverned files, or a runner older than this field), which is not
    // the same thing and must not read as one.
    if obligations.is_empty() {
        return vec![];
    }
    let (predicate, kinds) = match proposal.kind.as_str() {
        "ArchitecturalDecision" => ("isMotivatedBy", &MOTIVATION_KINDS[..]),
        "Lesson" => ("learnedFrom", &LESSON_SOURCE_KINDS[..]),
        _ => return vec![],
    };
    let derived = derive_relation(predicate, kinds, obligations, kind_of);
    if let Some(target) = derived.chosen.clone() {
        match predicate {
            "isMotivatedBy" => proposal.requirement = Some(target),
            _ => proposal.learned_from = Some(target),
        }
    }
    vec![derived]
}

fn base_proposal(
    kind: &str,
    title: String,
    description: String,
    evidence: Vec<String>,
    files: Vec<String>,
) -> KnowledgeProposal {
    KnowledgeProposal {
        kind: kind.into(),
        title,
        description,
        evidence,
        files,
        components: vec![],
        requirement: None,
        supersedes: None,
        retracts: None,
        learned_from: None,
        reconciled: vec![],
    }
}

/// Idempotent by operation id: a replay returns the stored typing.
pub async fn capture_type_operation(
    state: &AppState,
    request: CaptureTypeRequest,
) -> anyhow::Result<CaptureTypeResponse> {
    validate_id(&request.owner_id, "owner_id")?;
    let path = journal_path(state, &request.operation_id, "type.json")?;
    if let Some(stored) = load::<StoredTyping>(&path)? {
        anyhow::ensure!(
            stored.request == request,
            "operation_id was already used for a different capture typing"
        );
        return Ok(stored.response);
    }
    // The sensor runs outside the operation lock; see `journal` for the race
    // this admits on same-id retries.
    let response = type_note(state, &request).await?;
    save_operation(
        &path,
        &StoredTyping {
            request,
            response: response.clone(),
        },
    )?;
    Ok(response)
}

/// Types one note against the current graph; the caller journals the result.
async fn type_note(
    state: &AppState,
    request: &CaptureTypeRequest,
) -> anyhow::Result<CaptureTypeResponse> {
    ensure_unchanged(
        state,
        &request.knowledge_revision,
        "knowledge changed before capture typing; refresh context and retry",
    )?;
    let revision = request.knowledge_revision.clone();
    anyhow::ensure!(
        request.note.len() <= 16_000 && request.note_evidence.len() <= 64,
        "capture note or its evidence exceeds the typing bound"
    );
    let thresholds = ReconcileThresholds::from_env()?;
    let note = request.note.trim();
    let mut raw: Vec<(KnowledgeProposal, ProposalOrigin)> = Vec::new();
    let mut evidence = request.note_evidence.clone();
    if evidence.is_empty() {
        evidence.push(format!("approved plan: {}", request.plan_summary));
    }
    // A change carries a decision only when the note states one: a note that
    // declares nothing durable is the model's answer, not a record (no forced
    // records). The title names the note's claim; the approved plan stays in
    // the description, where reconciliation reads it as the record's key.
    let nothing_declared = declares_nothing(note);
    if !request.changed_files.is_empty() && !nothing_declared {
        let description = format!(
            "{note}\n\n{}\n\nFiles changed: {}.",
            approved_plan_line(&request.plan_summary),
            request.changed_files.join(", ")
        );
        raw.push((
            base_proposal(
                "ArchitecturalDecision",
                claim_title(note).unwrap_or_else(|| cap_title(&request.plan_summary)),
                description,
                evidence.clone(),
                request.changed_files.clone(),
            ),
            ProposalOrigin::SymbolicDecision,
        ));
    }
    // A check that failed, then passed after an edit, is a deterministic lesson.
    let mut lesson_commands: Vec<String> = Vec::new();
    for (index, outcome) in request.check_history.iter().enumerate() {
        if outcome.success || lesson_commands.contains(&outcome.command) {
            continue;
        }
        let recovered = request.check_history[index + 1..]
            .iter()
            .any(|later| later.command == outcome.command && later.success && later.after_edit);
        if recovered {
            lesson_commands.push(outcome.command.clone());
        }
    }
    for command in &lesson_commands {
        let mut description =
            format!("The required check `{command}` failed before the change and passed after it.");
        if !note.is_empty() {
            description.push_str(&format!("\n\nModel note: {note}"));
        }
        let mut lesson_evidence = vec![
            format!("check failed: {command}"),
            format!("check passed after edit: {command}"),
        ];
        lesson_evidence.extend(evidence.iter().cloned());
        raw.push((
            base_proposal(
                "Lesson",
                cap_title(&format!(
                    "Check {command} failed before the edit and passed after it"
                )),
                description,
                lesson_evidence,
                request.changed_files.clone(),
            ),
            ProposalOrigin::SymbolicLesson,
        ));
    }
    let sensor_enabled = state.llm_configured
        && state.engine_config.llm_assist_level != LlmAssistLevel::PureSymbolic
        && !note.is_empty();
    let (typing_mode, mut typing_note) = if sensor_enabled {
        (TypingMode::Sensor, None)
    } else {
        (
            TypingMode::SymbolicOnly,
            Some(if note.is_empty() {
                "empty note; symbolic typing only".to_string()
            } else if nothing_declared {
                "note declares nothing durable; no decision proposed".to_string()
            } else {
                "no daemon LLM sensor configured; symbolic typing only".to_string()
            }),
        )
    };
    if sensor_enabled {
        match sensor_typing(state, request).await {
            Ok(typed) => {
                // A sensor proposal that only re-titles the plan duplicates the
                // symbolic decision, whichever sentence names that one.
                let mut known: Vec<String> =
                    raw.iter().map(|(p, _)| normalized(&p.title)).collect();
                known.push(normalized(&request.plan_summary));
                for proposal in typed.proposals.into_iter().take(MAX_SENSOR_PROPOSALS) {
                    if !is_record_kind(&proposal.kind)
                        || proposal.title.trim().is_empty()
                        || proposal.description.trim().is_empty()
                        || known.contains(&normalized(&proposal.title))
                    {
                        continue;
                    }
                    raw.push((
                        base_proposal(
                            &proposal.kind,
                            cap_title(&proposal.title),
                            proposal.description.trim().to_string(),
                            evidence.clone(),
                            request.changed_files.clone(),
                        ),
                        ProposalOrigin::LlmSensor,
                    ));
                }
            }
            Err(error) => {
                typing_note = Some(format!(
                    "sensor typing failed; symbolic proposals only: {error}"
                ));
            }
        }
    }
    raw.truncate(MAX_PROPOSALS);
    let tiebreak_enabled = sensor_enabled
        && state.engine_config.llm_assist_level == LlmAssistLevel::SensorWithFallback;
    let mut proposals = Vec::new();
    for (index, (mut proposal, origin)) in raw.into_iter().enumerate() {
        // Before scoring, draw what the approved plan's obligations support.
        // This only ever sets `requirement`/`learned_from`, never a lifecycle
        // field, so a derived edge can never make the capture governing.
        let derived = derive_relations(&mut proposal, &request.obligation_iris, |iri| {
            // Re-validate rather than trust the runner's IRIs: the daemon owns
            // what reaches the graph, as it does for lifecycle targets. An IRI
            // that is not current ratified knowledge is simply not a candidate.
            let node = oxigraph::model::NamedNode::new(iri).ok()?;
            let class = graph::require_information_record(state, &node).ok()?;
            graph::in_working_set(&current_status(state, iri).unwrap_or_default()).then(|| {
                class
                    .rsplit(['#', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
        });
        let scored = score_proposal(state, &request.owner_id, &proposal, thresholds)?;
        let receipt_id = format!("{}-r{index}", request.operation_id);
        let mut resolved_by = "symbolic".to_string();
        let disposition = match scored.disposition.clone() {
            ScoredDisposition::Tiebreak {
                candidate_iri,
                score,
                boundary,
            } => {
                let candidate = scored
                    .candidates
                    .iter()
                    .find(|candidate| candidate.iri == candidate_iri)
                    .cloned();
                let verdict = if tiebreak_enabled {
                    sensor_tiebreak(
                        state,
                        &proposal,
                        candidate.as_ref().map(|c| c.title.as_str()),
                    )
                    .await
                } else {
                    None
                };
                match verdict.as_deref() {
                    Some("same") => {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Restates {
                            candidate_iri,
                            score,
                        }
                    }
                    Some("narrower")
                        if boundary == "refines"
                            || candidate.as_ref().is_some_and(|c| c.longer) =>
                    {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Refines {
                            candidate_iri,
                            score,
                            containment: candidate.map(|c| c.containment).unwrap_or(0.0),
                        }
                    }
                    Some(_) => {
                        resolved_by = "llm_sensor".into();
                        ScoredDisposition::Distinct { nearest: candidate }
                    }
                    None => ScoredDisposition::Distinct { nearest: candidate },
                }
            }
            other => other,
        };
        let candidate_digest = |iri: &str| {
            scored
                .candidates
                .iter()
                .find(|candidate| candidate.iri == iri)
                .map(|candidate| candidate.assertion_digest.clone())
        };
        let typed = match disposition {
            ScoredDisposition::Restates {
                candidate_iri,
                score,
            } => {
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "restates".into(),
                        candidate_iri: Some(candidate_iri.clone()),
                        candidate_digest: candidate_digest(&candidate_iri),
                        score,
                        confidence: score,
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                TypedDisposition::Restates {
                    candidate_iri,
                    score,
                    confidence: score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Refines {
                candidate_iri,
                score,
                containment,
            } => {
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "refines".into(),
                        candidate_iri: Some(candidate_iri.clone()),
                        candidate_digest: candidate_digest(&candidate_iri),
                        score,
                        confidence: score,
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                proposal.reconciled = vec![ReconciledRelation {
                    predicate: "refines".into(),
                    target_iri: candidate_iri.clone(),
                    confidence: score,
                    receipt_operation_id: receipt_id.clone(),
                }];
                TypedDisposition::Refines {
                    candidate_iri,
                    score,
                    containment,
                    confidence: score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Distinct { nearest } => {
                let nearest_iri = nearest.as_ref().map(|c| c.iri.clone());
                let score = nearest.as_ref().map(|c| c.score);
                record_receipt(
                    state,
                    ScoreReceipt {
                        operation_id: receipt_id.clone(),
                        owner_id: request.owner_id.clone(),
                        proposal_digest: scored.proposal_digest.clone(),
                        candidate_revision: scored.candidate_revision.clone(),
                        thresholds,
                        disposition: "distinct".into(),
                        candidate_iri: nearest_iri.clone(),
                        candidate_digest: nearest_iri.as_deref().and_then(candidate_digest),
                        score: score.unwrap_or(0.0),
                        confidence: 1.0 - score.unwrap_or(0.0),
                        resolved_by: resolved_by.clone(),
                    },
                )?;
                // A distinct claim under a title the graph already uses is
                // qualified so the ordinary capture path accepts it: by the
                // first changed file, else (or when that is taken too) by the
                // operation prefix, which a retype always renews.
                if title_in_use(state, &proposal.title) {
                    let operation: String = request.operation_id.chars().take(8).collect();
                    let by_file = request
                        .changed_files
                        .first()
                        .map(|file| qualified_title(&proposal.title, file))
                        .filter(|title| !title_in_use(state, title));
                    proposal.title =
                        by_file.unwrap_or_else(|| qualified_title(&proposal.title, &operation));
                }
                TypedDisposition::Distinct {
                    nearest_iri,
                    score,
                    receipt_operation_id: receipt_id,
                }
            }
            ScoredDisposition::Tiebreak { .. } => unreachable!("tiebreaks are resolved above"),
        };
        proposals.push(TypedProposal {
            proposal,
            origin,
            disposition: typed,
            resolved_by,
            derived,
        });
    }
    ensure_unchanged(
        state,
        &revision,
        "knowledge changed during capture typing; retry",
    )?;
    Ok(CaptureTypeResponse {
        revision,
        typing_mode,
        typing_note,
        thresholds,
        proposals,
    })
}

#[derive(serde::Serialize, Deserialize)]
struct StoredTyping {
    request: CaptureTypeRequest,
    response: CaptureTypeResponse,
}

async fn sensor_typing(
    state: &AppState,
    request: &CaptureTypeRequest,
) -> anyhow::Result<SensorTyping> {
    let prompt = format!(
        "You type one engineer's note into durable software-project knowledge. Use only what the note and the listed facts state; do not invent.\n\nObjective of the approved plan: {}\nFiles changed: {}\nChecks: {}\n\nNote:\n{}\n\nReturn up to {MAX_SENSOR_PROPOSALS} proposals. Kinds: ArchitecturalDecision (a choice and why), Lesson (a non-obvious gotcha), Constraint (a hard rule), Requirement (a need), Pattern (a deliberate recurring approach), AntiPattern (something to avoid). Titles are short names under 100 characters; descriptions state the claim in one or two sentences. Return an empty list when the note carries no durable claim.",
        request.plan_summary.trim(),
        if request.changed_files.is_empty() { "none".to_string() } else { request.changed_files.join(", ") },
        request
            .check_history
            .iter()
            .map(|c| format!("{} {}", c.command, if c.success { "passed" } else { "failed" }))
            .collect::<Vec<_>>()
            .join("; "),
        request.note.trim(),
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["proposals", "reason"],
        "properties": {
            "proposals": {
                "type": "array",
                "maxItems": MAX_SENSOR_PROPOSALS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "title", "description"],
                    "properties": {
                        "kind": {"type": "string", "enum": RECORD_KINDS},
                        "title": {"type": "string", "minLength": 1, "maxLength": 200},
                        "description": {"type": "string", "minLength": 1, "maxLength": 2000}
                    }
                }
            },
            "reason": {"type": "string", "maxLength": 1000}
        }
    });
    let text = state
        .llm
        .chat_completion_json_schema_checked(
            &state.model,
            &prompt,
            None,
            "harness_capture_typing",
            schema,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(serde_json::from_str(&text)?)
}

async fn sensor_tiebreak(
    state: &AppState,
    proposal: &KnowledgeProposal,
    candidate_title: Option<&str>,
) -> Option<String> {
    let candidate_title = candidate_title?;
    let prompt = format!(
        "Two software-project claims of the same kind.\nA (new): \"{}\" — {}\nB (existing): \"{candidate_title}\"\n\nIs A the same claim as B, a narrower special case of B, or a different claim? Answer with exactly one word: same, narrower, or different.",
        proposal.title, proposal.description
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string", "enum": ["same", "narrower", "different"]}}
    });
    let text = state
        .llm
        .chat_completion_json_schema_checked(
            &state.model,
            &prompt,
            None,
            "harness_reconcile_tiebreak",
            schema,
        )
        .await
        .ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["verdict"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{claim_title, declares_nothing, derive_relation, derive_relations};
    use crate::harness::protocol::KnowledgeProposal;

    #[test]
    fn a_note_declares_nothing_only_when_it_opens_with_the_phrase() {
        assert!(declares_nothing(""));
        assert!(declares_nothing("  \n"));
        assert!(declares_nothing("nothing beyond the diff"));
        assert!(declares_nothing("Nothing beyond the diff."));
        assert!(declares_nothing(
            "\"Nothing beyond the diff\"; the setup follows standard conventions."
        ));
        // A claim that merely ends with the phrase is still a claim.
        assert!(!declares_nothing(
            "I decided to split the crates to keep the engine free of UI types. Nothing beyond the diff."
        ));
        assert!(!declares_nothing("Nothing else surprised me."));
    }

    #[test]
    fn the_title_is_the_first_sentence_without_its_decision_opener() {
        assert_eq!(
            claim_title(
                "I decided to split the project into three crates (map, sim, tui) rather than \
                 modules within one crate to keep the engine free of UI types. Nothing else."
            )
            .as_deref(),
            Some(
                "Split the project into three crates (map, sim, tui) rather than modules within \
                 one crate to keep…"
            )
        );
        assert_eq!(
            claim_title("Decision: `.map` files and sqlite are the only crate boundary.\nWhy: …")
                .as_deref(),
            Some("`.map` files and sqlite are the only crate boundary")
        );
        assert_eq!(
            claim_title("v1.0 keeps the parser strict! Anything else is a bump.").as_deref(),
            Some("V1.0 keeps the parser strict")
        );
        assert_eq!(claim_title("   "), None);
    }

    /// A title is a name, so an over-long one is cut where a phrase ends.
    /// badciv-map captured "…to bridge the gap be…", which names nothing.
    #[test]
    fn an_over_long_title_is_cut_at_a_phrase_not_mid_word() {
        let long = claim_title(
            "The `From<String> for MapError` implementation is a deliberate design choice to \
             bridge the gap between internal parsing helpers and the public API.",
        )
        .unwrap();
        assert_eq!(
            long,
            "The `From<String> for MapError` implementation is a deliberate design choice to \
             bridge the gap…"
        );
        assert!(long.chars().count() <= super::MAX_TITLE_CHARS);

        // A clause boundary past the halfway point wins over the word boundary,
        // and its punctuation is not left dangling before the ellipsis.
        let clause = super::cap_title(
            "The parser accepts unknown header keys and ignores them, so a typo in an optional \
             key never fails an otherwise valid load",
        );
        assert_eq!(
            clause,
            "The parser accepts unknown header keys and ignores them…"
        );

        // An early comma does not halve the title; the word boundary wins.
        let early = super::cap_title(
            "Parsing, validation and writing stay in one crate, because the consumer ingests \
             files the same way it ingests handmade ones",
        );
        assert_eq!(
            early,
            "Parsing, validation and writing stay in one crate, because the consumer ingests \
             files the same way…"
        );

        // A title already within the cap is untouched.
        assert_eq!(super::cap_title("A short title"), "A short title");
    }

    const REQ: &str = "https://moosedev.dev/kg/Requirement/r1";
    const REQ2: &str = "https://moosedev.dev/kg/Requirement/r2";
    const AD: &str = "https://moosedev.dev/kg/ArchitecturalDecision/a1";
    const LESSON: &str = "https://moosedev.dev/kg/Lesson/l1";

    /// Kinds keyed by IRI; anything absent stands for a record that is not
    /// current ratified knowledge and is therefore not a candidate.
    fn kinds(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: Vec<(String, String)> = pairs
            .iter()
            .map(|(iri, kind)| ((*iri).to_string(), (*kind).to_string()))
            .collect();
        move |iri: &str| {
            map.iter()
                .find(|(key, _)| key == iri)
                .map(|(_, kind)| kind.clone())
        }
    }

    fn proposal(kind: &str) -> KnowledgeProposal {
        KnowledgeProposal {
            kind: kind.into(),
            title: "t".into(),
            description: "d".into(),
            evidence: vec![],
            files: vec![],
            components: vec![],
            requirement: None,
            supersedes: None,
            retracts: None,
            learned_from: None,
            reconciled: vec![],
        }
    }

    /// One legal candidate is not a judgement call: the human approved the plan
    /// against that obligation.
    #[test]
    fn exactly_one_legal_obligation_is_asserted() {
        let derived = derive_relation(
            "isMotivatedBy",
            &["Requirement", "Constraint"],
            &[REQ.into(), AD.into()],
            kinds(&[(REQ, "Requirement"), (AD, "ArchitecturalDecision")]),
        );
        assert_eq!(derived.chosen.as_deref(), Some(REQ));
        assert_eq!(derived.reason, "asserted");
        assert_eq!(
            derived.candidates_considered, 1,
            "the AD is not a legal target"
        );
    }

    /// Two is a judgement call, so nothing is drawn — the record keeps its SHACL
    /// warning for `under_linked_records` and `suggest_links` to surface later.
    #[test]
    fn two_legal_obligations_are_ambiguous_and_draw_nothing() {
        let derived = derive_relation(
            "isMotivatedBy",
            &["Requirement", "Constraint"],
            &[REQ.into(), REQ2.into()],
            kinds(&[(REQ, "Requirement"), (REQ2, "Requirement")]),
        );
        assert_eq!(derived.chosen, None);
        assert_eq!(derived.reason, "ambiguous");
        assert_eq!(derived.candidates_considered, 2, "both are journaled");
    }

    #[test]
    fn no_legal_obligation_draws_nothing() {
        let derived = derive_relation(
            "isMotivatedBy",
            &["Requirement", "Constraint"],
            &[AD.into()],
            kinds(&[(AD, "ArchitecturalDecision")]),
        );
        assert_eq!(derived.chosen, None);
        assert_eq!(derived.reason, "none_legal");
        assert_eq!(derived.candidates_considered, 0);
    }

    /// An obligation the daemon cannot resolve as current ratified knowledge is
    /// skipped: runner-supplied IRIs are re-validated, never trusted.
    #[test]
    fn an_unresolvable_obligation_is_not_a_candidate() {
        let derived = derive_relation(
            "isMotivatedBy",
            &["Requirement", "Constraint"],
            &[
                REQ.into(),
                "https://moosedev.dev/kg/Requirement/gone".into(),
            ],
            kinds(&[(REQ, "Requirement")]),
        );
        assert_eq!(derived.chosen.as_deref(), Some(REQ), "{derived:?}");
    }

    /// The ontology range for learnedFrom excludes Requirement and Constraint;
    /// choosing one would fail SHACL and take the whole capture down with it.
    #[test]
    fn a_requirement_is_never_a_learned_from_source() {
        let mut lesson = proposal("Lesson");
        let derived = derive_relations(&mut lesson, &[REQ.into(), AD.into()], |iri| {
            kinds(&[(REQ, "Requirement"), (AD, "ArchitecturalDecision")])(iri)
        });
        assert_eq!(lesson.learned_from.as_deref(), Some(AD));
        assert_eq!(derived[0].predicate, "learnedFrom");
        assert!(
            lesson.requirement.is_none(),
            "a Lesson takes no isMotivatedBy"
        );
    }

    /// A plan over ungoverned files, or a runner older than the obligations
    /// field, must journal nothing rather than a misleading `none_legal`.
    #[test]
    fn no_obligations_at_all_journals_nothing() {
        let mut decision = proposal("ArchitecturalDecision");
        let derived = derive_relations(&mut decision, &[], |_| None);
        assert!(derived.is_empty(), "{derived:?}");
        assert!(decision.requirement.is_none());
    }

    #[test]
    fn a_kind_with_no_derivation_draws_nothing() {
        let mut constraint = proposal("Constraint");
        let derived = derive_relations(&mut constraint, &[REQ.into()], |iri| {
            kinds(&[(REQ, "Requirement")])(iri)
        });
        assert!(derived.is_empty());
        assert!(constraint.requirement.is_none() && constraint.learned_from.is_none());
    }

    /// James's ruling as a test: a derived relation must never gate a task.
    /// Both predicates travel the ordinary relation path, so neither may reach
    /// `changes_lifecycle` — only `supersedes`/`retracts` do, and derivation
    /// never sets them.
    #[test]
    fn derived_relations_never_make_a_proposal_governing() {
        let mut decision = proposal("ArchitecturalDecision");
        decision.requirement = Some(REQ.into());
        decision.learned_from = Some(AD.into());
        assert!(!decision.changes_lifecycle());
        assert!(!decision.is_governing());

        let mut lesson = proposal("Lesson");
        lesson.learned_from = Some(AD.into());
        assert!(!lesson.is_governing());

        // The contrast: a lifecycle field is what governs, and derivation never
        // sets one.
        let mut superseding = proposal("ArchitecturalDecision");
        superseding.supersedes = Some(LESSON.into());
        assert!(
            superseding.is_governing(),
            "guard still works for real lifecycle change"
        );
    }
}
