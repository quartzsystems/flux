//! The RFC 2544 execution loop.
//!
//! This is the async half of the benchmark: it asks `orch::rfc2544` what to do,
//! does it, records the outcome, and asks again. Every decision — which rate to
//! try, when to stop, what to report — belongs to the pure search; this module
//! contributes only the engine calls, the clock, and the database writes.
//!
//! Keeping the split that clean is what makes the search testable. A bug in
//! "which rate next" is caught by a table test in milliseconds; a bug here shows
//! up as a run that does not progress, which is much more visible.

use flux_core::engine::{EnginePortId, PgId, StartOptions, StreamSpec};
use flux_core::flow::{FlowConfig, FrameSize, Rate};
use flux_core::rfc2544::Rfc2544Config;
use flux_core::types::{Id, TestType};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::rfc2544::{
    b2b_next, frameloss_next, search_window, throughput_next, BurstAction, BurstTrial, RateTrial,
    SearchAction, StopReason,
};
use super::run::RunPlan;
use super::translate;
use crate::collector::{Collector, RunProgress};
use crate::engine::EngineHandle;
use crate::store::{self, Store};

/// How long to let a back-to-back burst drain before reading counters.
///
/// The burst is transmitted as fast as the port allows; the frames still in
/// flight need somewhere to land before the receive counter is meaningful.
const BURST_SETTLE_SECS: f64 = 2.0;

/// Everything the benchmark loop needs.
pub struct Benchmark<'a> {
    /// The run being executed.
    pub run_id: Id,
    /// Which of the four RFC 2544 tests this is.
    pub test_type: TestType,
    /// The test's configuration.
    pub config: Rfc2544Config,
    /// What the run resolved to.
    pub plan: &'a RunPlan,
    /// The engine driving it.
    pub engine: &'a EngineHandle,
    /// Where results are written.
    pub store: &'a Store,
    /// Where progress is published.
    pub collector: &'a Collector,
    /// Cancelled when an operator stops the run.
    pub cancel: &'a CancellationToken,
}

impl Benchmark<'_> {
    /// Runs the benchmark to completion.
    ///
    /// Returns `Ok` once every frame size has been searched, or an error naming
    /// the step that failed. Cancellation is not an error: it unwinds through
    /// `Ok`, and the supervisor records it as cancelled.
    #[tracing::instrument(skip(self), fields(run_id = %self.run_id, kind = %self.test_type))]
    pub async fn run(&self) -> Result<(), String> {
        let mut iteration: i32 = 0;

        // Back-to-back searches burst length rather than rate, so it does not
        // share the per-frame-size rate loop.
        if self.test_type == TestType::Rfc2544B2b {
            return self.run_back_to_back(&mut iteration).await;
        }

        let sizes = self.config.frame_sizes.clone();
        for (index, frame_size) in sizes.iter().enumerate() {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            let overall = index as f64 / sizes.len() as f64;
            self.search_one_size(*frame_size, index, sizes.len(), overall, &mut iteration).await?;
        }

        Ok(())
    }

    /// Searches one frame size, recording every trial.
    async fn search_one_size(
        &self,
        frame_size: u32,
        size_index: usize,
        size_count: usize,
        overall: f64,
        iteration: &mut i32,
    ) -> Result<(), String> {
        let mut trials: Vec<RateTrial> = Vec::new();

        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            let action = match self.test_type {
                TestType::Rfc2544Frameloss => frameloss_next(&self.config, &trials),
                _ => throughput_next(&self.config, &trials),
            };

            match action {
                SearchAction::Trial { rate_pct } => {
                    let window = search_window(&self.config, &trials);

                    self.publish(RunProgress {
                        run_id: self.run_id.to_string(),
                        state: "running".into(),
                        iteration: Some(*iteration as u32 + 1),
                        frame_size: Some(frame_size),
                        trial_rate_pct: Some(rate_pct),
                        trial_remaining_secs: Some(self.config.trial_seconds),
                        progress: Some(overall),
                        message: Some(format!(
                            "frame {frame_size}B ({}/{size_count}) · trialling {rate_pct:.2}% · window {:.2}–{:.2}%",
                            size_index + 1,
                            window.lower_pct,
                            window.upper_pct
                        )),
                    })
                    .await;

                    let measured = self.rate_trial(frame_size, rate_pct).await?;
                    let passed = measured.passed(self.config.loss_tolerance_pct);

                    self.record_trial(*iteration, frame_size, &measured, passed, None).await?;
                    *iteration += 1;
                    trials.push(measured);
                }

                SearchAction::Converged { rate_pct, reason } => {
                    self.record_outcome(iteration, frame_size, rate_pct, reason, &trials).await?;
                    return Ok(());
                }
            }
        }
    }

    /// Records the converged result for a frame size.
    ///
    /// For the latency test this also runs one more trial at the found rate with
    /// latency measurement enabled — section 26.2 measures latency *at* the
    /// throughput rate, which is not known until the search finishes.
    async fn record_outcome(
        &self,
        iteration: &mut i32,
        frame_size: u32,
        rate_pct: Option<f64>,
        reason: StopReason,
        trials: &[RateTrial],
    ) -> Result<(), String> {
        let latency = match (self.test_type, rate_pct) {
            (TestType::Rfc2544Latency, Some(rate)) if !self.cancel.is_cancelled() => {
                self.publish(RunProgress {
                    run_id: self.run_id.to_string(),
                    state: "running".into(),
                    iteration: Some(*iteration as u32 + 1),
                    frame_size: Some(frame_size),
                    trial_rate_pct: Some(rate),
                    trial_remaining_secs: Some(self.config.trial_seconds),
                    progress: None,
                    message: Some(format!(
                        "frame {frame_size}B · measuring latency at {rate:.2}%"
                    )),
                })
                .await;

                Some(self.latency_trial(frame_size, rate).await?)
            }
            _ => None,
        };

        let params = json!({
            "frameSize": frame_size,
            "resultRatePct": rate_pct,
            "stopReason": reason.as_str(),
            "conclusive": reason.is_conclusive(),
            "trialsRun": trials.len(),
            "trialSeconds": self.config.trial_seconds,
            "lossTolerancePct": self.config.loss_tolerance_pct,
        });

        // The reported figure is the throughput in packets and bits per second,
        // not just a percentage: a percentage of an unstated line rate is not a
        // result anybody can compare against another tester.
        let line_pps = flux_core::rate::line_rate_pps(self.port_speed_mbps(), f64::from(frame_size));
        let result_pps = rate_pct.map(|pct| line_pps * pct / 100.0);

        let metrics = json!({
            "resultRatePct": rate_pct,
            "resultPps": result_pps,
            "resultBpsL1": result_pps.map(|pps| pps * flux_core::rate::wire_bits_per_frame(f64::from(frame_size))),
            "lineRatePps": line_pps,
            "latMinUs": latency.and_then(|l| l.min_us),
            "latAvgUs": latency.and_then(|l| l.avg_us),
            "latMaxUs": latency.and_then(|l| l.max_us),
            "latP50": latency.and_then(|l| l.p50_us),
            "latP99": latency.and_then(|l| l.p99_us),
            "latP999": latency.and_then(|l| l.p999_us),
            "jitterUs": latency.and_then(|l| l.jitter_us),
        });

        store::runs::add_result(
            self.store.pool(),
            self.run_id,
            *iteration,
            Some(frame_size as i32),
            &params,
            &metrics,
            // The summary row is the reportable result for this frame size; the
            // individual trials above it are the working.
            true,
        )
        .await
        .map_err(|e| format!("recording the result for {frame_size}B: {e}"))?;

        *iteration += 1;

        tracing::info!(
            frame_size,
            result = ?rate_pct,
            reason = reason.as_str(),
            trials = trials.len(),
            "frame size complete"
        );
        Ok(())
    }

    /// Runs the back-to-back burst search.
    async fn run_back_to_back(&self, iteration: &mut i32) -> Result<(), String> {
        let sizes = self.config.frame_sizes.clone();

        for (index, frame_size) in sizes.iter().enumerate() {
            let mut trials: Vec<BurstTrial> = Vec::new();

            loop {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }

                match b2b_next(&self.config, &trials) {
                    BurstAction::Trial { burst_frames } => {
                        self.publish(RunProgress {
                            run_id: self.run_id.to_string(),
                            state: "running".into(),
                            iteration: Some(*iteration as u32 + 1),
                            frame_size: Some(*frame_size),
                            trial_rate_pct: Some(100.0),
                            trial_remaining_secs: None,
                            progress: Some(index as f64 / sizes.len() as f64),
                            message: Some(format!(
                                "frame {frame_size}B · burst of {burst_frames} frames"
                            )),
                        })
                        .await;

                        let measured = self.burst_trial(*frame_size, burst_frames).await?;

                        let params = json!({
                            "frameSize": frame_size,
                            "burstFrames": burst_frames,
                        });
                        let metrics = json!({
                            "txPackets": measured.tx_packets,
                            "rxPackets": measured.rx_packets,
                            "lostPackets": measured.tx_packets.saturating_sub(measured.rx_packets),
                            "burstFrames": burst_frames,
                        });

                        store::runs::add_result(
                            self.store.pool(),
                            self.run_id,
                            *iteration,
                            Some(*frame_size as i32),
                            &params,
                            &metrics,
                            measured.passed(),
                        )
                        .await
                        .map_err(|e| format!("recording a burst trial: {e}"))?;

                        *iteration += 1;
                        trials.push(measured);
                    }

                    BurstAction::Converged { burst_frames, reason } => {
                        let params = json!({
                            "frameSize": frame_size,
                            "resultBurstFrames": burst_frames,
                            "stopReason": reason.as_str(),
                            "conclusive": reason.is_conclusive(),
                            "trialsRun": trials.len(),
                        });
                        let metrics = json!({
                            "resultBurstFrames": burst_frames,
                            // The figure operators compare is the burst duration,
                            // since that is what maps onto buffer depth.
                            "resultBurstMicros": burst_frames.map(|frames| {
                                let pps = flux_core::rate::line_rate_pps(
                                    self.port_speed_mbps(),
                                    f64::from(*frame_size),
                                );
                                if pps > 0.0 { frames as f64 / pps * 1e6 } else { 0.0 }
                            }),
                        });

                        store::runs::add_result(
                            self.store.pool(),
                            self.run_id,
                            *iteration,
                            Some(*frame_size as i32),
                            &params,
                            &metrics,
                            true,
                        )
                        .await
                        .map_err(|e| format!("recording the burst result: {e}"))?;

                        *iteration += 1;
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Engine interaction
    // -----------------------------------------------------------------------

    /// Runs one rate trial and returns what it measured.
    async fn rate_trial(&self, frame_size: u32, rate_pct: f64) -> Result<RateTrial, String> {
        let pgids = self.program(frame_size, None, false).await?;

        // The streams are programmed at full line rate and the search moves the
        // engine's multiplier. Reprogramming every trial would cost seconds per
        // iteration and risk the stream set drifting from what was measured.
        self.transmit(rate_pct / 100.0, Some(self.config.trial_seconds)).await?;

        let (tx, rx, _) = self.read(&pgids).await?;
        Ok(RateTrial { rate_pct, tx_packets: tx, rx_packets: rx })
    }

    /// Runs one trial with latency measurement enabled.
    async fn latency_trial(
        &self,
        frame_size: u32,
        rate_pct: f64,
    ) -> Result<flux_core::engine::LatencyStats, String> {
        let pgids = self.program(frame_size, None, true).await?;
        self.transmit(rate_pct / 100.0, Some(self.config.trial_seconds)).await?;

        let (_, _, latency) = self.read(&pgids).await?;
        Ok(latency)
    }

    /// Runs one burst trial.
    async fn burst_trial(
        &self,
        frame_size: u32,
        burst_frames: u64,
    ) -> Result<BurstTrial, String> {
        let pgids = self.program(frame_size, Some(burst_frames), false).await?;

        // Long enough for the burst itself at line rate, plus time for the tail
        // to arrive. Counters read before the burst drained would report loss
        // that is really just latency.
        let line_pps =
            flux_core::rate::line_rate_pps(self.port_speed_mbps(), f64::from(frame_size));
        let burst_secs = if line_pps > 0.0 { burst_frames as f64 / line_pps } else { 1.0 };

        self.transmit(1.0, Some(burst_secs + BURST_SETTLE_SECS)).await?;

        let (tx, rx, _) = self.read(&pgids).await?;
        Ok(BurstTrial { burst_frames, tx_packets: tx, rx_packets: rx })
    }

    /// Programs every flow at `frame_size`, returning the packet groups used.
    ///
    /// The test's frame size and full line rate override whatever the flow
    /// configured: an RFC 2544 test varies those itself, and the flow
    /// contributes the header stack, the ports, and the modifiers.
    async fn program(
        &self,
        frame_size: u32,
        total_packets: Option<u64>,
        force_latency: bool,
    ) -> Result<Vec<PgId>, String> {
        let mut pgids = Vec::new();
        let mut next_pgid: u32 = 1;
        let mut by_port: std::collections::HashMap<EnginePortId, Vec<StreamSpec>> =
            std::collections::HashMap::new();

        for planned in &self.plan.flows {
            let tx = *self
                .plan
                .engine_index
                .get(&planned.config.tx_port)
                .ok_or_else(|| format!("flow {} transmits outside the group", planned.flow.name))?;

            let benchmark_config = FlowConfig {
                size: FrameSize::Fixed { bytes: frame_size },
                rate: Rate::Percent { value: 100.0 },
                latency_track: planned.config.latency_track || force_latency,
                duration_secs: None,
                ..planned.config.clone()
            };

            let speed = self.port_speed_mbps();
            let mut streams = translate::to_streams(&benchmark_config, PgId(next_pgid), speed)
                .map_err(|e| format!("flow {}: {e}", planned.flow.name))?;

            for stream in &mut streams {
                stream.total_packets = total_packets;
                pgids.push(stream.pg_id);
            }
            next_pgid += streams.len() as u32;

            by_port.entry(tx).or_default().extend(streams);
        }

        for (port, streams) in by_port {
            self.engine
                .clear_streams(port)
                .await
                .map_err(|e| format!("clearing streams on port {port}: {e}"))?;
            self.engine
                .add_streams(port, streams)
                .await
                .map_err(|e| format!("programming port {port}: {e}"))?;
        }

        Ok(pgids)
    }

    /// Clears counters, transmits for `duration`, then stops.
    async fn transmit(&self, multiplier: f64, duration: Option<f64>) -> Result<(), String> {
        let tx_ports = self.transmit_ports();

        self.engine
            .clear_stats(&self.engine.all_ports())
            .await
            .map_err(|e| format!("clearing statistics: {e}"))?;

        self.engine
            .start_traffic(
                &tx_ports,
                StartOptions { multiplier, duration_secs: duration, force: false },
            )
            .await
            .map_err(|e| format!("starting traffic: {e}"))?;

        if let Some(seconds) = duration {
            let sleep = tokio::time::sleep(std::time::Duration::from_secs_f64(seconds));
            tokio::select! {
                () = sleep => {}
                () = self.cancel.cancelled() => {}
            }
        }

        // Always stop, including after cancellation: the trial is over either
        // way and an engine left transmitting is the failure that matters.
        self.engine
            .stop_traffic(&tx_ports)
            .await
            .map_err(|e| format!("stopping traffic: {e}"))?;

        Ok(())
    }

    /// Reads the totals for a set of packet groups.
    async fn read(
        &self,
        pgids: &[PgId],
    ) -> Result<(u64, u64, flux_core::engine::LatencyStats), String> {
        let stats = self
            .engine
            .pgid_stats(pgids)
            .await
            .map_err(|e| format!("reading trial statistics: {e}"))?;

        let tx = stats.iter().map(|s| s.tx_packets).sum();
        let rx = stats.iter().map(|s| s.rx_packets).sum();

        // Percentiles cannot be averaged across groups, so the reported latency
        // is the first group that measured any.
        let latency = stats
            .iter()
            .find(|s| s.latency.p50_us.is_some())
            .map(|s| s.latency)
            .unwrap_or_default();

        Ok((tx, rx, latency))
    }

    /// The engine ports the plan transmits from.
    fn transmit_ports(&self) -> Vec<EnginePortId> {
        let mut ports: Vec<EnginePortId> = self
            .plan
            .flows
            .iter()
            .filter_map(|f| self.plan.engine_index.get(&f.config.tx_port).copied())
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// The transmitting port's line speed, or zero when it has none.
    ///
    /// Zero propagates to a zero packet rate rather than an infinity, which is
    /// how a run against an unbound port reports "no measurement" instead of
    /// rendering NaN into a report.
    fn port_speed_mbps(&self) -> u32 {
        self.plan
            .ports
            .first()
            .and_then(|(_, port)| port.speed_mbps)
            .filter(|s| *s > 0)
            .unwrap_or(0) as u32
    }

    // -----------------------------------------------------------------------
    // Recording
    // -----------------------------------------------------------------------

    /// Writes one trial row.
    async fn record_trial(
        &self,
        iteration: i32,
        frame_size: u32,
        trial: &RateTrial,
        passed: bool,
        note: Option<&str>,
    ) -> Result<(), String> {
        let params = json!({
            "frameSize": frame_size,
            "ratePct": trial.rate_pct,
            "trialSeconds": self.config.trial_seconds,
            "note": note,
        });
        let metrics = json!({
            "txPackets": trial.tx_packets,
            "rxPackets": trial.rx_packets,
            "lostPackets": trial.tx_packets.saturating_sub(trial.rx_packets),
            "lossPct": trial.loss_pct(),
            "ratePct": trial.rate_pct,
        });

        store::runs::add_result(
            self.store.pool(),
            self.run_id,
            iteration,
            Some(frame_size as i32),
            &params,
            &metrics,
            passed,
        )
        .await
        .map_err(|e| format!("recording a trial: {e}"))?;

        tracing::debug!(
            frame_size,
            rate_pct = trial.rate_pct,
            loss_pct = trial.loss_pct(),
            passed,
            "trial complete"
        );
        Ok(())
    }

    /// Publishes progress for the live view.
    async fn publish(&self, progress: RunProgress) {
        self.collector.set_progress(self.run_id, progress).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_benchmark_overrides_the_flows_size_and_rate() {
        // The flow contributes its header stack, ports, and modifiers; the test
        // owns the frame size and the rate, because that is what it varies.
        let flow = FlowConfig {
            tx_port: Id::nil(),
            rx_port: Id::nil(),
            headers: vec![flux_core::flow::HeaderLayer::Ethernet(Default::default())],
            size: FrameSize::Fixed { bytes: 1518 },
            rate: Rate::Pps { value: 1000.0 },
            modifiers: Vec::new(),
            duration_secs: Some(30.0),
            latency_track: false,
        };

        // `force_latency` is what the latency test passes for its final trial.
        let force_latency = true;
        let benchmark = FlowConfig {
            size: FrameSize::Fixed { bytes: 64 },
            rate: Rate::Percent { value: 100.0 },
            latency_track: flow.latency_track || force_latency,
            duration_secs: None,
            ..flow.clone()
        };

        assert_eq!(benchmark.size, FrameSize::Fixed { bytes: 64 });
        assert_eq!(benchmark.rate, Rate::Percent { value: 100.0 });
        assert!(benchmark.latency_track);
        assert_eq!(
            benchmark.duration_secs, None,
            "the trial length comes from the test, not the flow"
        );
        assert_eq!(benchmark.headers, flow.headers, "the header stack is preserved");
    }

    #[test]
    fn a_burst_trial_waits_for_the_burst_plus_a_settling_period() {
        // Reading counters before the tail arrives would report latency as loss.
        let line_pps = flux_core::rate::line_rate_pps(10_000, 64.0);
        let burst_secs = 1_000_000.0 / line_pps;

        assert!(burst_secs > 0.0);
        assert!(burst_secs + BURST_SETTLE_SECS > burst_secs);
        // A million 64-byte frames at 10G is about 67 ms.
        assert!((burst_secs - 0.0672).abs() < 0.001, "got {burst_secs}");
    }
}
