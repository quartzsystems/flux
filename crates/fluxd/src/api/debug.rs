//! Debug endpoints for the simulated engine.
//!
//! These exist so the whole pipeline — orchestrator, collector, WebSocket,
//! charts, and eventually the RFC 2544 search — can be exercised against
//! conditions that are hard to arrange with real hardware. Injecting exactly
//! 0.7% loss on demand is how the binary search gets tested without a device
//! under test that happens to fail at that rate.
//!
//! Every route here refuses unless the engine backend is `mock`, and every one
//! requires an administrator. On a real appliance the whole router answers 404.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::Router;
use flux_core::engine::EnginePortId;
use flux_core::types::Id;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Json};
use crate::config::EngineBackend;
use crate::engine::mock::MockControls;
use crate::state::AppState;

/// Mounts the debug routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/engines/{group_id}/loss", post(set_loss))
        .route("/engines/{group_id}/latency", post(set_latency))
        .route("/engines/{group_id}/link", post(set_link))
}

/// How much traffic to drop.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LossRequest {
    /// Percentage of transmitted frames that will not arrive, 0 to 100.
    pub loss_pct: f64,
}

/// The knob settings after a change.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DebugState {
    /// Loss currently injected.
    pub loss_pct: f64,
}

/// Sets the loss a simulated engine injects.
#[tracing::instrument(skip(state), fields(%group_id, loss = body.loss_pct))]
async fn set_loss(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(group_id): Path<Id>,
    Json(body): Json<LossRequest>,
) -> ApiResult<Json<DebugState>> {
    if !(0.0..=100.0).contains(&body.loss_pct) {
        return Err(ApiError::field("lossPct", "must be between 0 and 100"));
    }

    let controls = mock_controls(&state, group_id).await?;
    controls.set_loss_pct(body.loss_pct);

    tracing::warn!(actor = %actor.username, "injected packet loss into a simulated engine");
    Ok(Json(DebugState { loss_pct: controls.loss_pct() }))
}

/// The latency distribution to simulate.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LatencyRequest {
    /// Median one-way latency in microseconds.
    pub median_us: f64,
    /// Shape parameter of the log-normal distribution.
    ///
    /// Larger values lengthen the tail. Around 0.35 looks like a lightly loaded
    /// cut-through switch; above 1.0 looks like something in trouble.
    pub sigma: f64,
}

/// Sets the latency distribution a simulated engine reports.
#[tracing::instrument(skip(state), fields(%group_id))]
async fn set_latency(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(group_id): Path<Id>,
    Json(body): Json<LatencyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if !body.median_us.is_finite() || body.median_us < 0.0 {
        return Err(ApiError::field("medianUs", "must be a non-negative number"));
    }
    if !body.sigma.is_finite() || !(0.0..=3.0).contains(&body.sigma) {
        return Err(ApiError::field("sigma", "must be between 0 and 3"));
    }

    mock_controls(&state, group_id).await?.set_latency(body.median_us, body.sigma);
    tracing::warn!(actor = %actor.username, "changed a simulated engine's latency distribution");

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Which port to change and how.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequest {
    /// Port index within the engine instance.
    pub port: u8,
    /// True to force the link down.
    pub down: bool,
}

/// Forces a simulated port's carrier state.
#[tracing::instrument(skip(state), fields(%group_id, port = body.port, down = body.down))]
async fn set_link(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(group_id): Path<Id>,
    Json(body): Json<LinkRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    mock_controls(&state, group_id).await?.set_link_down(EnginePortId(body.port), body.down);
    tracing::warn!(actor = %actor.username, "forced a simulated port's link state");

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Finds the mock knobs for a group, refusing on a real engine.
async fn mock_controls(state: &AppState, group_id: Id) -> ApiResult<MockControls> {
    if state.config.engine != EngineBackend::Mock {
        return Err(ApiError::Conflict(
            "debug controls are only available when FLUX_ENGINE=mock".into(),
        ));
    }

    // `ApiError::NotFound` renders as "{0} not found", so this is a noun phrase
    // rather than a sentence.
    state.mock_controls.read().await.get(&group_id).cloned().ok_or_else(|| {
        ApiError::NotFound(format!("a running simulated engine for port group {group_id}"))
    })
}
