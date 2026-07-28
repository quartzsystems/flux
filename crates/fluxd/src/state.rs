//! Shared application state.
//!
//! Everything here is cheap to clone — the pool and the port manager are
//! internally reference-counted — so axum's `State` extractor hands handlers a
//! clone per request rather than contending on a lock.

use std::sync::Arc;

use time::OffsetDateTime;

use crate::config::Config;
use crate::portmgr::PortManager;
use crate::store::Store;

/// State every handler can reach.
#[derive(Clone)]
pub struct AppState {
    /// Database access.
    pub store: Store,
    /// Hardware inventory and driver binding.
    pub ports: PortManager,
    /// Immutable daemon configuration.
    pub config: Arc<Config>,
    /// When the daemon started, for the uptime figure on the dashboard.
    pub started_at: OffsetDateTime,
}

impl AppState {
    /// Assembles the state.
    pub fn new(store: Store, ports: PortManager, config: Arc<Config>) -> Self {
        Self { store, ports, config, started_at: OffsetDateTime::now_utc() }
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
