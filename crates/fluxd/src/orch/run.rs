//! The run lifecycle.
//!
//! A run is a spawned task holding a cancellation token. It walks the state
//! machine — `pending` → `validating` → `preparing` → `running` → `analyzing` →
//! terminal — persisting every transition, so the dashboard and the run view are
//! reading the same source of truth as a restart-recovery sweep would.
//!
//! Milestone 2 implements the manual test type: program these flows, transmit
//! until stopped or until the configured duration elapses, record what happened.
//! Milestone 3 puts the RFC 2544 search on top of the same machinery.
//!
//! ## Cleanup is unconditional
//!
//! Whatever else happens, the run task stops traffic and releases its ports on
//! the way out. An engine left transmitting after its run has failed is the worst
//! outcome this system has: it saturates a link nobody is watching.

use std::collections::HashMap;
use std::sync::Arc;

use flux_core::config::{EngineInstanceConfig, Validate};
use flux_core::engine::{EnginePortId, PgId, StartOptions};
use flux_core::flow::FlowConfig;
use flux_core::profile::LoadProfileConfig;
use flux_core::rfc2544::Rfc2544Config;
use flux_core::types::{Id, RunState, TestType};
use serde_json::json;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::translate;
use crate::collector::{CollectionTarget, Collector, RunProgress};
use crate::config::Config;
use crate::engine::launch::{self, LaunchRequest};
use crate::engine::{EngineHandle, EngineRegistry};
use crate::store::models::{Flow, LoadProfile, Port, Test};
use crate::store::{self, Store};

/// A run that can be stopped.
#[derive(Clone)]
pub struct RunHandle {
    /// Which run.
    pub run_id: Id,
    cancel: CancellationToken,
}

impl RunHandle {
    /// Asks the run to stop. Returns immediately; the task unwinds on its own.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

/// Why a run could not be started.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The test or something it references does not exist.
    #[error("{0}")]
    NotFound(String),

    /// The configuration cannot be run as it stands.
    #[error("{0}")]
    Invalid(String),

    /// Something the run needs is busy.
    #[error("{0}")]
    Conflict(String),

    /// The database refused.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Owns every in-flight run.
#[derive(Clone)]
pub struct RunSupervisor {
    store: Store,
    engines: EngineRegistry,
    collector: Collector,
    config: Arc<Config>,
    /// Where a newly launched mock engine's knobs are published, so the debug
    /// endpoints can find them.
    mock_controls: crate::state::MockControlRegistry,
    active: Arc<RwLock<HashMap<Id, RunHandle>>>,
}

impl RunSupervisor {
    /// Builds a supervisor.
    pub fn new(
        store: Store,
        engines: EngineRegistry,
        collector: Collector,
        config: Arc<Config>,
        mock_controls: crate::state::MockControlRegistry,
    ) -> Self {
        Self {
            store,
            engines,
            collector,
            config,
            mock_controls,
            active: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The handle for a run, if it is still in flight.
    pub async fn get(&self, run_id: Id) -> Option<RunHandle> {
        self.active.read().await.get(&run_id).cloned()
    }

    /// Every run currently in flight.
    pub async fn active(&self) -> Vec<Id> {
        self.active.read().await.keys().copied().collect()
    }

    /// Stops a run. Returns false when it was not running.
    pub async fn stop(&self, run_id: Id) -> bool {
        match self.active.read().await.get(&run_id) {
            Some(handle) => {
                tracing::info!(run_id = %handle.run_id, "stopping run");
                handle.stop();
                true
            }
            None => false,
        }
    }

    /// Stops every run. Called on daemon shutdown.
    pub async fn stop_all(&self) {
        for handle in self.active.read().await.values() {
            handle.stop();
        }
    }

    /// Validates a test and starts a run for it.
    ///
    /// Validation happens before the run row is created, so a configuration
    /// error is a rejected request rather than a run that exists only to record
    /// its own impossibility.
    #[tracing::instrument(skip(self, dut_meta), fields(test_id = %test.id, test = %test.name))]
    pub async fn start(
        &self,
        test: &Test,
        started_by: Option<Id>,
        dut_meta: serde_json::Value,
    ) -> Result<Id, RunError> {
        let plan = self.plan(test).await?;

        let snapshot = json!({
            "test": { "id": test.id, "name": test.name, "type": test.test_type.as_str() },
            // The benchmark document travels with the run so a report can state
            // the trial length and loss tolerance it was measured under, long
            // after the test itself has been edited or deleted.
            "rfc2544": plan.rfc2544,
            "flows": plan.flows.iter().map(|f| json!({
                "id": f.flow.id,
                "name": f.flow.name,
                "config": f.flow.config,
            })).collect::<Vec<_>>(),
            // Profiles travel for the same reason flows do: a stateful run's
            // record has to say what load it applied, and the profile it names
            // may have been retuned or deleted by the time anyone reads it.
            "profiles": plan.profiles.iter().map(|p| json!({
                "id": p.profile.id,
                "name": p.profile.name,
                "config": p.profile.config,
            })).collect::<Vec<_>>(),
            "ports": plan.ports.iter().map(|(index, port)| json!({
                "id": port.id,
                "name": port.name,
                "pciAddr": port.pci_addr.as_str(),
                "engineIndex": index.0,
            })).collect::<Vec<_>>(),
        });

        let run = store::runs::create(
            self.store.pool(),
            Some(test.id),
            &test.name,
            test.test_type.as_str(),
            started_by,
            &dut_meta,
            &snapshot,
        )
        .await?;

        let cancel = CancellationToken::new();
        let handle = RunHandle { run_id: run.id, cancel: cancel.clone() };
        self.active.write().await.insert(run.id, handle.clone());

        let supervisor = self.clone();
        tokio::spawn(async move {
            let run_id = run.id;
            let outcome = supervisor.execute(run_id, plan, cancel).await;
            supervisor.finish(run_id, outcome).await;
        });

        tracing::info!(run_id = %run.id, "run started");
        Ok(run.id)
    }

    /// Works out what a test would actually do, without doing any of it.
    async fn plan(&self, test: &Test) -> Result<RunPlan, RunError> {
        if test.flow_ids.is_empty() && test.profile_ids.is_empty() {
            return Err(RunError::Invalid("this test drives no flows or profiles".into()));
        }

        // Flows are stateless streams and profiles are connection-level loads;
        // they are programmed through different engine calls and an instance is
        // in one mode or the other, so a test cannot mix them.
        if !test.flow_ids.is_empty() && !test.profile_ids.is_empty() {
            return Err(RunError::Invalid(
                "a test drives either flows or load profiles, not both".into(),
            ));
        }

        let flows = store::flows::get_many(self.store.pool(), &test.flow_ids).await?;
        if flows.len() != test.flow_ids.len() {
            return Err(RunError::NotFound(
                "one or more of this test's flows no longer exists".into(),
            ));
        }

        let profile_rows = store::profiles::get_many(self.store.pool(), &test.profile_ids).await?;
        if profile_rows.len() != test.profile_ids.len() {
            return Err(RunError::NotFound(
                "one or more of this test's load profiles no longer exists".into(),
            ));
        }

        // Resolve each flow's configuration and the ports it names.
        let mut resolved = Vec::with_capacity(flows.len());
        let mut group_id: Option<Id> = None;
        let mut ports_by_id: HashMap<Id, Port> = HashMap::new();

        for flow in flows {
            let config: FlowConfig = serde_json::from_value(flow.config.clone())
                .map_err(|e| RunError::Invalid(format!("flow {} is unreadable: {e}", flow.name)))?;

            for port_id in [config.tx_port, config.rx_port] {
                if ports_by_id.contains_key(&port_id) {
                    continue;
                }
                let port =
                    store::ports::get(self.store.pool(), port_id).await?.ok_or_else(|| {
                        RunError::NotFound(format!(
                            "flow {} names a port that no longer exists",
                            flow.name
                        ))
                    })?;

                let port_group = port.group_id.ok_or_else(|| {
                    RunError::Invalid(format!(
                        "port {} is not in a port group, so no engine can drive it",
                        port.name
                    ))
                })?;

                // One engine instance per group, so a run spanning two groups
                // would need two instances started in lockstep. That is
                // milestone 4's port-group work.
                match group_id {
                    None => group_id = Some(port_group),
                    Some(existing) if existing != port_group => {
                        return Err(RunError::Invalid(
                            "every flow in a test must use ports from one port group".into(),
                        ));
                    }
                    Some(_) => {}
                }

                ports_by_id.insert(port_id, port);
            }

            resolved.push(PlannedFlow { flow, config });
        }

        let mut planned_profiles = Vec::with_capacity(profile_rows.len());
        for row in profile_rows {
            let config: LoadProfileConfig =
                serde_json::from_value(row.config.clone()).map_err(|e| {
                    RunError::Invalid(format!("profile {} is unreadable: {e}", row.name))
                })?;

            for port_id in [config.client_port, config.server_port] {
                if ports_by_id.contains_key(&port_id) {
                    continue;
                }
                let port =
                    store::ports::get(self.store.pool(), port_id).await?.ok_or_else(|| {
                        RunError::NotFound(format!(
                            "profile {} names a port that no longer exists",
                            row.name
                        ))
                    })?;

                let port_group = port.group_id.ok_or_else(|| {
                    RunError::Invalid(format!(
                        "port {} is not in a port group, so no engine can drive it",
                        port.name
                    ))
                })?;

                match group_id {
                    None => group_id = Some(port_group),
                    Some(existing) if existing != port_group => {
                        return Err(RunError::Invalid(
                            "every profile in a test must use ports from one port group".into(),
                        ));
                    }
                    Some(_) => {}
                }

                ports_by_id.insert(port_id, port);
            }

            planned_profiles.push(PlannedProfile { profile: row, config });
        }

        let group_id = group_id.ok_or_else(|| RunError::Invalid("no ports resolved".into()))?;

        // Engine port numbering is the group's member ordering, which is what
        // every later call addresses.
        let member_ids = store::port_groups::member_ids(self.store.pool(), group_id).await?;
        let ports: Vec<(EnginePortId, Port)> = member_ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| ports_by_id.remove(id).map(|p| (EnginePortId(i as u8), p)))
            .collect();

        if ports.is_empty() {
            return Err(RunError::Invalid(
                "the port group has no members matching this test's flows".into(),
            ));
        }

        // Index every group member so translation can map a flow's port id onto
        // an engine port number.
        let engine_index: HashMap<Id, EnginePortId> =
            member_ids.iter().enumerate().map(|(i, id)| (*id, EnginePortId(i as u8))).collect();

        // The benchmark document is validated before the run row exists, so a
        // bad configuration is a rejected request rather than a run that exists
        // only to record its own impossibility.
        let rfc2544 = match test.test_type {
            TestType::Manual => None,
            _ => {
                let config: Rfc2544Config =
                    serde_json::from_value(test.config.clone()).map_err(|e| {
                        RunError::Invalid(format!("test configuration is unreadable: {e}"))
                    })?;

                config.validate().map_err(|errors| {
                    RunError::Invalid(
                        errors
                            .iter()
                            .map(|e| format!("{}: {}", e.path, e.msg))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                })?;

                Some(config)
            }
        };

        Ok(RunPlan {
            group_id,
            test_type: test.test_type,
            rfc2544,
            flows: resolved,
            profiles: planned_profiles,
            ports,
            engine_index,
            member_ids,
        })
    }

    /// Drives one run from start to finish.
    async fn execute(
        &self,
        run_id: Id,
        plan: RunPlan,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        self.transition(run_id, RunState::Validating, None).await;

        let engine = self.ensure_engine(&plan).await.map_err(|e| e.to_string())?;
        if !engine.is_ready() {
            return Err(format!("the engine for this port group is {:?}", engine.state()));
        }

        self.transition(run_id, RunState::Preparing, None).await;

        // Everything from here holds engine resources, so cleanup runs whatever
        // the outcome.
        let result = self.prepare_and_run(run_id, &plan, &engine, &cancel).await;
        self.cleanup(&plan, &engine).await;

        result
    }

    /// Programs the engine, transmits, and records the result.
    async fn prepare_and_run(
        &self,
        run_id: Id,
        plan: &RunPlan,
        engine: &EngineHandle,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let all_ports: Vec<EnginePortId> = plan.ports.iter().map(|(i, _)| *i).collect();

        engine.acquire(&all_ports, false).await.map_err(|e| format!("acquiring ports: {e}"))?;

        // A port with no carrier transmits into nothing and receives nothing,
        // which produces a run reporting 100% loss and no indication why. Say so
        // before spending the trial rather than after.
        let status = engine.port_status().await.map_err(|e| format!("reading port state: {e}"))?;
        let dark: Vec<String> = plan
            .ports
            .iter()
            .filter(|(index, _)| status.iter().any(|s| s.port == *index && !s.link_up))
            .map(|(_, port)| port.name.clone())
            .collect();
        if !dark.is_empty() {
            return Err(format!("no link on {}; check the cabling", dark.join(", ")));
        }

        // A stateful test programs a connection-level load instead of streams,
        // so it diverges before any of the stream machinery runs.
        if !plan.profiles.is_empty() {
            return self.run_profiles(run_id, plan, engine, cancel).await;
        }

        // Programme each flow onto its transmitting port, allocating packet
        // groups as we go so statistics can be attributed back to the flow.
        let mut pgid_map: Vec<(PgId, Id)> = Vec::new();
        let mut next_pgid: u32 = 1;
        let mut streams_by_port: HashMap<EnginePortId, Vec<flux_core::engine::StreamSpec>> =
            HashMap::new();

        for planned in &plan.flows {
            let tx = *plan.engine_index.get(&planned.config.tx_port).ok_or_else(|| {
                format!("flow {} transmits from a port outside the group", planned.flow.name)
            })?;

            let speed = plan
                .ports
                .iter()
                .find(|(i, _)| *i == tx)
                .and_then(|(_, p)| p.speed_mbps)
                .unwrap_or(0) as u32;

            let streams = translate::to_streams(&planned.config, PgId(next_pgid), speed)
                .map_err(|e| format!("flow {}: {e}", planned.flow.name))?;

            for stream in &streams {
                pgid_map.push((stream.pg_id, planned.flow.id));
            }
            next_pgid += streams.len() as u32;

            streams_by_port.entry(tx).or_default().extend(streams);
        }

        for (port, streams) in streams_by_port {
            engine
                .clear_streams(port)
                .await
                .map_err(|e| format!("clearing streams on port {port}: {e}"))?;
            engine
                .add_streams(port, streams)
                .await
                .map_err(|e| format!("programming streams on port {port}: {e}"))?;
        }

        engine.clear_stats(&all_ports).await.map_err(|e| format!("clearing statistics: {e}"))?;

        // Collection starts before traffic does, so the first sample after the
        // start is a real one rather than the difference from nothing.
        //
        // The packet-group numbering here is the same one the benchmark loop
        // reallocates on every trial (flow order, starting at one), so this map
        // stays correct across a whole RFC 2544 search.
        self.collector
            .start(CollectionTarget {
                group_id: plan.group_id,
                engine: engine.clone(),
                ports: plan.ports.iter().map(|(i, p)| (*i, p.id)).collect(),
                pgids: pgid_map,
                run_id: Some(run_id),
                stateful: false,
            })
            .await;

        // An RFC 2544 test drives the engine itself from here: it reprograms
        // streams per frame size and moves the multiplier per trial, so the
        // single start-and-wait below is the manual path only.
        if let Some(config) = &plan.rfc2544 {
            self.transition(run_id, RunState::Running, None).await;

            let benchmark = super::statemachine::Benchmark {
                run_id,
                test_type: plan.test_type,
                config: config.clone(),
                plan,
                engine,
                store: &self.store,
                collector: &self.collector,
                cancel,
            };
            benchmark.run().await?;

            self.transition(run_id, RunState::Analyzing, None).await;
            return Ok(());
        }

        // A manual test runs until stopped unless a flow set a duration; the
        // shortest one wins, because that flow stopping mid-run would make the
        // remaining measurement meaningless.
        let duration = plan
            .flows
            .iter()
            .filter_map(|f| f.config.duration_secs)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let tx_ports: Vec<EnginePortId> = streams_ports(plan);
        engine
            .start_traffic(
                &tx_ports,
                StartOptions { multiplier: 1.0, duration_secs: duration, force: false },
            )
            .await
            .map_err(|e| format!("starting traffic: {e}"))?;

        self.transition(run_id, RunState::Running, None).await;
        self.publish_progress(run_id, "running", duration, None).await;

        // Wait for whichever comes first: the operator, or the clock.
        match duration {
            Some(seconds) => {
                let sleep = tokio::time::sleep(std::time::Duration::from_secs_f64(seconds));
                tokio::select! {
                    () = sleep => tracing::info!(run_id = %run_id, "run reached its configured duration"),
                    () = cancel.cancelled() => tracing::info!(run_id = %run_id, "run cancelled"),
                }
            }
            None => cancel.cancelled().await,
        }

        self.transition(run_id, RunState::Analyzing, None).await;

        // Stop before reading, so the counters cannot move underneath the read.
        engine.stop_traffic(&tx_ports).await.map_err(|e| format!("stopping traffic: {e}"))?;
        self.record_results(run_id, plan, engine).await?;

        Ok(())
    }

    /// Programs and runs a stateful load.
    ///
    /// One profile per run: an engine instance holds a single ASTF document, so
    /// a test naming several would need them merged into one profile, which is
    /// a different feature from running them in sequence.
    async fn run_profiles(
        &self,
        run_id: Id,
        plan: &RunPlan,
        engine: &EngineHandle,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let planned = plan.profiles.first().ok_or_else(|| "no load profile to run".to_string())?;

        if plan.profiles.len() > 1 {
            return Err(format!(
                "this test names {} load profiles; an engine instance runs one at a time",
                plan.profiles.len()
            ));
        }

        let load = super::profile::to_astf(&planned.config, &plan.engine_index)
            .map_err(|e| format!("profile {}: {e}", planned.profile.name))?;

        let target_cps = load.target_cps;
        let warmup = load.warmup_secs;

        engine.load_astf_profile(load).await.map_err(|e| format!("programming the load: {e}"))?;

        // Collection starts before the load does, so the first sample after the
        // start is a real one rather than the difference from nothing.
        self.collector
            .start(CollectionTarget {
                group_id: plan.group_id,
                engine: engine.clone(),
                ports: plan.ports.iter().map(|(i, p)| (*i, p.id)).collect(),
                pgids: Vec::new(),
                run_id: Some(run_id),
                stateful: true,
            })
            .await;

        let duration = planned.config.duration_secs;
        engine.start_astf(duration).await.map_err(|e| format!("starting the load: {e}"))?;

        self.transition(run_id, RunState::Running, None).await;
        self.collector
            .set_progress(
                run_id,
                RunProgress {
                    run_id: run_id.to_string(),
                    state: "running".into(),
                    iteration: None,
                    frame_size: None,
                    trial_rate_pct: None,
                    trial_remaining_secs: duration,
                    progress: None,
                    message: Some(format!(
                        "{} · ramping to {target_cps:.0} connections/s over {warmup:.0}s",
                        planned.profile.name
                    )),
                },
            )
            .await;

        match duration {
            Some(seconds) => {
                let sleep = tokio::time::sleep(std::time::Duration::from_secs_f64(seconds));
                tokio::select! {
                    () = sleep => tracing::info!(%run_id, "load reached its configured duration"),
                    () = cancel.cancelled() => tracing::info!(%run_id, "load cancelled"),
                }
            }
            None => cancel.cancelled().await,
        }

        self.transition(run_id, RunState::Analyzing, None).await;

        // Stop before reading, so the counters cannot move underneath the read.
        engine.stop_astf().await.map_err(|e| format!("stopping the load: {e}"))?;

        let stats =
            engine.astf_stats().await.map_err(|e| format!("reading final statistics: {e}"))?;

        let params = json!({
            "profileId": planned.profile.id,
            "profileName": planned.profile.name,
            "targetCps": target_cps,
            "maxConcurrent": planned.config.max_concurrent,
            "warmupSecs": warmup,
            "app": planned.config.app,
        });
        let metrics = json!({
            "attempted": stats.attempted,
            "established": stats.established,
            "closed": stats.closed,
            "active": stats.active,
            "connectErrors": stats.connect_errors,
            "resets": stats.resets,
            "failurePct": stats.failure_pct(),
            "txBytes": stats.tx_bytes,
            "rxBytes": stats.rx_bytes,
        });

        store::runs::add_result(
            self.store.pool(),
            run_id,
            0,
            None,
            &params,
            &metrics,
            // A load test records what happened; it carries no pass criterion of
            // its own beyond the connection failures already in the metrics.
            stats.connect_errors == 0,
        )
        .await
        .map_err(|e| format!("recording results: {e}"))?;

        Ok(())
    }

    /// Reads final counters and writes one result row per flow.
    async fn record_results(
        &self,
        run_id: Id,
        plan: &RunPlan,
        engine: &EngineHandle,
    ) -> Result<(), String> {
        let mut next_pgid: u32 = 1;

        for (iteration, planned) in plan.flows.iter().enumerate() {
            let stream_count = translate::to_streams(&planned.config, PgId(next_pgid), 0)
                .map(|s| s.len() as u32)
                .unwrap_or(1);
            let pgids: Vec<PgId> = (0..stream_count).map(|i| PgId(next_pgid + i)).collect();
            next_pgid += stream_count;

            let stats = engine
                .pgid_stats(&pgids)
                .await
                .map_err(|e| format!("reading final statistics: {e}"))?;

            let tx: u64 = stats.iter().map(|s| s.tx_packets).sum();
            let rx: u64 = stats.iter().map(|s| s.rx_packets).sum();
            let loss_pct =
                if tx == 0 { 0.0 } else { (tx.saturating_sub(rx) as f64 / tx as f64) * 100.0 };
            let latency = stats.iter().find_map(|s| s.latency.p50_us.map(|_| s.latency));

            let metrics = json!({
                "txPackets": tx,
                "rxPackets": rx,
                "lostPackets": tx.saturating_sub(rx),
                "lossPct": loss_pct,
                "latMinUs": latency.and_then(|l| l.min_us),
                "latAvgUs": latency.and_then(|l| l.avg_us),
                "latMaxUs": latency.and_then(|l| l.max_us),
                "latP50": latency.and_then(|l| l.p50_us),
                "latP99": latency.and_then(|l| l.p99_us),
                "latP999": latency.and_then(|l| l.p999_us),
                "jitterUs": latency.and_then(|l| l.jitter_us),
            });

            let params = json!({
                "flowId": planned.flow.id,
                "flowName": planned.flow.name,
                "frameSize": planned.config.size,
                "rate": planned.config.rate,
            });

            store::runs::add_result(
                self.store.pool(),
                run_id,
                iteration as i32,
                match planned.config.size {
                    flux_core::flow::FrameSize::Fixed { bytes } => Some(bytes as i32),
                    _ => None,
                },
                &params,
                &metrics,
                // A manual test has no pass criterion; it records what happened.
                true,
            )
            .await
            .map_err(|e| format!("recording results: {e}"))?;
        }

        Ok(())
    }

    /// Returns the engine for a plan's group, launching one if needed.
    async fn ensure_engine(&self, plan: &RunPlan) -> Result<EngineHandle, RunError> {
        if let Some(handle) = self.engines.get(plan.group_id).await {
            if handle.is_ready() {
                return Ok(handle);
            }
            // A handle in any other state is stale; replace it rather than
            // failing a run against an engine that has already gone away.
            self.engines.remove(plan.group_id).await;
        }

        let group = store::port_groups::get(self.store.pool(), plan.group_id)
            .await?
            .ok_or_else(|| RunError::NotFound("the port group no longer exists".into()))?;

        let instance: EngineInstanceConfig =
            serde_json::from_value(group.trex_cfg.clone()).unwrap_or_default();

        let ordered = store::ports::get_many_ordered(self.store.pool(), &plan.member_ids).await?;
        let pci_addrs = ordered.iter().map(|p| p.pci_addr.clone()).collect();
        let numa_node =
            ordered.first().and_then(|p| p.numa_node).and_then(|n| u32::try_from(n).ok());

        let launched = launch::launch(
            &self.config,
            LaunchRequest {
                group_id: plan.group_id,
                mode: group.engine_mode,
                pci_addrs,
                numa_node,
                instance,
            },
        )
        .await
        .map_err(|e| RunError::Conflict(format!("could not start the engine: {e}")))?;

        // Give the actor its first health check before declaring the group ready.
        let mut state = launched.handle.watch_state();
        while *state.borrow() == crate::engine::EngineState::Starting {
            if state.changed().await.is_err() {
                break;
            }
        }

        if let Some(controls) = launched.controls {
            self.mock_controls.write().await.insert(plan.group_id, controls);
        }

        self.engines.insert(launched.handle.clone()).await;
        Ok(launched.handle)
    }

    /// Stops traffic, releases ports, and stops collection.
    ///
    /// Every step is best effort and independently logged: a failure to release
    /// must not skip stopping collection.
    async fn cleanup(&self, plan: &RunPlan, engine: &EngineHandle) {
        // Every port the instance owns, not only the ones this run used: a
        // previous run that failed mid-cleanup may have left one transmitting.
        let all_ports = engine.all_ports();
        let _ = plan;

        if let Err(err) = engine.stop_traffic(&all_ports).await {
            tracing::error!(%err, "could not stop traffic during cleanup");
        }
        if let Err(err) = engine.release(&all_ports).await {
            tracing::warn!(%err, "could not release ports during cleanup");
        }
        self.collector.stop(plan.group_id).await;
    }

    /// Records the outcome and forgets the run.
    async fn finish(&self, run_id: Id, outcome: Result<(), String>) {
        let (state, error) = match outcome {
            Ok(()) => (RunState::Complete, None),
            Err(message) => {
                tracing::error!(%run_id, %message, "run failed");
                (RunState::Failed, Some(message))
            }
        };

        // A cancelled run is not a failed one, and the operator who stopped it
        // should not see it reported as an error.
        let state = if self
            .active
            .read()
            .await
            .get(&run_id)
            .is_some_and(|h| h.cancel.is_cancelled() && state == RunState::Complete)
        {
            RunState::Cancelled
        } else {
            state
        };

        self.transition(run_id, state, error.as_deref()).await;
        self.collector.clear_progress(run_id).await;
        self.active.write().await.remove(&run_id);
        tracing::info!(%run_id, %state, "run finished");
    }

    /// Persists a state transition and republishes progress.
    async fn transition(&self, run_id: Id, state: RunState, error: Option<&str>) {
        if let Err(err) = store::runs::set_state(self.store.pool(), run_id, state, error).await {
            tracing::error!(%err, %run_id, %state, "could not persist a run transition");
        }
        self.publish_progress(run_id, state.as_str(), None, error).await;
    }

    /// Publishes progress for the WebSocket stream.
    async fn publish_progress(
        &self,
        run_id: Id,
        state: &str,
        _duration: Option<f64>,
        message: Option<&str>,
    ) {
        self.collector
            .set_progress(
                run_id,
                RunProgress {
                    run_id: run_id.to_string(),
                    state: state.to_string(),
                    iteration: None,
                    frame_size: None,
                    trial_rate_pct: None,
                    trial_remaining_secs: None,
                    progress: None,
                    message: message.map(str::to_string),
                },
            )
            .await;
    }
}

/// The engine ports a plan actually transmits from.
fn streams_ports(plan: &RunPlan) -> Vec<EnginePortId> {
    let mut ports: Vec<EnginePortId> = plan
        .flows
        .iter()
        .filter_map(|f| plan.engine_index.get(&f.config.tx_port).copied())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// What a test resolves to before anything is programmed.
pub struct RunPlan {
    /// Port group the run will use.
    pub group_id: Id,
    /// Which kind of test this is.
    pub test_type: TestType,
    /// The benchmark configuration, for the four RFC 2544 types.
    pub rfc2544: Option<Rfc2544Config>,
    /// The flows to drive, with their parsed configuration.
    pub flows: Vec<PlannedFlow>,
    /// The load profiles to drive, with their parsed configuration.
    pub profiles: Vec<PlannedProfile>,
    /// Engine port index paired with the database row it came from.
    pub ports: Vec<(EnginePortId, Port)>,
    /// Maps a database port id onto its engine port number.
    pub engine_index: HashMap<Id, EnginePortId>,
    /// Every member of the group, in engine port-number order.
    pub member_ids: Vec<Id>,
}

/// One flow with its parsed configuration.
pub struct PlannedFlow {
    /// The stored row.
    pub flow: Flow,
    /// Its deserialised configuration.
    pub config: FlowConfig,
}

/// One load profile with its parsed configuration.
pub struct PlannedProfile {
    /// The stored row.
    pub profile: LoadProfile,
    /// Its deserialised configuration.
    pub config: LoadProfileConfig,
}

#[cfg(test)]
mod tests {
    use flux_core::flow::{EthernetFields, FrameSize, HeaderLayer, Ipv4Fields, Rate, UdpFields};

    use super::*;

    /// A flow between two ports.
    fn config(tx: Id, rx: Id) -> FlowConfig {
        FlowConfig {
            tx_port: tx,
            rx_port: rx,
            headers: vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            size: FrameSize::Fixed { bytes: 64 },
            rate: Rate::Percent { value: 10.0 },
            modifiers: Vec::new(),
            duration_secs: None,
            latency_track: false,
        }
    }

    /// A plan over two ports with one flow.
    fn plan(tx: Id, rx: Id) -> RunPlan {
        let mut engine_index = HashMap::new();
        engine_index.insert(tx, EnginePortId(0));
        engine_index.insert(rx, EnginePortId(1));

        RunPlan {
            group_id: Id::new_v4(),
            test_type: TestType::Manual,
            rfc2544: None,
            profiles: Vec::new(),
            flows: vec![PlannedFlow {
                flow: Flow {
                    id: Id::new_v4(),
                    name: "f".into(),
                    config: serde_json::Value::Null,
                    created_by: None,
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    updated_at: time::OffsetDateTime::UNIX_EPOCH,
                },
                config: config(tx, rx),
            }],
            ports: Vec::new(),
            engine_index,
            member_ids: vec![tx, rx],
        }
    }

    #[test]
    fn only_transmitting_ports_are_started() {
        // Starting traffic on the receiving port would generate traffic nobody
        // asked for, in the opposite direction.
        let (tx, rx) = (Id::new_v4(), Id::new_v4());
        assert_eq!(streams_ports(&plan(tx, rx)), vec![EnginePortId(0)]);
    }

    #[test]
    fn two_flows_sharing_a_transmit_port_start_it_once() {
        let (tx, rx) = (Id::new_v4(), Id::new_v4());
        let mut p = plan(tx, rx);
        let second = p.flows[0].config.clone();
        p.flows.push(PlannedFlow { flow: p.flows[0].flow.clone(), config: second });

        assert_eq!(streams_ports(&p), vec![EnginePortId(0)]);
    }

    #[test]
    fn a_bidirectional_pair_starts_both_ports() {
        let (a, b) = (Id::new_v4(), Id::new_v4());
        let mut p = plan(a, b);
        p.flows.push(PlannedFlow { flow: p.flows[0].flow.clone(), config: config(b, a) });

        assert_eq!(streams_ports(&p), vec![EnginePortId(0), EnginePortId(1)]);
    }
}
