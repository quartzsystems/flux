//! Statistics collection.
//!
//! One task per active engine instance polls it once a second, converts the
//! cumulative counters into rates, and publishes a normalised sample onto a
//! broadcast channel. WebSocket sessions subscribe to that channel and filter by
//! what they asked for; nothing polls per subscriber.
//!
//! ## Rates, not counters
//!
//! Engines report counters that only go up. Everything an operator watches is a
//! rate, so the collector differences consecutive samples against the actual
//! elapsed time between them rather than assuming the interval was exactly one
//! second — under load it will not be, and assuming otherwise makes the charts
//! disagree with the totals.
//!
//! ## Backfill
//!
//! The last ten minutes of samples are kept in a ring buffer. A client that
//! connects mid-run gets them immediately, so its charts render full instead of
//! drawing themselves in from the right over the following ten minutes.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use flux_core::engine::{AstfStats, EnginePortId, LatencyStats, PgId, PgidStats, PortStats};
use flux_core::types::Id;
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use utoipa::ToSchema;

pub mod vm;

/// How often each engine instance is polled.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How many samples the ring buffer holds.
///
/// Ten minutes at one hertz. Long enough to cover an RFC 2544 trial and its
/// neighbours, short enough that the memory is irrelevant.
const HISTORY_DEPTH: usize = 600;

/// How many samples a slow subscriber may fall behind before it is dropped.
///
/// A subscriber that cannot keep up with one message a second is not going to
/// recover, and buffering for it would grow without bound.
const BROADCAST_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// Samples
// ---------------------------------------------------------------------------

/// One second of statistics, for everything currently being collected.
///
/// Serialised straight onto the WebSocket, which is why the field names are
/// camelCase and the maps are keyed by string ids.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsBatch {
    /// Unix timestamp in seconds.
    pub ts: i64,
    /// Per-port rates, keyed by database port id.
    pub ports: BTreeMap<String, PortSample>,
    /// Per-flow rates, keyed by flow id.
    pub streams: BTreeMap<String, StreamSample>,
    /// Progress of the run these samples belong to, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunProgress>,
    /// Connection-level rates, for a stateful run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections: Option<ConnectionSample>,
}

/// Connection-level rates for a stateful load.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSample {
    /// Connections established per second.
    pub cps: f64,
    /// Failed handshakes per second.
    pub errors_per_sec: f64,
    /// Connections currently open.
    pub active: u64,
    /// Cumulative connections attempted.
    pub attempted: u64,
    /// Cumulative connections established.
    pub established: u64,
    /// Cumulative failed handshakes.
    pub connect_errors: u64,
    /// Failed handshakes as a percentage of attempts.
    pub failure_pct: f64,
    /// Application bits per second sent by the clients.
    pub tx_bps: f64,
    /// Application bits per second received by the clients.
    pub rx_bps: f64,
}

/// One port's rates.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortSample {
    /// Frames transmitted per second.
    pub tx_pps: f64,
    /// Frames received per second.
    pub rx_pps: f64,
    /// Bits transmitted per second, at layer 2.
    pub tx_bps: f64,
    /// Bits received per second, at layer 2.
    pub rx_bps: f64,
    /// Cumulative frames transmitted since the last clear.
    pub tx_packets: u64,
    /// Cumulative frames received since the last clear.
    pub rx_packets: u64,
    /// Cumulative transmit errors.
    pub tx_errors: u64,
    /// Cumulative receive errors.
    pub rx_errors: u64,
}

/// One flow's rates.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamSample {
    /// Frames transmitted per second.
    pub tx_pps: f64,
    /// Frames received per second.
    pub rx_pps: f64,
    /// Frames lost per second.
    pub loss_pps: f64,
    /// Loss as a percentage of frames transmitted, cumulative.
    pub loss_pct: f64,
    /// Cumulative frames transmitted.
    pub tx_packets: u64,
    /// Cumulative frames received.
    pub rx_packets: u64,
    /// Latency summary, when the flow is tracked.
    pub latency: LatencyStats,
}

/// Where a run has got to.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunProgress {
    /// Which run.
    pub run_id: String,
    /// Its lifecycle state.
    pub state: String,
    /// Trial number, for tests that iterate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
    /// Frame size under test, for tests that vary it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_size: Option<u32>,
    /// Rate being trialled, as a percentage of line rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_rate_pct: Option<f64>,
    /// Seconds left in the current trial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_remaining_secs: Option<f64>,
    /// Overall completion, 0 to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// Human-readable note about what is happening.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// What to collect
// ---------------------------------------------------------------------------

/// One engine instance's collection assignment.
#[derive(Clone)]
pub struct CollectionTarget {
    /// Port group being collected.
    pub group_id: Id,
    /// The engine to poll.
    pub engine: crate::engine::EngineHandle,
    /// Engine port index to database port id.
    pub ports: Vec<(EnginePortId, Id)>,
    /// Packet group to flow id. Several groups may map to one flow when the
    /// flow uses a frame-size mixture.
    pub pgids: Vec<(PgId, Id)>,
    /// The run these samples belong to, if any.
    pub run_id: Option<Id>,
    /// Whether to poll connection-level statistics instead of packet groups.
    pub stateful: bool,
}

// ---------------------------------------------------------------------------
// The collector
// ---------------------------------------------------------------------------

/// Owns the polling tasks, the ring buffer, and the fan-out channel.
#[derive(Clone)]
pub struct Collector {
    tx: broadcast::Sender<Arc<StatsBatch>>,
    history: Arc<RwLock<VecDeque<Arc<StatsBatch>>>>,
    tasks: Arc<RwLock<HashMap<Id, JoinHandle<()>>>>,
    metrics: Option<vm::MetricsWriter>,
    /// Latest progress per run, republished with every batch so a client that
    /// connects between trials still learns where the run is.
    progress: Arc<RwLock<HashMap<Id, RunProgress>>>,
}

impl Collector {
    /// Builds a collector, optionally writing to VictoriaMetrics.
    pub fn new(metrics: Option<vm::MetricsWriter>) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            tx,
            history: Arc::new(RwLock::new(VecDeque::with_capacity(HISTORY_DEPTH))),
            tasks: Arc::new(RwLock::new(HashMap::new())),
            metrics,
            progress: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Subscribes to the live sample stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StatsBatch>> {
        self.tx.subscribe()
    }

    /// The buffered history, oldest first.
    pub async fn backfill(&self) -> Vec<Arc<StatsBatch>> {
        self.history.read().await.iter().cloned().collect()
    }

    /// Starts polling `target`, replacing any existing task for its group.
    pub async fn start(&self, target: CollectionTarget) {
        let group_id = target.group_id;
        let collector = self.clone();

        let handle = tokio::spawn(async move { poll_loop(collector, target).await });

        if let Some(previous) = self.tasks.write().await.insert(group_id, handle) {
            previous.abort();
        }
        tracing::info!(%group_id, "started collecting");
    }

    /// Stops polling a group.
    pub async fn stop(&self, group_id: Id) {
        if let Some(handle) = self.tasks.write().await.remove(&group_id) {
            handle.abort();
            tracing::info!(%group_id, "stopped collecting");
        }
    }

    /// Stops every polling task.
    pub async fn stop_all(&self) {
        let mut tasks = self.tasks.write().await;
        for (_, handle) in tasks.drain() {
            handle.abort();
        }
    }

    /// Records a run's progress, to be attached to subsequent batches.
    pub async fn set_progress(&self, run_id: Id, progress: RunProgress) {
        self.progress.write().await.insert(run_id, progress);
    }

    /// Forgets a finished run's progress.
    pub async fn clear_progress(&self, run_id: Id) {
        self.progress.write().await.remove(&run_id);
    }

    /// Publishes one batch to the history and every subscriber.
    async fn publish(&self, batch: StatsBatch) {
        let batch = Arc::new(batch);

        {
            let mut history = self.history.write().await;
            if history.len() == HISTORY_DEPTH {
                history.pop_front();
            }
            history.push_back(Arc::clone(&batch));
        }

        // A send failure means nobody is listening, which is the normal state
        // when no browser has the run view open.
        let _ = self.tx.send(batch);
    }

    /// How many polling tasks are running.
    pub async fn active_count(&self) -> usize {
        self.tasks.read().await.len()
    }
}

/// Polls one engine instance until cancelled.
#[tracing::instrument(skip(collector, target), fields(group_id = %target.group_id))]
async fn poll_loop(collector: Collector, target: CollectionTarget) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    // Skip the immediate first tick: there is no previous sample to difference
    // against, so it would only produce a zero.
    ticker.tick().await;

    let mut previous: Option<Previous> = None;

    loop {
        ticker.tick().await;

        let engine_ports: Vec<EnginePortId> = target.ports.iter().map(|(e, _)| *e).collect();
        let engine_pgids: Vec<PgId> = target.pgids.iter().map(|(p, _)| *p).collect();

        let port_stats = match target.engine.port_stats(&engine_ports).await {
            Ok(stats) => stats,
            Err(err) => {
                // A single failed poll is not fatal — the engine may be busy
                // reprogramming streams between trials.
                tracing::warn!(%err, "port statistics poll failed");
                continue;
            }
        };

        let pgid_stats = match target.engine.pgid_stats(&engine_pgids).await {
            Ok(stats) => stats,
            Err(err) => {
                tracing::warn!(%err, "packet-group statistics poll failed");
                continue;
            }
        };

        // A stateful instance has no packet groups; asking for connection
        // counters instead is the whole difference between the two modes here.
        let connections = if target.stateful {
            match target.engine.astf_stats().await {
                Ok(stats) => stats,
                Err(err) => {
                    tracing::warn!(%err, "connection statistics poll failed");
                    continue;
                }
            }
        } else {
            AstfStats::default()
        };

        let now = Instant::now();
        let current =
            Previous { at: now, ports: port_stats.clone(), pgids: pgid_stats.clone(), connections };

        if let Some(prev) = &previous {
            let elapsed = now.duration_since(prev.at).as_secs_f64();
            let batch = build_batch(&target, prev, &current, elapsed, &collector).await;

            if let Some(writer) = &collector.metrics {
                writer.write(&batch, target.run_id).await;
            }
            collector.publish(batch).await;
        }

        previous = Some(current);
    }
}

/// The previous poll, for differencing.
struct Previous {
    at: Instant,
    ports: Vec<PortStats>,
    pgids: Vec<PgidStats>,
    connections: AstfStats,
}

/// Converts two connection samples into rates.
fn connection_sample(before: AstfStats, now: AstfStats, elapsed: f64) -> ConnectionSample {
    ConnectionSample {
        cps: rate(before.established, now.established, elapsed),
        errors_per_sec: rate(before.connect_errors, now.connect_errors, elapsed),
        active: now.active,
        attempted: now.attempted,
        established: now.established,
        connect_errors: now.connect_errors,
        failure_pct: now.failure_pct(),
        tx_bps: rate(before.tx_bytes, now.tx_bytes, elapsed) * 8.0,
        rx_bps: rate(before.rx_bytes, now.rx_bytes, elapsed) * 8.0,
    }
}

/// Builds one batch from two consecutive polls.
async fn build_batch(
    target: &CollectionTarget,
    previous: &Previous,
    current: &Previous,
    elapsed: f64,
    collector: &Collector,
) -> StatsBatch {
    let mut ports = BTreeMap::new();
    for (i, (_, port_id)) in target.ports.iter().enumerate() {
        let (Some(now), Some(before)) = (current.ports.get(i), previous.ports.get(i)) else {
            continue;
        };
        ports.insert(port_id.to_string(), port_sample(*before, *now, elapsed));
    }

    // Several packet groups can belong to one flow, when the flow uses a frame
    // size mixture. Their rates add.
    let mut streams: BTreeMap<String, StreamSample> = BTreeMap::new();
    for (i, (_, flow_id)) in target.pgids.iter().enumerate() {
        let (Some(now), Some(before)) = (current.pgids.get(i), previous.pgids.get(i)) else {
            continue;
        };
        let sample = stream_sample(*before, *now, elapsed);
        streams
            .entry(flow_id.to_string())
            .and_modify(|existing| merge_stream(existing, &sample))
            .or_insert(sample);
    }

    let run = match target.run_id {
        Some(run_id) => collector.progress.read().await.get(&run_id).cloned(),
        None => None,
    };

    let connections = target
        .stateful
        .then(|| connection_sample(previous.connections, current.connections, elapsed));

    StatsBatch { ts: unix_now(), ports, streams, run, connections }
}

/// Converts two port counter samples into rates.
fn port_sample(before: PortStats, now: PortStats, elapsed: f64) -> PortSample {
    PortSample {
        tx_pps: rate(before.tx_packets, now.tx_packets, elapsed),
        rx_pps: rate(before.rx_packets, now.rx_packets, elapsed),
        // Byte counters are layer 2, so these are layer 2 bit rates. The layer 1
        // figure needs the frame size, which a port counter does not carry.
        tx_bps: rate(before.tx_bytes, now.tx_bytes, elapsed) * 8.0,
        rx_bps: rate(before.rx_bytes, now.rx_bytes, elapsed) * 8.0,
        tx_packets: now.tx_packets,
        rx_packets: now.rx_packets,
        tx_errors: now.tx_errors,
        rx_errors: now.rx_errors,
    }
}

/// Converts two packet-group samples into rates.
fn stream_sample(before: PgidStats, now: PgidStats, elapsed: f64) -> StreamSample {
    let tx_pps = rate(before.tx_packets, now.tx_packets, elapsed);
    let rx_pps = rate(before.rx_packets, now.rx_packets, elapsed);

    StreamSample {
        tx_pps,
        rx_pps,
        // Instantaneous loss can read slightly negative when a sample lands
        // between a frame's transmit and its receive; clamping is more honest
        // than showing a negative loss rate.
        loss_pps: (tx_pps - rx_pps).max(0.0),
        loss_pct: now.loss_pct(),
        tx_packets: now.tx_packets,
        rx_packets: now.rx_packets,
        latency: now.latency,
    }
}

/// Adds one flow component's rates into another's.
///
/// Latency is taken from whichever component reports it rather than averaged:
/// averaging percentiles is not meaningful, and in practice only one component
/// of a mixture carries the latency tag.
fn merge_stream(into: &mut StreamSample, other: &StreamSample) {
    into.tx_pps += other.tx_pps;
    into.rx_pps += other.rx_pps;
    into.loss_pps += other.loss_pps;
    into.tx_packets += other.tx_packets;
    into.rx_packets += other.rx_packets;

    into.loss_pct = if into.tx_packets == 0 {
        0.0
    } else {
        (into.tx_packets.saturating_sub(into.rx_packets) as f64 / into.tx_packets as f64) * 100.0
    };

    if into.latency.p50_us.is_none() {
        into.latency = other.latency;
    }
}

/// Rate of change of a counter, per second.
///
/// Saturating rather than wrapping: a counter that went backwards means the
/// engine restarted or the stats were cleared, and reporting zero for one sample
/// is better than reporting eighteen quintillion packets per second.
fn rate(before: u64, now: u64, elapsed: f64) -> f64 {
    if elapsed <= 0.0 {
        return 0.0;
    }
    now.saturating_sub(before) as f64 / elapsed
}

/// The current Unix timestamp in seconds.
fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port counters with the given packet totals.
    fn port(tx: u64, rx: u64) -> PortStats {
        PortStats {
            tx_packets: tx,
            rx_packets: rx,
            tx_bytes: tx * 64,
            rx_bytes: rx * 64,
            ..Default::default()
        }
    }

    /// Packet-group counters with the given totals.
    fn pgid(tx: u64, rx: u64) -> PgidStats {
        PgidStats { tx_packets: tx, rx_packets: rx, ..Default::default() }
    }

    #[test]
    fn rates_come_from_the_difference_over_the_actual_elapsed_time() {
        // Assuming a one-second interval would make every chart disagree with
        // the totals whenever the daemon is busy.
        assert_eq!(rate(1000, 2000, 1.0), 1000.0);
        assert_eq!(rate(1000, 2000, 2.0), 500.0);
        assert_eq!(rate(1000, 2000, 0.5), 2000.0);
    }

    #[test]
    fn a_counter_that_went_backwards_reports_zero_rather_than_wrapping() {
        assert_eq!(rate(1_000_000, 5, 1.0), 0.0);
    }

    #[test]
    fn a_zero_interval_cannot_produce_an_infinite_rate() {
        assert_eq!(rate(0, 1000, 0.0), 0.0);
        assert!(rate(0, 1000, 0.0).is_finite());
    }

    #[test]
    fn port_samples_carry_both_rates_and_running_totals() {
        // The chart wants the rate; the results table wants the total.
        let sample = port_sample(port(1000, 990), port(2000, 1980), 1.0);

        assert_eq!(sample.tx_pps, 1000.0);
        assert_eq!(sample.rx_pps, 990.0);
        assert_eq!(sample.tx_bps, 1000.0 * 64.0 * 8.0);
        assert_eq!(sample.tx_packets, 2000);
        assert_eq!(sample.rx_packets, 1980);
    }

    #[test]
    fn stream_loss_is_the_shortfall_between_transmit_and_receive() {
        let sample = stream_sample(pgid(1000, 1000), pgid(2000, 1900), 1.0);

        assert_eq!(sample.tx_pps, 1000.0);
        assert_eq!(sample.rx_pps, 900.0);
        assert_eq!(sample.loss_pps, 100.0);
        assert!((sample.loss_pct - 5.0).abs() < 1e-9, "5% of 2000 is 100");
    }

    #[test]
    fn receiving_more_than_transmitted_in_one_sample_does_not_show_negative_loss() {
        // Happens when a poll lands between a frame's transmit and its receive.
        let sample = stream_sample(pgid(1000, 900), pgid(2000, 2000), 1.0);
        assert_eq!(sample.loss_pps, 0.0);
        assert!(sample.loss_pps >= 0.0);
    }

    #[test]
    fn mixture_components_add_into_one_flow_figure() {
        // A flow using an IMIX has one packet group per component; the operator
        // configured one flow and should see one row.
        let mut combined = stream_sample(pgid(0, 0), pgid(700, 690), 1.0);
        let second = stream_sample(pgid(0, 0), pgid(300, 300), 1.0);
        merge_stream(&mut combined, &second);

        assert_eq!(combined.tx_pps, 1000.0);
        assert_eq!(combined.rx_pps, 990.0);
        assert_eq!(combined.tx_packets, 1000);
        assert_eq!(combined.rx_packets, 990);
        assert!((combined.loss_pct - 1.0).abs() < 1e-9);
    }

    #[test]
    fn merging_keeps_whichever_component_carries_latency() {
        let mut without = stream_sample(pgid(0, 0), pgid(100, 100), 1.0);
        assert!(without.latency.p50_us.is_none());

        let mut with = pgid(100, 100);
        with.latency = LatencyStats { p50_us: Some(24.0), ..Default::default() };
        let tracked = stream_sample(pgid(0, 0), with, 1.0);

        merge_stream(&mut without, &tracked);
        assert_eq!(without.latency.p50_us, Some(24.0));
    }

    #[tokio::test]
    async fn the_history_ring_never_grows_past_its_depth() {
        let collector = Collector::new(None);

        for i in 0..HISTORY_DEPTH + 50 {
            collector
                .publish(StatsBatch {
                    ts: i as i64,
                    ports: BTreeMap::new(),
                    streams: BTreeMap::new(),
                    run: None,
                    connections: None,
                })
                .await;
        }

        let backfill = collector.backfill().await;
        assert_eq!(backfill.len(), HISTORY_DEPTH);
        // Oldest first, and the oldest 50 have aged out.
        assert_eq!(backfill[0].ts, 50);
        assert_eq!(backfill[backfill.len() - 1].ts, (HISTORY_DEPTH + 49) as i64);
    }

    #[tokio::test]
    async fn subscribers_receive_published_batches() {
        let collector = Collector::new(None);
        let mut rx = collector.subscribe();

        collector
            .publish(StatsBatch {
                ts: 42,
                ports: BTreeMap::new(),
                streams: BTreeMap::new(),
                run: None,
                connections: None,
            })
            .await;

        let received = rx.recv().await.unwrap();
        assert_eq!(received.ts, 42);
    }

    #[tokio::test]
    async fn publishing_with_no_subscribers_is_not_an_error() {
        // The normal state: nobody has the run view open.
        let collector = Collector::new(None);
        collector
            .publish(StatsBatch {
                ts: 1,
                ports: BTreeMap::new(),
                streams: BTreeMap::new(),
                run: None,
                connections: None,
            })
            .await;
        assert_eq!(collector.backfill().await.len(), 1);
    }

    #[tokio::test]
    async fn run_progress_is_remembered_until_the_run_is_cleared() {
        let collector = Collector::new(None);
        let run_id = Id::new_v4();

        collector
            .set_progress(
                run_id,
                RunProgress {
                    run_id: run_id.to_string(),
                    state: "running".into(),
                    iteration: Some(3),
                    frame_size: Some(512),
                    trial_rate_pct: Some(87.5),
                    trial_remaining_secs: Some(12.0),
                    progress: Some(0.42),
                    message: None,
                },
            )
            .await;

        assert!(collector.progress.read().await.contains_key(&run_id));

        collector.clear_progress(run_id).await;
        assert!(!collector.progress.read().await.contains_key(&run_id));
    }

    #[test]
    fn a_batch_serialises_with_the_field_names_the_client_expects() {
        let mut ports = BTreeMap::new();
        ports.insert("p1".to_string(), PortSample { tx_pps: 1000.0, ..Default::default() });

        let batch = StatsBatch {
            ts: 1712345678,
            ports,
            streams: BTreeMap::new(),
            run: None,
            connections: None,
        };
        let json = serde_json::to_value(&batch).unwrap();

        assert_eq!(json["ts"], 1712345678i64);
        assert_eq!(json["ports"]["p1"]["txPps"], 1000.0);
        assert!(json.get("run").is_none(), "an absent run must not serialise as null");
    }
}
