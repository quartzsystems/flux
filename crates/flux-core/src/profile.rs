//! Stateful (L4-7) load profiles.
//!
//! Where a flow describes frames, a profile describes *conversations*: a pool of
//! clients opening connections to a pool of servers at some rate, exchanging an
//! application payload, and closing. The engine runs both sides, so this is the
//! one place in Flux where the appliance is emulating a server as well as a
//! client.
//!
//! The document here is what an operator configures. `AstfProfile` — further
//! down — is the engine-agnostic form the translator produces, standing in the
//! same relationship as `StreamSpec` does to `FlowConfig`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::{Validate, Validation};
use crate::types::Id;

/// Largest connection rate a profile may ask for.
///
/// Two million connections per second is beyond what any single appliance in
/// this class sustains; a larger number is a typo, and catching it here beats
/// discovering it as an engine that refuses to start.
pub const MAX_TARGET_CPS: f64 = 2_000_000.0;

/// Largest concurrency a profile may ask for.
///
/// Each connection costs memory in the engine, and ten million is already past
/// what a sensibly provisioned instance holds.
pub const MAX_CONCURRENT: u64 = 10_000_000;

// ---------------------------------------------------------------------------
// The profile document
// ---------------------------------------------------------------------------

/// A complete L4-7 load definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoadProfileConfig {
    /// Port the emulated clients live behind.
    #[schema(value_type = String, format = Uuid)]
    pub client_port: Id,
    /// Port the emulated servers live behind.
    #[schema(value_type = String, format = Uuid)]
    pub server_port: Id,
    /// Addresses the clients draw from.
    pub client_pool: IpPool,
    /// Addresses the servers listen on.
    pub server_pool: IpPool,
    /// What the two sides say to each other.
    pub app: AppSpec,
    /// Connections per second to establish once warmed up.
    pub target_cps: f64,
    /// Ceiling on simultaneously open connections.
    pub max_concurrent: u64,
    /// How the rate gets to its target.
    #[serde(default)]
    pub ramp: Ramp,
    /// Stop after this many seconds. `None` runs until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

impl Validate for LoadProfileConfig {
    fn validate_into(&self, v: &mut Validation) {
        v.scope("clientPool", |v| self.client_pool.validate_into(v));
        v.scope("serverPool", |v| self.server_pool.validate_into(v));
        v.scope("app", |v| self.app.validate_into(v));
        v.scope("ramp", |v| self.ramp.validate_into(v));

        v.require(
            self.client_port != self.server_port,
            "serverPort",
            "the client and server sides must use different ports",
        );

        v.require(self.target_cps > 0.0, "targetCps", "must be greater than zero");
        v.require(
            self.target_cps <= MAX_TARGET_CPS,
            "targetCps",
            format!("must be at most {MAX_TARGET_CPS:.0}"),
        );
        v.require(self.target_cps.is_finite(), "targetCps", "must be a finite number");

        v.require(self.max_concurrent >= 1, "maxConcurrent", "must be at least 1");
        v.require(
            self.max_concurrent <= MAX_CONCURRENT,
            "maxConcurrent",
            format!("must be at most {MAX_CONCURRENT}"),
        );

        // Concurrency is the product of arrival rate and how long a connection
        // lives. A ceiling below one second of arrivals throttles the profile to
        // something well under its target rate, which is almost never intended.
        if self.target_cps > 0.0 && (self.max_concurrent as f64) < self.target_cps {
            v.error(
                "maxConcurrent",
                format!(
                    "{} concurrent connections cannot sustain {:.0} per second; \
                     raise the ceiling or lower the rate",
                    self.max_concurrent, self.target_cps
                ),
            );
        }

        // Every client needs a source address and port; the pool has to be big
        // enough to hold the concurrency the profile asks for, or the engine
        // reuses tuples that are still open.
        let client_capacity = self.client_pool.capacity();
        if client_capacity > 0 && client_capacity < self.max_concurrent {
            v.error(
                "clientPool",
                format!(
                    "this pool yields {client_capacity} address/port pairs, \
                     fewer than the {} concurrent connections requested",
                    self.max_concurrent
                ),
            );
        }

        if let Some(seconds) = self.duration_secs {
            v.require(seconds > 0.0, "durationSecs", "must be greater than zero");
            v.require(seconds <= 86_400.0, "durationSecs", "must be at most 24 hours");
        }
    }
}

// ---------------------------------------------------------------------------
// Address pools
// ---------------------------------------------------------------------------

/// A block of addresses and the port range drawn from each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IpPool {
    /// Address block in CIDR notation, e.g. `10.0.0.0/16`.
    pub cidr: String,
    /// Lowest port used from each address.
    #[serde(default = "default_port_min")]
    pub port_min: u16,
    /// Highest port used from each address.
    #[serde(default = "default_port_max")]
    pub port_max: u16,
}

/// The start of the ephemeral range IANA suggests.
fn default_port_min() -> u16 {
    1024
}

/// The top of the port space.
fn default_port_max() -> u16 {
    65_535
}

impl Default for IpPool {
    fn default() -> Self {
        Self { cidr: "10.0.0.0/16".into(), port_min: default_port_min(), port_max: default_port_max() }
    }
}

impl IpPool {
    /// Splits the CIDR into its base address and prefix length.
    pub fn parse(&self) -> Option<(std::net::Ipv4Addr, u8)> {
        let (address, prefix) = self.cidr.split_once('/')?;
        let address: std::net::Ipv4Addr = address.trim().parse().ok()?;
        let prefix: u8 = prefix.trim().parse().ok()?;

        if prefix > 32 {
            return None;
        }
        Some((address, prefix))
    }

    /// How many addresses the block holds.
    ///
    /// The network and broadcast addresses are excluded for any prefix short
    /// enough to have them, because a host emulated at either one is not a host
    /// a device under test will answer.
    pub fn address_count(&self) -> u64 {
        let Some((_, prefix)) = self.parse() else { return 0 };
        let total = 1u64 << (32 - u32::from(prefix));
        if total > 2 {
            total - 2
        } else {
            total
        }
    }

    /// How many distinct address and port pairs the pool yields.
    ///
    /// This is the real limit on concurrency: a connection occupies one pair
    /// until it closes.
    pub fn capacity(&self) -> u64 {
        if self.port_max < self.port_min {
            return 0;
        }
        let ports = u64::from(self.port_max - self.port_min) + 1;
        self.address_count().saturating_mul(ports)
    }
}

impl Validate for IpPool {
    fn validate_into(&self, v: &mut Validation) {
        match self.parse() {
            None => v.error("cidr", "must be a block like 10.0.0.0/16"),
            Some((_, prefix)) => {
                v.require(
                    prefix <= 30,
                    "cidr",
                    "must be a /30 or shorter; a longer prefix holds no usable hosts",
                );
            }
        }

        v.require(
            self.port_min <= self.port_max,
            "portMax",
            "must be at least the minimum port",
        );
        v.require(self.port_min >= 1, "portMin", "must be at least 1");
    }
}

// ---------------------------------------------------------------------------
// Application behaviour
// ---------------------------------------------------------------------------

/// What the emulated client and server exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppSpec {
    /// A single HTTP GET and its response.
    HttpGet {
        /// Path requested.
        #[serde(default = "default_path")]
        path: String,
        /// Bytes the server returns in the body.
        #[serde(default = "default_response_bytes")]
        response_bytes: u32,
    },
    /// An opaque request and response of fixed sizes.
    ///
    /// For measuring the transport rather than any particular protocol.
    Raw {
        /// Bytes the client sends.
        request_bytes: u32,
        /// Bytes the server sends back.
        response_bytes: u32,
    },
    /// Replay of a captured conversation.
    Pcap {
        /// Name of a capture stored on the appliance.
        pcap_ref: String,
    },
}

/// The conventional root.
fn default_path() -> String {
    "/".into()
}

/// A response big enough to be interesting without dominating the link.
fn default_response_bytes() -> u32 {
    32_768
}

impl Default for AppSpec {
    fn default() -> Self {
        AppSpec::HttpGet { path: default_path(), response_bytes: default_response_bytes() }
    }
}

impl AppSpec {
    /// Roughly how many bytes one completed conversation moves.
    ///
    /// Used to show an operator the throughput their connection rate implies,
    /// which is the figure that usually reveals a profile is asking for more
    /// than the link can carry.
    pub fn bytes_per_connection(&self) -> u64 {
        match self {
            // The request line and headers a minimal GET carries, plus the body.
            AppSpec::HttpGet { path, response_bytes } => {
                let request = 80 + path.len() as u64;
                let response_headers = 120;
                request + response_headers + u64::from(*response_bytes)
            }
            AppSpec::Raw { request_bytes, response_bytes } => {
                u64::from(*request_bytes) + u64::from(*response_bytes)
            }
            // A capture's size is not known until it is loaded.
            AppSpec::Pcap { .. } => 0,
        }
    }
}

impl Validate for AppSpec {
    fn validate_into(&self, v: &mut Validation) {
        match self {
            AppSpec::HttpGet { path, response_bytes } => {
                v.require(path.starts_with('/'), "path", "must begin with a slash");
                v.require(path.len() <= 1024, "path", "must be at most 1024 characters");
                v.require(
                    *response_bytes <= 64 * 1024 * 1024,
                    "responseBytes",
                    "must be at most 64 MB",
                );
            }
            AppSpec::Raw { request_bytes, response_bytes } => {
                v.require(
                    *request_bytes > 0 || *response_bytes > 0,
                    "requestBytes",
                    "a conversation that moves no bytes measures nothing",
                );
                v.require(
                    *request_bytes <= 64 * 1024 * 1024,
                    "requestBytes",
                    "must be at most 64 MB",
                );
                v.require(
                    *response_bytes <= 64 * 1024 * 1024,
                    "responseBytes",
                    "must be at most 64 MB",
                );
            }
            AppSpec::Pcap { pcap_ref } => {
                v.require(!pcap_ref.trim().is_empty(), "pcapRef", "must name a stored capture");
                // The reference becomes a filename on the appliance, so it may
                // not contain a path.
                v.require(
                    !pcap_ref.contains('/') && !pcap_ref.contains('\\') && !pcap_ref.contains(".."),
                    "pcapRef",
                    "must be a plain name, not a path",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ramp
// ---------------------------------------------------------------------------

/// How the connection rate reaches its target.
///
/// Starting a hundred thousand connections per second from a standing start
/// measures the device's response to a step, not its steady-state capacity.
/// A warm-up is the difference between the two.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ramp {
    /// Seconds spent climbing linearly from zero to the target rate.
    pub warmup_secs: f64,
    /// Seconds to ignore after the warm-up before results count.
    ///
    /// Connection tables and caches settle in this window; measuring through it
    /// reports a transient.
    pub settle_secs: f64,
}

impl Default for Ramp {
    fn default() -> Self {
        Self { warmup_secs: 10.0, settle_secs: 5.0 }
    }
}

impl Ramp {
    /// The fraction of the target rate in effect `elapsed` seconds in.
    pub fn factor_at(&self, elapsed: f64) -> f64 {
        if self.warmup_secs <= 0.0 {
            return 1.0;
        }
        (elapsed / self.warmup_secs).clamp(0.0, 1.0)
    }

    /// When measurement should start.
    pub fn measurement_starts_at(&self) -> f64 {
        self.warmup_secs + self.settle_secs
    }
}

impl Validate for Ramp {
    fn validate_into(&self, v: &mut Validation) {
        v.require(self.warmup_secs >= 0.0, "warmupSecs", "must not be negative");
        v.require(self.warmup_secs <= 3600.0, "warmupSecs", "must be at most one hour");
        v.require(self.settle_secs >= 0.0, "settleSecs", "must not be negative");
        v.require(self.settle_secs <= 3600.0, "settleSecs", "must be at most one hour");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile that validates.
    fn profile() -> LoadProfileConfig {
        LoadProfileConfig {
            client_port: Id::from_u128(1),
            server_port: Id::from_u128(2),
            client_pool: IpPool::default(),
            server_pool: IpPool { cidr: "10.1.0.0/24".into(), ..Default::default() },
            app: AppSpec::default(),
            target_cps: 10_000.0,
            max_concurrent: 100_000,
            ramp: Ramp::default(),
            duration_secs: None,
        }
    }

    #[test]
    fn a_reasonable_profile_validates() {
        assert!(profile().validate().is_ok(), "{:?}", profile().validate());
    }

    #[test]
    fn the_two_sides_must_use_different_ports() {
        // Both sides on one port is a loopback the engine cannot arrange.
        let mut p = profile();
        p.server_port = p.client_port;

        let errors = p.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "serverPort"));
    }

    #[test]
    fn a_concurrency_ceiling_below_the_arrival_rate_is_rejected() {
        // Concurrency is arrival rate times connection lifetime. A ceiling below
        // one second of arrivals silently throttles the profile.
        let mut p = profile();
        p.target_cps = 50_000.0;
        p.max_concurrent = 1_000;

        let errors = p.validate().unwrap_err();
        let message = &errors.iter().find(|e| e.path == "maxConcurrent").unwrap().msg;
        assert!(message.contains("cannot sustain"), "{message}");
    }

    #[test]
    fn a_client_pool_too_small_for_the_concurrency_is_rejected() {
        // Without enough address/port pairs the engine reuses tuples that are
        // still open, and the device under test sees something nobody asked for.
        let mut p = profile();
        p.client_pool = IpPool { cidr: "10.0.0.0/30".into(), port_min: 1024, port_max: 1033 };
        p.max_concurrent = 100_000;
        p.target_cps = 1_000.0;

        let errors = p.validate().unwrap_err();
        let message = &errors.iter().find(|e| e.path == "clientPool").unwrap().msg;
        assert!(message.contains("address/port pairs"), "{message}");
    }

    // -----------------------------------------------------------------------
    // Pools
    // -----------------------------------------------------------------------

    #[test]
    fn a_cidr_splits_into_its_address_and_prefix() {
        let pool = IpPool { cidr: "10.0.0.0/16".into(), ..Default::default() };
        assert_eq!(pool.parse(), Some(("10.0.0.0".parse().unwrap(), 16)));
    }

    #[test]
    fn address_counts_exclude_the_network_and_broadcast() {
        // A host emulated at either is not one a device under test will answer.
        assert_eq!(IpPool { cidr: "10.0.0.0/24".into(), ..Default::default() }.address_count(), 254);
        assert_eq!(
            IpPool { cidr: "10.0.0.0/16".into(), ..Default::default() }.address_count(),
            65_534
        );
        assert_eq!(IpPool { cidr: "10.0.0.0/30".into(), ..Default::default() }.address_count(), 2);
    }

    #[test]
    fn a_single_address_block_still_yields_that_address() {
        // A /32 has no network or broadcast to exclude.
        assert_eq!(IpPool { cidr: "10.0.0.1/32".into(), ..Default::default() }.address_count(), 1);
    }

    #[test]
    fn capacity_is_addresses_times_ports() {
        let pool = IpPool { cidr: "10.0.0.0/24".into(), port_min: 1024, port_max: 2023 };
        assert_eq!(pool.capacity(), 254 * 1000);
    }

    #[test]
    fn an_inverted_port_range_yields_no_capacity() {
        let pool = IpPool { cidr: "10.0.0.0/24".into(), port_min: 5000, port_max: 1000 };
        assert_eq!(pool.capacity(), 0);
    }

    #[test]
    fn a_malformed_cidr_is_rejected_rather_than_assumed() {
        for bad in ["10.0.0.0", "10.0.0.0/33", "not-an-address/16", "10.0.0.0/", "/16", ""] {
            let pool = IpPool { cidr: bad.into(), ..Default::default() };
            assert_eq!(pool.parse(), None, "{bad} should not parse");
            assert!(pool.validate().is_err(), "{bad} should not validate");
        }
    }

    #[test]
    fn a_prefix_with_no_usable_hosts_is_rejected() {
        let pool = IpPool { cidr: "10.0.0.0/31".into(), ..Default::default() };
        let errors = pool.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "cidr"));
    }

    // -----------------------------------------------------------------------
    // Application
    // -----------------------------------------------------------------------

    #[test]
    fn an_http_conversation_counts_its_headers_as_well_as_its_body() {
        // The figure is for showing an operator the throughput their rate
        // implies, so ignoring headers would understate a small-response profile
        // considerably.
        let app = AppSpec::HttpGet { path: "/".into(), response_bytes: 0 };
        assert!(app.bytes_per_connection() > 100, "headers should count");

        let big = AppSpec::HttpGet { path: "/".into(), response_bytes: 32_768 };
        assert!(big.bytes_per_connection() > 32_768);
    }

    #[test]
    fn a_raw_conversation_is_the_sum_of_both_directions() {
        let app = AppSpec::Raw { request_bytes: 100, response_bytes: 900 };
        assert_eq!(app.bytes_per_connection(), 1000);
    }

    #[test]
    fn a_capture_reports_no_size_until_it_is_loaded() {
        assert_eq!(AppSpec::Pcap { pcap_ref: "web.pcap".into() }.bytes_per_connection(), 0);
    }

    #[test]
    fn an_http_path_must_be_a_path() {
        let app = AppSpec::HttpGet { path: "index.html".into(), response_bytes: 100 };
        let errors = app.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "path"));
    }

    #[test]
    fn a_conversation_that_moves_nothing_is_rejected() {
        let app = AppSpec::Raw { request_bytes: 0, response_bytes: 0 };
        assert!(app.validate().is_err());
    }

    #[test]
    fn a_capture_reference_may_not_be_a_path() {
        // The reference becomes a filename on the appliance.
        for bad in ["../../etc/passwd", "dir/file.pcap", "..\\windows", "  "] {
            let app = AppSpec::Pcap { pcap_ref: bad.into() };
            assert!(app.validate().is_err(), "{bad} should be rejected");
        }
        assert!(AppSpec::Pcap { pcap_ref: "web-browsing.pcap".into() }.validate().is_ok());
    }

    #[test]
    fn the_app_spec_round_trips_through_its_tagged_wire_form() {
        let app = AppSpec::Raw { request_bytes: 128, response_bytes: 4096 };
        let json = serde_json::to_string(&app).unwrap();

        assert!(json.contains("\"type\":\"raw\""), "got {json}");
        assert_eq!(serde_json::from_str::<AppSpec>(&json).unwrap(), app);
    }

    // -----------------------------------------------------------------------
    // Ramp
    // -----------------------------------------------------------------------

    #[test]
    fn the_ramp_climbs_linearly_and_then_holds() {
        let ramp = Ramp { warmup_secs: 10.0, settle_secs: 5.0 };

        assert_eq!(ramp.factor_at(0.0), 0.0);
        assert_eq!(ramp.factor_at(5.0), 0.5);
        assert_eq!(ramp.factor_at(10.0), 1.0);
        assert_eq!(ramp.factor_at(60.0), 1.0, "it holds at the target, never above");
    }

    #[test]
    fn no_warmup_means_full_rate_immediately() {
        let ramp = Ramp { warmup_secs: 0.0, settle_secs: 0.0 };
        assert_eq!(ramp.factor_at(0.0), 1.0);
    }

    #[test]
    fn measurement_starts_after_the_warmup_and_the_settle() {
        // Measuring through the settle window reports a transient, not capacity.
        let ramp = Ramp { warmup_secs: 10.0, settle_secs: 5.0 };
        assert_eq!(ramp.measurement_starts_at(), 15.0);
    }

    #[test]
    fn a_negative_ramp_is_rejected() {
        let ramp = Ramp { warmup_secs: -1.0, settle_secs: 0.0 };
        assert!(ramp.validate().is_err());
    }

    #[test]
    fn a_profile_round_trips_through_json_unchanged() {
        // Stored as JSONB and restored from a run snapshot.
        let p = profile();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<LoadProfileConfig>(&json).unwrap(), p);
    }
}
