//! User administration. Every route here is admin-only.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::Router;
use flux_core::config::Validation;
use flux_core::types::{Id, Role};
use serde::Deserialize;
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Json};
use crate::auth;
use crate::state::AppState;
use crate::store::models::UserView;
use crate::store::{is_unique_violation, sessions, users};

/// Mounts the user routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(get_one).patch(update).delete(delete))
}

/// Every account.
async fn list(State(state): State<AppState>, _auth: AdminAuth) -> ApiResult<Json<Vec<UserView>>> {
    let users = users::list(state.store.pool()).await?;
    Ok(Json(users.into_iter().map(UserView::from).collect()))
}

/// One account.
async fn get_one(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<UserView>> {
    let user = users::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("user {id}")))?;
    Ok(Json(user.into()))
}

/// A new account.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateUser {
    /// Login name.
    pub username: String,
    /// Initial password. Subject to the length policy.
    pub password: String,
    /// Access level.
    pub role: Role,
}

/// Creates an account.
#[tracing::instrument(skip_all, fields(username = %body.username, role = %body.role))]
async fn create(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Json(body): Json<CreateUser>,
) -> ApiResult<Json<UserView>> {
    let mut v = Validation::new();
    validate_username(&body.username, &mut v);
    validate_password(&body.password, &mut v);
    v.finish()?;

    let hash = auth::hash_password(&body.password)
        .map_err(|e| ApiError::field("password", e.to_string()))?;

    let user = users::create(state.store.pool(), body.username.trim(), &hash, body.role)
        .await
        .map_err(|err| {
            if is_unique_violation(&err) {
                ApiError::Conflict("that username is already taken".into())
            } else {
                err.into()
            }
        })?;

    tracing::info!(actor = %actor.username, user_id = %user.id, "user created");
    Ok(Json(user.into()))
}

/// Changes to an account. Absent fields are left alone.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUser {
    /// New access level.
    #[serde(default)]
    pub role: Option<Role>,
    /// New password.
    #[serde(default)]
    pub password: Option<String>,
}

/// Updates an account.
///
/// Changing a password invalidates that account's existing sessions. An admin
/// resetting a password is usually responding to a suspected compromise, and a
/// reset that left the attacker's cookie working would not address it.
#[tracing::instrument(skip_all, fields(user_id = %id))]
async fn update(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
    Json(body): Json<UpdateUser>,
) -> ApiResult<Json<UserView>> {
    let mut v = Validation::new();
    if let Some(password) = &body.password {
        validate_password(password, &mut v);
    }
    v.require(
        body.role.is_some() || body.password.is_some(),
        "role",
        "provide at least one of role or password",
    );
    v.finish()?;

    let existing = users::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("user {id}")))?;

    if let Some(role) = body.role {
        if existing.role == Role::Admin && role != Role::Admin {
            ensure_another_admin_remains(&state, id, "demote").await?;
        }
        users::set_role(state.store.pool(), id, role).await?;
        tracing::info!(actor = %actor.username, from = %existing.role, to = %role, "user role changed");
    }

    if let Some(password) = &body.password {
        let hash = auth::hash_password(password)
            .map_err(|e| ApiError::field("password", e.to_string()))?;
        users::set_password(state.store.pool(), id, &hash).await?;
        let revoked = sessions::delete_for_user(state.store.pool(), id).await?;
        tracing::info!(actor = %actor.username, revoked, "user password reset");
    }

    let updated = users::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("user {id}")))?;
    Ok(Json(updated.into()))
}

/// Deletes an account and its sessions.
#[tracing::instrument(skip(state), fields(user_id = %id))]
async fn delete(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    let existing = users::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("user {id}")))?;

    if existing.role == Role::Admin {
        ensure_another_admin_remains(&state, id, "delete").await?;
    }

    users::delete(state.store.pool(), id).await?;
    tracing::info!(actor = %actor.username, deleted = %existing.username, "user deleted");
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Refuses an operation that would leave the appliance with no administrator.
///
/// There is no recovery path from that state short of reinstalling, so this is
/// checked even when the admin is deliberately removing themselves.
async fn ensure_another_admin_remains(
    state: &AppState,
    excluding: Id,
    verb: &str,
) -> ApiResult<()> {
    let remaining = users::count_admins_excluding(state.store.pool(), Some(excluding)).await?;
    if remaining == 0 {
        return Err(ApiError::Conflict(format!(
            "cannot {verb} the last administrator; promote another account first"
        )));
    }
    Ok(())
}

/// Applies the username rules.
fn validate_username(username: &str, v: &mut Validation) {
    let trimmed = username.trim();
    v.require(!trimmed.is_empty(), "username", "must not be empty");
    v.require(trimmed.chars().count() <= 64, "username", "must be at most 64 characters");
    v.require(
        trimmed.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "username",
        "may contain only letters, digits, and - _ .",
    );
}

/// Applies the password length policy, reporting against the right field path.
fn validate_password(password: &str, v: &mut Validation) {
    if let Err(err) = auth::check_password_policy(password) {
        v.error("password", err.to_string());
    }
}

#[cfg(test)]
mod tests {
    use flux_core::config::FieldError;

    use super::*;

    /// Runs the username rules and returns the failing paths.
    fn username_errors(name: &str) -> Vec<FieldError> {
        let mut v = Validation::new();
        validate_username(name, &mut v);
        v.finish().err().unwrap_or_default()
    }

    #[test]
    fn usernames_are_restricted_to_identifier_characters() {
        assert!(username_errors("operator").is_empty());
        assert!(username_errors("net.eng_2").is_empty());

        for bad in ["", "  ", "has space", "drop;table", "quote'", "sl/ash"] {
            assert!(!username_errors(bad).is_empty(), "should have rejected {bad:?}");
        }
    }

    #[test]
    fn the_password_policy_is_reported_against_the_password_field() {
        let mut v = Validation::new();
        validate_password("short", &mut v);
        let errs = v.finish().unwrap_err();
        assert_eq!(errs[0].path, "password");
        assert!(errs[0].msg.contains("at least"));
    }

    #[test]
    fn a_compliant_password_produces_no_errors() {
        let mut v = Validation::new();
        validate_password("a sufficiently long passphrase", &mut v);
        assert!(v.finish().is_ok());
    }
}
