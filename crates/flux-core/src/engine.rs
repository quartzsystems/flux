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

use crate::flow::ModifierMode;
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

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

/// One programmed stream, in an engine-agnostic form.
///
/// This sits deliberately between the flow document and TRex's stream schema.
/// The orchestrator's translator produces it from a [`FlowConfig`], and each
/// engine renders it into whatever its own protocol wants — TRex into JSON-RPC,
/// the mock into a rate to simulate.
///
/// Passing engine-native JSON straight through would have been less code, but it
/// would leave the mock unable to know what rate it was asked for, which is the
/// one thing the mock exists to simulate.
///
/// [`FlowConfig`]: crate::flow::FlowConfig
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSpec {
    /// Packet group this stream's statistics are attributed to.
    pub pg_id: PgId,
    /// The frame as it goes on the wire, excluding the FCS the NIC appends.
    pub packet: Vec<u8>,
    /// On-wire frame length including FCS, for rate accounting.
    pub wire_len: u32,
    /// Frames per second at multiplier 1.0.
    pub pps: f64,
    /// Fields varied across generated frames, resolved to byte offsets.
    #[serde(default)]
    pub modifiers: Vec<StreamModifier>,
    /// Whether frames carry a latency timestamp.
    #[serde(default)]
    pub latency: bool,
    /// Stop after this many frames. `None` transmits until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_packets: Option<u64>,
}

/// A field varied across a stream's frames, resolved to a concrete position.
///
/// The flow document names fields symbolically (`ipv4_src`); by the time a
/// stream is programmed, the translator has turned that into an offset and a
/// width, because that is what both engines actually apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamModifier {
    /// Byte offset into the frame.
    pub offset: u16,
    /// Field width in bytes. One, two, or four.
    pub width: u8,
    /// How the value walks its range.
    pub mode: ModifierMode,
    /// First value in the range.
    pub min: u64,
    /// Last value in the range.
    pub max: u64,
    /// Distance between consecutive values.
    pub step: u64,
}

// ---------------------------------------------------------------------------
// Stateful (ASTF) traffic
// ---------------------------------------------------------------------------

/// A programmed L4-7 load, in an engine-agnostic form.
///
/// Stands in the same relationship to [`LoadProfileConfig`] as [`StreamSpec`]
/// does to `FlowConfig`: the orchestrator's translator produces it, and each
/// engine renders it into whatever its own protocol wants.
///
/// [`LoadProfileConfig`]: crate::profile::LoadProfileConfig
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AstfProfile {
    /// Engine port the emulated clients sit behind.
    pub client_port: EnginePortId,
    /// Engine port the emulated servers sit behind.
    pub server_port: EnginePortId,
    /// Client address block, in CIDR form.
    pub client_cidr: String,
    /// Server address block, in CIDR form.
    pub server_cidr: String,
    /// Lowest client source port.
    pub client_port_min: u16,
    /// Highest client source port.
    pub client_port_max: u16,
    /// The destination port servers listen on.
    pub server_listen_port: u16,
    /// Bytes the client sends per connection.
    pub request_bytes: u32,
    /// Bytes the server returns per connection.
    pub response_bytes: u32,
    /// Connections per second once warmed up.
    pub target_cps: f64,
    /// Ceiling on simultaneously open connections.
    pub max_concurrent: u64,
    /// Seconds spent climbing to the target rate.
    pub warmup_secs: f64,
    /// A capture to replay instead of the synthetic exchange, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcap_ref: Option<String>,
}

/// Connection-level counters, cumulative since the last clear.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AstfStats {
    /// Connections the clients attempted.
    pub attempted: u64,
    /// Connections that completed their handshake.
    pub established: u64,
    /// Connections that closed cleanly.
    pub closed: u64,
    /// Connections currently open.
    pub active: u64,
    /// Handshakes that never completed.
    pub connect_errors: u64,
    /// Connections reset by either side.
    pub resets: u64,
    /// Application bytes sent by the clients.
    pub tx_bytes: u64,
    /// Application bytes received by the clients.
    pub rx_bytes: u64,
}

impl AstfStats {
    /// Fraction of attempted connections that failed to establish.
    ///
    /// Returns `0.0` when nothing was attempted, which is the meaningful answer
    /// for a profile that has not started rather than a NaN.
    pub fn failure_pct(&self) -> f64 {
        if self.attempted == 0 {
            return 0.0;
        }
        (self.connect_errors as f64 / self.attempted as f64) * 100.0
    }
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
    async fn add_streams(
        &self,
        port: EnginePortId,
        streams: Vec<StreamSpec>,
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

    // -----------------------------------------------------------------------
    // Stateful mode
    //
    // These have default implementations that refuse, because a stateless
    // instance genuinely cannot do them. Making them part of this trait rather
    // than a second one keeps a single handle type and a single registry: a
    // port group is one instance in one mode, and the mode is a property of the
    // instance rather than of the code that holds it.
    // -----------------------------------------------------------------------

    /// Programs a stateful load.
    async fn load_astf_profile(&self, _profile: AstfProfile) -> Result<(), EngineError> {
        Err(EngineError::Rejected(
            "this engine instance is stateless; create the port group with engine mode `astf`"
                .into(),
        ))
    }

    /// Starts the programmed stateful load.
    async fn start_astf(&self, _duration_secs: Option<f64>) -> Result<(), EngineError> {
        Err(EngineError::Rejected("this engine instance is stateless".into()))
    }

    /// Stops the stateful load.
    async fn stop_astf(&self) -> Result<(), EngineError> {
        Err(EngineError::Rejected("this engine instance is stateless".into()))
    }

    /// Reads connection-level counters.
    async fn astf_stats(&self) -> Result<AstfStats, EngineError> {
        Err(EngineError::Rejected("this engine instance is stateless".into()))
    }
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
