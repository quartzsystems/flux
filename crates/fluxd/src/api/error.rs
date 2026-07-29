//! The single error type every handler returns.
//!
//! Modelling failures as one enum that implements `IntoResponse` means a handler
//! can `?` its way through the happy path and every failure still comes back in
//! the same JSON shape with the right status code. The alternative — building
//! responses inline — reliably drifts into three different error formats.
//!
//! The internal variant deliberately does not put its detail in the response
//! body. A database error message can name tables, columns, and constraints; the
//! client gets a correlation-free generic message and the detail goes to the log.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use flux_core::config::FieldError;
use flux_core::port::PortError;
use serde::Serialize;
use utoipa::ToSchema;

use crate::portmgr::PortMgrError;

/// Everything a handler can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// No valid session. The client should log in.
    #[error("authentication required")]
    Unauthorized,

    /// Authenticated, but the account's role is insufficient.
    #[error("insufficient permissions")]
    Forbidden,

    /// The addressed object does not exist.
    #[error("{0} not found")]
    NotFound(String),

    /// The request is well-formed but conflicts with current state.
    #[error("{0}")]
    Conflict(String),

    /// The request body failed field-level validation.
    #[error("validation failed")]
    Validation(Vec<FieldError>),

    /// The request is malformed in a way no field path describes.
    #[error("{0}")]
    BadRequest(String),

    /// A dependency the request needs is not currently available.
    #[error("{0}")]
    Unavailable(String),

    /// Anything unexpected. The detail is logged, not returned.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    /// Convenience for the common single-field validation failure.
    pub fn field(path: impl Into<String>, msg: impl Into<String>) -> Self {
        ApiError::Validation(vec![FieldError::new(path, msg)])
    }

    /// The HTTP status this failure maps to.
    fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable machine-readable code the UI branches on.
    fn code(&self) -> &'static str {
        match self {
            ApiError::Unauthorized => "unauthorized",
            ApiError::Forbidden => "forbidden",
            ApiError::NotFound(_) => "not_found",
            ApiError::Conflict(_) => "conflict",
            ApiError::Validation(_) => "validation",
            ApiError::BadRequest(_) => "bad_request",
            ApiError::Unavailable(_) => "unavailable",
            ApiError::Internal(_) => "internal",
        }
    }
}

/// The JSON body every failed request returns.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    /// Stable machine-readable class, e.g. `not_found`.
    pub code: String,
    /// Human-readable summary, safe to show an operator.
    pub message: String,
    /// Field-level failures, present only for validation errors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FieldError>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code().to_string();

        let (message, errors) = match self {
            ApiError::Validation(errors) => ("one or more fields are invalid".to_string(), errors),
            ApiError::Internal(err) => {
                // The only place the real cause is recorded. `{err:#}` walks the
                // context chain, which is what makes anyhow's context worth adding.
                tracing::error!(error = format!("{err:#}"), "unhandled error");
                ("an internal error occurred".to_string(), Vec::new())
            }
            ref other => (other.to_string(), Vec::new()),
        };

        (status, Json(ErrorBody { code, message, errors })).into_response()
    }
}

// ---------------------------------------------------------------------------
// Conversions from the layers below
// ---------------------------------------------------------------------------

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            // `RowNotFound` reaching a handler means a `fetch_one` that should
            // have been a `fetch_optional`; treat it as a 404 rather than a 500,
            // which is almost always what the caller meant.
            sqlx::Error::RowNotFound => ApiError::NotFound("record".into()),
            e if crate::store::is_unique_violation(e) => {
                ApiError::Conflict("a record with that identity already exists".into())
            }
            e if crate::store::is_foreign_key_violation(e) => {
                ApiError::Conflict("a referenced record does not exist".into())
            }
            _ => ApiError::Internal(anyhow::Error::new(err).context("database query failed")),
        }
    }
}

impl From<PortError> for ApiError {
    fn from(err: PortError) -> Self {
        match err {
            PortError::NotFound(pci) => ApiError::NotFound(format!("device {pci}")),
            PortError::NotAllowed(_) => ApiError::Forbidden,
            PortError::Invalid(msg) => ApiError::BadRequest(msg),
            PortError::Unavailable(msg) => {
                ApiError::Unavailable(format!("the privileged port helper is unreachable: {msg}"))
            }
            PortError::Failed(msg) => ApiError::Conflict(msg),
        }
    }
}

impl From<PortMgrError> for ApiError {
    fn from(err: PortMgrError) -> Self {
        match err {
            PortMgrError::NotFound(id) => ApiError::NotFound(format!("port {id}")),
            PortMgrError::Busy(msg) => ApiError::Conflict(msg),
            PortMgrError::Port(e) => e.into(),
            PortMgrError::Db(e) => e.into(),
        }
    }
}

impl From<Vec<FieldError>> for ApiError {
    fn from(errors: Vec<FieldError>) -> Self {
        ApiError::Validation(errors)
    }
}

/// Handler result alias.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a response back into its status and decoded body.
    async fn render(err: ApiError) -> (StatusCode, ErrorBodyOwned) {
        use http_body_util::BodyExt;
        let response = err.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// Deserialisable mirror of `ErrorBody`, since the real type is write-only.
    #[derive(Debug, serde::Deserialize)]
    struct ErrorBodyOwned {
        code: String,
        message: String,
        #[serde(default)]
        errors: Vec<FieldError>,
    }

    #[tokio::test]
    async fn each_variant_maps_to_its_status_and_code() {
        for (err, status, code) in [
            (ApiError::Unauthorized, StatusCode::UNAUTHORIZED, "unauthorized"),
            (ApiError::Forbidden, StatusCode::FORBIDDEN, "forbidden"),
            (ApiError::NotFound("port".into()), StatusCode::NOT_FOUND, "not_found"),
            (ApiError::Conflict("held".into()), StatusCode::CONFLICT, "conflict"),
            (ApiError::BadRequest("nope".into()), StatusCode::BAD_REQUEST, "bad_request"),
            (
                ApiError::Unavailable("no helper".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
            ),
        ] {
            let (got_status, body) = render(err).await;
            assert_eq!(got_status, status);
            assert_eq!(body.code, code);
            assert!(!body.message.is_empty());
        }
    }

    #[tokio::test]
    async fn validation_errors_carry_their_field_paths_to_the_client() {
        let err = ApiError::Validation(vec![
            FieldError::new("rate.value", "exceeds port line rate"),
            FieldError::new("size.type", "unknown size type"),
        ]);
        let (status, body) = render(err).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body.code, "validation");
        assert_eq!(body.errors.len(), 2);
        assert_eq!(body.errors[0].path, "rate.value");
        assert_eq!(body.errors[0].msg, "exceeds port line rate");
    }

    #[tokio::test]
    async fn internal_errors_never_leak_their_cause_to_the_client() {
        let err = ApiError::Internal(anyhow::anyhow!(
            "relation \"users\" does not exist; connection to 10.0.0.5 as flux_admin"
        ));
        let (status, body) = render(err).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.message, "an internal error occurred");
        assert!(!body.message.contains("users"));
        assert!(!body.message.contains("flux_admin"));
        assert!(body.errors.is_empty());
    }

    #[tokio::test]
    async fn non_validation_errors_omit_the_errors_array_entirely() {
        use http_body_util::BodyExt;
        let response = ApiError::NotFound("port".into()).into_response();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(raw.get("errors").is_none(), "empty arrays should not be serialised");
    }

    #[test]
    fn a_missing_row_from_sqlx_becomes_a_404_not_a_500() {
        let err: ApiError = sqlx::Error::RowNotFound.into();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn an_unreachable_helper_is_a_503_and_a_refused_device_is_a_403() {
        let err: ApiError = PortError::Unavailable("socket missing".into()).into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);

        let pci = flux_core::port::PciAddr::parse("0000:81:00.0").unwrap();
        let err: ApiError = PortError::NotAllowed(pci).into();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }
}
