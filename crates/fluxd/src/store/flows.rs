//! Flow persistence.

use flux_core::types::Id;
use sqlx::PgPool;

use super::models::Flow;

/// Columns selected wherever a full [`Flow`] is returned.
const COLUMNS: &str = "id, name, config, created_by, created_at, updated_at";

/// Every flow, alphabetically.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<Flow>> {
    sqlx::query_as::<_, Flow>(&format!("SELECT {COLUMNS} FROM flows ORDER BY name"))
        .fetch_all(pool)
        .await
}

/// One flow by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<Flow>> {
    sqlx::query_as::<_, Flow>(&format!("SELECT {COLUMNS} FROM flows WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Several flows by primary key, in the order requested.
///
/// A test names its flows in a specific order and that order is the port
/// numbering, so this reorders the result to match the request rather than
/// letting Postgres return them however it likes.
pub async fn get_many(pool: &PgPool, ids: &[Id]) -> sqlx::Result<Vec<Flow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, Flow>(&format!(
        "SELECT {COLUMNS} FROM flows WHERE id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;

    let mut by_id: std::collections::HashMap<Id, Flow> =
        rows.into_iter().map(|f| (f.id, f)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// Creates a flow.
pub async fn create(
    pool: &PgPool,
    name: &str,
    config: &serde_json::Value,
    created_by: Option<Id>,
) -> sqlx::Result<Flow> {
    sqlx::query_as::<_, Flow>(&format!(
        "INSERT INTO flows (name, config, created_by)
         VALUES ($1, $2, $3)
         RETURNING {COLUMNS}"
    ))
    .bind(name)
    .bind(config)
    .bind(created_by)
    .fetch_one(pool)
    .await
}

/// Replaces a flow's name and configuration.
pub async fn update(
    pool: &PgPool,
    id: Id,
    name: &str,
    config: &serde_json::Value,
) -> sqlx::Result<Option<Flow>> {
    sqlx::query_as::<_, Flow>(&format!(
        "UPDATE flows
         SET name = $2, config = $3, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(name)
    .bind(config)
    .fetch_optional(pool)
    .await
}

/// Deletes a flow.
pub async fn delete(pool: &PgPool, id: Id) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM flows WHERE id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Names of the tests that reference a flow.
///
/// Deleting a flow a test depends on would leave that test unable to run, with
/// nothing to say why. The API checks this first and refuses with the list.
pub async fn referencing_tests(pool: &PgPool, id: Id) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT name FROM tests WHERE $1 = ANY(flow_ids) ORDER BY name")
        .bind(id)
        .fetch_all(pool)
        .await
}
