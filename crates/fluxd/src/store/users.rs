//! User account persistence.

use flux_core::types::{Id, Role};
use sqlx::PgPool;

use super::models::User;

/// Columns selected wherever a full [`User`] is returned.
const COLUMNS: &str =
    "id, username, pw_hash, role, created_at, updated_at, last_login_at";

/// Every account, newest first.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<User>> {
    sqlx::query_as::<_, User>(&format!(
        "SELECT {COLUMNS} FROM users ORDER BY created_at DESC"
    ))
    .fetch_all(pool)
    .await
}

/// One account by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>(&format!("SELECT {COLUMNS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// One account by login name, matched case-insensitively.
///
/// Operators type their username into a login box; `Admin` and `admin` must be
/// the same account, and the unique index on `lower(username)` guarantees this
/// can only ever match one row.
pub async fn find_by_username(pool: &PgPool, username: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>(&format!(
        "SELECT {COLUMNS} FROM users WHERE lower(username) = lower($1)"
    ))
    .bind(username)
    .fetch_optional(pool)
    .await
}

/// Inserts an account.
///
/// Returns a unique-violation error if the name is taken; callers turn that into
/// a 409 rather than checking first, which would race.
pub async fn create(
    pool: &PgPool,
    username: &str,
    pw_hash: &str,
    role: Role,
) -> sqlx::Result<User> {
    sqlx::query_as::<_, User>(&format!(
        "INSERT INTO users (username, pw_hash, role)
         VALUES ($1, $2, $3)
         RETURNING {COLUMNS}"
    ))
    .bind(username)
    .bind(pw_hash)
    .bind(role.as_str())
    .fetch_one(pool)
    .await
}

/// Changes an account's role.
pub async fn set_role(pool: &PgPool, id: Id, role: Role) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>(&format!(
        "UPDATE users SET role = $2, updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(role.as_str())
    .fetch_optional(pool)
    .await
}

/// Replaces an account's password hash.
pub async fn set_password(pool: &PgPool, id: Id, pw_hash: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>(&format!(
        "UPDATE users SET pw_hash = $2, updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(pw_hash)
    .fetch_optional(pool)
    .await
}

/// Deletes an account. Its sessions cascade away with it.
pub async fn delete(pool: &PgPool, id: Id) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Stamps a successful authentication.
pub async fn touch_login(pool: &PgPool, id: Id) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// How many accounts exist. Used to decide whether to bootstrap the first admin.
pub async fn count(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users").fetch_one(pool).await
}

/// How many admins exist, excluding `except` when given.
///
/// This is what stops an admin from demoting or deleting themselves into an
/// appliance with no administrator — a state only a reinstall could recover from.
pub async fn count_admins_excluding(pool: &PgPool, except: Option<Id>) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM users WHERE role = 'admin' AND ($1::uuid IS NULL OR id <> $1)",
    )
    .bind(except)
    .fetch_one(pool)
    .await
}
