//! Context for the runner: the governing knowledge of the files it reads,
//! the accepted-revision fingerprint every later operation is pinned to, and
//! the current record inventory the link path chooses from.
use super::*;
use crate::harness::digest::sha256_hex;

pub async fn context(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ContextRequest>,
) -> Result<Json<ContextResponse>, ApiError> {
    Ok(Json(context_snapshot(&state, &request)?))
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
    let records = graph::relevant_context_snapshot(state, Some(&request.topic), 12, false)?;
    // An evidence-only request (the model's search) returns just the topic's
    // records with their complete claims.
    let evidence_iris = if request.evidence_only {
        records.iter().map(|record| record.iri.clone()).collect()
    } else {
        Vec::new()
    };
    let mut context = String::new();
    if !request.evidence_only {
        let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
        context.push_str("Recall: get_relevant_context(no topic, limit=100) inventory, then topic recall (limit=12).\nThe broad inventory is bounded and contains names only; search with words from a record name returns its complete claims. Attached file dossiers carry the complete claims of records linked to the file's code.\n\nCurrent knowledge inventory:\n");
        for record in inventory {
            context.push_str(&format!(
                "[{}] {} ({})\n",
                record.kind, record.label, record.iri
            ));
        }
        context
            .push_str("\nTopic evidence (complete claims; up to six relationships per record):\n");
    }
    for record in records {
        context.push_str(&format!(
            "\n[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
        graph::render_claim_body(&record, &mut context);
    }
    let root = state.project_root();
    let mut files = Vec::new();
    for file in &request.files {
        validate_path(file)?;
        let push = policy::evaluate(
            state,
            &root,
            // No host bound: required context fails rather than truncating.
            &PolicyEvent::EntityTouched {
                file: file.clone(),
                line: None,
                col: None,
                max_bytes: None,
            },
        )?;
        let dossier = match push {
            PolicyDecision::Inject {
                dossier_markdown, ..
            } => dossier_markdown,
            _ => "No recorded entity knowledge is linked to this file. Topic recall still applies."
                .into(),
        };
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
    })
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
    accepted_revision_masked(state, &HashSet::new())
}

pub(super) fn accepted_revision_masked(
    state: &AppState,
    masked: &HashSet<String>,
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
        .collect();
    canonical.sort();
    Ok(sha256_hex(canonical.join("\n")))
}
