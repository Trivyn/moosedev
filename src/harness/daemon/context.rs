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
    state.try_ensure_enriched()?;
    let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
    let records = graph::relevant_context_snapshot(state, Some(&request.topic), 12, false)?;
    let mut context = String::from("Recall: get_relevant_context(no topic, limit=100) inventory, then topic recall (limit=12).\nThe broad inventory is bounded and contains names only; retrieve more context when scope expands. Attached file dossiers remain complete.\n\nCurrent knowledge inventory:\n");
    for record in inventory {
        context.push_str(&format!(
            "[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
    }
    context.push_str("\nTopic evidence (complete claims; up to six relationships per record):\n");
    for record in records {
        context.push_str(&format!(
            "\n[{}] {} ({})\n",
            record.kind, record.label, record.iri
        ));
        for property in record.properties.iter().filter(|property| {
            property.is_literal
                && !matches!(
                    property.predicate.as_str(),
                    "hasTitle" | "label" | "hasTimestamp" | "hasAuthor" | "hasLifecycleStatus"
                )
        }) {
            context.push_str(&format!("{}: {}\n", property.predicate, property.value));
        }
        let mut links: Vec<_> = record
            .properties
            .iter()
            .filter(|property| !property.is_literal)
            .collect();
        links.sort_by(|a, b| {
            graph::edge_priority(&a.predicate)
                .cmp(&graph::edge_priority(&b.predicate))
                .then_with(|| a.predicate.cmp(&b.predicate))
                .then_with(|| a.value.cmp(&b.value))
        });
        for link in links.iter().take(6) {
            context.push_str(&format!("{}: {}\n", link.predicate, link.value));
        }
        if links.len() > 6 {
            context.push_str(&format!(
                "{} further relationships omitted; retrieve them if relevant.\n",
                links.len() - 6
            ));
        }
    }
    let root = state.project_root();
    let mut files = Vec::new();
    for file in &request.files {
        validate_path(file)?;
        let push = policy::evaluate(
            state,
            &root,
            &PolicyEvent::EntityTouched {
                file: file.clone(),
                line: None,
                col: None,
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
        capture_contracts: vec![2],
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
