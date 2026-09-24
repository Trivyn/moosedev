//! Checkpoints: the durable, conforming state of an operation's proposals and
//! the accepted revision the runner compares its approval against.
use super::*;

#[derive(Deserialize)]
pub struct CheckpointQuery {
    pub operation_id: Option<String>,
}

pub async fn checkpoint(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    let _guard = lock_operations()?;
    // A legacy browser can issue an Origin-less GET without Fetch Metadata.
    // Reading status must never enrich the graph or publish the canonical file.
    Ok(Json(checkpoint_status(
        &state,
        query.operation_id.as_deref(),
        false,
    )?))
}

pub async fn publish_checkpoint(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<CheckpointResponse>, ApiError> {
    let _guard = lock_operations()?;
    Ok(Json(checkpoint_snapshot(
        &state,
        query.operation_id.as_deref(),
    )?))
}

pub fn checkpoint_snapshot(
    state: &AppState,
    operation_id: Option<&str>,
) -> anyhow::Result<CheckpointResponse> {
    checkpoint_status(state, operation_id, true)
}

fn checkpoint_status(
    state: &AppState,
    operation_id: Option<&str>,
    publish: bool,
) -> anyhow::Result<CheckpointResponse> {
    let generation = state.project_write_generation();
    let mut pending = BTreeSet::new();
    if let Some(id) = operation_id {
        let operation: Operation =
            serde_json::from_slice(&std::fs::read(journal_path(state, id, "json")?)?)?;
        if !operation.captured || !operation.reviewed {
            pending.insert(format!("operation:{id}"));
        }
        for (index, entry) in operation.entries.into_iter().enumerate() {
            // An entry the human rejected within an accept resolves as rejected.
            let review = operation
                .review
                .map(|accepted| accepted && !operation.review_rejected.contains(&index));
            for iri in std::iter::once(entry.response.iri).chain(entry.response.links) {
                // A completed local journal is not proof that the canonical
                // graph still contains its writes (for example after a branch
                // switch). Missing or conflicting records remain obligations.
                let status = current_status(state, &iri);
                let resolved = matches!(
                    (review, status.as_deref()),
                    (Some(true), Some("accepted" | "superseded" | "deprecated"))
                        | (Some(false), Some("rejected"))
                );
                if !resolved {
                    pending.insert(iri);
                }
            }
        }
    }
    if publish {
        state.try_ensure_enriched()?;
    }
    let report = crate::validation::validate_project(state)?;
    if publish {
        durable_flush(state)?;
    }
    let revision = accepted_revision(state)?;
    anyhow::ensure!(
        generation == state.project_write_generation(),
        "project knowledge changed during checkpoint validation; retry checkpoint"
    );
    Ok(CheckpointResponse {
        conforms: report.conforms(),
        // A GET reports current state, not evidence of successful publication.
        durable: publish,
        revision,
        pending: pending.into_iter().collect(),
    })
}
