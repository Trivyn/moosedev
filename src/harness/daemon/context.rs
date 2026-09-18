//! Context for the runner: the governing knowledge of the files it reads,
//! the accepted-revision fingerprint every later operation is pinned to, and
//! the current record inventory the link path chooses from (listed in the
//! context only while the walk supplies no rules or linked evidence).
use super::*;
use crate::harness::digest::sha256_hex;

pub async fn context(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ContextRequest>,
) -> Result<Json<ContextResponse>, ApiError> {
    Ok(Json(context_snapshot(&state, &request)?))
}

/// Bytes one prompt may spend on record claims, tunable so the floor study can sweep it.
/// The default leaves room for the task, plan and history in a 64k window once record
/// lines (the inventory, never bounded) are paid for.
fn claim_budget() -> usize {
    std::env::var("MOOSEDEV_HARNESS_CLAIM_BUDGET")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(12_000)
}

pub fn context_snapshot(
    state: &AppState,
    request: &ContextRequest,
) -> anyhow::Result<ContextResponse> {
    let generation = state.project_write_generation();
    anyhow::ensure!(!request.topic.trim().is_empty(), "context topic is empty");
    anyhow::ensure!(
        request.files.len() <= 100,
        "at most 100 files per context request"
    );
    anyhow::ensure!(
        !request.evidence_only || request.files.is_empty(),
        "an evidence-only context request takes no files"
    );
    state.try_ensure_enriched()?;
    let mut context = String::new();
    let mut governing_constraints = Vec::new();
    let evidence_iris = if request.evidence_only {
        // An evidence-only request (the model's search) returns just the
        // topic's records with their complete claims.
        let records = graph::relevant_context_snapshot(state, Some(&request.topic), 12, false)?;
        render_topic_records(state, &mut context, &records);
        records.into_iter().map(|record| record.iri).collect()
    } else {
        // Linked evidence leads (AD 85da8700): the walk from the files' code
        // replaces similarity-ranked topic recall, which remains only as a
        // fallback when nothing is linked beyond what the dossiers print.
        let linked = graph::linked_evidence(state, &request.files)?;
        governing_constraints = graph::governing_constraints(&linked)
            .into_iter()
            .map(|rule| GoverningConstraint {
                via: rule.hop.via(&rule.source),
                iri: rule.iri,
                label: rule.label,
                claim: rule.claim,
            })
            .collect();
        // The record-name inventory stays only while the walk supplies no
        // rules and no linked evidence, as on the first request with no files.
        if linked.records.is_empty() && governing_constraints.is_empty() {
            context.push_str(&format!("Recall: the inventory lists current record names only; {RECALL}\n\nCurrent knowledge inventory:\n"));
            for record in graph::relevant_context_snapshot(state, None, 100, false)? {
                context.push_str(&format!(
                    "[{}] {} ({})\n",
                    record.kind, record.label, record.iri
                ));
            }
        } else {
            context.push_str(&format!("Recall: {RECALL}\n"));
        }
        if linked.records.is_empty() {
            let fallback: Vec<_> =
                graph::relevant_context_snapshot(state, Some(&request.topic), 5, false)?
                    .into_iter()
                    .filter(|record| !linked.excluded.contains(&record.iri))
                    .collect();
            context.push_str("\nTopic evidence (fallback; nothing is linked beyond the file dossiers; complete claims; up to six relationships per record):\n");
            render_topic_records(state, &mut context, &fallback);
        } else {
            context.push_str("\nLinked evidence (records linked to the files' code and components; complete claims):\n");
            context.push_str(&graph::render_linked_evidence(&graph::with_rule_pointers(
                &linked.records,
            )));
        }
        Vec::new()
    };
    let root = state.project_root();
    let mut files = Vec::new();
    // One dedup state for the whole prompt, not one per file: these dossiers are
    // rendered separately but reach the model together, so a record linked from
    // several files should show its claim body once, as it already does within a
    // single file's render.
    // …and one claim budget for the whole prompt (task #43). The file dossiers were the
    // only unbounded section of the push: file_entity_iris returns EVERY definition in a
    // file, direct_records is capped nowhere, and this path alone passed no byte bound —
    // one file measured 86,693 bytes. Because model.rs counts dossiers as MANDATORY, that
    // growth evicted history, observations and navigation before overflowing the window,
    // which is the study's dominant harness-specific failure (Lesson af16b95e).
    //
    // The budget binds CLAIMS only, never record lines. The line carries kind, title,
    // lifecycle status, timestamp and linking predicate, so set-completeness, negation and
    // currency all survive it intact; the claim is prose, roughly 1,000 bytes against 150,
    // and retrievable on demand. Bounding the inventory instead would shrink exactly what
    // the harness exists to deliver. AD 21855a2a allows a byte bound with an explicit
    // notice, which the response carries below; the bound is computed here in the daemon,
    // so no surface grows its own policy (Constraint 2ba76439).
    let mut shown = graph::ShownInPush::with_claim_budget(claim_budget());
    for file in &request.files {
        validate_path(file)?;
        // Records are never dropped; only their claims are bounded, and the push says so.
        let dossier = graph::harness_file_dossier(state, file, &mut shown)?.unwrap_or_else(|| {
            "No recorded entity knowledge is linked to this file. Topic recall still applies."
                .into()
        });
        let policy = policy::evaluate(
            state,
            &root,
            &PolicyEvent::EditProposed {
                file: file.clone(),
                line: None,
                col: None,
                anchor: None,
            },
        )?;
        files.push(FileContext {
            file: file.clone(),
            dossier,
            policy,
        });
    }
    // The explicit notice AD 21855a2a requires. Without it a shorter dossier reads as a
    // smaller graph — the same misreading that had two models report an empty graph.
    if shown.claims_withheld() > 0 {
        context.push_str(&format!(
            "\n\n{} record claim(s) withheld by the {}-byte push bound ({}). Every record above \
             is still listed with its kind, title and lifecycle status; retrieve any claim in full \
             with get_entity_dossier.\n",
            shown.claims_withheld(),
            claim_budget(),
            shown.claims_withheld_by_kind()
        ));
    }
    let revision = accepted_revision(state)?;
    anyhow::ensure!(
        generation == state.project_write_generation(),
        "project knowledge changed while assembling context; retry retrieval"
    );
    Ok(ContextResponse {
        project_root: root.to_string_lossy().into_owned(),
        revision,
        context,
        files,
        evidence_iris,
        capture_contracts: vec![2, 3],
        intent_contracts: vec![2],
        governing_constraints,
    })
}

/// How recall reaches claims, after the inventory sentence when it is listed.
const RECALL: &str = "search with words from a record name returns its complete claims. A structural walk from the attached files' code and components supplies the linked evidence; topic recall appears only when nothing is linked beyond the file dossiers. Attached file dossiers carry the complete claims of records linked to the file's code, and governing Constraints appear with their claims under Project rules.";

/// Topic recall records: a header per record, then its harness-style claim body.
fn render_topic_records(state: &AppState, context: &mut String, records: &[graph::ContextItem]) {
    for record in records {
        context.push_str(&format!(
            "\n[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
        graph::render_styled_claim_body(state, record, graph::ClaimStyle::Harness, context);
    }
}

/// The current knowledge records offered to the link path: the bounded
/// inventory plus the topic recall for change obligations.
pub(super) fn current_record_targets(state: &AppState) -> anyhow::Result<Vec<CaptureTarget>> {
    let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
    let topical =
        graph::relevant_context_snapshot(state, Some("change purpose obligations"), 12, false)?;
    let mut targets: Vec<CaptureTarget> = Vec::new();
    for record in inventory.iter().chain(&topical) {
        if is_record_kind(&record.kind)
            && current_status(state, &record.iri)
                .is_some_and(|status| graph::in_working_set(&status))
            && !targets.iter().any(|target| target.iri == record.iri)
        {
            targets.push(CaptureTarget {
                iri: record.iri.clone(),
                label: record.label.clone(),
                kind: record.kind.clone(),
            });
        }
    }
    Ok(targets)
}

/// Ignore unratified subjects AND inferred incoming links to those subjects.
/// A proposed capture must not invalidate the approval that led to its creation.
pub fn accepted_revision(state: &AppState) -> anyhow::Result<String> {
    accepted_revision_excluding(state, &HashSet::new(), &HashSet::new())
}

/// The accepted revision ignoring `masked` subjects (with the quads that
/// reference them) and the exact `own_quads` a review wrote onto records that
/// existed before it.
pub(super) fn accepted_revision_excluding(
    state: &AppState,
    masked: &HashSet<String>,
    own_quads: &HashSet<String>,
) -> anyhow::Result<String> {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let quads = state
        .store
        .quads_for_pattern(None, None, None, Some(graph))
        .collect::<Result<Vec<_>, _>>()?;
    let mut excluded: HashSet<String> = quads
        .iter()
        .filter_map(|q| {
            if q.predicate.as_str() != state.capture.status {
                return None;
            }
            match &q.object {
                Term::Literal(status) if !graph::in_working_set(status.value()) => {
                    Some(q.subject.to_string())
                }
                _ => None,
            }
        })
        .collect();
    excluded.extend(masked.iter().cloned());
    let mut canonical: Vec<String> = quads
        .iter()
        .filter(|q| {
            !excluded.contains(&q.subject.to_string()) && !excluded.contains(&q.object.to_string())
        })
        .map(ToString::to_string)
        .filter(|quad| !own_quads.contains(quad))
        .collect();
    canonical.sort();
    Ok(sha256_hex(canonical.join("\n")))
}
