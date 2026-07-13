use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::api::handlers;
use crate::graph::AppState;

pub fn build_routes(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/health", get(handlers::health))
        .route("/chat", post(handlers::chat))
        .route("/chat/sessions", get(handlers::list_sessions))
        .route(
            "/chat/sessions/{session_id}",
            get(handlers::get_session).delete(handlers::delete_session),
        )
        .route("/sparql/query", post(handlers::query))
        .route("/graph/export", get(handlers::export_graph))
        .route("/graph/import", post(handlers::import_graph))
        .route("/adrs", get(handlers::list_adrs))
        .route("/adrs/archive.zip", get(handlers::download_adr_archive))
        .route("/adrs/{num}", get(handlers::get_adr))
        .route("/records/{uuid}", get(handlers::get_record))
        .route("/constraints", get(handlers::list_constraints))
        .route(
            "/constraints/archive.zip",
            get(handlers::download_constraint_archive),
        )
        .route("/constraints/{num}", get(handlers::get_constraint))
        .route("/requirements", get(handlers::list_requirements))
        .route(
            "/requirements/archive.zip",
            get(handlers::download_requirement_archive),
        )
        .route("/requirements/{num}", get(handlers::get_requirement))
        .route("/lessons", get(handlers::list_lessons))
        .route(
            "/lessons/archive.zip",
            get(handlers::download_lesson_archive),
        )
        .route("/lessons/{num}", get(handlers::get_lesson))
        .route("/debt", get(handlers::why_coverage))
        .route("/policy", post(handlers::evaluate_policy))
        .route("/capture", post(handlers::capture_decision_point))
        .route("/proposals", get(handlers::list_proposals))
        .route("/proposals/{id}/accept", post(handlers::accept_proposal))
        .route("/proposals/{id}/reject", post(handlers::reject_proposal));

    Router::new()
        .nest("/api/v1", api)
        .fallback(handlers::static_files::serve_static)
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
}
