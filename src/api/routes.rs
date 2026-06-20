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
        .route("/sparql/query", post(handlers::query));

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
