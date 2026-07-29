//! Port group endpoints.
//!
//! A group is the unit an engine instance is launched over. Milestone 1 manages
//! the grouping and its configuration; milestone 2 attaches the engine lifecycle
//! to it.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::config::{EngineInstanceConfig, Validate, Validation};
use flux_core::types::{EngineMode, Id, PortGroupState};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Auth, Json};
use crate::state::AppState;
use crate::store::models::PortGroup;
use crate::store::{is_unique_violation, port_groups, ports};

/// Mounts the port group routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(get_one).put(update).delete(delete))
        .route("/{id}/start", post(start))
        .route("/{id}/stop", post(stop))
}

/// Brings up the engine instance for a group.
///
/// Runs launch an engine on demand, so this exists for the case an operator
/// wants to know a group works — and for relaunching one that has failed —
/// without committing to a test.
#[tracing::instrument(skip(state), fields(group_id = %id))]
async fn start(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<PortGroupView>> {
    let group = port_groups::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("port group {id}")))?;

    if let Some(existing) = state.engines.get(id).await {
        if existing.is_ready() {
            return Err(ApiError::Conflict("this group already has a running engine".into()));
        }
        // Anything other than ready is stale; replace it rather than refusing
        // to relaunch a group whose engine has already gone away.
        state.engines.remove(id).await;
    }

    let member_ids = port_groups::member_ids(state.store.pool(), id).await?;
    let members = ports::get_many_ordered(state.store.pool(), &member_ids).await?;
    if members.is_empty() {
        return Err(ApiError::Conflict("this group has no member ports".into()));
    }

    let instance: flux_core::config::EngineInstanceConfig =
        serde_json::from_value(group.trex_cfg.clone()).unwrap_or_default();

    port_groups::set_state(state.store.pool(), id, PortGroupState::Starting, None).await?;

    let launched = crate::engine::launch::launch(
        &state.config,
        crate::engine::launch::LaunchRequest {
            group_id: id,
            mode: group.engine_mode,
            pci_addrs: members.iter().map(|p| p.pci_addr.clone()).collect(),
            numa_node: members
                .first()
                .and_then(|p| p.numa_node)
                .and_then(|n| u32::try_from(n).ok()),
            instance,
        },
    )
    .await;

    let launched = match launched {
        Ok(launched) => launched,
        Err(err) => {
            // The failure is recorded on the group so an operator sees it on the
            // ports page rather than only in this response.
            port_groups::set_state(
                state.store.pool(),
                id,
                PortGroupState::Error,
                Some(&err.to_string()),
            )
            .await?;
            return Err(ApiError::Conflict(format!("could not start the engine: {err}")));
        }
    };

    // Wait for the first health check before reporting the group ready.
    let mut engine_state = launched.handle.watch_state();
    while *engine_state.borrow() == crate::engine::EngineState::Starting {
        if engine_state.changed().await.is_err() {
            break;
        }
    }

    let (group_state, error) = match launched.handle.state() {
        crate::engine::EngineState::Ready => (PortGroupState::Ready, None),
        other => (PortGroupState::Error, Some(format!("{other:?}"))),
    };

    if let Some(controls) = launched.controls {
        state.mock_controls.write().await.insert(id, controls);
    }
    state.engines.insert(launched.handle).await;

    let group = port_groups::set_state(state.store.pool(), id, group_state, error.as_deref())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("port group {id}")))?;

    tracing::info!(actor = %actor.username, state = %group_state, "port group engine started");
    Ok(Json(PortGroupView { group, port_ids: member_ids }))
}

/// Stops the engine instance for a group.
#[tracing::instrument(skip(state), fields(group_id = %id))]
async fn stop(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<PortGroupView>> {
    // A run holding this group would have its engine pulled out from under it,
    // which does not stop traffic cleanly.
    for run_id in state.runs.active().await {
        if let Some(run) = crate::store::runs::get(state.store.pool(), run_id).await? {
            if !run.state.is_terminal() {
                return Err(ApiError::Conflict(format!(
                    "run {} is still in flight; stop it first",
                    run.test_name
                )));
            }
        }
    }

    if state.engines.remove(id).await.is_none() {
        return Err(ApiError::Conflict("this group has no running engine".into()));
    }
    state.collector.stop(id).await;
    state.mock_controls.write().await.remove(&id);

    let group = port_groups::set_state(state.store.pool(), id, PortGroupState::Stopped, None)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("port group {id}")))?;
    let port_ids = port_groups::member_ids(state.store.pool(), id).await?;

    tracing::info!(actor = %actor.username, "port group engine stopped");
    Ok(Json(PortGroupView { group, port_ids }))
}

/// A group with its member ports resolved.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortGroupView {
    /// The group.
    #[serde(flatten)]
    pub group: PortGroup,
    /// Member port ids, in engine port-number order.
    #[schema(value_type = Vec<String>)]
    pub port_ids: Vec<Id>,
}

/// Every group with its membership.
async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<PortGroupView>>> {
    let groups = port_groups::list(state.store.pool()).await?;

    let mut views = Vec::with_capacity(groups.len());
    for group in groups {
        let port_ids = port_groups::member_ids(state.store.pool(), group.id).await?;
        views.push(PortGroupView { group, port_ids });
    }
    Ok(Json(views))
}

/// One group.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<PortGroupView>> {
    let group = port_groups::get(state.store.pool(), id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("port group {id}")))?;
    let port_ids = port_groups::member_ids(state.store.pool(), id).await?;
    Ok(Json(PortGroupView { group, port_ids }))
}

/// The body for creating or replacing a group.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortGroupInput {
    /// Operator-assigned label.
    pub name: String,
    /// Stateless or stateful engine personality.
    pub engine_mode: EngineMode,
    /// Engine instance settings. Defaulted when omitted.
    #[serde(default)]
    pub trex_cfg: Option<EngineInstanceConfig>,
    /// Member ports, in the order they should be numbered by the engine.
    #[serde(default)]
    #[schema(value_type = Vec<String>)]
    pub port_ids: Vec<Id>,
}

impl PortGroupInput {
    /// Checks the request in isolation.
    fn validate(&self) -> Result<(), Vec<flux_core::config::FieldError>> {
        let mut v = Validation::new();

        let name = self.name.trim();
        v.require(!name.is_empty(), "name", "must not be empty");
        v.require(name.chars().count() <= 64, "name", "must be at most 64 characters");

        // TRex takes ports in pairs; an odd member count leaves one with nothing
        // to receive from, which fails at engine start rather than here.
        v.require(
            self.port_ids.len() % 2 == 0,
            "portIds",
            "a group must contain an even number of ports",
        );

        let mut seen = std::collections::HashSet::new();
        v.require(
            self.port_ids.iter().all(|id| seen.insert(*id)),
            "portIds",
            "a port may appear only once in a group",
        );

        if let Some(cfg) = &self.trex_cfg {
            v.scope("trexCfg", |v| cfg.validate_into(v));
        }

        v.finish()
    }
}

/// Creates a group and assigns its members.
#[tracing::instrument(skip_all, fields(name = %body.name))]
async fn create(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Json(body): Json<PortGroupInput>,
) -> ApiResult<Json<PortGroupView>> {
    body.validate()?;
    ensure_ports_are_assignable(&state, &body.port_ids, None).await?;

    let cfg = serde_json::to_value(body.trex_cfg.clone().unwrap_or_default())
        .map_err(|e| ApiError::Internal(e.into()))?;

    let group = port_groups::create(state.store.pool(), body.name.trim(), body.engine_mode, &cfg)
        .await
        .map_err(name_conflict)?;

    port_groups::set_members(state.store.pool(), group.id, &body.port_ids).await?;

    tracing::info!(actor = %actor.username, group_id = %group.id, "port group created");
    Ok(Json(PortGroupView { group, port_ids: body.port_ids }))
}

/// Replaces a group's definition and membership.
#[tracing::instrument(skip_all, fields(group_id = %id))]
async fn update(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
    Json(body): Json<PortGroupInput>,
) -> ApiResult<Json<PortGroupView>> {
    body.validate()?;
    ensure_ports_are_assignable(&state, &body.port_ids, Some(id)).await?;

    let cfg = serde_json::to_value(body.trex_cfg.clone().unwrap_or_default())
        .map_err(|e| ApiError::Internal(e.into()))?;

    let group =
        port_groups::update(state.store.pool(), id, body.name.trim(), body.engine_mode, &cfg)
            .await
            .map_err(name_conflict)?
            .ok_or_else(|| ApiError::NotFound(format!("port group {id}")))?;

    port_groups::set_members(state.store.pool(), id, &body.port_ids).await?;

    tracing::info!(actor = %actor.username, "port group updated");
    Ok(Json(PortGroupView { group, port_ids: body.port_ids }))
}

/// Deletes a group, leaving its ports ungrouped.
#[tracing::instrument(skip(state), fields(group_id = %id))]
async fn delete(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    if !port_groups::delete(state.store.pool(), id).await? {
        return Err(ApiError::NotFound(format!("port group {id}")));
    }
    tracing::info!(actor = %actor.username, "port group deleted");
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Refuses membership that names a missing port or one another group holds.
///
/// `owner` is the group being edited, whose own existing members are of course
/// allowed to stay.
async fn ensure_ports_are_assignable(
    state: &AppState,
    port_ids: &[Id],
    owner: Option<Id>,
) -> ApiResult<()> {
    let mut errors = Vec::new();

    for (i, port_id) in port_ids.iter().enumerate() {
        match ports::get(state.store.pool(), *port_id).await? {
            None => errors.push(flux_core::config::FieldError::new(
                format!("portIds.{i}"),
                format!("no port with id {port_id}"),
            )),
            Some(port) => {
                if let Some(existing) = port.group_id {
                    if Some(existing) != owner {
                        errors.push(flux_core::config::FieldError::new(
                            format!("portIds.{i}"),
                            format!("port {} already belongs to another group", port.name),
                        ));
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// Turns a duplicate-name insert into a field-level conflict.
fn name_conflict(err: sqlx::Error) -> ApiError {
    if is_unique_violation(&err) {
        ApiError::field("name", "a port group with that name already exists")
    } else {
        err.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid input with the given members.
    fn input(port_ids: Vec<Id>) -> PortGroupInput {
        PortGroupInput {
            name: "pair-a".into(),
            engine_mode: EngineMode::Stl,
            trex_cfg: None,
            port_ids,
        }
    }

    #[test]
    fn a_group_must_hold_an_even_number_of_ports() {
        assert!(input(vec![]).validate().is_ok());
        assert!(input(vec![Id::new_v4(), Id::new_v4()]).validate().is_ok());

        let errs = input(vec![Id::new_v4()]).validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "portIds"));
    }

    #[test]
    fn a_port_may_not_be_listed_twice_in_one_group() {
        let id = Id::new_v4();
        let errs = input(vec![id, id]).validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "portIds" && e.msg.contains("only once")));
    }

    #[test]
    fn engine_config_failures_are_reported_under_their_own_prefix() {
        let mut body = input(vec![]);
        body.trex_cfg =
            Some(EngineInstanceConfig { rpc_port: 4501, async_port: 4501, ..Default::default() });
        let errs = body.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "trexCfg.asyncPort"),
            "expected a nested path, got {errs:?}"
        );
    }

    #[test]
    fn a_blank_name_is_rejected() {
        let mut body = input(vec![]);
        body.name = "   ".into();
        let errs = body.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "name"));
    }
}
