//! Run history and control.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::types::{Id, RunState};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json, OperatorAuth};
use crate::state::AppState;
use crate::store::models::{Run, RunResult};
use crate::store::runs;

/// Largest page a client may ask for.
///
/// An appliance accumulates runs indefinitely; without a ceiling a forgotten
/// `?limit=1000000` would try to serialise the entire history into one response.
const MAX_LIMIT: i64 = 200;

/// Default page size.
const DEFAULT_LIMIT: i64 = 50;

/// Mounts the run routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/{id}", get(get_one))
        .route("/{id}/stop", post(stop))
        .route("/{id}/report", get(report))
}

/// Renders a run as a self-contained printable HTML document.
///
/// Served as a document rather than as JSON because it is an artefact people
/// archive and print. `Content-Disposition: inline` so a browser shows it;
/// the filename is there for whoever saves it.
async fn report(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse;

    let run = runs::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("run {id}")))?;
    let results = runs::results(state.store.pool(), id).await?;

    let input = super::report::ReportInput {
        run: &run,
        results: &results,
        version: super::system::VERSION,
    };

    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", input.filename())),
        ],
        input.render(),
    )
        .into_response())
}

/// Filters and pagination for the history.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Only runs in this state.
    #[serde(default)]
    pub state: Option<RunState>,
    /// Only runs of this test.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = Uuid)]
    pub test_id: Option<Id>,
    /// How many to return.
    #[serde(default)]
    pub limit: Option<i64>,
    /// How many to skip.
    #[serde(default)]
    pub offset: Option<i64>,
}

/// One page of run history.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunPage {
    /// The runs, newest first.
    pub runs: Vec<Run>,
    /// How many match the filter in total.
    pub total: i64,
    /// The page size used.
    pub limit: i64,
    /// The offset used.
    pub offset: i64,
}

/// Run history, newest first.
async fn list(
    State(state): State<AppState>,
    _auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<RunPage>> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = query.offset.unwrap_or(0).max(0);

    let runs = runs::list(state.store.pool(), query.state, query.test_id, limit, offset).await?;
    let total = runs::count(state.store.pool(), query.state, query.test_id).await?;

    Ok(Json(RunPage { runs, total, limit, offset }))
}

/// A run with its trials.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    /// The run itself.
    #[serde(flatten)]
    pub run: Run,
    /// Every trial recorded, in order.
    pub results: Vec<RunResult>,
    /// Whether the run is still in flight and can be stopped.
    pub stoppable: bool,
}

/// One run with its results.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<RunDetail>> {
    let run = runs::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("run {id}")))?;

    let results = runs::results(state.store.pool(), id).await?;
    let stoppable = state.runs.get(id).await.is_some();

    Ok(Json(RunDetail { run, results, stoppable }))
}

/// Stops an in-flight run.
///
/// Returns immediately; the run task unwinds on its own, stopping traffic and
/// releasing ports before it records the outcome. A client watching the
/// WebSocket sees the transition when it actually happens.
#[tracing::instrument(skip(state), fields(run_id = %id))]
async fn stop(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    if state.runs.stop(id).await {
        tracing::info!(actor = %actor.username, "run stop requested");
        return Ok(Json(serde_json::json!({ "stopping": true })));
    }

    // Distinguish "already finished" from "never existed": one is a stale UI,
    // the other is a bad link.
    match runs::get(state.store.pool(), id).await? {
        Some(run) => Err(ApiError::Conflict(format!("run is already {}", run.state))),
        None => Err(ApiError::NotFound(format!("run {id}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies the same clamping the handler does.
    fn clamp(limit: Option<i64>) -> i64 {
        limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    #[test]
    fn page_size_is_bounded_in_both_directions() {
        // Without a ceiling, one forgotten query serialises the whole history.
        assert_eq!(clamp(None), DEFAULT_LIMIT);
        assert_eq!(clamp(Some(10)), 10);
        assert_eq!(clamp(Some(1_000_000)), MAX_LIMIT);
        assert_eq!(clamp(Some(0)), 1);
        assert_eq!(clamp(Some(-5)), 1);
    }

    /// Applies the same offset clamping the handler does.
    fn offset(raw: Option<i64>) -> i64 {
        raw.unwrap_or(0).max(0)
    }

    #[test]
    fn a_negative_offset_is_treated_as_the_first_page() {
        assert_eq!(offset(None), 0);
        assert_eq!(offset(Some(-10)), 0);
        assert_eq!(offset(Some(100)), 100);
    }

    #[test]
    fn the_state_filter_parses_from_a_query_string() {
        let query: ListQuery = serde_urlencoded::from_str("state=running").unwrap();
        assert_eq!(query.state, Some(RunState::Running));

        let empty: ListQuery = serde_urlencoded::from_str("").unwrap();
        assert!(empty.state.is_none());
    }
}
