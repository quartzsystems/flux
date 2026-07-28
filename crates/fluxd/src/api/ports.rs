//! Port endpoints: inventory, naming, driver binding, and reservations.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::Router;
use flux_core::config::{FieldError, Validation};
use flux_core::types::{Id, PortMode};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Auth, Json, OperatorAuth};
use crate::state::AppState;
use crate::store::models::{PortView, ReservationView};
use crate::store::{ports, reservations};

/// Longest a port may be held.
///
/// A reservation is a courtesy lock, not a security boundary. Capping it keeps a
/// forgotten hold from making a port permanently unusable to everyone else.
const MAX_RESERVATION_HOURS: i64 = 24 * 7;

/// Default hold length when the caller does not say.
const DEFAULT_RESERVATION_HOURS: i64 = 8;

/// Mounts the port routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).put(bulk_update))
        .route("/refresh", post(refresh))
        .route("/{id}", get(get_one).patch(update))
        .route("/{id}/reserve", put(reserve).delete(release))
}

/// Every port, with its group and current reservation.
async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<PortView>>> {
    let rows = ports::list_joined(state.store.pool()).await?;
    let views = rows
        .into_iter()
        .map(PortView::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::Internal)?;
    Ok(Json(views))
}

/// One port.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<PortView>> {
    let row = ports::get_joined(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("port {id}")))?;
    Ok(Json(PortView::try_from(row).map_err(ApiError::Internal)?))
}

/// Re-reads the hardware inventory and reconciles it into the database.
async fn refresh(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> ApiResult<Json<Vec<PortView>>> {
    state.ports.refresh_inventory().await?;
    let rows = ports::list_joined(state.store.pool()).await?;
    let views = rows
        .into_iter()
        .map(PortView::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::Internal)?;
    Ok(Json(views))
}

/// Changes to apply to a port. Absent fields are left alone.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortUpdate {
    /// New operator-assigned label.
    #[serde(default)]
    pub name: Option<String>,
    /// New driver ownership. Triggers an actual rebind.
    #[serde(default)]
    pub mode: Option<PortMode>,
}

impl PortUpdate {
    /// Checks the update in isolation, before any hardware is touched.
    fn validate(&self, prefix: Option<usize>) -> Result<(), Vec<FieldError>> {
        let mut v = Validation::new();
        let check = |v: &mut Validation| {
            if let Some(name) = &self.name {
                let trimmed = name.trim();
                v.require(!trimmed.is_empty(), "name", "must not be empty");
                v.require(trimmed.len() <= 64, "name", "must be at most 64 characters");
                v.require(
                    trimmed.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
                    "name",
                    "may contain only letters, digits, and - _ .",
                );
            }
            v.require(
                self.name.is_some() || self.mode.is_some(),
                "name",
                "provide at least one of name or mode",
            );
        };

        // In a bulk request each entry's errors must point at its own index, or
        // the UI cannot highlight the right row.
        match prefix {
            Some(i) => v.scope("updates", |v| v.scope(i.to_string(), check)),
            None => check(&mut v),
        }
        v.finish()
    }
}

/// Applies changes to one port.
///
/// Renaming and rebinding are separate operations underneath: the rename is a
/// database write, the rebind reaches the privileged helper. Renaming happens
/// first because it cannot fail for hardware reasons, so a request that does both
/// does not leave a rebind applied under the old name.
#[tracing::instrument(skip(state, body), fields(port_id = %id))]
async fn update(
    State(state): State<AppState>,
    AdminAuth(identity): AdminAuth,
    Path(id): Path<Id>,
    Json(body): Json<PortUpdate>,
) -> ApiResult<Json<PortView>> {
    body.validate(None)?;
    apply_update(&state, id, &body).await?;
    tracing::info!(actor = %identity.username, "port updated");
    get_one(State(state), Auth(identity), Path(id)).await
}

/// A bulk port update request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkPortUpdate {
    /// One entry per port to change.
    pub updates: Vec<BulkPortUpdateEntry>,
}

/// One entry in a bulk update.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkPortUpdateEntry {
    /// Port to change.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// The changes.
    #[serde(flatten)]
    pub update: PortUpdate,
}

/// Applies changes to several ports.
///
/// Every entry is validated before any is applied, so a typo in the last row
/// does not leave the first three already rebound. Application itself is
/// sequential and not transactional — rebinding is a hardware action that cannot
/// be rolled back — so a mid-list hardware failure stops there and reports which
/// port failed, leaving earlier ports changed.
#[tracing::instrument(skip_all, fields(count = body.updates.len()))]
async fn bulk_update(
    State(state): State<AppState>,
    AdminAuth(identity): AdminAuth,
    Json(body): Json<BulkPortUpdate>,
) -> ApiResult<Json<Vec<PortView>>> {
    if body.updates.is_empty() {
        return Err(ApiError::field("updates", "must contain at least one entry"));
    }

    let mut errors = Vec::new();
    for (i, entry) in body.updates.iter().enumerate() {
        if let Err(mut e) = entry.update.validate(Some(i)) {
            errors.append(&mut e);
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }

    for (i, entry) in body.updates.iter().enumerate() {
        apply_update(&state, entry.id, &entry.update).await.map_err(|err| match err {
            // Point the failure at the entry that caused it; without the index an
            // operator changing eight ports cannot tell which one refused.
            ApiError::Conflict(msg) => {
                ApiError::Validation(vec![FieldError::new(format!("updates.{i}"), msg)])
            }
            other => other,
        })?;
    }

    tracing::info!(actor = %identity.username, "bulk port update applied");
    list(State(state), Auth(identity)).await
}

/// Performs one validated update.
async fn apply_update(state: &AppState, id: Id, update: &PortUpdate) -> ApiResult<()> {
    if let Some(name) = &update.name {
        ports::rename(state.store.pool(), id, name.trim())
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("port {id}")))?;
    }
    if let Some(mode) = update.mode {
        state.ports.set_mode(id, mode).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reservations
// ---------------------------------------------------------------------------

/// A request to hold a port.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReserveRequest {
    /// Why, shown to other operators who find the port held.
    #[serde(default)]
    pub note: String,
    /// How long to hold it. Defaults to eight hours.
    #[serde(default)]
    pub hours: Option<i64>,
}

/// Takes or extends a hold on a port.
#[tracing::instrument(skip(state, body), fields(port_id = %id, user = %identity.username))]
async fn reserve(
    State(state): State<AppState>,
    OperatorAuth(identity): OperatorAuth,
    Path(id): Path<Id>,
    Json(body): Json<ReserveRequest>,
) -> ApiResult<Json<ReservationView>> {
    let hours = body.hours.unwrap_or(DEFAULT_RESERVATION_HOURS);
    if !(1..=MAX_RESERVATION_HOURS).contains(&hours) {
        return Err(ApiError::field(
            "hours",
            format!("must be between 1 and {MAX_RESERVATION_HOURS}"),
        ));
    }
    if body.note.chars().count() > 200 {
        return Err(ApiError::field("note", "must be at most 200 characters"));
    }

    if ports::get(state.store.pool(), id).await?.is_none() {
        return Err(ApiError::NotFound(format!("port {id}")));
    }

    let expires_at = OffsetDateTime::now_utc() + Duration::hours(hours);

    match reservations::reserve(state.store.pool(), id, identity.user_id, &body.note, expires_at)
        .await
    {
        Ok(reservation) => {
            tracing::info!(%expires_at, "port reserved");
            Ok(Json(reservation))
        }
        // The `ON CONFLICT ... WHERE` clause matches no row when someone else
        // holds the port, so the statement returns nothing rather than erroring.
        Err(sqlx::Error::RowNotFound) => {
            let holder = reservations::get_for_port(state.store.pool(), id).await?;
            Err(ApiError::Conflict(match holder {
                Some(r) => format!("port is reserved by {} until {}", r.username, r.expires_at),
                None => "port could not be reserved".into(),
            }))
        }
        Err(err) => Err(err.into()),
    }
}

/// Releases a hold.
#[tracing::instrument(skip(state), fields(port_id = %id, user = %identity.username))]
async fn release(
    State(state): State<AppState>,
    OperatorAuth(identity): OperatorAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<ReleaseResponse>> {
    let released =
        reservations::release(state.store.pool(), id, identity.user_id, identity.role).await?;

    if !released {
        // Distinguish "nothing to release" from "not yours to release", because
        // the fix is different: one is a stale UI, the other needs an admin.
        return match reservations::get_for_port(state.store.pool(), id).await? {
            Some(r) => Err(ApiError::Conflict(format!(
                "port is reserved by {}; only they or an admin can release it",
                r.username
            ))),
            None => Err(ApiError::NotFound(format!("reservation for port {id}"))),
        };
    }

    tracing::info!("port reservation released");
    Ok(Json(ReleaseResponse { released: true }))
}

/// Result of releasing a hold.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResponse {
    /// Always true; the failure cases are error responses.
    pub released: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An update carrying just a name.
    fn named(name: &str) -> PortUpdate {
        PortUpdate { name: Some(name.into()), mode: None }
    }

    #[test]
    fn a_port_name_must_be_usable_as_an_identifier() {
        assert!(named("ens1f0").validate(None).is_ok());
        assert!(named("dut-uplink_1.a").validate(None).is_ok());

        for bad in ["", "   ", "has space", "semi;colon", "quote'", "slash/es"] {
            assert!(named(bad).validate(None).is_err(), "should have rejected {bad:?}");
        }
        assert!(named(&"x".repeat(65)).validate(None).is_err());
    }

    #[test]
    fn an_update_that_changes_nothing_is_rejected() {
        let empty = PortUpdate { name: None, mode: None };
        let errs = empty.validate(None).unwrap_err();
        assert_eq!(errs[0].path, "name");
    }

    #[test]
    fn a_mode_only_update_is_accepted() {
        let update = PortUpdate { name: None, mode: Some(PortMode::Dpdk) };
        assert!(update.validate(None).is_ok());
    }

    #[test]
    fn bulk_validation_errors_are_addressed_to_their_own_row() {
        let errs = named("bad name").validate(Some(3)).unwrap_err();
        assert_eq!(errs[0].path, "updates.3.name");
    }
}
