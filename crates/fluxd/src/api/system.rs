//! System endpoints: health, hugepages, and appliance settings.
//!
//! `/system/health` is the one endpoint that answers "is this appliance ready to
//! run a test", so it reaches every dependency rather than reporting only that
//! the HTTP server is up. A health check that cannot fail is not a health check.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::Router;
use flux_core::port::{HugepageSize, HugepagesStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Auth, Json};
use crate::config::{EngineBackend, PortdBackend};
use crate::state::AppState;
use crate::store::models::Setting;
use crate::store::{ports, settings};

/// Version of the running daemon, stamped in at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Mounts the system routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/hugepages", get(hugepages_status).post(hugepages_setup))
}

/// Mounts the settings routes.
pub fn settings_router() -> Router<AppState> {
    Router::new().route("/", get(list_settings)).route("/{key}", get(get_setting).put(put_setting))
}

/// The appliance health report.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// Daemon version.
    pub version: String,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
    /// True when nothing is degraded and a test could be started.
    pub healthy: bool,
    /// True when neither the engine nor the port layer touches real hardware.
    pub mocked: bool,
    /// Packet engine status.
    pub engine: SubsystemHealth,
    /// Privileged helper status.
    pub portd: SubsystemHealth,
    /// Database status.
    pub database: SubsystemHealth,
    /// Hugepage allocation, when the port layer could report it.
    pub hugepages: Option<HugepagesStatus>,
    /// Port counts by state.
    pub ports: PortCounts,
    /// Filesystem usage.
    pub disks: Vec<DiskUsage>,
    /// Physical memory in bytes.
    pub memory_total_bytes: u64,
    /// Physical memory not in use, in bytes.
    pub memory_available_bytes: u64,
}

/// One dependency's status.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubsystemHealth {
    /// Which implementation is configured, e.g. `mock` or `trex`.
    pub backend: String,
    /// Whether it answered.
    pub ok: bool,
    /// Why not, when it did not.
    pub detail: Option<String>,
}

impl SubsystemHealth {
    /// A healthy subsystem.
    fn ok(backend: impl Into<String>) -> Self {
        Self { backend: backend.into(), ok: true, detail: None }
    }

    /// A failing subsystem.
    fn failed(backend: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { backend: backend.into(), ok: false, detail: Some(detail.into()) }
    }
}

/// Port counts for the dashboard health cards.
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortCounts {
    /// Ports present in the chassis.
    pub total: i64,
    /// Ports with carrier.
    pub up: i64,
    /// Ports without carrier.
    pub down: i64,
    /// Ports whose carrier state is not currently observable.
    pub unknown: i64,
}

/// One filesystem's usage.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    /// Where it is mounted.
    pub mount: String,
    /// Capacity in bytes.
    pub total_bytes: u64,
    /// Free space in bytes.
    pub available_bytes: u64,
}

/// Reports the state of every dependency a test run needs.
async fn health(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Health>> {
    let database = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(state.store.pool())
        .await
    {
        Ok(_) => SubsystemHealth::ok("postgres"),
        Err(err) => SubsystemHealth::failed("postgres", err.to_string()),
    };

    let portd_backend = match state.config.portd {
        PortdBackend::Mock => "mock",
        PortdBackend::Unix => "unix",
    };
    let (portd, hugepages) = match state.ports.hugepages_status().await {
        Ok(status) => (SubsystemHealth::ok(portd_backend), Some(status)),
        Err(err) => (SubsystemHealth::failed(portd_backend, err.to_string()), None),
    };

    // Milestone 1 has no engine instances yet. The mock backend is reported ready
    // because it always is; the real backend reports what it is, not a guess.
    let engine = match state.config.engine {
        EngineBackend::Mock => SubsystemHealth::ok("mock"),
        EngineBackend::Trex => SubsystemHealth::failed(
            "trex",
            "no engine instances are running; start a port group",
        ),
    };

    let counts = port_counts(&state).await?;
    let (disks, memory_total_bytes, memory_available_bytes) = host_resources();

    // Health is about whether a test could run. The engine is deliberately left
    // out: with no port group started there is nothing for it to report, and
    // failing the whole appliance for that would make the dashboard cry wolf.
    let healthy = database.ok && portd.ok;

    Ok(Json(Health {
        version: VERSION.to_string(),
        uptime_secs: state.uptime_secs(),
        healthy,
        mocked: state.config.is_fully_mocked(),
        engine,
        portd,
        database,
        hugepages,
        ports: counts,
        disks,
        memory_total_bytes,
        memory_available_bytes,
    }))
}

/// Tallies present ports by link state.
async fn port_counts(state: &AppState) -> ApiResult<PortCounts> {
    let mut counts = PortCounts::default();
    for (link_state, n) in ports::count_by_link_state(state.store.pool()).await? {
        counts.total += n;
        match link_state.as_str() {
            "up" => counts.up += n,
            "down" => counts.down += n,
            _ => counts.unknown += n,
        }
    }
    Ok(counts)
}

/// Reads filesystem and memory usage from the host.
///
/// Runs inline because both are cheap reads of already-collected kernel state.
fn host_resources() -> (Vec<DiskUsage>, u64, u64) {
    let disks = sysinfo::Disks::new_with_refreshed_list()
        .list()
        .iter()
        .map(|disk| DiskUsage {
            mount: disk.mount_point().to_string_lossy().into_owned(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
        })
        .collect();

    let mut system = sysinfo::System::new();
    system.refresh_memory();

    (disks, system.total_memory(), system.available_memory())
}

/// Reads the hugepage allocation.
async fn hugepages_status(
    State(state): State<AppState>,
    _auth: Auth,
) -> ApiResult<Json<HugepagesStatus>> {
    Ok(Json(state.ports.hugepages_status().await?))
}

/// A hugepage allocation request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HugepagesRequest {
    /// Number of pages to allocate.
    pub count: u64,
    /// Page size class.
    pub size: HugepageSize,
}

/// Requests a hugepage allocation.
#[tracing::instrument(skip(state), fields(count = body.count, size = %body.size))]
async fn hugepages_setup(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Json(body): Json<HugepagesRequest>,
) -> ApiResult<Json<HugepagesStatus>> {
    // An unbounded count would let an admin ask the kernel to hand every page of
    // physical memory to the hugepage pool and wedge the appliance.
    if body.count > 1024 {
        return Err(ApiError::field("count", "must be at most 1024 pages"));
    }

    let status = state.ports.hugepages_setup(body.count, body.size).await?;
    tracing::info!(actor = %actor.username, "hugepage allocation changed");
    Ok(Json(status))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Every setting.
async fn list_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> ApiResult<Json<Vec<Setting>>> {
    Ok(Json(settings::list(state.store.pool()).await?))
}

/// One setting.
async fn get_setting(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(key): Path<String>,
) -> ApiResult<Json<Setting>> {
    settings::get(state.store.pool(), &key)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("setting {key}")))
}

/// Writes a setting.
#[tracing::instrument(skip(state, value), fields(%key))]
async fn put_setting(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(key): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<Setting>> {
    if key.trim().is_empty() || key.len() > 64 {
        return Err(ApiError::field("key", "must be between 1 and 64 characters"));
    }

    let setting = settings::put(state.store.pool(), &key, &value, Some(actor.user_id)).await?;
    tracing::info!(actor = %actor.username, "setting updated");
    Ok(Json(setting))
}
