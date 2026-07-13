use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use crate::api::error::ApiError;
use crate::api::models::{ComponentCoverageDto, WhyCoverageResponse};
use crate::graph::{compute_why_coverage, AppState};

/// `GET /api/v1/debt` — per-component why-coverage (documented fraction of the
/// public code surface). Thin adapter over `graph::compute_why_coverage`.
pub async fn why_coverage(
    State(state): State<Arc<AppState>>,
) -> Result<Json<WhyCoverageResponse>, ApiError> {
    let report = compute_why_coverage(&state)?;
    let components = report
        .components
        .into_iter()
        .map(|c| {
            let coverage = c.ratio();
            ComponentCoverageDto {
                iri: c.iri,
                name: c.name,
                numerator: c.numerator,
                denominator: c.denominator,
                coverage,
                undocumented: c.undocumented,
            }
        })
        .collect();
    Ok(Json(WhyCoverageResponse {
        components,
        unmapped: report.unmapped,
    }))
}
