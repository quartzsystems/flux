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

pub mod analytics;
pub mod auth;
pub mod debug;
pub mod error;
pub mod extract;
pub mod flows;
pub mod middleware;
pub mod openapi;
pub mod pcap;
pub mod port_groups;
pub mod ports;
pub mod profiles;
pub mod report;
pub mod runs;
pub mod settings;
pub mod spa;
pub mod system;
pub mod tests;
pub mod topology;
pub mod users;
pub mod ws;

/// Builds the complete application router.
pub fn router(state: AppState) -> Router {
    let web_root = state.config.web_root.clone();
    let mocked = state.config.engine == crate::config::EngineBackend::Mock;

    let mut api = Router::new()
        .nest("/auth", auth::router())
        .nest("/ports", ports::router())
        .nest("/port-groups", port_groups::router())
        .nest("/flows", flows::router())
        .nest("/load-profiles", profiles::router())
        .nest("/analytics", analytics::router())
        .nest("/tests", tests::router())
        .nest("/runs", runs::router())
        .nest("/users", users::router())
        .nest("/system", system::router())
        .nest("/settings", settings::router())
        .nest("/topology", topology::router())
        .nest("/stream", ws::router())
        .route("/openapi.json", get(openapi::document));

    // The debug router is not mounted at all on a real appliance, so an
    // operator cannot discover endpoints that would only ever refuse them.
    if mocked {
        tracing::warn!("mounting /api/v1/debug — simulated engine controls are reachable");
        api = api.nest("/debug", debug::router());
    }

    // Anything under /api/v1 that does not match is a client error worth
    // naming, not a request for the UI shell.
    let api = api.fallback(unknown_endpoint);

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
