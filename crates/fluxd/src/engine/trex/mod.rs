//! The real TRex engine.
//!
//! Implements [`Engine`] over TRex's JSON-RPC-over-ZeroMQ interface. Per the
//! milestone plan this is structured and unit-tested but not runtime-verified:
//! it needs a machine with DPDK-capable NICs, which no development environment
//! has. Every field name or semantic taken from documentation rather than from a
//! running instance is marked `TODO(trex-verify)`, and all of them live in this
//! directory.
//!
//! ## Statistics are relative
//!
//! TRex counters are cumulative from process start and there is no RPC to zero
//! them. `clear_stats` therefore records the current values as a baseline and
//! every later read subtracts it — which is exactly what the Python client does.
//! RFC 2544 depends on this: a trial measures what happened during the trial,
//! not since the engine booted.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use flux_core::engine::{
    AstfProfile, AstfStats, Engine, EngineError, EngineHealth, EnginePortId, EnginePortStatus,
    LatencyStats, PgId, PgidStats, PortStats, StartOptions, StreamSpec,
};
use flux_core::types::EngineMode;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

pub mod astf;
pub mod config;
pub mod rpc;
pub mod stream;
pub mod supervisor;
pub mod transport;

use rpc::RpcClient;
use transport::{RpcTransport, ZmqTransport};

/// Identifies this client to TRex, which reports it in its console.
const CLIENT_USER: &str = "flux";

/// A connection to one TRex instance.
pub struct TrexEngine {
    mode: EngineMode,
    port_count: u8,
    session_id: u32,
    /// The RPC client. Async-locked because the trait takes `&self` while a call
    /// needs `&mut` on the socket; the engine actor already guarantees there is
    /// only ever one caller, so this never actually contends.
    rpc: AsyncMutex<RpcClient>,
    /// Per-port ownership handles returned by `acquire`, required by most calls.
    handles: Mutex<HashMap<u8, String>>,
    /// Counter baselines recorded by `clear_stats`.
    baselines: Mutex<Baselines>,
}

/// What `clear_stats` recorded, subtracted from every later read.
#[derive(Debug, Default)]
struct Baselines {
    ports: HashMap<u8, PortStats>,
    pgids: HashMap<u32, PgidStats>,
}

impl TrexEngine {
    /// Builds an engine talking to `host:rpc_port`.
    pub fn connect(mode: EngineMode, port_count: u8, host: &str, rpc_port: u16) -> Self {
        Self::with_transport(mode, port_count, Box::new(ZmqTransport::new(host, rpc_port)))
    }

    /// Builds an engine over an arbitrary transport, for tests.
    pub fn with_transport(
        mode: EngineMode,
        port_count: u8,
        transport: Box<dyn RpcTransport>,
    ) -> Self {
        Self {
            mode,
            port_count,
            // TRex uses this to tell concurrent clients apart. It only has to be
            // unique among live sessions, not globally.
            session_id: rand::random(),
            rpc: AsyncMutex::new(RpcClient::new(transport)),
            handles: Mutex::new(HashMap::new()),
            baselines: Mutex::new(Baselines::default()),
        }
    }

    /// Negotiates the API version. Must run before any other call.
    pub async fn handshake(&self) -> Result<String, EngineError> {
        self.rpc.lock().await.api_sync().await
    }

    /// The ownership handle for a port, or an error naming what is missing.
    fn handle_for(&self, port: EnginePortId) -> Result<String, EngineError> {
        self.handles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&port.0)
            .cloned()
            .ok_or(EngineError::NotOwned(port))
    }

    /// Rejects a port index this instance does not have.
    fn check_port(&self, port: EnginePortId) -> Result<(), EngineError> {
        if port.0 < self.port_count {
            Ok(())
        } else {
            Err(EngineError::Rejected(format!(
                "port {port} does not exist; this instance owns {} ports",
                self.port_count
            )))
        }
    }
}

#[async_trait]
impl Engine for TrexEngine {
    async fn health(&self) -> Result<EngineHealth, EngineError> {
        // TODO(trex-verify): `get_version` returns `version`, `build_date`, and
        // `mode` on the builds documented; older ones spell it `get_sys_info`.
        let result: Value = self.rpc.lock().await.call_raw("get_version", json!({})).await?;

        Ok(EngineHealth {
            connected: true,
            version: result.get("version").and_then(Value::as_str).map(str::to_owned),
            mode: self.mode,
            port_count: self.port_count,
            uptime_secs: result.get("uptime").and_then(Value::as_u64),
        })
    }

    async fn port_status(&self) -> Result<Vec<EnginePortStatus>, EngineError> {
        // One batched round trip rather than one per port: an eight-port
        // instance polled every second is otherwise eight RPCs a second doing
        // nothing but reading state.
        let calls: Vec<(String, Value)> = (0..self.port_count)
            .map(|i| ("get_port_status".to_string(), json!({ "port_id": i })))
            .collect();

        let results = self.rpc.lock().await.call_batch(calls).await?;

        Ok(results
            .into_iter()
            .enumerate()
            .map(|(i, result)| {
                // TODO(trex-verify): field names. `state` is one of "IDLE",
                // "STREAMS", "TX", "PAUSE"; `attr.link.up` carries carrier state.
                let attr = result.get("attr");
                EnginePortStatus {
                    port: EnginePortId(i as u8),
                    owned: self
                        .handles
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .contains_key(&(i as u8)),
                    link_up: attr
                        .and_then(|a| a.get("link"))
                        .and_then(|l| l.get("up"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    speed_mbps: result
                        .get("speed")
                        .and_then(Value::as_i64)
                        // TODO(trex-verify): `speed` is reported in gigabits.
                        .map(|g| (g * 1000) as i32),
                    transmitting: result
                        .get("state")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case("TX")),
                    src_mac: attr
                        .and_then(|a| a.get("src_mac"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }
            })
            .collect())
    }

    async fn acquire(&self, ports: &[EnginePortId], force: bool) -> Result<(), EngineError> {
        for port in ports {
            self.check_port(*port)?;
        }

        let calls: Vec<(String, Value)> = ports
            .iter()
            .map(|port| {
                (
                    "acquire".to_string(),
                    json!({
                        "port_id": port.0,
                        "user": CLIENT_USER,
                        "session_id": self.session_id,
                        "force": force,
                    }),
                )
            })
            .collect();

        let results = self.rpc.lock().await.call_batch(calls).await?;

        // TODO(trex-verify): `acquire` returns the handle as a bare string.
        let mut handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        for (port, result) in ports.iter().zip(results) {
            let handle = result.as_str().ok_or_else(|| {
                EngineError::Protocol(format!("acquire on port {port} returned {result}"))
            })?;
            handles.insert(port.0, handle.to_owned());
        }

        tracing::info!(ports = ?ports, "acquired TRex ports");
        Ok(())
    }

    async fn release(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        let mut calls = Vec::new();
        for port in ports {
            // Releasing a port we do not hold is not an error — the shutdown
            // path releases everything without checking first.
            if let Ok(handle) = self.handle_for(*port) {
                calls
                    .push(("release".to_string(), json!({ "port_id": port.0, "handler": handle })));
            }
        }

        if calls.is_empty() {
            return Ok(());
        }

        self.rpc.lock().await.call_batch(calls).await?;

        let mut handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        for port in ports {
            handles.remove(&port.0);
        }

        Ok(())
    }

    async fn clear_streams(&self, port: EnginePortId) -> Result<(), EngineError> {
        let handle = self.handle_for(port)?;
        self.rpc
            .lock()
            .await
            .call_raw("remove_all_streams", json!({ "port_id": port.0, "handler": handle }))
            .await?;
        Ok(())
    }

    async fn add_streams(
        &self,
        port: EnginePortId,
        streams: Vec<StreamSpec>,
    ) -> Result<(), EngineError> {
        if streams.is_empty() {
            return Ok(());
        }

        let handle = self.handle_for(port)?;

        // One batch. A hundred streams as a hundred round trips is the slowest
        // thing a naive client does, and a REQ socket cannot pipeline them.
        let calls: Vec<(String, Value)> = streams
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let stream_id = i as u32 + 1;
                (
                    "add_stream".to_string(),
                    json!({
                        "port_id": port.0,
                        "handler": handle,
                        "stream_id": stream_id,
                        "stream": stream::encode(spec, stream_id),
                    }),
                )
            })
            .collect();

        self.rpc.lock().await.call_batch(calls).await?;
        tracing::debug!(port = %port, count = streams.len(), "programmed streams");
        Ok(())
    }

    async fn start_traffic(
        &self,
        ports: &[EnginePortId],
        opts: StartOptions,
    ) -> Result<(), EngineError> {
        let mut calls = Vec::with_capacity(ports.len());
        for port in ports {
            let handle = self.handle_for(*port)?;
            calls.push((
                "start_traffic".to_string(),
                json!({
                    "port_id": port.0,
                    "handler": handle,
                    "mul": stream::multiplier(opts.multiplier),
                    // TODO(trex-verify): a negative duration means "until
                    // stopped"; zero is rejected.
                    "duration": opts.duration_secs.unwrap_or(-1.0),
                    "force": opts.force,
                    "core_mask": u64::MAX,
                }),
            ));
        }

        self.rpc.lock().await.call_batch(calls).await?;
        tracing::info!(ports = ?ports, multiplier = opts.multiplier, "traffic started");
        Ok(())
    }

    async fn stop_traffic(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        let mut calls = Vec::new();
        for port in ports {
            if let Ok(handle) = self.handle_for(*port) {
                calls.push((
                    "stop_traffic".to_string(),
                    json!({ "port_id": port.0, "handler": handle }),
                ));
            }
        }

        if calls.is_empty() {
            return Ok(());
        }

        self.rpc.lock().await.call_batch(calls).await?;
        tracing::info!(ports = ?ports, "traffic stopped");
        Ok(())
    }

    async fn clear_stats(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        // TRex has no counter-reset RPC, so "clear" means "record where we are
        // now and subtract it from here on".
        let raw = self.read_raw_port_stats(ports).await?;

        let mut baselines = self.baselines.lock().unwrap_or_else(|p| p.into_inner());
        for (port, stats) in ports.iter().zip(raw) {
            baselines.ports.insert(port.0, stats);
        }
        // Per-group baselines are cleared rather than recorded: TRex resets its
        // packet-group counters when the groups are reprogrammed, which happens
        // between trials anyway.
        baselines.pgids.clear();

        Ok(())
    }

    async fn port_stats(&self, ports: &[EnginePortId]) -> Result<Vec<PortStats>, EngineError> {
        let raw = self.read_raw_port_stats(ports).await?;
        let baselines = self.baselines.lock().unwrap_or_else(|p| p.into_inner());

        Ok(ports
            .iter()
            .zip(raw)
            .map(|(port, stats)| match baselines.ports.get(&port.0) {
                Some(base) => subtract(stats, *base),
                None => stats,
            })
            .collect())
    }

    async fn pgid_stats(&self, pgids: &[PgId]) -> Result<Vec<PgidStats>, EngineError> {
        if pgids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<u32> = pgids.iter().map(|p| p.0).collect();
        // TODO(trex-verify): `get_pgid_stats` takes `{"pgids": [...]}` and
        // returns `{"flow_stats": {...}, "latency": {...}}` keyed by id as a
        // string.
        let result: Value =
            self.rpc.lock().await.call_raw("get_pgid_stats", json!({ "pgids": ids })).await?;

        Ok(pgids.iter().map(|pgid| decode_pgid(&result, *pgid)).collect())
    }

    // -----------------------------------------------------------------------
    // Stateful mode
    // -----------------------------------------------------------------------

    async fn load_astf_profile(&self, profile: AstfProfile) -> Result<(), EngineError> {
        if self.mode != EngineMode::Astf {
            return Err(EngineError::Rejected(
                "this instance was started in stateless mode; TRex cannot switch at run time"
                    .into(),
            ));
        }

        // TODO(trex-verify): `profile_load` takes the document under `profile`
        // along with a fragment marker, because large profiles are sent in
        // pieces. One-shot delivery sets first and last together.
        let document = astf::encode(&profile);
        self.rpc
            .lock()
            .await
            .call_raw(
                "profile_load",
                json!({
                    "handler": self.session_id,
                    "profile": document,
                    "fragment_first": true,
                    "fragment_last": true,
                }),
            )
            .await?;

        tracing::info!(
            target_cps = profile.target_cps,
            max_concurrent = profile.max_concurrent,
            "loaded a stateful profile"
        );
        Ok(())
    }

    async fn start_astf(&self, duration_secs: Option<f64>) -> Result<(), EngineError> {
        // TODO(trex-verify): `start` in ASTF mode takes `mult`, `duration`, and
        // `nc` (do not block on completion). A negative duration runs until
        // stopped, as in stateless mode.
        self.rpc
            .lock()
            .await
            .call_raw(
                "start",
                json!({
                    "handler": self.session_id,
                    "mult": 1.0,
                    "duration": duration_secs.unwrap_or(-1.0),
                    "nc": true,
                }),
            )
            .await?;

        tracing::info!(?duration_secs, "stateful load started");
        Ok(())
    }

    async fn stop_astf(&self) -> Result<(), EngineError> {
        self.rpc.lock().await.call_raw("stop", json!({ "handler": self.session_id })).await?;

        tracing::info!("stateful load stopped");
        Ok(())
    }

    async fn astf_stats(&self) -> Result<AstfStats, EngineError> {
        let result: Value = self.rpc.lock().await.call_raw("get_astf_stats", json!({})).await?;
        Ok(astf::decode_stats(&result))
    }
}

impl TrexEngine {
    /// Reads per-port counters without baseline subtraction.
    async fn read_raw_port_stats(
        &self,
        ports: &[EnginePortId],
    ) -> Result<Vec<PortStats>, EngineError> {
        if ports.is_empty() {
            return Ok(Vec::new());
        }

        let calls: Vec<(String, Value)> = ports
            .iter()
            .map(|port| ("get_port_stats".to_string(), json!({ "port_id": port.0 })))
            .collect();

        let results = self.rpc.lock().await.call_batch(calls).await?;
        Ok(results.iter().map(decode_port_stats).collect())
    }
}

/// Reads a counter object into [`PortStats`].
///
/// TODO(trex-verify): TRex names these `opackets`, `ipackets`, `obytes`,
/// `ibytes`, `oerrors`, `ierrors`. Missing counters decode as zero rather than
/// failing the whole read — a build that omits one should not take out the
/// collector.
fn decode_port_stats(value: &Value) -> PortStats {
    let get = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);

    PortStats {
        tx_packets: get("opackets"),
        rx_packets: get("ipackets"),
        tx_bytes: get("obytes"),
        rx_bytes: get("ibytes"),
        tx_errors: get("oerrors"),
        rx_errors: get("ierrors"),
        rx_dropped: get("imissed"),
    }
}

/// Extracts one packet group's counters from a `get_pgid_stats` reply.
fn decode_pgid(result: &Value, pgid: PgId) -> PgidStats {
    let key = pgid.0.to_string();

    let flow = result.get("flow_stats").and_then(|f| f.get(&key));
    let get = |field: &str| -> u64 {
        flow.and_then(|f| f.get(field))
            // TODO(trex-verify): each counter is an object keyed by port id plus
            // a "total". The total is what a flow-level figure wants.
            .and_then(|f| f.get("total"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };

    let latency = result.get("latency").and_then(|l| l.get(&key));
    let hist = latency.and_then(|l| l.get("latency"));
    let lat = |field: &str| hist.and_then(|h| h.get(field)).and_then(Value::as_f64);

    PgidStats {
        tx_packets: get("tp"),
        rx_packets: get("rp"),
        tx_bytes: get("tb"),
        rx_bytes: get("rb"),
        latency: LatencyStats {
            min_us: lat("total_min"),
            avg_us: lat("average"),
            max_us: lat("total_max"),
            // TODO(trex-verify): percentiles come from the `histogram` map,
            // which needs interpolating rather than reading directly.
            p50_us: lat("p50"),
            p99_us: lat("p99"),
            p999_us: lat("p999"),
            jitter_us: lat("jitter"),
        },
    }
}

/// Subtracts a baseline, saturating at zero.
///
/// Counters only go up, so a negative difference means the engine restarted and
/// its counters went back to zero. Saturating reports the post-restart absolute
/// value, which is wrong but bounded; underflowing would report about 18
/// quintillion packets.
fn subtract(current: PortStats, base: PortStats) -> PortStats {
    PortStats {
        tx_packets: current.tx_packets.saturating_sub(base.tx_packets),
        rx_packets: current.rx_packets.saturating_sub(base.rx_packets),
        tx_bytes: current.tx_bytes.saturating_sub(base.tx_bytes),
        rx_bytes: current.rx_bytes.saturating_sub(base.rx_bytes),
        tx_errors: current.tx_errors.saturating_sub(base.tx_errors),
        rx_errors: current.rx_errors.saturating_sub(base.rx_errors),
        rx_dropped: current.rx_dropped.saturating_sub(base.rx_dropped),
    }
}

#[cfg(test)]
mod tests {
    use super::transport::testing::FakeTransport;
    use super::*;

    /// An engine over canned replies.
    fn engine(replies: &[&str]) -> (TrexEngine, FakeTransport) {
        let fake = FakeTransport::with_replies(replies.iter().map(|s| (*s).to_owned()));
        (TrexEngine::with_transport(EngineMode::Stl, 2, Box::new(fake.clone())), fake)
    }

    /// A batch reply acquiring ports 0 and 1.
    const ACQUIRE_TWO: &str =
        r#"[{"jsonrpc":"2.0","id":1,"result":"h0"},{"jsonrpc":"2.0","id":2,"result":"h1"}]"#;

    #[tokio::test]
    async fn acquiring_stores_the_handles_later_calls_need() {
        let (engine, _) = engine(&[ACQUIRE_TWO]);
        engine.acquire(&[EnginePortId(0), EnginePortId(1)], false).await.unwrap();

        assert_eq!(engine.handle_for(EnginePortId(0)).unwrap(), "h0");
        assert_eq!(engine.handle_for(EnginePortId(1)).unwrap(), "h1");
    }

    #[tokio::test]
    async fn calls_on_an_unacquired_port_report_it_rather_than_sending_a_null_handle() {
        let (engine, fake) = engine(&[]);
        let result = engine.clear_streams(EnginePortId(0)).await;

        assert!(matches!(result, Err(EngineError::NotOwned(_))));
        assert!(fake.sent().is_empty(), "nothing should reach the wire");
    }

    #[tokio::test]
    async fn a_port_beyond_the_instance_is_rejected_before_any_rpc() {
        let (engine, fake) = engine(&[]);
        assert!(matches!(
            engine.acquire(&[EnginePortId(9)], false).await,
            Err(EngineError::Rejected(_))
        ));
        assert!(fake.sent().is_empty());
    }

    #[tokio::test]
    async fn programming_streams_is_a_single_batched_round_trip() {
        let (engine, fake) = engine(&[
            ACQUIRE_TWO,
            r#"[{"jsonrpc":"2.0","id":3,"result":{}},{"jsonrpc":"2.0","id":4,"result":{}},{"jsonrpc":"2.0","id":5,"result":{}}]"#,
        ]);
        engine.acquire(&[EnginePortId(0), EnginePortId(1)], false).await.unwrap();

        let specs: Vec<StreamSpec> = (0..3)
            .map(|i| StreamSpec {
                pg_id: PgId(i),
                packet: vec![0; 60],
                wire_len: 64,
                pps: 1000.0,
                modifiers: Vec::new(),
                latency: false,
                total_packets: None,
            })
            .collect();

        engine.add_streams(EnginePortId(0), specs).await.unwrap();

        assert_eq!(fake.sent().len(), 2, "acquire, then one batch for all three streams");
        let batch: Value = serde_json::from_str(&fake.sent()[1]).unwrap();
        assert_eq!(batch.as_array().unwrap().len(), 3);
        assert_eq!(batch[0]["params"]["handler"], "h0");
    }

    #[tokio::test]
    async fn programming_no_streams_does_not_reach_the_wire() {
        let (engine, fake) = engine(&[ACQUIRE_TWO]);
        engine.acquire(&[EnginePortId(0), EnginePortId(1)], false).await.unwrap();

        engine.add_streams(EnginePortId(0), Vec::new()).await.unwrap();
        assert_eq!(fake.sent().len(), 1, "only the acquire");
    }

    #[tokio::test]
    async fn releasing_a_port_we_never_held_is_not_an_error() {
        // The shutdown path releases everything without checking first.
        let (engine, fake) = engine(&[]);
        assert!(engine.release(&[EnginePortId(0)]).await.is_ok());
        assert!(fake.sent().is_empty());
    }

    #[tokio::test]
    async fn stopping_traffic_on_a_port_we_never_held_is_not_an_error() {
        let (engine, fake) = engine(&[]);
        assert!(engine.stop_traffic(&[EnginePortId(0), EnginePortId(1)]).await.is_ok());
        assert!(fake.sent().is_empty());
    }

    #[test]
    fn port_counters_decode_from_trex_field_names() {
        let raw = json!({
            "opackets": 1000, "ipackets": 990,
            "obytes": 64000, "ibytes": 63360,
            "oerrors": 1, "ierrors": 2, "imissed": 3
        });
        let stats = decode_port_stats(&raw);

        assert_eq!(stats.tx_packets, 1000);
        assert_eq!(stats.rx_packets, 990);
        assert_eq!(stats.tx_bytes, 64000);
        assert_eq!(stats.rx_errors, 2);
        assert_eq!(stats.rx_dropped, 3);
    }

    #[test]
    fn a_missing_counter_decodes_to_zero_rather_than_failing_the_read() {
        // A build that omits one counter should not take out the collector.
        let stats = decode_port_stats(&json!({ "opackets": 5 }));
        assert_eq!(stats.tx_packets, 5);
        assert_eq!(stats.rx_packets, 0);
    }

    #[test]
    fn packet_group_counters_and_latency_decode_together() {
        let raw = json!({
            "flow_stats": {
                "7": {
                    "tp": {"total": 1000, "0": 1000},
                    "rp": {"total": 995, "1": 995},
                    "tb": {"total": 64000},
                    "rb": {"total": 63680}
                }
            },
            "latency": {
                "7": { "latency": { "total_min": 12.5, "average": 24.0, "total_max": 180.0, "jitter": 3.5 } }
            }
        });

        let stats = decode_pgid(&raw, PgId(7));
        assert_eq!(stats.tx_packets, 1000);
        assert_eq!(stats.rx_packets, 995);
        assert_eq!(stats.latency.min_us, Some(12.5));
        assert_eq!(stats.latency.avg_us, Some(24.0));
        assert_eq!(stats.latency.jitter_us, Some(3.5));
        assert!((stats.loss_pct() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn an_absent_packet_group_decodes_to_zeroes_not_a_failure() {
        let stats = decode_pgid(&json!({ "flow_stats": {} }), PgId(99));
        assert_eq!(stats.tx_packets, 0);
        assert!(stats.latency.avg_us.is_none());
    }

    #[test]
    fn a_baseline_makes_counters_relative_to_the_last_clear() {
        // This is what makes a trial measure the trial rather than everything
        // since the engine booted.
        let base = PortStats { tx_packets: 1_000_000, rx_packets: 999_000, ..Default::default() };
        let now = PortStats { tx_packets: 1_500_000, rx_packets: 1_498_000, ..Default::default() };

        let relative = subtract(now, base);
        assert_eq!(relative.tx_packets, 500_000);
        assert_eq!(relative.rx_packets, 499_000);
    }

    #[test]
    fn a_counter_that_went_backwards_saturates_instead_of_wrapping() {
        // An engine restart resets counters. Reporting a small wrong number
        // beats reporting eighteen quintillion packets.
        let base = PortStats { tx_packets: 1_000_000, ..Default::default() };
        let now = PortStats { tx_packets: 5, ..Default::default() };
        assert_eq!(subtract(now, base).tx_packets, 0);
    }

    #[tokio::test]
    async fn clear_stats_records_a_baseline_that_later_reads_subtract() {
        let (engine, _) = engine(&[
            // clear_stats reads current counters...
            r#"[{"jsonrpc":"2.0","id":1,"result":{"opackets":1000,"ipackets":1000}}]"#,
            // ...then a later read sees a larger absolute value.
            r#"[{"jsonrpc":"2.0","id":2,"result":{"opackets":1600,"ipackets":1550}}]"#,
        ]);

        engine.clear_stats(&[EnginePortId(0)]).await.unwrap();
        let stats = engine.port_stats(&[EnginePortId(0)]).await.unwrap();

        assert_eq!(stats[0].tx_packets, 600);
        assert_eq!(stats[0].rx_packets, 550);
    }

    #[tokio::test]
    async fn without_a_clear_counters_are_reported_absolutely() {
        let (engine, _) = engine(&[r#"[{"jsonrpc":"2.0","id":1,"result":{"opackets":1600}}]"#]);
        let stats = engine.port_stats(&[EnginePortId(0)]).await.unwrap();
        assert_eq!(stats[0].tx_packets, 1600);
    }

    #[tokio::test]
    async fn reading_no_ports_or_groups_does_not_reach_the_wire() {
        let (engine, fake) = engine(&[]);
        assert!(engine.port_stats(&[]).await.unwrap().is_empty());
        assert!(engine.pgid_stats(&[]).await.unwrap().is_empty());
        assert!(fake.sent().is_empty());
    }

    #[tokio::test]
    async fn health_reports_the_version_trex_returned() {
        let (engine, _) = engine(&[r#"{"jsonrpc":"2.0","id":1,"result":{"version":"v3.06"}}"#]);
        let health = engine.health().await.unwrap();

        assert!(health.connected);
        assert_eq!(health.version.as_deref(), Some("v3.06"));
        assert_eq!(health.port_count, 2);
    }

    #[tokio::test]
    async fn port_status_decodes_link_and_transmit_state() {
        let (engine, _) = engine(&[
            r#"[{"jsonrpc":"2.0","id":1,"result":{"state":"TX","speed":100,"attr":{"link":{"up":true},"src_mac":"00:11:22:33:44:55"}}},
                {"jsonrpc":"2.0","id":2,"result":{"state":"IDLE","speed":100,"attr":{"link":{"up":false}}}}]"#,
        ]);

        let status = engine.port_status().await.unwrap();
        assert!(status[0].link_up);
        assert!(status[0].transmitting);
        assert_eq!(status[0].speed_mbps, Some(100_000));
        assert_eq!(status[0].src_mac.as_deref(), Some("00:11:22:33:44:55"));

        assert!(!status[1].link_up);
        assert!(!status[1].transmitting);
    }

    #[tokio::test]
    async fn starting_traffic_sends_the_multiplier_and_duration() {
        let (engine, fake) = engine(&[ACQUIRE_TWO, r#"[{"jsonrpc":"2.0","id":3,"result":{}}]"#]);
        engine.acquire(&[EnginePortId(0), EnginePortId(1)], false).await.unwrap();

        engine
            .start_traffic(
                &[EnginePortId(0)],
                StartOptions { multiplier: 0.75, duration_secs: Some(60.0), force: false },
            )
            .await
            .unwrap();

        let batch: Value = serde_json::from_str(&fake.sent()[1]).unwrap();
        assert_eq!(batch[0]["params"]["mul"]["value"], 0.75);
        assert_eq!(batch[0]["params"]["duration"], 60.0);
        assert_eq!(batch[0]["params"]["handler"], "h0");
    }

    #[tokio::test]
    async fn an_unbounded_start_sends_a_negative_duration() {
        let (engine, fake) = engine(&[ACQUIRE_TWO, r#"[{"jsonrpc":"2.0","id":3,"result":{}}]"#]);
        engine.acquire(&[EnginePortId(0), EnginePortId(1)], false).await.unwrap();
        engine.start_traffic(&[EnginePortId(0)], StartOptions::default()).await.unwrap();

        let batch: Value = serde_json::from_str(&fake.sent()[1]).unwrap();
        assert_eq!(batch[0]["params"]["duration"], -1.0);
    }
}
