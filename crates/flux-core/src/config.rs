//! Declarative configuration documents.
//!
//! Every configurable object in Flux is a JSON document stored in a JSONB column
//! with a handful of typed columns alongside it for querying. The Rust struct is
//! the single definition of that document's shape: it is what gets serialised
//! into the column, what the REST API accepts, and what `web/lib/api-types.ts`
//! mirrors.
//!
//! Validation lives with the struct rather than in the HTTP layer so that the
//! same rules apply to a config that arrives over REST, is restored from a
//! backup, or is loaded from a run's `config_snapshot`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Field-level validation
// ---------------------------------------------------------------------------

/// One validation failure, addressed by a JSON path into the submitted document.
///
/// The path is what lets the UI attach the message to the specific input that
/// caused it (`rate.value`, `headers.2.fields.ttl`) rather than showing a
/// form-level banner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FieldError {
    /// Dotted path to the offending field, relative to the request body.
    pub path: String,
    /// Human-readable explanation, written for an operator not a developer.
    pub msg: String,
}

impl FieldError {
    /// Builds a field error.
    pub fn new(path: impl Into<String>, msg: impl Into<String>) -> Self {
        Self { path: path.into(), msg: msg.into() }
    }
}

/// Accumulates field errors so a request reports every problem at once.
///
/// Validating to a list rather than short-circuiting on the first failure is a
/// deliberate choice: an operator filling in a flow editor should see all the
/// bad fields highlighted in one round trip.
#[derive(Debug, Default, Clone)]
pub struct Validation {
    errors: Vec<FieldError>,
    prefix: Vec<String>,
}

impl Validation {
    /// Starts an empty validation pass.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a failure at `path` (relative to the current prefix).
    pub fn error(&mut self, path: &str, msg: impl Into<String>) {
        let full = if self.prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}.{}", self.prefix.join("."), path)
        };
        self.errors.push(FieldError::new(full, msg));
    }

    /// Records a failure only when `cond` holds.
    pub fn require(&mut self, cond: bool, path: &str, msg: impl Into<String>) {
        if !cond {
            self.error(path, msg);
        }
    }

    /// Runs `f` with `segment` pushed onto the path prefix.
    ///
    /// This is how nested documents report `clientPool.cidr` instead of `cidr`.
    pub fn scope<R>(&mut self, segment: impl Into<String>, f: impl FnOnce(&mut Self) -> R) -> R {
        self.prefix.push(segment.into());
        let out = f(self);
        self.prefix.pop();
        out
    }

    /// True when nothing failed.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Consumes the pass, yielding `Ok(())` or the collected failures.
    pub fn finish(self) -> Result<(), Vec<FieldError>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }
}

/// A configuration document that can check itself.
pub trait Validate {
    /// Appends any problems with this document to `v`.
    fn validate_into(&self, v: &mut Validation);

    /// Convenience wrapper returning the collected errors directly.
    fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut v = Validation::new();
        self.validate_into(&mut v);
        v.finish()
    }
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// An emulated host behind a port: the L2/L3 identity Flux sources traffic from.
///
/// `count` turns a single definition into a contiguous block of addresses, which
/// is how a test emulates thousands of hosts without thousands of rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceConfig {
    /// Base hardware address, lowercase colon-separated.
    pub mac: String,
    /// Base IPv4 address in dotted-quad form.
    pub ipv4: String,
    /// Network prefix length, 1-32.
    pub prefix: u8,
    /// Next hop for traffic leaving this subnet.
    pub gateway: String,
    /// Optional 802.1Q VLAN id, 1-4094.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<u16>,
    /// How many consecutive addresses this definition covers. Defaults to 1.
    #[serde(default = "one")]
    pub count: u32,
}

fn one() -> u32 {
    1
}

impl Validate for DeviceConfig {
    fn validate_into(&self, v: &mut Validation) {
        v.require(is_mac(&self.mac), "mac", "must be a MAC address like 00:11:22:33:44:55");
        v.require(is_ipv4(&self.ipv4), "ipv4", "must be a dotted-quad IPv4 address");
        v.require(is_ipv4(&self.gateway), "gateway", "must be a dotted-quad IPv4 address");
        v.require((1..=32).contains(&self.prefix), "prefix", "must be between 1 and 32");
        if let Some(vlan) = self.vlan {
            v.require((1..=4094).contains(&vlan), "vlan", "must be between 1 and 4094");
        }
        v.require(self.count >= 1, "count", "must be at least 1");
        // A /24 holds 256 addresses; refuse a block that cannot fit in its prefix
        // rather than silently wrapping into the neighbouring subnet.
        if self.prefix <= 32 && self.count > 1 {
            let capacity = 1u64 << (32 - self.prefix.min(32));
            v.require(
                u64::from(self.count) <= capacity,
                "count",
                format!("a /{} subnet holds only {} addresses", self.prefix, capacity),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Port groups
// ---------------------------------------------------------------------------

/// Everything needed to launch one engine instance over a set of ports.
///
/// Serialised into `port_groups.trex_cfg`. The engine supervisor renders this
/// into a TRex YAML config at spawn time; keeping it as structured data means the
/// UI can edit it and a run can snapshot it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineInstanceConfig {
    /// ZMQ RPC port for this instance. Each instance needs its own.
    pub rpc_port: u16,
    /// ZMQ async publisher port for this instance.
    pub async_port: u16,
    /// Number of worker threads per interface.
    pub threads_per_port: u8,
    /// CPU cores this instance may use, as a NUMA-aware list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cores: Vec<u16>,
    /// Memory to reserve, in megabytes. `None` lets the engine choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    /// Enable the low-latency measurement path.
    #[serde(default)]
    pub latency_enabled: bool,
}

impl Default for EngineInstanceConfig {
    fn default() -> Self {
        Self {
            rpc_port: 4501,
            async_port: 4500,
            threads_per_port: 1,
            cores: Vec::new(),
            memory_mb: None,
            latency_enabled: true,
        }
    }
}

impl Validate for EngineInstanceConfig {
    fn validate_into(&self, v: &mut Validation) {
        v.require(self.rpc_port >= 1024, "rpcPort", "must be an unprivileged port (>= 1024)");
        v.require(self.async_port >= 1024, "asyncPort", "must be an unprivileged port (>= 1024)");
        v.require(
            self.rpc_port != self.async_port,
            "asyncPort",
            "must differ from the RPC port",
        );
        v.require(
            (1..=16).contains(&self.threads_per_port),
            "threadsPerPort",
            "must be between 1 and 16",
        );
        if let Some(mb) = self.memory_mb {
            v.require(mb >= 128, "memoryMb", "must be at least 128 MB");
        }
    }
}

// ---------------------------------------------------------------------------
// Small format checks
// ---------------------------------------------------------------------------

/// True for `xx:xx:xx:xx:xx:xx` with hex octets.
pub fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// True for a dotted-quad IPv4 literal.
pub fn is_ipv4(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_collects_every_failure_not_just_the_first() {
        let d = DeviceConfig {
            mac: "nope".into(),
            ipv4: "999.1.1.1".into(),
            prefix: 40,
            gateway: "also-nope".into(),
            vlan: Some(9999),
            count: 0,
        };
        let errs = d.validate().unwrap_err();
        let paths: Vec<&str> = errs.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"mac"));
        assert!(paths.contains(&"ipv4"));
        assert!(paths.contains(&"gateway"));
        assert!(paths.contains(&"prefix"));
        assert!(paths.contains(&"vlan"));
        assert!(paths.contains(&"count"));
    }

    #[test]
    fn a_valid_device_passes() {
        let d = DeviceConfig {
            mac: "00:11:22:33:44:55".into(),
            ipv4: "10.0.0.1".into(),
            prefix: 24,
            gateway: "10.0.0.254".into(),
            vlan: Some(100),
            count: 200,
        };
        assert!(d.validate().is_ok(), "{:?}", d.validate());
    }

    #[test]
    fn a_device_block_may_not_overflow_its_subnet() {
        let d = DeviceConfig {
            mac: "00:11:22:33:44:55".into(),
            ipv4: "10.0.0.1".into(),
            prefix: 24,
            gateway: "10.0.0.254".into(),
            vlan: None,
            count: 300, // a /24 holds 256
        };
        let errs = d.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "count"));
    }

    #[test]
    fn scoped_validation_produces_nested_paths() {
        let mut v = Validation::new();
        v.scope("clientPool", |v| v.error("cidr", "bad"));
        let errs = v.finish().unwrap_err();
        assert_eq!(errs[0].path, "clientPool.cidr");
    }

    #[test]
    fn engine_config_rejects_colliding_zmq_ports() {
        let c = EngineInstanceConfig { rpc_port: 4501, async_port: 4501, ..Default::default() };
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "asyncPort"));
    }

    #[test]
    fn mac_and_ipv4_checks_reject_near_misses() {
        assert!(is_mac("00:11:22:33:44:55"));
        assert!(!is_mac("00:11:22:33:44"));
        assert!(!is_mac("00-11-22-33-44-55"));
        assert!(!is_mac("zz:11:22:33:44:55"));
        assert!(is_ipv4("192.168.1.1"));
        assert!(!is_ipv4("192.168.1"));
        assert!(!is_ipv4("256.1.1.1"));
    }
}
