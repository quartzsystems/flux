//! Engine lifecycle: the actor that owns an instance, and the registry of them.
//!
//! A ZMQ socket cannot be shared across tasks, so an engine instance is owned by
//! exactly one task. Everything else reaches it through an [`EngineHandle`],
//! which sends a command down an `mpsc` channel and waits on a `oneshot` reply.
//! That is what makes the `Engine` trait's `Send + Sync` bound safe to rely on:
//! the handle is shared, the engine itself never is.
//!
//! The registry maps a port group to its running instance. There is at most one
//! per group, because a group *is* the set of ports an instance owns.

use std::collections::HashMap;
use std::sync::Arc;

use flux_core::engine::{
    AstfProfile, AstfStats, Engine, EngineError, EngineHealth, EnginePortId, EnginePortStatus,
    PgId, PgidStats, PortStats, StartOptions, StreamSpec,
};
use flux_core::types::Id;
use tokio::sync::{mpsc, oneshot, watch, RwLock};

pub mod launch;
pub mod mock;
pub mod trex;

/// How many commands may queue before a caller waits.
///
/// Engine calls are rare and fast; a deep queue would only hide a wedged
/// instance behind a growing backlog.
const COMMAND_QUEUE: usize = 32;

/// How long a caller waits for the engine task to answer.
///
/// Generous enough for a real TRex call that has to touch hardware, short enough
/// that a request path never hangs indefinitely on a wedged instance.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Lifecycle of an engine instance, as observers see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineState {
    /// Process spawned, not yet answering.
    Starting,
    /// Answering and owning its ports.
    Ready,
    /// Stopped cleanly.
    Stopped,
    /// Failed; the string says why.
    Failed(String),
}

/// One request to the engine task.
///
/// Spelled out as an enum rather than boxed closures so a stuck engine shows a
/// nameable command in a stack trace rather than an anonymous future.
enum Command {
    Health(oneshot::Sender<Result<EngineHealth, EngineError>>),
    PortStatus(oneshot::Sender<Result<Vec<EnginePortStatus>, EngineError>>),
    Acquire(Vec<EnginePortId>, bool, oneshot::Sender<Result<(), EngineError>>),
    Release(Vec<EnginePortId>, oneshot::Sender<Result<(), EngineError>>),
    ClearStreams(EnginePortId, oneshot::Sender<Result<(), EngineError>>),
    AddStreams(EnginePortId, Vec<StreamSpec>, oneshot::Sender<Result<(), EngineError>>),
    StartTraffic(Vec<EnginePortId>, StartOptions, oneshot::Sender<Result<(), EngineError>>),
    StopTraffic(Vec<EnginePortId>, oneshot::Sender<Result<(), EngineError>>),
    ClearStats(Vec<EnginePortId>, oneshot::Sender<Result<(), EngineError>>),
    PortStats(Vec<EnginePortId>, oneshot::Sender<Result<Vec<PortStats>, EngineError>>),
    PgidStats(Vec<PgId>, oneshot::Sender<Result<Vec<PgidStats>, EngineError>>),
    LoadAstf(Box<AstfProfile>, oneshot::Sender<Result<(), EngineError>>),
    StartAstf(Option<f64>, oneshot::Sender<Result<(), EngineError>>),
    StopAstf(oneshot::Sender<Result<(), EngineError>>),
    AstfStats(oneshot::Sender<Result<AstfStats, EngineError>>),
    Shutdown,
}

/// A shareable reference to one running engine instance.
#[derive(Clone)]
pub struct EngineHandle {
    /// Port group this instance serves.
    pub group_id: Id,
    /// How many ports the instance owns.
    pub port_count: u8,
    commands: mpsc::Sender<Command>,
    state: watch::Receiver<EngineState>,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("group_id", &self.group_id)
            .field("port_count", &self.port_count)
            .field("state", &*self.state.borrow())
            .finish()
    }
}

impl EngineHandle {
    /// Spawns the owning task for `engine` and returns a handle to it.
    pub fn spawn(group_id: Id, port_count: u8, engine: Box<dyn Engine>) -> Self {
        let (commands, rx) = mpsc::channel(COMMAND_QUEUE);
        let (state_tx, state) = watch::channel(EngineState::Starting);

        tokio::spawn(run_engine(group_id, engine, rx, state_tx));

        Self { group_id, port_count, commands, state }
    }

    /// The instance's current lifecycle state, without a round trip.
    pub fn state(&self) -> EngineState {
        self.state.borrow().clone()
    }

    /// A receiver for lifecycle transitions.
    pub fn watch_state(&self) -> watch::Receiver<EngineState> {
        self.state.clone()
    }

    /// True when the instance is answering.
    pub fn is_ready(&self) -> bool {
        matches!(self.state(), EngineState::Ready)
    }

    /// Sends a command and waits for its reply.
    ///
    /// Three things can go wrong and each maps to the same conclusion from the
    /// caller's point of view — the engine is not usable — but they are
    /// distinguished in the message because they need different fixes.
    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Command,
    ) -> Result<T, EngineError> {
        let (tx, rx) = oneshot::channel();

        self.commands
            .send(build(tx))
            .await
            .map_err(|_| EngineError::Unavailable("the engine task has stopped".into()))?;

        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                Err(EngineError::Unavailable("the engine task dropped the request".into()))
            }
            Err(_) => Err(EngineError::Timeout(CALL_TIMEOUT)),
        }
    }

    /// Liveness probe.
    pub async fn health(&self) -> Result<EngineHealth, EngineError> {
        self.call(Command::Health).await
    }

    /// Per-port state for every port the instance owns.
    pub async fn port_status(&self) -> Result<Vec<EnginePortStatus>, EngineError> {
        self.call(Command::PortStatus).await
    }

    /// Takes the exclusive lock on `ports`.
    pub async fn acquire(&self, ports: &[EnginePortId], force: bool) -> Result<(), EngineError> {
        self.call(|tx| Command::Acquire(ports.to_vec(), force, tx)).await
    }

    /// Releases the exclusive lock on `ports`.
    pub async fn release(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        self.call(|tx| Command::Release(ports.to_vec(), tx)).await
    }

    /// Removes every stream programmed on `port`.
    pub async fn clear_streams(&self, port: EnginePortId) -> Result<(), EngineError> {
        self.call(|tx| Command::ClearStreams(port, tx)).await
    }

    /// Programs `streams` onto `port`.
    pub async fn add_streams(
        &self,
        port: EnginePortId,
        streams: Vec<StreamSpec>,
    ) -> Result<(), EngineError> {
        self.call(|tx| Command::AddStreams(port, streams, tx)).await
    }

    /// Starts transmitting.
    pub async fn start_traffic(
        &self,
        ports: &[EnginePortId],
        opts: StartOptions,
    ) -> Result<(), EngineError> {
        self.call(|tx| Command::StartTraffic(ports.to_vec(), opts, tx)).await
    }

    /// Stops transmitting.
    pub async fn stop_traffic(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        self.call(|tx| Command::StopTraffic(ports.to_vec(), tx)).await
    }

    /// Zeroes counters. Called at the start of every trial.
    pub async fn clear_stats(&self, ports: &[EnginePortId]) -> Result<(), EngineError> {
        self.call(|tx| Command::ClearStats(ports.to_vec(), tx)).await
    }

    /// Reads cumulative per-port counters.
    pub async fn port_stats(&self, ports: &[EnginePortId]) -> Result<Vec<PortStats>, EngineError> {
        self.call(|tx| Command::PortStats(ports.to_vec(), tx)).await
    }

    /// Reads cumulative per-packet-group counters.
    pub async fn pgid_stats(&self, pgids: &[PgId]) -> Result<Vec<PgidStats>, EngineError> {
        self.call(|tx| Command::PgidStats(pgids.to_vec(), tx)).await
    }

    /// Programs a stateful load.
    pub async fn load_astf_profile(&self, profile: AstfProfile) -> Result<(), EngineError> {
        self.call(|tx| Command::LoadAstf(Box::new(profile), tx)).await
    }

    /// Starts the programmed stateful load.
    pub async fn start_astf(&self, duration_secs: Option<f64>) -> Result<(), EngineError> {
        self.call(|tx| Command::StartAstf(duration_secs, tx)).await
    }

    /// Stops the stateful load.
    pub async fn stop_astf(&self) -> Result<(), EngineError> {
        self.call(Command::StopAstf).await
    }

    /// Reads connection-level counters.
    pub async fn astf_stats(&self) -> Result<AstfStats, EngineError> {
        self.call(Command::AstfStats).await
    }

    /// Asks the owning task to stop.
    ///
    /// Best effort: a task that has already exited is the outcome we wanted.
    pub async fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown).await;
    }

    /// All engine port indices, in order.
    pub fn all_ports(&self) -> Vec<EnginePortId> {
        (0..self.port_count).map(EnginePortId).collect()
    }
}

/// The task that owns an engine instance and serves its command channel.
#[tracing::instrument(skip(engine, commands, state), fields(group_id = %group_id))]
async fn run_engine(
    group_id: Id,
    engine: Box<dyn Engine>,
    mut commands: mpsc::Receiver<Command>,
    state: watch::Sender<EngineState>,
) {
    // The first health check decides whether the instance is usable. Reporting
    // Ready before asking would make the dashboard claim an engine is up while
    // it is still spawning.
    match engine.health().await {
        Ok(health) => {
            tracing::info!(version = ?health.version, mode = %health.mode, "engine is ready");
            let _ = state.send(EngineState::Ready);
        }
        Err(err) => {
            tracing::error!(%err, "engine failed its initial health check");
            let _ = state.send(EngineState::Failed(err.to_string()));
        }
    }

    while let Some(command) = commands.recv().await {
        match command {
            Command::Shutdown => break,
            Command::Health(tx) => reply(tx, engine.health().await),
            Command::PortStatus(tx) => reply(tx, engine.port_status().await),
            Command::Acquire(ports, force, tx) => {
                reply(tx, engine.acquire(&ports, force).await);
            }
            Command::Release(ports, tx) => reply(tx, engine.release(&ports).await),
            Command::ClearStreams(port, tx) => reply(tx, engine.clear_streams(port).await),
            Command::AddStreams(port, streams, tx) => {
                reply(tx, engine.add_streams(port, streams).await);
            }
            Command::StartTraffic(ports, opts, tx) => {
                reply(tx, engine.start_traffic(&ports, opts).await);
            }
            Command::StopTraffic(ports, tx) => reply(tx, engine.stop_traffic(&ports).await),
            Command::ClearStats(ports, tx) => reply(tx, engine.clear_stats(&ports).await),
            Command::PortStats(ports, tx) => reply(tx, engine.port_stats(&ports).await),
            Command::PgidStats(pgids, tx) => reply(tx, engine.pgid_stats(&pgids).await),
            Command::LoadAstf(profile, tx) => {
                reply(tx, engine.load_astf_profile(*profile).await);
            }
            Command::StartAstf(duration, tx) => reply(tx, engine.start_astf(duration).await),
            Command::StopAstf(tx) => reply(tx, engine.stop_astf().await),
            Command::AstfStats(tx) => reply(tx, engine.astf_stats().await),
        }
    }

    // Stop traffic on the way out. An engine left transmitting after its owner
    // goes away is the worst failure mode this product has.
    let ports: Vec<EnginePortId> = (0..u8::MAX).map(EnginePortId).collect();
    if let Err(err) = engine.stop_traffic(&ports).await {
        tracing::warn!(%err, "could not stop traffic during engine shutdown");
    }
    // A stateless instance refuses this, which is the expected outcome and not
    // worth logging as a failure.
    let _ = engine.stop_astf().await;

    let _ = state.send(EngineState::Stopped);
    tracing::info!("engine task stopped");
}

/// Sends a reply, tolerating a caller that gave up.
///
/// A dropped receiver means the caller timed out or was cancelled — normal, and
/// not worth a log line per occurrence.
fn reply<T>(tx: oneshot::Sender<Result<T, EngineError>>, result: Result<T, EngineError>) {
    let _ = tx.send(result);
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Every running engine instance, keyed by the port group it serves.
#[derive(Clone, Default)]
pub struct EngineRegistry {
    instances: Arc<RwLock<HashMap<Id, EngineHandle>>>,
}

impl EngineRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a handle, replacing and shutting down any previous instance for
    /// the same group.
    ///
    /// Replacing rather than refusing is deliberate: a group whose engine
    /// crashed and was relaunched must end up with the new instance, and leaving
    /// the dead one in the map would make every later call fail.
    pub async fn insert(&self, handle: EngineHandle) {
        let previous = self.instances.write().await.insert(handle.group_id, handle);
        if let Some(old) = previous {
            tracing::info!(group_id = %old.group_id, "replacing an existing engine instance");
            old.shutdown().await;
        }
    }

    /// The instance serving `group_id`, if there is one.
    pub async fn get(&self, group_id: Id) -> Option<EngineHandle> {
        self.instances.read().await.get(&group_id).cloned()
    }

    /// Removes and stops the instance serving `group_id`.
    pub async fn remove(&self, group_id: Id) -> Option<EngineHandle> {
        let handle = self.instances.write().await.remove(&group_id);
        if let Some(h) = &handle {
            h.shutdown().await;
        }
        handle
    }

    /// Every running instance.
    pub async fn all(&self) -> Vec<EngineHandle> {
        self.instances.read().await.values().cloned().collect()
    }

    /// How many instances are running.
    pub async fn len(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Stops every instance. Called on daemon shutdown.
    pub async fn shutdown_all(&self) {
        let handles: Vec<EngineHandle> =
            self.instances.write().await.drain().map(|(_, h)| h).collect();
        for handle in handles {
            handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use flux_core::types::EngineMode;

    use super::mock::MockEngine;
    use super::*;

    /// Spawns a handle over a two-port mock.
    async fn handle() -> EngineHandle {
        let engine = MockEngine::new(EngineMode::Stl, 2);
        let handle = EngineHandle::spawn(Id::new_v4(), 2, Box::new(engine));
        // The task reports Ready after its first health check; give it a turn.
        wait_for_ready(&handle).await;
        handle
    }

    /// Waits until the instance leaves `Starting`.
    async fn wait_for_ready(handle: &EngineHandle) {
        let mut state = handle.watch_state();
        while *state.borrow() == EngineState::Starting {
            if state.changed().await.is_err() {
                break;
            }
        }
    }

    #[tokio::test]
    async fn a_spawned_engine_reports_ready_after_its_health_check() {
        let handle = handle().await;
        assert_eq!(handle.state(), EngineState::Ready);
        assert!(handle.is_ready());
    }

    #[tokio::test]
    async fn commands_reach_the_engine_through_the_handle() {
        let handle = handle().await;

        let health = handle.health().await.unwrap();
        assert_eq!(health.port_count, 2);
        assert!(health.connected);

        let status = handle.port_status().await.unwrap();
        assert_eq!(status.len(), 2);
    }

    #[tokio::test]
    async fn all_ports_enumerates_the_instances_port_indices() {
        let handle = handle().await;
        assert_eq!(handle.all_ports(), vec![EnginePortId(0), EnginePortId(1)]);
    }

    #[tokio::test]
    async fn calls_after_shutdown_report_the_engine_as_unavailable() {
        let handle = handle().await;
        handle.shutdown().await;

        // Let the task drain and drop its receiver.
        for _ in 0..100 {
            if handle.health().await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("calls should stop succeeding once the engine task has exited");
    }

    #[tokio::test]
    async fn the_registry_returns_what_was_inserted() {
        let registry = EngineRegistry::new();
        let handle = handle().await;
        let group_id = handle.group_id;

        assert_eq!(registry.len().await, 0);
        registry.insert(handle).await;

        assert_eq!(registry.len().await, 1);
        assert!(registry.get(group_id).await.is_some());
        assert!(registry.get(Id::new_v4()).await.is_none());
    }

    #[tokio::test]
    async fn inserting_for_the_same_group_replaces_the_previous_instance() {
        // A relaunched engine must end up in the map; leaving the dead one there
        // would make every later call fail.
        let registry = EngineRegistry::new();
        let group_id = Id::new_v4();

        let first = EngineHandle::spawn(group_id, 2, Box::new(MockEngine::new(EngineMode::Stl, 2)));
        registry.insert(first).await;

        let second =
            EngineHandle::spawn(group_id, 4, Box::new(MockEngine::new(EngineMode::Stl, 4)));
        registry.insert(second).await;

        assert_eq!(registry.len().await, 1);
        assert_eq!(registry.get(group_id).await.unwrap().port_count, 4);
    }

    #[tokio::test]
    async fn removing_stops_the_instance_and_empties_the_registry() {
        let registry = EngineRegistry::new();
        let handle = handle().await;
        let group_id = handle.group_id;
        registry.insert(handle).await;

        assert!(registry.remove(group_id).await.is_some());
        assert_eq!(registry.len().await, 0);
        assert!(registry.remove(group_id).await.is_none());
    }

    #[tokio::test]
    async fn shutting_down_clears_every_instance() {
        let registry = EngineRegistry::new();
        for _ in 0..3 {
            registry.insert(handle().await).await;
        }
        assert_eq!(registry.len().await, 3);

        registry.shutdown_all().await;
        assert_eq!(registry.len().await, 0);
    }
}
