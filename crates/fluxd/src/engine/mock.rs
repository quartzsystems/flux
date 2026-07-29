//! A simulated packet engine.
//!
//! This is what makes `FLUX_ENGINE=mock` a complete development environment. It
//! is not a stub: it tracks programmed streams, honours start and stop, and
//! derives counters from elapsed wall-clock time against the configured rate, so
//! a flow set to 14.88 Mpps produces 14.88 million packets of counter movement
//! per second and the charts show the shape they will show in the lab.
//!
//! ## Real time
//!
//! A 60-second trial takes 60 seconds. That is deliberate — the orchestrator's
//! timing, the collector's polling, and the UI's countdown are all things worth
//! exercising at their real cadence. `FLUX_MOCK_TIMESCALE` multiplies the clock
//! for tests that cannot afford to wait.
//!
//! ## What is not simulated
//!
//! Nothing here models a device under test. Received counters are transmitted
//! counters minus injected loss, which means a mock run measures the mock. The
//! point is to exercise the pipeline end to end, not to predict a result.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use flux_core::engine::{
    Engine, EngineError, EngineHealth, EnginePortId, EnginePortStatus, LatencyStats, PgId,
    PgidStats, PortStats, StartOptions, StreamSpec,
};
use flux_core::types::EngineMode;
use rand::Rng;

/// Environment variable that speeds up the simulated clock.
const TIMESCALE_VAR: &str = "FLUX_MOCK_TIMESCALE";

/// Median one-way latency the mock reports, in microseconds.
///
/// A few tens of microseconds is what a cut-through switch under light load
/// actually looks like, which makes the charts read plausibly.
const DEFAULT_LATENCY_MEDIAN_US: f64 = 24.0;

/// Shape parameter of the latency distribution.
///
/// Latency is modelled log-normal because real forwarding latency is: bounded
/// below by the wire and the pipeline, with a long tail from queueing. A normal
/// distribution would produce negative latencies at the low end.
const DEFAULT_LATENCY_SIGMA: f64 = 0.35;

/// How many latency samples to draw per statistics read.
const LATENCY_SAMPLES: usize = 64;

/// z-scores for the percentiles reported, from the standard normal.
const Z_P99: f64 = 2.326_347_9;
/// 99.9th percentile z-score.
const Z_P999: f64 = 3.090_232_3;

// ---------------------------------------------------------------------------
// Injectable behaviour
// ---------------------------------------------------------------------------

/// The knobs a debug endpoint can turn on a running mock.
///
/// Held behind an `Arc` so a caller can keep a handle after the engine has been
/// boxed into its owning task — which is the only way to reach it, since the
/// task owns the engine exclusively.
#[derive(Clone, Default)]
pub struct MockControls {
    knobs: Arc<Mutex<Knobs>>,
}

/// What can be injected.
#[derive(Debug, Clone)]
struct Knobs {
    /// Fraction of transmitted frames that never arrive, as a percentage.
    loss_pct: f64,
    /// Median of the latency distribution, in microseconds.
    latency_median_us: f64,
    /// Shape parameter of the latency distribution.
    latency_sigma: f64,
    /// Ports forced to report no carrier.
    link_down: Vec<u8>,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            loss_pct: 0.0,
            latency_median_us: DEFAULT_LATENCY_MEDIAN_US,
            latency_sigma: DEFAULT_LATENCY_SIGMA,
            link_down: Vec::new(),
        }
    }
}

impl MockControls {
    /// Reads the current knob settings.
    fn get(&self) -> Knobs {
        self.knobs.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Sets the loss percentage applied to received counters.
    ///
    /// Clamped rather than rejected: this is a debug affordance, and a caller
    /// asking for 150% loss means "drop everything".
    pub fn set_loss_pct(&self, pct: f64) {
        let mut knobs = self.knobs.lock().unwrap_or_else(|p| p.into_inner());
        knobs.loss_pct = pct.clamp(0.0, 100.0);
    }

    /// The loss percentage currently injected.
    pub fn loss_pct(&self) -> f64 {
        self.get().loss_pct
    }

    /// Sets the latency distribution.
    pub fn set_latency(&self, median_us: f64, sigma: f64) {
        let mut knobs = self.knobs.lock().unwrap_or_else(|p| p.into_inner());
        knobs.latency_median_us = median_us.max(0.0);
        knobs.latency_sigma = sigma.clamp(0.0, 3.0);
    }

    /// Forces a port's carrier state.
    pub fn set_link_down(&self, port: EnginePortId, down: bool) {
        let mut knobs = self.knobs.lock().unwrap_or_else(|p| p.into_inner());
        knobs.link_down.retain(|p| *p != port.0);
        if down {
            knobs.link_down.push(port.0);
        }
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// A simulated engine instance.
pub struct MockEngine {
    mode: EngineMode,
    port_count: u8,
    started_at: Instant,
    timescale: f64,
    controls: MockControls,
    state: Mutex<State>,
}

/// Everything the mock mutates.
#[derive(Debug)]
struct State {
    ports: Vec<MockPort>,
}

/// One simulated port.
#[derive(Debug, Clone)]
struct MockPort {
    owned: bool,
    streams: Vec<StreamSpec>,
    /// When transmission began, and under what options.
    transmit: Option<Transmit>,
    /// Counters from completed transmit periods, carried across start/stop.
    settled: PortStats,
    /// Per-group counters from completed transmit periods.
    settled_pgids: HashMap<u32, PgidStats>,
}

/// An in-progress transmission.
#[derive(Debug, Clone, Copy)]
struct Transmit {
    since: Instant,
    multiplier: f64,
    duration_secs: Option<f64>,
}

impl MockEngine {
    /// Builds a simulated instance with `port_count` ports.
    pub fn new(mode: EngineMode, port_count: u8) -> Self {
        let timescale = std::env::var(TIMESCALE_VAR)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0);

        if timescale != 1.0 {
            tracing::info!(timescale, "mock engine clock is scaled");
        }

        let ports = (0..port_count)
            .map(|_| MockPort {
                owned: false,
                streams: Vec::new(),
                transmit: None,
                settled: PortStats::default(),
                settled_pgids: HashMap::new(),
            })
            .collect();

        Self {
            mode,
            port_count,
            started_at: Instant::now(),
            timescale,
            controls: MockControls::default(),
            state: Mutex::new(State { ports }),
        }
    }

    /// A handle to this instance's injectable behaviour.
    ///
    /// Take this before boxing the engine — afterwards the owning task holds the
    /// only reference to the engine itself.
    pub fn controls(&self) -> MockControls {
        self.controls.clone()
    }

    /// Locks the simulated state, recovering from a poisoned lock.
    ///
    /// A panic in one operation should not take the whole instance down for a
    /// development fake.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Checks a port index is one this instance has.
    fn check_port(&self, port: EnginePortId) -> Result<usize, EngineError> {
        if port.0 < self.port_count {
            Ok(usize::from(port.0))
        } else {
            Err(EngineError::Rejected(format!(
                "port {port} does not exist; this instance owns {} ports",
                self.port_count
            )))
        }
    }

    /// Simulated seconds elapsed since `since`, honouring the timescale.
    fn elapsed(&self, transmit: &Transmit) -> f64 {
        let real = transmit.since.elapsed().as_secs_f64() * self.timescale;
        match transmit.duration_secs {
            // A bounded transmission stops accumulating once its time is up,
            // which is what makes a 60-second trial actually measure 60 seconds
            // even if the collector reads it late.
            Some(limit) => real.min(limit),
            None => real,
        }
    }

    /// The counters a port has accumulated, settled plus in-flight.
    fn port_totals(&self, port: &MockPort) -> PortStats {
        let mut totals = port.settled;

        if let Some(transmit) = &port.transmit {
            let seconds = self.elapsed(transmit);
            let loss = self.controls.get().loss_pct / 100.0;

            for stream in &port.streams {
                let frames = stream.pps * transmit.multiplier * seconds;

                // Byte counts are derived from the whole-frame count, not from
                // the fractional one. A NIC counts frames it actually sent, so
                // bytes must be an exact multiple of the frame length —
                // truncating the two independently makes them disagree by a
                // few bytes, which looks like a real counter inconsistency.
                let sent = frames as u64;
                let received = (frames * (1.0 - loss)) as u64;

                totals.tx_packets += sent;
                totals.tx_bytes += sent * u64::from(stream.wire_len);
                totals.rx_packets += received;
                totals.rx_bytes += received * u64::from(stream.wire_len);
            }
        }

        totals
    }

    /// The counters one packet group has accumulated across all ports.
    fn pgid_totals(&self, state: &State, pgid: PgId) -> PgidStats {
        let mut totals = PgidStats::default();
        let loss = self.controls.get().loss_pct / 100.0;

        for port in &state.ports {
            if let Some(settled) = port.settled_pgids.get(&pgid.0) {
                totals.tx_packets += settled.tx_packets;
                totals.rx_packets += settled.rx_packets;
                totals.tx_bytes += settled.tx_bytes;
                totals.rx_bytes += settled.rx_bytes;
            }

            let Some(transmit) = &port.transmit else { continue };
            let seconds = self.elapsed(transmit);

            for stream in port.streams.iter().filter(|s| s.pg_id == pgid) {
                let frames = stream.pps * transmit.multiplier * seconds;
                let sent = frames as u64;
                let received = (frames * (1.0 - loss)) as u64;

                totals.tx_packets += sent;
                totals.tx_bytes += sent * u64::from(stream.wire_len);
                totals.rx_packets += received;
                totals.rx_bytes += received * u64::from(stream.wire_len);
            }
        }

        totals.latency = self.sample_latency(state, pgid);
        totals
    }

    /// Draws a latency summary for a packet group.
    ///
    /// Percentiles come from the distribution analytically — they are exact for
    /// the model and do not jitter between reads the way a small sample would.
    /// Minimum, mean, and maximum are sampled, because those are the figures an
    /// operator watches move.
    fn sample_latency(&self, state: &State, pgid: PgId) -> LatencyStats {
        let tracked = state
            .ports
            .iter()
            .any(|p| p.streams.iter().any(|s| s.pg_id == pgid && s.latency));

        if !tracked {
            return LatencyStats::default();
        }

        let knobs = self.controls.get();
        let median = knobs.latency_median_us;
        let sigma = knobs.latency_sigma;

        let mut rng = rand::thread_rng();
        let mut samples: Vec<f64> = (0..LATENCY_SAMPLES)
            .map(|_| median * (sigma * standard_normal(&mut rng)).exp())
            .collect();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let min = samples.first().copied().unwrap_or(median);
        let max = samples.last().copied().unwrap_or(median);
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;

        LatencyStats {
            min_us: Some(min),
            avg_us: Some(mean),
            max_us: Some(max),
            p50_us: Some(median),
            p99_us: Some(median * (sigma * Z_P99).exp()),
            p999_us: Some(median * (sigma * Z_P999).exp()),
            // For a log-normal, the spread either side of the median is a
            // reasonable stand-in for inter-packet delay variation.
            jitter_us: Some(median * ((sigma).exp() - (-sigma).exp()) / 2.0),
        }
    }

    /// Folds a port's in-flight counters into its settled totals.
    ///
    /// Called when transmission stops, so the numbers do not jump backwards the
    /// moment the clock stops advancing them.
    fn settle(&self, index: usize, state: &mut State) {
        let port = &state.ports[index];
        let Some(transmit) = port.transmit else { return };

        let seconds = self.elapsed(&transmit);
        let loss = self.controls.get().loss_pct / 100.0;
        let totals = self.port_totals(port);

        let mut per_group: HashMap<u32, PgidStats> = port.settled_pgids.clone();
        for stream in &port.streams {
            let frames = stream.pps * transmit.multiplier * seconds;
            let sent = frames as u64;
            let received = (frames * (1.0 - loss)) as u64;

            let entry = per_group.entry(stream.pg_id.0).or_default();
            entry.tx_packets += sent;
            entry.tx_bytes += sent * u64::from(stream.wire_len);
            entry.rx_packets += received;
            entry.rx_bytes += received * u64::from(stream.wire_len);
        }

        let port = &mut state.ports[index];
        port.settled = totals;
        port.settled_pgids = per_group;
        port.transmit = None;
    }
}

/// One draw from the standard normal, by the Box-Muller transform.
///
/// `rand` alone gives uniforms; this converts a pair of them into a normal
/// without pulling in a distributions crate for one function.
fn standard_normal<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    // Exclude zero: ln(0) is negative infinity.
    let u1: f64 = rng.gen_range(f64::MIN_POSITIVE..1.0);
    let u2: f64 = rng.gen_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[async_trait]
impl Engine for MockEngine {
    async fn health(&self) -> Result<EngineHealth, EngineError> {
        Ok(EngineHealth {
            connected: true,
            version: Some(format!("mock-{}", env!("CARGO_PKG_VERSION"))),
            mode: self.mode,
            port_count: self.port_count,
            uptime_secs: Some(self.started_at.elapsed().as_secs()),
        })
    }

    async fn port_status(&self) -> Result<Vec<EnginePortStatus>, EngineError> {
        let knobs = self.controls.get();
        let state = self.state();

        Ok(state
            .ports
            .iter()
            .enumerate()
            .map(|(i, port)| EnginePortStatus {
                port: EnginePortId(i as u8),
                owned: port.owned,
                link_up: !knobs.link_down.contains(&(i as u8)),
                speed_mbps: Some(100_000),
                transmitting: port
                    .transmit
                    .as_ref()
                    .is_some_and(|t| t.duration_secs.is_none_or(|d| self.elapsed(t) < d)),
                src_mac: Some(format!("00:1b:21:aa:bb:{:02x}", 0xc0 + i)),
            })
            .collect())
    }

    async fn acquire(&self, ports: &[EnginePortId], _force: bool) -> Result<(), EngineError> {
        let mut state = self.state();
        for port in ports {
            let index = self.check_port(*port)?;
            state.ports[index].owned = true;
        }
        Ok(())
    }

    async fn release(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        let mut state = self.state();
        for port in ports {
            let index = self.check_port(*port)?;
            state.ports[index].owned = false;
        }
        Ok(())
    }

    async fn clear_streams(&self, port: EnginePortId) -> Result<(), EngineError> {
        let index = self.check_port(port)?;
        let mut state = self.state();
        state.ports[index].streams.clear();
        Ok(())
    }

    async fn add_streams(
        &self,
        port: EnginePortId,
        streams: Vec<StreamSpec>,
    ) -> Result<(), EngineError> {
        let index = self.check_port(port)?;
        let mut state = self.state();

        if !state.ports[index].owned {
            return Err(EngineError::NotOwned(port));
        }

        state.ports[index].streams.extend(streams);
        Ok(())
    }

    async fn start_traffic(
        &self,
        ports: &[EnginePortId],
        opts: StartOptions,
    ) -> Result<(), EngineError> {
        let mut state = self.state();

        for port in ports {
            let index = self.check_port(*port)?;
            if !state.ports[index].owned {
                return Err(EngineError::NotOwned(*port));
            }
            if state.ports[index].streams.is_empty() {
                return Err(EngineError::Rejected(format!(
                    "port {port} has no streams programmed"
                )));
            }
        }

        for port in ports {
            let index = usize::from(port.0);
            // Restarting an already-transmitting port settles what it did first,
            // so counters carry forward rather than restarting from the new
            // multiplier.
            self.settle(index, &mut state);
            state.ports[index].transmit = Some(Transmit {
                since: Instant::now(),
                multiplier: opts.multiplier,
                duration_secs: opts.duration_secs,
            });
        }

        Ok(())
    }

    async fn stop_traffic(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        let mut state = self.state();
        for port in ports {
            // Stopping a port this instance does not have is not an error: the
            // shutdown path stops every possible index rather than looking them up.
            if usize::from(port.0) < state.ports.len() {
                self.settle(usize::from(port.0), &mut state);
            }
        }
        Ok(())
    }

    async fn clear_stats(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        let mut state = self.state();
        for port in ports {
            let index = self.check_port(*port)?;
            state.ports[index].settled = PortStats::default();
            state.ports[index].settled_pgids.clear();
            // Restart the clock so in-flight counters begin from zero too;
            // otherwise a clear during transmission would be undone on the next read.
            if let Some(transmit) = &mut state.ports[index].transmit {
                transmit.since = Instant::now();
            }
        }
        Ok(())
    }

    async fn port_stats(&self, ports: &[EnginePortId]) -> Result<Vec<PortStats>, EngineError> {
        let state = self.state();
        ports
            .iter()
            .map(|port| {
                let index = self.check_port(*port)?;
                Ok(self.port_totals(&state.ports[index]))
            })
            .collect()
    }

    async fn pgid_stats(&self, pgids: &[PgId]) -> Result<Vec<PgidStats>, EngineError> {
        let state = self.state();
        Ok(pgids.iter().map(|pgid| self.pgid_totals(&state, *pgid)).collect())
    }
}

#[cfg(test)]
mod tests {
    use flux_core::engine::StreamModifier;

    use super::*;

    /// A stream at a known rate, so counters can be predicted.
    fn stream(pg_id: u32, pps: f64) -> StreamSpec {
        StreamSpec {
            pg_id: PgId(pg_id),
            packet: vec![0; 60],
            wire_len: 64,
            pps,
            modifiers: Vec::<StreamModifier>::new(),
            latency: false,
            total_packets: None,
        }
    }

    /// An acquired single-port engine with one stream programmed.
    async fn armed(pps: f64) -> MockEngine {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();
        engine.add_streams(EnginePortId(0), vec![stream(1, pps)]).await.unwrap();
        engine
    }

    #[tokio::test]
    async fn a_fresh_engine_reports_its_ports_and_version() {
        let engine = MockEngine::new(EngineMode::Stl, 4);
        let health = engine.health().await.unwrap();

        assert!(health.connected);
        assert_eq!(health.port_count, 4);
        assert_eq!(health.mode, EngineMode::Stl);
        assert!(health.version.unwrap().starts_with("mock-"));
    }

    #[tokio::test]
    async fn streams_cannot_be_programmed_onto_an_unacquired_port() {
        // TRex enforces this and so must the mock, or code that forgets to
        // acquire works in development and fails in the lab.
        let engine = MockEngine::new(EngineMode::Stl, 1);
        let result = engine.add_streams(EnginePortId(0), vec![stream(1, 100.0)]).await;
        assert!(matches!(result, Err(EngineError::NotOwned(_))));
    }

    #[tokio::test]
    async fn a_port_index_beyond_the_instance_is_rejected() {
        let engine = MockEngine::new(EngineMode::Stl, 2);
        assert!(matches!(
            engine.acquire(&[EnginePortId(7)], false).await,
            Err(EngineError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn starting_with_no_streams_is_refused() {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();

        let result = engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await;
        assert!(matches!(result, Err(EngineError::Rejected(_))));
    }

    #[tokio::test]
    async fn counters_stay_at_zero_until_traffic_starts() {
        let engine = armed(1_000_000.0).await;
        let stats = engine.port_stats(&[EnginePortId(0)]).await.unwrap();
        assert_eq!(stats[0].tx_packets, 0);
        assert_eq!(stats[0].rx_packets, 0);
    }

    #[tokio::test]
    async fn counters_track_the_configured_rate_over_elapsed_time() {
        let engine = armed(1_000_000.0).await;
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let stats = engine.port_stats(&[EnginePortId(0)]).await.unwrap();

        // At 1 Mpps, 120 ms is about 120,000 frames. The bound is loose because
        // this is wall-clock time on a shared machine.
        assert!(
            (60_000..400_000).contains(&stats[0].tx_packets),
            "got {} packets",
            stats[0].tx_packets
        );
        assert_eq!(stats[0].tx_bytes, stats[0].tx_packets * 64);
    }

    #[tokio::test]
    async fn the_multiplier_scales_the_rate() {
        let engine = armed(1_000_000.0).await;
        engine
            .start_traffic(
                &[EnginePortId(0)],
                StartOptions { multiplier: 0.5, ..Default::default() },
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let halved = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;

        let full = armed(1_000_000.0).await;
        full.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let whole = full.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;

        assert!(halved < whole, "half rate {halved} should be below full rate {whole}");
    }

    #[tokio::test]
    async fn with_no_loss_injected_everything_transmitted_arrives() {
        let engine = armed(1_000_000.0).await;
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let stats = engine.port_stats(&[EnginePortId(0)]).await.unwrap();
        assert_eq!(stats[0].tx_packets, stats[0].rx_packets);
    }

    #[tokio::test]
    async fn injected_loss_shows_up_as_a_receive_shortfall() {
        let engine = armed(1_000_000.0).await;
        engine.controls().set_loss_pct(10.0);
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let stats = engine.pgid_stats(&[PgId(1)]).await.unwrap();
        let loss = stats[0].loss_pct();
        assert!((loss - 10.0).abs() < 1.0, "expected about 10% loss, got {loss}");
    }

    #[tokio::test]
    async fn injected_loss_is_clamped_to_a_sane_range() {
        let controls = MockControls::default();

        controls.set_loss_pct(-5.0);
        assert_eq!(controls.loss_pct(), 0.0);

        controls.set_loss_pct(150.0);
        assert_eq!(controls.loss_pct(), 100.0);
    }

    #[tokio::test]
    async fn a_bounded_transmission_stops_accumulating_when_its_time_is_up() {
        // The trial length is what the result means; a collector that reads late
        // must not inflate the transmitted count.
        let engine = armed(1_000_000.0).await;
        engine
            .start_traffic(
                &[EnginePortId(0)],
                StartOptions { duration_secs: Some(0.05), ..Default::default() },
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let first = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let second = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;

        assert_eq!(first, second, "counters must freeze once the duration elapses");
    }

    #[tokio::test]
    async fn a_port_reports_itself_idle_once_its_duration_expires() {
        let engine = armed(1_000.0).await;
        engine
            .start_traffic(
                &[EnginePortId(0)],
                StartOptions { duration_secs: Some(0.03), ..Default::default() },
            )
            .await
            .unwrap();

        assert!(engine.port_status().await.unwrap()[0].transmitting);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(!engine.port_status().await.unwrap()[0].transmitting);
    }

    #[tokio::test]
    async fn counters_survive_a_stop_rather_than_resetting() {
        let engine = armed(1_000_000.0).await;
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        engine.stop_traffic(&[EnginePortId(0)]).await.unwrap();

        let after_stop = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;
        assert!(after_stop > 0);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let later = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;
        assert_eq!(after_stop, later, "a stopped port must not keep counting");
    }

    #[tokio::test]
    async fn clearing_stats_zeroes_the_counters_even_mid_transmission() {
        // Every RFC 2544 trial clears and then measures; a clear that did not
        // take effect would carry the previous trial's traffic into this one.
        let engine = armed(1_000_000.0).await;
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        engine.clear_stats(&[EnginePortId(0)]).await.unwrap();
        let just_after = engine.port_stats(&[EnginePortId(0)]).await.unwrap()[0].tx_packets;

        assert!(just_after < 50_000, "expected a near-zero count, got {just_after}");
    }

    #[tokio::test]
    async fn per_group_counters_separate_streams_on_the_same_port() {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();
        engine
            .add_streams(EnginePortId(0), vec![stream(1, 1_000_000.0), stream(2, 250_000.0)])
            .await
            .unwrap();
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let stats = engine.pgid_stats(&[PgId(1), PgId(2)]).await.unwrap();

        let ratio = stats[0].tx_packets as f64 / stats[1].tx_packets.max(1) as f64;
        assert!((ratio - 4.0).abs() < 0.5, "expected a 4:1 split, got {ratio}");
    }

    #[tokio::test]
    async fn latency_is_reported_only_for_streams_that_asked_for_it() {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();

        let mut tracked = stream(1, 1000.0);
        tracked.latency = true;
        engine.add_streams(EnginePortId(0), vec![tracked, stream(2, 1000.0)]).await.unwrap();

        let stats = engine.pgid_stats(&[PgId(1), PgId(2)]).await.unwrap();
        assert!(stats[0].latency.p50_us.is_some(), "a tracked stream reports latency");
        assert!(stats[1].latency.p50_us.is_none(), "an untracked one does not");
    }

    #[tokio::test]
    async fn latency_percentiles_are_ordered_and_plausible() {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();

        let mut tracked = stream(1, 1000.0);
        tracked.latency = true;
        engine.add_streams(EnginePortId(0), vec![tracked]).await.unwrap();

        let latency = engine.pgid_stats(&[PgId(1)]).await.unwrap()[0].latency;
        let (min, p50, p99, p999, max) = (
            latency.min_us.unwrap(),
            latency.p50_us.unwrap(),
            latency.p99_us.unwrap(),
            latency.p999_us.unwrap(),
            latency.max_us.unwrap(),
        );

        assert!(min > 0.0, "one-way latency cannot be negative or zero");
        assert!(min <= p50, "minimum {min} above median {p50}");
        assert!(p50 < p99, "median {p50} not below p99 {p99}");
        assert!(p99 < p999, "p99 {p99} not below p99.9 {p999}");
        assert!(max >= min);
    }

    #[tokio::test]
    async fn the_latency_distribution_follows_its_configured_median() {
        let engine = MockEngine::new(EngineMode::Stl, 1);
        engine.acquire(&[EnginePortId(0)], false).await.unwrap();

        let mut tracked = stream(1, 1000.0);
        tracked.latency = true;
        engine.add_streams(EnginePortId(0), vec![tracked]).await.unwrap();

        engine.controls().set_latency(500.0, 0.2);
        let latency = engine.pgid_stats(&[PgId(1)]).await.unwrap()[0].latency;
        assert_eq!(latency.p50_us, Some(500.0));
        assert!(latency.avg_us.unwrap() > 400.0 && latency.avg_us.unwrap() < 700.0);
    }

    #[tokio::test]
    async fn a_forced_link_down_shows_in_port_status() {
        let engine = MockEngine::new(EngineMode::Stl, 2);
        assert!(engine.port_status().await.unwrap()[1].link_up);

        engine.controls().set_link_down(EnginePortId(1), true);
        let status = engine.port_status().await.unwrap();
        assert!(status[0].link_up);
        assert!(!status[1].link_up);

        engine.controls().set_link_down(EnginePortId(1), false);
        assert!(engine.port_status().await.unwrap()[1].link_up);
    }

    #[tokio::test]
    async fn stopping_a_port_this_instance_does_not_have_is_not_an_error() {
        // The shutdown path stops every possible index without looking them up.
        let engine = MockEngine::new(EngineMode::Stl, 2);
        let everything: Vec<EnginePortId> = (0..16).map(EnginePortId).collect();
        assert!(engine.stop_traffic(&everything).await.is_ok());
    }

    #[test]
    fn box_muller_produces_a_standard_normal() {
        let mut rng = rand::thread_rng();
        let samples: Vec<f64> = (0..20_000).map(|_| standard_normal(&mut rng)).collect();

        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let variance =
            samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples.len() as f64;

        assert!(mean.abs() < 0.05, "mean should be near zero, got {mean}");
        assert!((variance - 1.0).abs() < 0.1, "variance should be near one, got {variance}");
        assert!(samples.iter().all(|s| s.is_finite()), "no sample may be infinite or NaN");
    }
}
