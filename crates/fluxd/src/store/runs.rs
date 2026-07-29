//! Run and result persistence.

use flux_core::types::{Id, RunState};
use sqlx::PgPool;

use super::models::{Run, RunResult};

/// Columns selected wherever a full [`Run`] is returned.
const COLUMNS: &str = "id, test_id, test_name, type, state, started_by, started_at, \
                       finished_at, dut_meta, config_snapshot, error";

/// Creates a run in `pending`.
///
/// `test_name`, `test_type`, and `snapshot` are copied in rather than referenced
/// so the run remains interpretable after the test is edited or deleted.
#[allow(
    clippy::too_many_arguments,
    reason = "a run row genuinely has this many independent fields"
)]
pub async fn create(
    pool: &PgPool,
    test_id: Option<Id>,
    test_name: &str,
    test_type: &str,
    started_by: Option<Id>,
    dut_meta: &serde_json::Value,
    snapshot: &serde_json::Value,
) -> sqlx::Result<Run> {
    sqlx::query_as::<_, Run>(&format!(
        "INSERT INTO runs (test_id, test_name, type, state, started_by, dut_meta, config_snapshot)
         VALUES ($1, $2, $3, 'pending', $4, $5, $6)
         RETURNING {COLUMNS}"
    ))
    .bind(test_id)
    .bind(test_name)
    .bind(test_type)
    .bind(started_by)
    .bind(dut_meta)
    .bind(snapshot)
    .fetch_one(pool)
    .await
}

/// One run by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<Run>> {
    sqlx::query_as::<_, Run>(&format!("SELECT {COLUMNS} FROM runs WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Run history, newest first, optionally filtered.
pub async fn list(
    pool: &PgPool,
    state: Option<RunState>,
    test_id: Option<Id>,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<Run>> {
    sqlx::query_as::<_, Run>(&format!(
        "SELECT {COLUMNS} FROM runs
         WHERE ($1::text IS NULL OR state = $1)
           AND ($2::uuid IS NULL OR test_id = $2)
         ORDER BY started_at DESC
         LIMIT $3 OFFSET $4"
    ))
    .bind(state.map(|s| s.as_str()))
    .bind(test_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// How many runs match a filter, for pagination.
pub async fn count(
    pool: &PgPool,
    state: Option<RunState>,
    test_id: Option<Id>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM runs
         WHERE ($1::text IS NULL OR state = $1)
           AND ($2::uuid IS NULL OR test_id = $2)",
    )
    .bind(state.map(|s| s.as_str()))
    .bind(test_id)
    .fetch_one(pool)
    .await
}

/// Records a lifecycle transition.
///
/// `finished_at` is stamped by the database when the state is terminal, so the
/// timestamp comes from one clock regardless of which task made the transition.
pub async fn set_state(
    pool: &PgPool,
    id: Id,
    state: RunState,
    error: Option<&str>,
) -> sqlx::Result<Option<Run>> {
    sqlx::query_as::<_, Run>(&format!(
        "UPDATE runs
         SET state = $2,
             error = COALESCE($3, error),
             finished_at = CASE
                 WHEN $2 IN ('complete', 'failed', 'cancelled') THEN now()
                 ELSE finished_at
             END
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(state.as_str())
    .bind(error)
    .fetch_optional(pool)
    .await
}

/// Fails every non-terminal run.
///
/// Called once at startup. A run that was in flight when the daemon stopped has
/// no engine state left to resume from, so marking it failed with a reason is the
/// honest outcome — leaving it `running` forever would make the dashboard lie.
pub async fn fail_interrupted(pool: &PgPool, reason: &str) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE runs
         SET state = 'failed', error = $1, finished_at = now()
         WHERE state NOT IN ('complete', 'failed', 'cancelled')",
    )
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Columns selected wherever a full [`RunResult`] is returned.
const RESULT_COLUMNS: &str =
    "id, run_id, iteration, frame_size, params, metrics, passed, created_at";

/// Records one trial.
pub async fn add_result(
    pool: &PgPool,
    run_id: Id,
    iteration: i32,
    frame_size: Option<i32>,
    params: &serde_json::Value,
    metrics: &serde_json::Value,
    passed: bool,
) -> sqlx::Result<RunResult> {
    sqlx::query_as::<_, RunResult>(&format!(
        "INSERT INTO run_results (run_id, iteration, frame_size, params, metrics, passed)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {RESULT_COLUMNS}"
    ))
    .bind(run_id)
    .bind(iteration)
    .bind(frame_size)
    .bind(params)
    .bind(metrics)
    .bind(passed)
    .fetch_one(pool)
    .await
}

/// Every trial for a run, in order.
pub async fn results(pool: &PgPool, run_id: Id) -> sqlx::Result<Vec<RunResult>> {
    sqlx::query_as::<_, RunResult>(&format!(
        "SELECT {RESULT_COLUMNS} FROM run_results WHERE run_id = $1 ORDER BY iteration"
    ))
    .bind(run_id)
    .fetch_all(pool)
    .await
}
