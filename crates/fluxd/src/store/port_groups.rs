//! Port group persistence.
//!
//! A group is the unit an engine instance is launched over: the members become
//! the instance's ports, numbered by `group_index`. Membership changes therefore
//! run in a transaction — a group that is half-reassigned would launch an engine
//! whose port numbering does not match what the orchestrator believes.

use flux_core::types::{EngineMode, Id, PortGroupState};
use sqlx::PgPool;

use super::models::PortGroup;

/// Columns selected wherever a full [`PortGroup`] is returned.
const COLUMNS: &str = "id, name, engine_mode, state, trex_cfg, error, created_at, updated_at";

/// Every group, alphabetically.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<PortGroup>> {
    sqlx::query_as::<_, PortGroup>(&format!("SELECT {COLUMNS} FROM port_groups ORDER BY name"))
        .fetch_all(pool)
        .await
}

/// One group by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<PortGroup>> {
    sqlx::query_as::<_, PortGroup>(&format!("SELECT {COLUMNS} FROM port_groups WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Creates a group.
pub async fn create(
    pool: &PgPool,
    name: &str,
    engine_mode: EngineMode,
    trex_cfg: &serde_json::Value,
) -> sqlx::Result<PortGroup> {
    sqlx::query_as::<_, PortGroup>(&format!(
        "INSERT INTO port_groups (name, engine_mode, trex_cfg)
         VALUES ($1, $2, $3)
         RETURNING {COLUMNS}"
    ))
    .bind(name)
    .bind(engine_mode.as_str())
    .bind(trex_cfg)
    .fetch_one(pool)
    .await
}

/// Updates a group's name, mode, and engine configuration.
pub async fn update(
    pool: &PgPool,
    id: Id,
    name: &str,
    engine_mode: EngineMode,
    trex_cfg: &serde_json::Value,
) -> sqlx::Result<Option<PortGroup>> {
    sqlx::query_as::<_, PortGroup>(&format!(
        "UPDATE port_groups
         SET name = $2, engine_mode = $3, trex_cfg = $4, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(name)
    .bind(engine_mode.as_str())
    .bind(trex_cfg)
    .fetch_optional(pool)
    .await
}

/// Records an engine lifecycle transition.
///
/// `error` is written unconditionally so that a successful start clears whatever
/// failure the previous attempt left behind.
#[allow(dead_code, reason = "called by the engine supervisor, which arrives in milestone 2")]
pub async fn set_state(
    pool: &PgPool,
    id: Id,
    state: PortGroupState,
    error: Option<&str>,
) -> sqlx::Result<Option<PortGroup>> {
    sqlx::query_as::<_, PortGroup>(&format!(
        "UPDATE port_groups
         SET state = $2, error = $3, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(state.as_str())
    .bind(error)
    .fetch_optional(pool)
    .await
}

/// Deletes a group. Member ports fall back to being ungrouped.
pub async fn delete(pool: &PgPool, id: Id) -> sqlx::Result<bool> {
    // `ports.group_id` is ON DELETE SET NULL, but `group_index` is not, and the
    // CHECK constraint requires the two to be null together. Clear both first.
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE ports SET group_id = NULL, group_index = NULL WHERE group_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let result =
        sqlx::query("DELETE FROM port_groups WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Replaces a group's membership with `port_ids`, in order.
///
/// The index a port receives is its position in `port_ids`, which becomes the
/// port number the engine instance uses. Running as one transaction means a
/// failure partway leaves the previous membership intact rather than a group
/// with holes in its numbering.
pub async fn set_members(pool: &PgPool, id: Id, port_ids: &[Id]) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("UPDATE ports SET group_id = NULL, group_index = NULL WHERE group_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    for (index, port_id) in port_ids.iter().enumerate() {
        sqlx::query(
            "UPDATE ports SET group_id = $1, group_index = $2, updated_at = now() WHERE id = $3",
        )
        .bind(id)
        .bind(index as i16)
        .bind(port_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

/// The ports in a group, in engine port-number order.
pub async fn member_ids(pool: &PgPool, id: Id) -> sqlx::Result<Vec<Id>> {
    sqlx::query_scalar::<_, Id>(
        "SELECT id FROM ports WHERE group_id = $1 ORDER BY group_index",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}
