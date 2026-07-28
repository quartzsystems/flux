//! Daemon configuration, read from the environment.
//!
//! An appliance is configured by its systemd unit and an `EnvironmentFile`, not
//! by command-line flags, so the environment is the only source. Every setting
//! has a default that makes sense on a real appliance; the development defaults
//! are selected by `FLUX_ENGINE=mock` / `FLUX_PORTD=mock` rather than by a
//! separate profile, so there is exactly one code path.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;

/// Which packet engine implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineBackend {
    /// Simulated engine. No DPDK, no hardware, full UI.
    Mock,
    /// Real TRex processes over ZMQ JSON-RPC.
    Trex,
}

/// Which port controller implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortdBackend {
    /// Simulated four-port chassis, in process.
    Mock,
    /// The privileged `flux-portd` helper over its unix socket.
    Unix,
}

/// Everything the daemon needs to start.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds.
    pub bind: SocketAddr,
    /// Postgres connection string.
    pub database_url: String,
    /// Upper bound on pooled database connections.
    pub database_max_connections: u32,
    /// Engine implementation.
    pub engine: EngineBackend,
    /// Port controller implementation.
    pub portd: PortdBackend,
    /// Control socket for the privileged helper.
    pub portd_socket: PathBuf,
    /// Directory holding the exported Next.js build, served at `/`.
    pub web_root: PathBuf,
    /// How long a session stays valid after login.
    pub session_ttl: Duration,
    /// Whether to set `Secure` on the session cookie.
    ///
    /// Defaults to off so that plain-HTTP development works; the installer turns
    /// it on when TLS is configured. A `Secure` cookie over HTTP is silently
    /// dropped by the browser, which looks exactly like a broken login.
    pub cookie_secure: bool,
    /// Password for the bootstrap admin account, when one must be created.
    pub bootstrap_admin_password: Option<String>,
    /// Base URL of the local VictoriaMetrics instance.
    ///
    /// Parsed and validated from the start so a bad value in the systemd unit is
    /// caught on the boot that introduced it, rather than on the first test run
    /// after the collector lands in milestone 2.
    #[allow(dead_code, reason = "read by the collector, which arrives in milestone 2")]
    pub victoria_metrics_url: String,
}

impl Config {
    /// Reads configuration from the environment.
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env_or("FLUX_BIND", "0.0.0.0:8080")
            .parse::<SocketAddr>()
            .context("FLUX_BIND must be an address like 0.0.0.0:8080")?;

        let database_url = std::env::var("DATABASE_URL").context(
            "DATABASE_URL is required, e.g. postgres://flux:flux@localhost/flux",
        )?;

        let database_max_connections = env_or("FLUX_DB_MAX_CONNECTIONS", "16")
            .parse()
            .context("FLUX_DB_MAX_CONNECTIONS must be a positive integer")?;

        let engine = match env_or("FLUX_ENGINE", "trex").as_str() {
            "mock" => EngineBackend::Mock,
            "trex" => EngineBackend::Trex,
            other => anyhow::bail!("FLUX_ENGINE must be `mock` or `trex`, got {other:?}"),
        };

        // The port backend follows the engine unless it is set explicitly, since
        // wanting a mock engine but real NIC binding is not a combination anyone
        // asks for by accident.
        let portd_default = match engine {
            EngineBackend::Mock => "mock",
            EngineBackend::Trex => "unix",
        };
        let portd = match env_or("FLUX_PORTD", portd_default).as_str() {
            "mock" => PortdBackend::Mock,
            "unix" => PortdBackend::Unix,
            other => anyhow::bail!("FLUX_PORTD must be `mock` or `unix`, got {other:?}"),
        };

        let session_ttl_hours: u64 = env_or("FLUX_SESSION_TTL_HOURS", "12")
            .parse()
            .context("FLUX_SESSION_TTL_HOURS must be a positive integer")?;
        anyhow::ensure!(session_ttl_hours > 0, "FLUX_SESSION_TTL_HOURS must be greater than 0");

        Ok(Self {
            bind,
            database_url,
            database_max_connections,
            engine,
            portd,
            portd_socket: env_or("FLUX_PORTD_SOCKET", "/run/flux/portd.sock").into(),
            web_root: env_or("FLUX_WEB_ROOT", "/usr/share/flux/web").into(),
            session_ttl: Duration::from_secs(session_ttl_hours * 3600),
            cookie_secure: env_flag("FLUX_COOKIE_SECURE", false),
            bootstrap_admin_password: std::env::var("FLUX_BOOTSTRAP_ADMIN_PASSWORD").ok(),
            victoria_metrics_url: env_or("FLUX_VM_URL", "http://127.0.0.1:8428"),
        })
    }

    /// True when neither the engine nor the port layer touches real hardware.
    pub fn is_fully_mocked(&self) -> bool {
        self.engine == EngineBackend::Mock && self.portd == PortdBackend::Mock
    }
}

/// Reads a variable, falling back to a default.
fn env_or(key: &str, default: &str) -> String {
    value_or(std::env::var(key).ok(), default)
}

/// Reads a boolean-ish variable.
fn env_flag(key: &str, default: bool) -> bool {
    flag_or(std::env::var(key).ok(), default)
}

/// Resolves a raw variable against a default.
///
/// Empty and whitespace-only values count as unset: a systemd `EnvironmentFile`
/// routinely contains `KEY=` to mean "not configured", and taking that literally
/// would bind the server to an empty string instead of the default.
///
/// Split out from [`env_or`] so the rule can be tested without mutating the
/// process environment, which no test can do safely alongside others.
fn value_or(raw: Option<String>, default: &str) -> String {
    match raw {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}

/// Resolves a raw variable as a boolean.
fn flag_or(raw: Option<String>, default: bool) -> bool {
    match raw {
        Some(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_variable_is_treated_as_unset() {
        assert_eq!(value_or(Some(String::new()), "fallback"), "fallback");
        assert_eq!(value_or(Some("   ".into()), "fallback"), "fallback");
        assert_eq!(value_or(None, "fallback"), "fallback");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_off_values() {
        // Shell heredocs and editors leave trailing spaces in EnvironmentFiles.
        assert_eq!(value_or(Some(" 0.0.0.0:8080 ".into()), "x"), "0.0.0.0:8080");
    }

    #[test]
    fn flags_accept_the_spellings_people_actually_write() {
        for truthy in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(flag_or(Some(truthy.into()), false), "{truthy} should be true");
        }
        for falsy in ["0", "false", "no", "off", "banana", ""] {
            assert!(!flag_or(Some(falsy.into()), true), "{falsy} should be false");
        }
        assert!(flag_or(None, true), "an unset flag keeps its default");
        assert!(!flag_or(None, false), "an unset flag keeps its default");
    }
}
