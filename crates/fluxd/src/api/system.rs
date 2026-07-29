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
    /// How many engine instances are running.
    pub engine_instances: usize,
    /// How many engine instances are being polled for statistics.
    pub collectors_active: usize,
    /// How many runs are in flight.
    pub active_runs: usize,
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

    let engine = engine_health(&state).await;

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
        engine_instances: state.engines.len().await,
        collectors_active: state.collector.active_count().await,
        active_runs: state.runs.active().await.len(),
    }))
}

/// Summarises the running engine instances.
///
/// No instances is not a failure: engines are launched on demand when a run
/// starts, so an idle appliance legitimately has none. Reporting that as
/// degraded would make the dashboard cry wolf on every quiet appliance.
async fn engine_health(state: &AppState) -> SubsystemHealth {
    let backend = match state.config.engine {
        EngineBackend::Mock => "mock",
        EngineBackend::Trex => "trex",
    };

    let instances = state.engines.all().await;
    if instances.is_empty() {
        return SubsystemHealth {
            backend: backend.to_string(),
            ok: true,
            detail: Some("no engine instances running".into()),
        };
    }

    let mut versions: Vec<String> = Vec::new();
    let mut problems = Vec::new();

    for handle in &instances {
        match handle.state() {
            crate::engine::EngineState::Ready => {
                // Ask the instance itself rather than trusting the cached state:
                // an engine that died since its last command still reads Ready
                // until something tries to talk to it.
                match handle.health().await {
                    Ok(health) => {
                        versions.push(health.version.unwrap_or_else(|| "unknown".into()));
                    }
                    Err(err) => {
                        problems.push(format!("group {}: {err}", handle.group_id));
                    }
                }
            }
            other => problems.push(format!("group {}: {other:?}", handle.group_id)),
        }
    }

    versions.sort();
    versions.dedup();

    SubsystemHealth {
        backend: backend.to_string(),
        ok: problems.is_empty(),
        detail: Some(if problems.is_empty() {
            format!("{} instance(s) ready, {}", instances.len(), versions.join(", "))
        } else {
            problems.join("; ")
        }),
    }
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
