//! The packet-engine boundary.
//!
//! Everything `fluxd` does to generate traffic goes through [`Engine`]. Two
//! implementations exist: `MockEngine`, which simulates ports and emits
//! plausible synthetic statistics, and `TrexEngine`, which speaks TRex's
//! JSON-RPC-over-ZMQ protocol and supervises the TRex process.
//!
//! The trait is defined here, in a crate with no ZMQ or DPDK dependency, so the
//! orchestrator and collector can be written and tested against the mock without
//! any of that machinery being present. Implementations land in milestone 2;
//! milestone 1 only needs the vocabulary to exist so the daemon can report
//! engine state on the dashboard.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::EngineMode;

/// Index of a port *within an engine instance*.
///
/// TRex numbers the ports it owns from zero in the order they appear in its
/// config file. This is deliberately not the database port id: the mapping
/// between the two lives in the port group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct EnginePortId(pub u8);

impl std::fmt::Display for EnginePortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A packet-group id, used to attribute per-stream statistics.
///
/// TRex tracks flow statistics per pgid; the orchestrator allocates one per
/// stream so tx/rx/loss can be reconciled per flow rather than per port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct PgId(pub u32);

/// Liveness and identity of an engine instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineHealth {
    /// Whether the instance is answering RPCs.
    pub connected: bool,
    /// Engine version string, e.g. `v3.06`.
    pub version: Option<String>,
    /// Mode the instance was started in.
    pub mode: EngineMode,
    /// Number of ports the instance owns.
    pub port_count: u8,
    /// Seconds since the instance started, when known.
    pub uptime_secs: Option<u64>,
}

/// Per-port state as reported by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnginePortStatus {
    /// Port index within the instance.
    pub port: EnginePortId,
    /// Whether this daemon holds the exclusive lock on the port.
    pub owned: bool,
    /// Carrier state as the poll-mode driver sees it.
    pub link_up: bool,
    /// Negotiated speed in megabits per second.
    pub speed_mbps: Option<i32>,
    /// True while the port is transmitting.
    pub transmitting: bool,
    /// Source MAC the engine will use for generated frames.
    pub src_mac: Option<String>,
}

/// A snapshot of one port's counters at a point in time.
///
/// Counters are cumulative since the last `clear_stats`; the collector converts
/// them to rates by differencing consecutive samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortStats {
    /// Frames transmitted.
    pub tx_packets: u64,
    /// Frames received.
    pub rx_packets: u64,
    /// Bytes transmitted, on the wire.
    pub tx_bytes: u64,
    /// Bytes received, on the wire.
    pub rx_bytes: u64,
    /// Transmit errors.
    pub tx_errors: u64,
    /// Receive errors, including CRC failures.
    pub rx_errors: u64,
    /// Frames the driver dropped because the receive ring was full.
    pub rx_dropped: u64,
}

/// Latency histogram summary for one packet group.
///
/// All values are microseconds. `None` means the stream was not latency-tracked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    /// Smallest observed one-way latency.
    pub min_us: Option<f64>,
    /// Mean observed latency.
    pub avg_us: Option<f64>,
    /// Largest observed latency.
    pub max_us: Option<f64>,
    /// Median.
    pub p50_us: Option<f64>,
    /// 99th percentile.
    pub p99_us: Option<f64>,
    /// 99.9th percentile.
    pub p999_us: Option<f64>,
    /// Inter-packet delay variation.
    pub jitter_us: Option<f64>,
}

/// Counters attributed to a single packet group, i.e. a single flow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PgidStats {
    /// Frames transmitted for this group.
    pub tx_packets: u64,
    /// Frames received for this group.
    pub rx_packets: u64,
    /// Bytes transmitted for this group.
    pub tx_bytes: u64,
    /// Bytes received for this group.
    pub rx_bytes: u64,
    /// Latency summary, when the stream carries a latency tag.
    pub latency: LatencyStats,
}

impl PgidStats {
    /// Frames sent but never received, saturating at zero.
    ///
    /// Receive counts can briefly exceed transmit counts when a sample lands
    /// mid-flight, so this saturates rather than underflowing.
    pub fn lost_packets(&self) -> u64 {
        self.tx_packets.saturating_sub(self.rx_packets)
    }

    /// Loss as a percentage of transmitted frames.
    ///
    /// Returns `0.0` when nothing was transmitted, which is the meaningful
    /// answer for a trial that never started rather than a NaN.
    pub fn loss_pct(&self) -> f64 {
        if self.tx_packets == 0 {
            return 0.0;
        }
        (self.lost_packets() as f64 / self.tx_packets as f64) * 100.0
    }
}

/// Failures the engine boundary can produce.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine process is not running or not answering.
    #[error("engine is not available: {0}")]
    Unavailable(String),

    /// Another client holds the port lock.
    #[error("port {0} is held by another client")]
    NotOwned(EnginePortId),

    /// The engine rejected the request.
    #[error("engine rejected the request: {0}")]
    Rejected(String),

    /// The engine did not answer inside the deadline.
    #[error("engine call timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The engine answered with something we could not parse.
    #[error("malformed engine response: {0}")]
    Protocol(String),
}

/// Options controlling a traffic start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    /// Multiplier applied to the configured stream rates. `1.0` means run the
    /// streams exactly as specified; RFC 2544 drives its binary search through
    /// this field rather than by rewriting streams every trial.
    pub multiplier: f64,
    /// Stop automatically after this many seconds. `None` runs until stopped.
    pub duration_secs: Option<f64>,
    /// Refuse to start if the requested rate exceeds port line rate.
    pub force: bool,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self { multiplier: 1.0, duration_secs: None, force: false }
    }
}

/// The packet generator, as the rest of the daemon sees it.
///
/// Implementations must be safe to call from a single owning task only: engine
/// access is serialised behind an actor task because a ZMQ socket cannot be
/// shared. The trait is `Send + Sync` so the handle can live in shared state,
/// not so that concurrent calls are legal.
#[async_trait::async_trait]
pub trait Engine: Send + Sync + 'static {
    /// Liveness probe. Cheap enough to call on every dashboard refresh.
    async fn health(&self) -> Result<EngineHealth, EngineError>;

    /// Reads per-port state for every port the instance owns.
    async fn port_status(&self) -> Result<Vec<EnginePortStatus>, EngineError>;

    /// Takes the exclusive lock on `ports`.
    async fn acquire(&self, ports: &[EnginePortId], force: bool) -> Result<(), EngineError>;

    /// Releases the exclusive lock on `ports`.
    async fn release(&self, ports: &[EnginePortId]) -> Result<(), EngineError>;

    /// Removes every stream currently programmed on `port`.
    async fn clear_streams(&self, port: EnginePortId) -> Result<(), EngineError>;

    /// Programs `streams` onto `port`.
    ///
    /// `streams` is the engine-native representation produced by
    /// `orch::translate`; it stays opaque here so this crate need not model
    /// TRex's stream schema.
    async fn add_streams(
        &self,
        port: EnginePortId,
        streams: Vec<serde_json::Value>,
    ) -> Result<(), EngineError>;

    /// Starts transmitting on `ports`.
    async fn start_traffic(
        &self,
        ports: &[EnginePortId],
        opts: StartOptions,
    ) -> Result<(), EngineError>;

    /// Stops transmitting on `ports`.
    async fn stop_traffic(&self, ports: &[EnginePortId]) -> Result<(), EngineError>;

    /// Zeroes the counters for `ports`. Called at the start of every trial.
    async fn clear_stats(&self, ports: &[EnginePortId]) -> Result<(), EngineError>;

    /// Reads cumulative counters for `ports`, in the order requested.
    async fn port_stats(&self, ports: &[EnginePortId])
        -> Result<Vec<PortStats>, EngineError>;

    /// Reads per-packet-group counters for `pgids`, in the order requested.
    async fn pgid_stats(&self, pgids: &[PgId]) -> Result<Vec<PgidStats>, EngineError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_percentage_is_computed_against_transmitted_frames() {
        let s = PgidStats { tx_packets: 1000, rx_packets: 990, ..Default::default() };
        assert_eq!(s.lost_packets(), 10);
        assert!((s.loss_pct() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_trial_that_sent_nothing_reports_zero_loss_not_nan() {
        let s = PgidStats::default();
        assert_eq!(s.loss_pct(), 0.0);
        assert!(s.loss_pct().is_finite());
    }

    #[test]
    fn receiving_more_than_sent_saturates_instead_of_underflowing() {
        // Happens when a stats poll lands between the last tx and its rx.
        let s = PgidStats { tx_packets: 100, rx_packets: 105, ..Default::default() };
        assert_eq!(s.lost_packets(), 0);
        assert_eq!(s.loss_pct(), 0.0);
    }

    #[test]
    fn default_start_options_run_streams_as_configured() {
        let o = StartOptions::default();
        assert_eq!(o.multiplier, 1.0);
        assert!(o.duration_secs.is_none());
        assert!(!o.force);
    }
}
