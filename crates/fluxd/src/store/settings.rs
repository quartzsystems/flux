//! Appliance settings: a typed-by-convention key/value store.
//!
//! Settings are rare, small, and read on demand, so they live in one table
//! keyed by name with a JSON payload rather than in a wide row that every new
//! option would have to migrate.

use flux_core::types::Id;
use sqlx::PgPool;

use super::models::Setting;

/// The key holding the device under test's description.
///
/// Named here rather than spelled as a literal at each use, because the
/// topology endpoint that writes it and the run-start path that copies it into
/// a run's record depend on the two agreeing.
pub const DUT_KEY: &str = "dut";

/// Every setting, alphabetically.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<Setting>> {
    sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM settings ORDER BY key")
        .fetch_all(pool)
        .await
}

/// One setting by key.
pub async fn get(pool: &PgPool, key: &str) -> sqlx::Result<Option<Setting>> {
    sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// Writes a setting, creating it if absent.
pub async fn put(
    pool: &PgPool,
    key: &str,
    value: &serde_json::Value,
    updated_by: Option<Id>,
) -> sqlx::Result<Setting> {
    sqlx::query_as::<_, Setting>(
        "INSERT INTO settings (key, value, updated_by)
         VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE
             SET value = EXCLUDED.value,
                 updated_by = EXCLUDED.updated_by,
                 updated_at = now()
         RETURNING key, value, updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(updated_by)
    .fetch_one(pool)
    .await
}
