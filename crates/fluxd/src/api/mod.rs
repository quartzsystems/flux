//! The HTTP surface: router assembly, middleware wiring, and handlers.
//!
//! One `Router` serves both the REST API under `/api/v1` and the exported UI at
//! `/`. Authentication runs once for every request including static ones, which
//! costs a session lookup on asset requests but means there is exactly one place
//! that decides who a request is from.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub mod auth;
pub mod error;
pub mod extract;
pub mod middleware;
pub mod openapi;
pub mod port_groups;
pub mod ports;
pub mod spa;
pub mod system;
pub mod users;

/// Builds the complete application router.
pub fn router(state: AppState) -> Router {
    let web_root = state.config.web_root.clone();

    let api = Router::new()
        .nest("/auth", auth::router())
        .nest("/ports", ports::router())
        .nest("/port-groups", port_groups::router())
        .nest("/users", users::router())
        .nest("/system", system::router())
        .nest("/settings", system::settings_router())
        .route("/openapi.json", get(openapi::document))
        // Anything under /api/v1 that does not match is a client error worth
        // naming, not a request for the UI shell.
        .fallback(unknown_endpoint);

    Router::new()
        .nest("/api/v1", api)
        .fallback_service(spa::service(web_root))
        .layer(axum::middleware::from_fn_with_state(state.clone(), middleware::authenticate))
        .layer(middleware::security_headers())
        // Compression sits outside the tracing layer so the log records the
        // handler's status, not the compressor's view of it.
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .with_state(state)
}

/// Response for an unrecognised `/api/v1` path.
async fn unknown_endpoint(uri: axum::http::Uri) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "code": "not_found",
            "message": format!("no API endpoint at {}", uri.path()),
        })),
    )
}
