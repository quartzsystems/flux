//! Port reservations — the soft lock that stops two operators running tests over
//! the same cable at once.
//!
//! Reservations always carry an expiry. An operator who reserves a port and then
//! goes home must not leave the appliance unusable, so the hold lapses on its own
//! and the janitor task sweeps the row away.

use flux_core::types::{Id, Role};
use sqlx::PgPool;
use time::OffsetDateTime;

use super::models::ReservationView;

/// Takes or extends a hold on a port.
///
/// Re-reserving a port you already hold extends it; the partial unique index on
/// `port_id` means someone else's live hold makes this a unique violation, which
/// the API turns into a 409.
pub async fn reserve(
    pool: &PgPool,
    port_id: Id,
    user_id: Id,
    note: &str,
    expires_at: OffsetDateTime,
) -> sqlx::Result<ReservationView> {
    // Clear a lapsed hold first: the unique index does not know about expiry, so
    // a stale row would otherwise block a legitimate new reservation.
    sqlx::query("DELETE FROM reservations WHERE port_id = $1 AND expires_at <= now()")
        .bind(port_id)
        .execute(pool)
        .await?;

    sqlx::query_as::<_, ReservationView>(
        "WITH upserted AS (
             INSERT INTO reservations (port_id, user_id, note, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (port_id) DO UPDATE
                 SET note = EXCLUDED.note, expires_at = EXCLUDED.expires_at
                 WHERE reservations.user_id = EXCLUDED.user_id
             RETURNING id, port_id, user_id, note, expires_at
         )
         SELECT r.id, r.port_id, r.user_id, u.username, r.note, r.expires_at
         FROM upserted r JOIN users u ON u.id = r.user_id",
    )
    .bind(port_id)
    .bind(user_id)
    .bind(note)
    .bind(expires_at)
    .fetch_one(pool)
    .await
}

/// Releases a hold.
///
/// A viewer or operator may only release their own; an admin may release
/// anyone's, which is the escape hatch for a colleague who left for the weekend.
pub async fn release(pool: &PgPool, port_id: Id, user_id: Id, role: Role) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "DELETE FROM reservations
         WHERE port_id = $1 AND ($3 OR user_id = $2)",
    )
    .bind(port_id)
    .bind(user_id)
    .bind(role == Role::Admin)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The live hold on a port, if there is one.
pub async fn get_for_port(pool: &PgPool, port_id: Id) -> sqlx::Result<Option<ReservationView>> {
    sqlx::query_as::<_, ReservationView>(
        "SELECT r.id, r.port_id, r.user_id, u.username, r.note, r.expires_at
         FROM reservations r JOIN users u ON u.id = r.user_id
         WHERE r.port_id = $1 AND r.expires_at > now()",
    )
    .bind(port_id)
    .fetch_optional(pool)
    .await
}

/// Removes lapsed holds. Run periodically by the daemon's janitor task.
pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<u64> {
    let result =
        sqlx::query("DELETE FROM reservations WHERE expires_at <= now()").execute(pool).await?;
    Ok(result.rows_affected())
}
