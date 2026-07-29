//! Bringing an engine instance up for a port group.
//!
//! Which implementation gets built is the only place `FLUX_ENGINE` is consulted.
//! Everything downstream holds an [`EngineHandle`] and cannot tell the two apart,
//! which is what makes the mock a real development environment rather than a
//! special case threaded through the daemon.
//!
//! For TRex this is a four-step sequence — render the platform configuration,
//! spawn the process, wait for its RPC socket, negotiate the API version — and
//! each step fails with a message naming which one it was. "Engine failed to
//! start" is not something an operator can act on; "TRex did not answer on port
//! 4501 within 60 seconds" is.

use std::path::PathBuf;
use std::time::Duration;

use flux_core::config::EngineInstanceConfig;
use flux_core::engine::EngineError;
use flux_core::port::PciAddr;
use flux_core::types::{EngineMode, Id};

use super::mock::{MockControls, MockEngine};
use super::trex::supervisor::{LaunchSpec, Supervisor};
use super::trex::{config as trex_config, TrexEngine};
use super::EngineHandle;
use crate::config::{Config, EngineBackend};

/// How long to wait for a freshly spawned TRex to answer its RPC socket.
///
/// TRex binds NICs, allocates hugepages, and starts its poll-mode drivers before
/// it listens. On a many-port instance that is genuinely tens of seconds.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// How often to retry the handshake while waiting for startup.
const HANDSHAKE_RETRY: Duration = Duration::from_secs(2);

/// Where generated TRex configurations are written.
const CONFIG_DIR: &str = "/var/lib/flux";

/// What was launched, and the mock controls if it was a mock.
pub struct Launched {
    /// Handle to the running instance.
    pub handle: EngineHandle,
    /// Injectable behaviour, present only for a mock engine.
    pub controls: Option<MockControls>,
}

/// Everything needed to bring one instance up.
pub struct LaunchRequest {
    /// Port group this instance serves.
    pub group_id: Id,
    /// Stateless or stateful.
    pub mode: EngineMode,
    /// Member ports, in the order they should be numbered.
    pub pci_addrs: Vec<PciAddr>,
    /// NUMA node the ports sit on, for core placement.
    pub numa_node: Option<u32>,
    /// Instance settings from the port group.
    pub instance: EngineInstanceConfig,
}

/// Builds and starts the engine for a port group.
pub async fn launch(config: &Config, request: LaunchRequest) -> Result<Launched, EngineError> {
    let port_count = request.pci_addrs.len() as u8;
    if port_count == 0 {
        return Err(EngineError::Rejected(
            "a port group needs at least one port before an engine can start".into(),
        ));
    }

    match config.engine {
        EngineBackend::Mock => Ok(launch_mock(&request, port_count)),
        EngineBackend::Trex => launch_trex(request, port_count).await,
    }
}

/// Builds a simulated instance.
fn launch_mock(request: &LaunchRequest, port_count: u8) -> Launched {
    let engine = MockEngine::new(request.mode, port_count);
    let controls = engine.controls();
    let handle = EngineHandle::spawn(request.group_id, port_count, Box::new(engine));

    tracing::info!(group_id = %request.group_id, port_count, "launched a simulated engine");
    Launched { handle, controls: Some(controls) }
}

/// Renders a configuration, spawns TRex, and connects to it.
#[tracing::instrument(skip(request), fields(group_id = %request.group_id))]
async fn launch_trex(request: LaunchRequest, port_count: u8) -> Result<Launched, EngineError> {
    let config_path = write_platform_config(&request)?;

    let spec = LaunchSpec {
        // TODO(trex-verify): the installed path. `/opt/trex/current` is the
        // symlink the TRex installer creates.
        binary: PathBuf::from("/opt/trex/current/t-rex-64"),
        working_dir: PathBuf::from("/opt/trex/current"),
        config_path,
        rpc_port: request.instance.rpc_port,
        async_port: request.instance.async_port,
        astf: request.mode == EngineMode::Astf,
    };

    let supervisor = Supervisor::new(spec);
    let child = supervisor
        .spawn()
        .map_err(|e| EngineError::Unavailable(format!("could not start TRex: {e}")))?;

    // The child is held by a watchdog task that keeps it alive for as long as
    // the engine is registered. Dropping it here would kill TRex immediately,
    // because the command is configured to kill on drop.
    tokio::spawn(watchdog(request.group_id, supervisor, child));

    let engine = wait_for_rpc(&request, port_count).await?;
    let handle = EngineHandle::spawn(request.group_id, port_count, Box::new(engine));

    tracing::info!(
        group_id = %request.group_id,
        port_count,
        rpc_port = request.instance.rpc_port,
        "connected to a TRex instance"
    );
    Ok(Launched { handle, controls: None })
}

/// Renders the platform configuration and writes it to disk.
fn write_platform_config(request: &LaunchRequest) -> Result<PathBuf, EngineError> {
    let document = trex_config::build(&request.pci_addrs, request.numa_node, &request.instance);
    let yaml = trex_config::to_yaml(&document)
        .map_err(|e| EngineError::Rejected(format!("rendering the TRex configuration: {e}")))?;

    let dir = PathBuf::from(CONFIG_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| EngineError::Unavailable(format!("creating {}: {e}", dir.display())))?;

    let path = dir.join(format!("trex-{}.yaml", request.group_id));
    std::fs::write(&path, yaml)
        .map_err(|e| EngineError::Unavailable(format!("writing {}: {e}", path.display())))?;

    tracing::debug!(path = %path.display(), "wrote the TRex platform configuration");
    Ok(path)
}

/// Polls the RPC socket until TRex answers or the deadline passes.
///
/// A freshly spawned TRex refuses connections for tens of seconds while it binds
/// NICs. Retrying with a deadline turns that into a wait rather than an
/// immediate, confusing failure.
async fn wait_for_rpc(request: &LaunchRequest, port_count: u8) -> Result<TrexEngine, EngineError> {
    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    let mut last: Option<EngineError> = None;

    while tokio::time::Instant::now() < deadline {
        let engine =
            TrexEngine::connect(request.mode, port_count, "127.0.0.1", request.instance.rpc_port);

        // The API handshake has to succeed before anything else; without it TRex
        // rejects every later call, and that failure would look like a
        // permissions problem rather than a version negotiation one.
        match engine.handshake().await {
            Ok(_) => return Ok(engine),
            Err(err) => {
                tracing::debug!(%err, "TRex is not answering yet");
                last = Some(err);
                tokio::time::sleep(HANDSHAKE_RETRY).await;
            }
        }
    }

    Err(EngineError::Unavailable(format!(
        "TRex did not answer on port {} within {STARTUP_TIMEOUT:?}{}",
        request.instance.rpc_port,
        last.map(|e| format!(" (last error: {e})")).unwrap_or_default()
    )))
}

/// Keeps a TRex process alive, restarting it within the supervisor's budget.
///
/// Owning the `Child` is what keeps the process running: `Command` is configured
/// to kill on drop, so a dropped handle takes TRex down with it.
async fn watchdog(group_id: Id, mut supervisor: Supervisor, mut child: tokio::process::Child) {
    use super::trex::supervisor::RestartDecision;

    loop {
        let started = std::time::Instant::now();
        let status = child.wait().await;
        let uptime = started.elapsed();

        match status {
            Ok(status) => {
                tracing::warn!(%group_id, ?status, ?uptime, "TRex exited")
            }
            Err(err) => {
                tracing::error!(%group_id, %err, "could not wait on the TRex process");
                return;
            }
        }

        match supervisor.note_exit(uptime) {
            RestartDecision::RestartAfter(wait) => {
                tokio::time::sleep(wait).await;
                tracing::info!(
                    %group_id,
                    attempt = supervisor.restart_count(),
                    "relaunching TRex"
                );
                match supervisor.spawn() {
                    Ok(replacement) => child = replacement,
                    Err(err) => {
                        tracing::error!(%group_id, %err, "could not relaunch TRex");
                        return;
                    }
                }
            }
            RestartDecision::GiveUp => {
                tracing::error!(
                    %group_id,
                    "TRex will not stay up; this port group needs an operator"
                );
                return;
            }
        }
    }
}
