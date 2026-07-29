//! Load profile persistence.

use flux_core::types::Id;
use sqlx::PgPool;

use super::models::LoadProfile;

/// Columns selected wherever a full [`LoadProfile`] is returned.
const COLUMNS: &str = "id, name, config, created_by, created_at, updated_at";

/// Every profile, alphabetically.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<LoadProfile>> {
    sqlx::query_as::<_, LoadProfile>(&format!(
        "SELECT {COLUMNS} FROM load_profiles ORDER BY name"
    ))
    .fetch_all(pool)
    .await
}

/// One profile by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<LoadProfile>> {
    sqlx::query_as::<_, LoadProfile>(&format!(
        "SELECT {COLUMNS} FROM load_profiles WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Several profiles by primary key, in the order requested.
pub async fn get_many(pool: &PgPool, ids: &[Id]) -> sqlx::Result<Vec<LoadProfile>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, LoadProfile>(&format!(
        "SELECT {COLUMNS} FROM load_profiles WHERE id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;

    let mut by_id: std::collections::HashMap<Id, LoadProfile> =
        rows.into_iter().map(|p| (p.id, p)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// Creates a profile.
pub async fn create(
    pool: &PgPool,
    name: &str,
    config: &serde_json::Value,
    created_by: Option<Id>,
) -> sqlx::Result<LoadProfile> {
    sqlx::query_as::<_, LoadProfile>(&format!(
        "INSERT INTO load_profiles (name, config, created_by)
         VALUES ($1, $2, $3)
         RETURNING {COLUMNS}"
    ))
    .bind(name)
    .bind(config)
    .bind(created_by)
    .fetch_one(pool)
    .await
}

/// Replaces a profile's name and configuration.
pub async fn update(
    pool: &PgPool,
    id: Id,
    name: &str,
    config: &serde_json::Value,
) -> sqlx::Result<Option<LoadProfile>> {
    sqlx::query_as::<_, LoadProfile>(&format!(
        "UPDATE load_profiles
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

/// Deletes a profile.
pub async fn delete(pool: &PgPool, id: Id) -> sqlx::Result<bool> {
    let result =
        sqlx::query("DELETE FROM load_profiles WHERE id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Names of the tests that reference a profile.
///
/// Deleting a profile a test depends on would leave that test unable to run
/// with nothing to say why, so the API checks this first and refuses with the
/// list.
pub async fn referencing_tests(pool: &PgPool, id: Id) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM tests WHERE $1 = ANY(profile_ids) ORDER BY name",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}
