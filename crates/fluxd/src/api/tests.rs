//! Test definition endpoints, and starting a run from one.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::types::{Id, TestType};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json, OperatorAuth};
use crate::orch::run::RunError;
use crate::state::AppState;
use crate::store::models::Test;
use crate::store::tests as test_store;
use crate::store::{flows, is_unique_violation};

/// Mounts the test routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(get_one).put(update).delete(delete))
        .route("/{id}/run", post(run))
}

/// Every test.
async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<Test>>> {
    Ok(Json(test_store::list(state.store.pool()).await?))
}

/// One test.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<Test>> {
    test_store::get(state.store.pool(), id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("test {id}")))
}

/// The body for creating or replacing a test.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestInput {
    /// Operator-assigned label.
    pub name: String,
    /// Which kind of test.
    #[serde(rename = "type")]
    pub test_type: TestType,
    /// Type-specific configuration. Empty for a manual test.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Flows this test drives, in order.
    #[schema(value_type = Vec<String>)]
    pub flow_ids: Vec<Id>,
}

impl TestInput {
    /// Checks the request in isolation.
    fn validate(&self) -> ApiResult<()> {
        let mut v = flux_core::config::Validation::new();

        let name = self.name.trim();
        v.require(!name.is_empty(), "name", "must not be empty");
        v.require(name.chars().count() <= 64, "name", "must be at most 64 characters");
        v.require(!self.flow_ids.is_empty(), "flowIds", "a test needs at least one flow");

        let mut seen = std::collections::HashSet::new();
        v.require(
            self.flow_ids.iter().all(|id| seen.insert(*id)),
            "flowIds",
            "a flow may appear only once in a test",
        );

        v.finish()?;
        Ok(())
    }
}

/// Creates a test.
#[tracing::instrument(skip_all, fields(name = %body.name, kind = %body.test_type))]
async fn create(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Json(body): Json<TestInput>,
) -> ApiResult<Json<Test>> {
    body.validate()?;
    check_flows_exist(&state, &body.flow_ids).await?;

    let test = test_store::create(
        state.store.pool(),
        body.name.trim(),
        body.test_type,
        &body.config,
        &body.flow_ids,
        Some(actor.user_id),
    )
    .await
    .map_err(name_conflict)?;

    tracing::info!(actor = %actor.username, test_id = %test.id, "test created");
    Ok(Json(test))
}

/// Replaces a test.
#[tracing::instrument(skip_all, fields(test_id = %id))]
async fn update(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
    Json(body): Json<TestInput>,
) -> ApiResult<Json<Test>> {
    body.validate()?;
    check_flows_exist(&state, &body.flow_ids).await?;

    let test = test_store::update(
        state.store.pool(),
        id,
        body.name.trim(),
        body.test_type,
        &body.config,
        &body.flow_ids,
    )
    .await
    .map_err(name_conflict)?
    .ok_or_else(|| ApiError::NotFound(format!("test {id}")))?;

    tracing::info!(actor = %actor.username, "test updated");
    Ok(Json(test))
}

/// Deletes a test. Its runs survive, holding their own configuration snapshot.
#[tracing::instrument(skip(state), fields(test_id = %id))]
async fn delete(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    if !test_store::delete(state.store.pool(), id).await? {
        return Err(ApiError::NotFound(format!("test {id}")));
    }
    tracing::info!(actor = %actor.username, "test deleted");
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// What starting a run returns.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStarted {
    /// The new run.
    #[schema(value_type = String, format = Uuid)]
    pub run_id: Id,
}

/// Starts a run for a test.
///
/// The body is optional in practice — an operator pressing "run" sends nothing —
/// so it is taken as raw JSON and defaulted rather than as a required extractor
/// that would reject an empty request.
#[tracing::instrument(skip_all, fields(test_id = %id))]
async fn run(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<Json<RunStarted>> {
    let test = test_store::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("test {id}")))?;

    let dut_meta = body
        .and_then(|Json(v)| v.get("dutMeta").cloned())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));

    let run_id = state
        .runs
        .start(&test, Some(actor.user_id), dut_meta)
        .await
        .map_err(map_run_error)?;

    tracing::info!(actor = %actor.username, %run_id, "run started");
    Ok(Json(RunStarted { run_id }))
}

/// Maps a start failure onto the API taxonomy.
fn map_run_error(err: RunError) -> ApiError {
    match err {
        RunError::NotFound(message) => ApiError::NotFound(message),
        // A configuration problem is the operator's to fix, and the message
        // names what is wrong, so it goes back as a field-level error against
        // the test rather than a bare 409.
        RunError::Invalid(message) => ApiError::field("flowIds", message),
        RunError::Conflict(message) => ApiError::Conflict(message),
        RunError::Db(e) => e.into(),
    }
}

/// Rejects a test naming a flow that does not exist.
async fn check_flows_exist(state: &AppState, flow_ids: &[Id]) -> ApiResult<()> {
    let found = flows::get_many(state.store.pool(), flow_ids).await?;
    if found.len() == flow_ids.len() {
        return Ok(());
    }

    let present: std::collections::HashSet<Id> = found.iter().map(|f| f.id).collect();
    let errors = flow_ids
        .iter()
        .enumerate()
        .filter(|(_, id)| !present.contains(id))
        .map(|(i, id)| {
            flux_core::config::FieldError::new(
                format!("flowIds.{i}"),
                format!("no flow with id {id}"),
            )
        })
        .collect();

    Err(ApiError::Validation(errors))
}

/// Turns a duplicate-name insert into a field-level conflict.
fn name_conflict(err: sqlx::Error) -> ApiError {
    if is_unique_violation(&err) {
        ApiError::field("name", "a test with that name already exists")
    } else {
        err.into()
    }
}

#[cfg(test)]
mod test_endpoints {
    use super::*;

    /// A valid input with the given flows.
    fn input(flow_ids: Vec<Id>) -> TestInput {
        TestInput {
            name: "throughput".into(),
            test_type: TestType::Manual,
            config: serde_json::json!({}),
            flow_ids,
        }
    }

    #[test]
    fn a_test_needs_at_least_one_flow() {
        let err = input(Vec::new()).validate().unwrap_err();
        match err {
            ApiError::Validation(errors) => {
                assert!(errors.iter().any(|e| e.path == "flowIds"));
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_flow_may_not_be_listed_twice() {
        // Two copies of one flow would programme the same streams twice and
        // double the rate without the operator asking for it.
        let id = Id::new_v4();
        assert!(input(vec![id, id]).validate().is_err());
        assert!(input(vec![id, Id::new_v4()]).validate().is_ok());
    }

    #[test]
    fn a_blank_name_is_rejected() {
        let mut body = input(vec![Id::new_v4()]);
        body.name = "   ".into();
        assert!(body.validate().is_err());
    }

    #[test]
    fn an_unrunnable_configuration_comes_back_against_its_field() {
        // The operator has to be told which part of the test to fix.
        let err = map_run_error(RunError::Invalid("no ports in a group".into()));
        match err {
            ApiError::Validation(errors) => assert_eq!(errors[0].path, "flowIds"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }
}
