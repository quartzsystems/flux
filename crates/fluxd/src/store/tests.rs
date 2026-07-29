//! Test definition persistence.

use flux_core::types::{Id, TestType};
use sqlx::PgPool;

use super::models::Test;

/// Columns selected wherever a full [`Test`] is returned.
const COLUMNS: &str =
    "id, name, type, config, flow_ids, profile_ids, created_by, created_at, updated_at";

/// Every test, alphabetically.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<Test>> {
    sqlx::query_as::<_, Test>(&format!("SELECT {COLUMNS} FROM tests ORDER BY name"))
        .fetch_all(pool)
        .await
}

/// One test by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<Test>> {
    sqlx::query_as::<_, Test>(&format!("SELECT {COLUMNS} FROM tests WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Creates a test.
pub async fn create(
    pool: &PgPool,
    name: &str,
    test_type: TestType,
    config: &serde_json::Value,
    flow_ids: &[Id],
    profile_ids: &[Id],
    created_by: Option<Id>,
) -> sqlx::Result<Test> {
    sqlx::query_as::<_, Test>(&format!(
        "INSERT INTO tests (name, type, config, flow_ids, profile_ids, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {COLUMNS}"
    ))
    .bind(name)
    .bind(test_type.as_str())
    .bind(config)
    .bind(flow_ids)
    .bind(profile_ids)
    .bind(created_by)
    .fetch_one(pool)
    .await
}

/// Replaces a test's definition.
pub async fn update(
    pool: &PgPool,
    id: Id,
    name: &str,
    test_type: TestType,
    config: &serde_json::Value,
    flow_ids: &[Id],
    profile_ids: &[Id],
) -> sqlx::Result<Option<Test>> {
    sqlx::query_as::<_, Test>(&format!(
        "UPDATE tests
         SET name = $2, type = $3, config = $4, flow_ids = $5, profile_ids = $6,
             updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(name)
    .bind(test_type.as_str())
    .bind(config)
    .bind(flow_ids)
    .bind(profile_ids)
    .fetch_optional(pool)
    .await
}

/// Deletes a test. Its runs survive, holding their own snapshot.
pub async fn delete(pool: &PgPool, id: Id) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM tests WHERE id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}
