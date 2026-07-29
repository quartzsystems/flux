//! Session persistence.
//!
//! Only the SHA-256 of a session token is ever stored. The plaintext exists in
//! the operator's cookie and nowhere else, so a database disclosure yields no
//! usable sessions.

use flux_core::types::Id;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::models::SessionWithUser;

/// Creates a session for `user_id` expiring at `expires_at`.
pub async fn create(
    pool: &PgPool,
    user_id: Id,
    token_hash: &str,
    expires_at: OffsetDateTime,
    user_agent: Option<&str>,
    remote_ip: Option<&str>,
) -> sqlx::Result<Id> {
    sqlx::query_scalar::<_, Id>(
        "INSERT INTO sessions (user_id, token_hash, expires_at, user_agent, remote_ip)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .bind(user_agent)
    .bind(remote_ip)
    .fetch_one(pool)
    .await
}

/// Resolves a token hash to its session and owner, if the session is still live.
///
/// The expiry check is in SQL rather than in Rust so that a clock difference
/// between the daemon and the database cannot produce a session that one of them
/// thinks is valid and the other does not.
pub async fn lookup(pool: &PgPool, token_hash: &str) -> sqlx::Result<Option<SessionWithUser>> {
    sqlx::query_as::<_, SessionWithUser>(
        "SELECT s.id, s.user_id, u.username, u.role, s.expires_at
         FROM sessions s
         JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now()",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
}

/// Deletes one session, by token hash. This is what logout does.
pub async fn delete_by_token(pool: &PgPool, token_hash: &str) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Deletes every session belonging to a user.
///
/// Called after a password change so that a stolen cookie stops working the
/// moment the operator reacts to the theft.
pub async fn delete_for_user(pool: &PgPool, user_id: Id) -> sqlx::Result<u64> {
    let result =
        sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(user_id).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Removes expired sessions. Run periodically by the daemon's janitor task.
pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<u64> {
    let result =
        sqlx::query("DELETE FROM sessions WHERE expires_at <= now()").execute(pool).await?;
    Ok(result.rows_affected())
}
