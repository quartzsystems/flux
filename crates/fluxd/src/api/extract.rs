//! Request extractors: the authenticated principal, and a JSON body extractor
//! that reports failures in the API's own error shape.
//!
//! Role enforcement lives in the type an argument is declared as. A handler that
//! takes [`AdminAuth`] cannot be routed without an admin check, and a handler
//! that takes no auth extractor at all is visibly public. That is stronger than a
//! per-route middleware table, which drifts the moment a route is added.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use flux_core::types::Role;
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::error::ApiError;
use crate::auth::Identity;

/// Any authenticated account. Viewers included.
#[derive(Debug, Clone)]
pub struct Auth(pub Identity);

/// An account that may change test configuration and drive runs.
#[derive(Debug, Clone)]
pub struct OperatorAuth(pub Identity);

/// An account that may manage users, settings, and port bindings.
#[derive(Debug, Clone)]
pub struct AdminAuth(pub Identity);

/// Pulls the identity the authentication middleware attached, if any.
fn identity(parts: &Parts) -> Option<Identity> {
    parts.extensions.get::<Identity>().cloned()
}

/// Builds the extractor for a minimum role.
///
/// The distinction between 401 and 403 matters to the UI: 401 sends the operator
/// to the login page, 403 tells them their account cannot do this and logging in
/// again will not help.
fn require(parts: &Parts, minimum: Role) -> Result<Identity, ApiError> {
    let identity = identity(parts).ok_or(ApiError::Unauthorized)?;
    if identity.can(minimum) {
        Ok(identity)
    } else {
        tracing::warn!(
            user = %identity.username,
            role = %identity.role,
            required = %minimum,
            "refused a request the account is not permitted to make"
        );
        Err(ApiError::Forbidden)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        require(parts, Role::Viewer).map(Auth)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for OperatorAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        require(parts, Role::Operator).map(OperatorAuth)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        require(parts, Role::Admin).map(AdminAuth)
    }
}

/// A JSON body type that fails in the API's error shape.
///
/// Used for both directions. As an extractor it replaces axum's own `Json`,
/// which rejects with a plain-text body and its own status codes — that would
/// make malformed input the one case where a client sees a different error
/// format from every other failure. As a response it is a plain pass-through to
/// axum's, so handlers can use one type for both.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

impl<T, S> axum::extract::OptionalFromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    /// Lets a handler take `Option<Json<T>>` for a genuinely optional body.
    ///
    /// A request with no `Content-Type` is treated as having no body at all,
    /// which is what a `POST` with nothing in it looks like — an operator
    /// pressing "run" sends exactly that. A body that *is* present still has to
    /// parse, so a malformed one is an error rather than being silently ignored.
    async fn from_request(req: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        if req.headers().get(axum::http::header::CONTENT_TYPE).is_none() {
            return Ok(None);
        }
        <Json<T> as FromRequest<S>>::from_request(req, state).await.map(Some)
    }
}

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => Err(match rejection {
                // serde's message names the offending field ("missing field
                // `username`"), which is the useful part to pass through.
                JsonRejection::JsonDataError(e) => ApiError::BadRequest(e.body_text()),
                JsonRejection::JsonSyntaxError(_) => {
                    ApiError::BadRequest("request body is not valid JSON".into())
                }
                JsonRejection::MissingJsonContentType(_) => {
                    ApiError::BadRequest("expected Content-Type: application/json".into())
                }
                other => ApiError::BadRequest(other.body_text()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use flux_core::types::Id;

    use super::*;

    /// Builds request parts carrying `identity`, as the middleware would.
    fn parts_with(identity: Option<Identity>) -> Parts {
        let mut req = Request::new(());
        if let Some(id) = identity {
            req.extensions_mut().insert(id);
        }
        req.into_parts().0
    }

    /// An identity at `role`.
    fn who(role: Role) -> Identity {
        Identity { session_id: Id::nil(), user_id: Id::nil(), username: role.to_string(), role }
    }

    #[test]
    fn an_anonymous_request_is_unauthorised_not_forbidden() {
        let parts = parts_with(None);
        assert!(matches!(require(&parts, Role::Viewer), Err(ApiError::Unauthorized)));
        assert!(matches!(require(&parts, Role::Admin), Err(ApiError::Unauthorized)));
    }

    #[test]
    fn an_underprivileged_request_is_forbidden_not_unauthorised() {
        // The difference decides whether the UI sends the operator to the login
        // page or tells them their account cannot do this.
        let parts = parts_with(Some(who(Role::Viewer)));
        assert!(matches!(require(&parts, Role::Operator), Err(ApiError::Forbidden)));
        assert!(matches!(require(&parts, Role::Admin), Err(ApiError::Forbidden)));
    }

    #[test]
    fn each_role_reaches_exactly_the_levels_at_or_below_it() {
        let viewer = parts_with(Some(who(Role::Viewer)));
        assert!(require(&viewer, Role::Viewer).is_ok());

        let operator = parts_with(Some(who(Role::Operator)));
        assert!(require(&operator, Role::Viewer).is_ok());
        assert!(require(&operator, Role::Operator).is_ok());
        assert!(require(&operator, Role::Admin).is_err());

        let admin = parts_with(Some(who(Role::Admin)));
        assert!(require(&admin, Role::Viewer).is_ok());
        assert!(require(&admin, Role::Operator).is_ok());
        assert!(require(&admin, Role::Admin).is_ok());
    }
}
