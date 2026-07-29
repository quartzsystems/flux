//! Authentication endpoints: login, logout, and "who am I".

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use flux_core::types::Role;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json};
use crate::auth;
use crate::state::AppState;
use crate::store::{sessions, users};

/// Mounts the authentication routes.
pub fn router() -> Router<AppState> {
    Router::new().route("/login", post(login)).route("/logout", post(logout)).route("/me", get(me))
}

/// Login request body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    /// Account name, matched case-insensitively.
    pub username: String,
    /// Plaintext password, verified against the stored Argon2id hash.
    pub password: String,
}

/// The authenticated account, returned by login and by `/me`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    /// Account primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: flux_core::types::Id,
    /// Account name.
    pub username: String,
    /// Access level, which the UI uses to hide controls the account cannot use.
    pub role: Role,
}

/// Exchanges credentials for a session cookie.
///
/// Failure is always the same message and the same status regardless of whether
/// the username exists, and always costs one Argon2 verification. Between them
/// those two properties keep the endpoint from confirming which accounts are
/// real.
#[tracing::instrument(skip_all, fields(username = %body.username))]
async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let user = users::find_by_username(state.store.pool(), &body.username).await?;

    let Some(user) = user else {
        auth::verify_dummy(&body.password);
        tracing::warn!("login failed: no such account");
        return Err(ApiError::Unauthorized);
    };

    if !auth::verify_password(&body.password, &user.pw_hash) {
        tracing::warn!(user_id = %user.id, "login failed: wrong password");
        return Err(ApiError::Unauthorized);
    }

    let token = auth::generate_token();
    let token_hash = auth::hash_token(&token);
    let expires_at = OffsetDateTime::now_utc() + state.config.session_ttl;

    let session_id = sessions::create(
        state.store.pool(),
        user.id,
        &token_hash,
        expires_at,
        header_str(&headers, axum::http::header::USER_AGENT),
        client_ip(&headers).as_deref(),
    )
    .await?;

    users::touch_login(state.store.pool(), user.id).await?;

    tracing::info!(user_id = %user.id, %session_id, role = %user.role, "login succeeded");

    let jar = jar.add(session_cookie(&state, token, expires_at));
    Ok((jar, axum::Json(MeResponse { id: user.id, username: user.username, role: user.role })))
}

/// Ends the current session.
///
/// Deleting the row rather than only clearing the cookie is what makes logout
/// meaningful: a token copied out of the browser stops working too.
#[tracing::instrument(skip_all, fields(user = %identity.username))]
async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    Auth(identity): Auth,
) -> ApiResult<impl IntoResponse> {
    if let Some(cookie) = jar.get(auth::SESSION_COOKIE) {
        sessions::delete_by_token(state.store.pool(), &auth::hash_token(cookie.value())).await?;
    }
    tracing::info!(session_id = %identity.session_id, "logout");

    // Removal has to carry the same path and attributes the cookie was set with,
    // or the browser keeps the original.
    let mut removal = Cookie::from(auth::SESSION_COOKIE);
    removal.set_path("/");
    Ok((jar.remove(removal), axum::Json(serde_json::json!({ "ok": true }))))
}

/// Reports the current account. The UI calls this on load to decide whether to
/// show the app or the login page.
async fn me(Auth(identity): Auth) -> ApiResult<axum::Json<MeResponse>> {
    Ok(axum::Json(MeResponse {
        id: identity.user_id,
        username: identity.username,
        role: identity.role,
    }))
}

/// Builds the session cookie.
///
/// `HttpOnly` keeps the token out of reach of any script on the page, and
/// `SameSite=Strict` means a cross-site request cannot carry it — which for an
/// API whose endpoints start traffic is the CSRF defence.
fn session_cookie(state: &AppState, token: String, expires_at: OffsetDateTime) -> Cookie<'static> {
    let mut cookie = Cookie::new(auth::SESSION_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(state.config.cookie_secure);
    cookie.set_expires(expires_at);
    cookie
}

/// Reads a header as a string, if it is present and valid UTF-8.
fn header_str(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

/// Best-effort client address for the session audit trail.
///
/// Only consulted for display. It is not used for any authorisation decision,
/// because `X-Forwarded-For` is client-controlled unless a trusted proxy is
/// known to overwrite it, and this appliance is usually reached directly.
fn client_ip(headers: &HeaderMap) -> Option<String> {
    let forwarded = headers.get("x-forwarded-for")?.to_str().ok()?;
    forwarded.split(',').next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::*;

    #[test]
    fn the_client_ip_is_the_first_entry_in_the_forwarded_chain() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers).as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn a_missing_or_empty_forwarded_header_yields_no_address() {
        assert_eq!(client_ip(&HeaderMap::new()), None);

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "  ".parse().unwrap());
        assert_eq!(client_ip(&headers), None);
    }
}
