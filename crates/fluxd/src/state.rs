//! Shared application state.
//!
//! Everything here is cheap to clone — pools, registries, and channel senders
//! are internally reference-counted — so axum's `State` extractor hands handlers
//! a clone per request rather than contending on a lock.

use std::collections::HashMap;
use std::sync::Arc;

use flux_core::types::Id;
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::collector::Collector;
use crate::config::Config;
use crate::engine::mock::MockControls;
use crate::engine::EngineRegistry;
use crate::orch::RunSupervisor;
use crate::portmgr::PortManager;
use crate::store::Store;

/// Injectable behaviour for simulated engines, keyed by port group.
///
/// Only populated when `FLUX_ENGINE=mock`. Kept beside the registry rather than
/// inside the engine handle because it is a development affordance, not part of
/// the engine contract — a real TRex has nothing to put here.
pub type MockControlRegistry = Arc<RwLock<HashMap<Id, MockControls>>>;

/// The pieces [`AppState`] is assembled from.
pub struct AppStateParts {
    /// Database access.
    pub store: Store,
    /// Hardware inventory and driver binding.
    pub ports: PortManager,
    /// Running engine instances.
    pub engines: EngineRegistry,
    /// Statistics polling and fan-out.
    pub collector: Collector,
    /// In-flight runs.
    pub runs: RunSupervisor,
    /// Mock engine knobs.
    pub mock_controls: MockControlRegistry,
    /// Shared outbound HTTP client.
    pub http: reqwest::Client,
    /// Immutable daemon configuration.
    pub config: Arc<Config>,
}

/// State every handler can reach.
#[derive(Clone)]
pub struct AppState {
    /// Database access.
    pub store: Store,
    /// Hardware inventory and driver binding.
    pub ports: PortManager,
    /// Running engine instances.
    pub engines: EngineRegistry,
    /// Statistics polling and fan-out.
    pub collector: Collector,
    /// In-flight runs.
    pub runs: RunSupervisor,
    /// Mock engine knobs, for the debug endpoints.
    pub mock_controls: MockControlRegistry,
    /// Shared outbound HTTP client, for the VictoriaMetrics query proxy.
    ///
    /// One client rather than one per request: it holds the connection pool, and
    /// building a fresh one per analytics query would open a new socket for
    /// every point an operator drags a time range over.
    pub http: reqwest::Client,
    /// Immutable daemon configuration.
    pub config: Arc<Config>,
    /// When the daemon started, for the uptime figure on the dashboard.
    pub started_at: OffsetDateTime,
}

impl AppState {
    /// Assembles the state.
    ///
    /// Takes its parts as a struct rather than a positional list: there are
    /// enough of them now that two same-typed arguments could be transposed
    /// without the compiler noticing.
    pub fn new(parts: AppStateParts) -> Self {
        Self {
            store: parts.store,
            ports: parts.ports,
            engines: parts.engines,
            collector: parts.collector,
            runs: parts.runs,
            mock_controls: parts.mock_controls,
            http: parts.http,
            config: parts.config,
            started_at: OffsetDateTime::now_utc(),
        }
    }

    /// Seconds since the daemon started.
    ///
    /// Saturates rather than going negative: a backwards step in the system clock
    /// (NTP settling shortly after boot is the usual cause) must not turn uptime
    /// into a nonsense number.
    pub fn uptime_secs(&self) -> u64 {
        (OffsetDateTime::now_utc() - self.started_at).whole_seconds().max(0) as u64
    }
}
