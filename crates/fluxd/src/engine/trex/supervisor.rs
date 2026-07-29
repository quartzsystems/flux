//! Spawning and supervising a TRex process.
//!
//! TRex is a long-running process that owns NICs exclusively. If it dies while a
//! test is running the ports are stranded until something restarts it, so this
//! module watches it and brings it back — with a backoff, because a process that
//! cannot start will not start any better on the twentieth immediate attempt.
//!
//! Restarts are bounded. Past the budget the port group goes to `error` and
//! stays there: an appliance quietly restarting a crashing engine forever looks
//! healthy from the outside while getting nothing done.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use tokio::process::{Child, Command};

/// How long to wait after the first crash before restarting.
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);

/// Longest wait between restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How many consecutive failures before giving up on the instance.
const MAX_RESTARTS: u32 = 5;

/// How long a process must stay up for its start to count as successful.
///
/// TRex takes a few seconds to bind its ports and open its RPC socket. Anything
/// shorter than this is a startup failure, not a crash, and must not reset the
/// backoff — otherwise a config error produces an infinite fast restart loop.
const HEALTHY_AFTER: Duration = Duration::from_secs(15);

/// How to launch one instance.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Path to the TRex binary, usually `/opt/trex/current/t-rex-64`.
    pub binary: PathBuf,
    /// Working directory. TRex expects to run from its own installation.
    pub working_dir: PathBuf,
    /// Path to the generated platform configuration.
    pub config_path: PathBuf,
    /// ZMQ port for synchronous RPC.
    pub rpc_port: u16,
    /// ZMQ port for the asynchronous statistics publisher.
    pub async_port: u16,
    /// Whether to start in stateful mode rather than stateless.
    pub astf: bool,
}

impl LaunchSpec {
    /// The command line this spec produces.
    ///
    /// Split out so it can be asserted on without launching anything.
    ///
    /// TODO(trex-verify): flag spellings. `-i` for interactive (stateless
    /// server) mode, `--astf` for stateful, `--cfg` for the platform file, and
    /// `--iom 0` to suppress the console output that has nowhere to go under
    /// systemd. `--no-scapy-server` is worth adding if the build ships one.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--cfg".to_string(),
            self.config_path.display().to_string(),
            "--iom".to_string(),
            "0".to_string(),
        ];

        if self.astf {
            args.push("--astf".to_string());
        } else {
            // Interactive mode is what opens the RPC server; without it TRex
            // runs a batch test and exits.
            args.push("-i".to_string());
        }

        args.push("--rpc-port".to_string());
        args.push(self.rpc_port.to_string());
        args.push("--pub-port".to_string());
        args.push(self.async_port.to_string());

        args
    }
}

/// Supervises one TRex process.
pub struct Supervisor {
    spec: LaunchSpec,
    restarts: u32,
    backoff: Duration,
}

impl Supervisor {
    /// Builds a supervisor for `spec`.
    pub fn new(spec: LaunchSpec) -> Self {
        Self { spec, restarts: 0, backoff: INITIAL_BACKOFF }
    }

    /// Launches the process.
    #[tracing::instrument(skip(self), fields(binary = %self.spec.binary.display()))]
    pub fn spawn(&self) -> anyhow::Result<Child> {
        let args = self.spec.args();
        tracing::info!(args = ?args, "launching TRex");

        Command::new(&self.spec.binary)
            .args(&args)
            .current_dir(&self.spec.working_dir)
            // TRex writes a great deal to stdout; the daemon's own logging is
            // the record we keep, so this goes to the void rather than filling a
            // pipe nobody drains — a full pipe would block the process.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {}", self.spec.binary.display()))
    }

    /// Records that an instance exited, and says whether to try again.
    ///
    /// `uptime` decides whether this counts as a crash or a failure to start.
    /// A process that ran for a while and then died gets a fresh budget; one
    /// that never came up does not, which is what stops a bad configuration from
    /// spinning forever.
    pub fn note_exit(&mut self, uptime: Duration) -> RestartDecision {
        if uptime >= HEALTHY_AFTER {
            tracing::warn!(?uptime, "TRex exited after running normally; restarting");
            self.restarts = 0;
            self.backoff = INITIAL_BACKOFF;
            return RestartDecision::RestartAfter(INITIAL_BACKOFF);
        }

        self.restarts += 1;
        if self.restarts > MAX_RESTARTS {
            tracing::error!(
                restarts = self.restarts,
                "TRex failed to stay up; giving up on this instance"
            );
            return RestartDecision::GiveUp;
        }

        let wait = self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);

        tracing::warn!(
            ?uptime,
            restarts = self.restarts,
            ?wait,
            "TRex exited during startup; backing off"
        );
        RestartDecision::RestartAfter(wait)
    }

    /// How many consecutive failures have been seen.
    pub fn restart_count(&self) -> u32 {
        self.restarts
    }
}

/// What to do after an instance exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Wait this long, then launch again.
    RestartAfter(Duration),
    /// Stop trying; the port group is broken until an operator intervenes.
    GiveUp,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stateless launch spec.
    fn spec() -> LaunchSpec {
        LaunchSpec {
            binary: "/opt/trex/current/t-rex-64".into(),
            working_dir: "/opt/trex/current".into(),
            config_path: "/var/lib/flux/trex-group-a.yaml".into(),
            rpc_port: 4501,
            async_port: 4500,
            astf: false,
        }
    }

    #[test]
    fn a_stateless_launch_asks_for_the_interactive_rpc_server() {
        // Without -i, TRex runs a batch test and exits, and nothing ever
        // connects to it.
        let args = spec().args();
        assert!(args.contains(&"-i".to_string()), "got {args:?}");
        assert!(!args.contains(&"--astf".to_string()));
    }

    #[test]
    fn a_stateful_launch_selects_astf_mode() {
        let args = LaunchSpec { astf: true, ..spec() }.args();
        assert!(args.contains(&"--astf".to_string()));
        assert!(!args.contains(&"-i".to_string()));
    }

    #[test]
    fn the_command_line_names_the_config_and_both_zmq_ports() {
        let args = spec().args();
        let joined = args.join(" ");

        assert!(joined.contains("--cfg /var/lib/flux/trex-group-a.yaml"), "got {joined}");
        assert!(joined.contains("--rpc-port 4501"), "got {joined}");
        assert!(joined.contains("--pub-port 4500"), "got {joined}");
    }

    #[test]
    fn a_crash_after_running_normally_restarts_promptly_with_a_fresh_budget() {
        let mut supervisor = Supervisor::new(spec());

        // Burn most of the budget on startup failures.
        for _ in 0..3 {
            supervisor.note_exit(Duration::from_secs(1));
        }
        assert_eq!(supervisor.restart_count(), 3);

        // A process that ran normally and then died is a different situation.
        let decision = supervisor.note_exit(Duration::from_secs(3600));
        assert_eq!(decision, RestartDecision::RestartAfter(INITIAL_BACKOFF));
        assert_eq!(supervisor.restart_count(), 0, "a healthy run resets the budget");
    }

    #[test]
    fn repeated_startup_failures_back_off_exponentially() {
        let mut supervisor = Supervisor::new(spec());

        let first = supervisor.note_exit(Duration::from_secs(1));
        let second = supervisor.note_exit(Duration::from_secs(1));
        let third = supervisor.note_exit(Duration::from_secs(1));

        assert_eq!(first, RestartDecision::RestartAfter(Duration::from_secs(2)));
        assert_eq!(second, RestartDecision::RestartAfter(Duration::from_secs(4)));
        assert_eq!(third, RestartDecision::RestartAfter(Duration::from_secs(8)));
    }

    #[test]
    fn the_backoff_is_capped() {
        let mut supervisor = Supervisor::new(spec());
        let mut last = Duration::ZERO;

        for _ in 0..MAX_RESTARTS {
            if let RestartDecision::RestartAfter(wait) =
                supervisor.note_exit(Duration::from_secs(1))
            {
                last = wait;
            }
        }
        assert!(last <= MAX_BACKOFF);
    }

    #[test]
    fn an_engine_that_never_starts_is_eventually_given_up_on() {
        // An appliance quietly restarting a broken engine forever looks healthy
        // from the outside while getting nothing done.
        let mut supervisor = Supervisor::new(spec());

        for _ in 0..MAX_RESTARTS {
            assert!(matches!(
                supervisor.note_exit(Duration::from_millis(500)),
                RestartDecision::RestartAfter(_)
            ));
        }

        assert_eq!(supervisor.note_exit(Duration::from_millis(500)), RestartDecision::GiveUp);
    }

    #[test]
    fn a_process_that_almost_stayed_up_still_counts_as_a_startup_failure() {
        // TRex needs about fifteen seconds to bind its ports; anything shorter
        // never got as far as being usable.
        let mut supervisor = Supervisor::new(spec());
        supervisor.note_exit(HEALTHY_AFTER - Duration::from_secs(1));
        assert_eq!(supervisor.restart_count(), 1);
    }
}
