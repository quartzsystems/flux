//! Core domain vocabulary shared across every Flux module.
//!
//! Every type here is wire-facing: it is serialised into REST responses, the
//! WebSocket stream, or a JSONB column. The rule is `rename_all = "camelCase"`
//! for structs and lowercase snake tokens for enum variants, because those enum
//! tokens double as the values in Postgres `CHECK` constraints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Identifier used for every persisted object. Aliased so the intent reads
/// clearly at call sites and so the underlying type could change once.
pub type Id = Uuid;

// ---------------------------------------------------------------------------
// Users & authorisation
// ---------------------------------------------------------------------------

/// Access level attached to a user account.
///
/// Roles are totally ordered by privilege, which is what makes the
/// `has_at_least` check on route guards a single comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read-only: may issue `GET` requests and watch live streams.
    Viewer = 0,
    /// May create/modify test configuration and start or stop runs.
    Operator = 1,
    /// Full control, including users, settings, and port binding.
    Admin = 2,
}

impl Role {
    /// All roles, lowest privilege first.
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];

    /// True when `self` is at least as privileged as `required`.
    pub fn has_at_least(self, required: Role) -> bool {
        self >= required
    }

    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "viewer" => Ok(Role::Viewer),
            "operator" => Ok(Role::Operator),
            "admin" => Ok(Role::Admin),
            other => Err(ParseEnumError::new("role", other)),
        }
    }
}

/// Returned when a database or wire token does not map onto a known enum variant.
#[derive(Debug, Clone, thiserror::Error)]
#[error("`{value}` is not a valid {kind}")]
pub struct ParseEnumError {
    /// Name of the enum that failed to parse, for the error message.
    pub kind: &'static str,
    /// The offending token.
    pub value: String,
}

impl ParseEnumError {
    fn new(kind: &'static str, value: impl Into<String>) -> Self {
        Self { kind, value: value.into() }
    }
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Which driver stack currently owns a NIC.
///
/// `Kernel` means the normal Linux driver is bound and the interface is visible
/// to the OS. `Dpdk` means it has been handed to a userspace poll-mode driver
/// (`vfio-pci`) and is therefore usable by TRex but invisible to the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortMode {
    /// Bound to the in-tree kernel driver.
    Kernel,
    /// Bound to a DPDK-compatible userspace driver.
    Dpdk,
}

impl PortMode {
    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            PortMode::Kernel => "kernel",
            PortMode::Dpdk => "dpdk",
        }
    }
}

impl std::fmt::Display for PortMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PortMode {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "kernel" => Ok(PortMode::Kernel),
            "dpdk" => Ok(PortMode::Dpdk),
            other => Err(ParseEnumError::new("port mode", other)),
        }
    }
}

/// Physical link state as last observed for a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Carrier detected.
    Up,
    /// No carrier.
    Down,
    /// Not yet probed, or the owning driver cannot report it.
    Unknown,
}

impl LinkState {
    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkState::Up => "up",
            LinkState::Down => "down",
            LinkState::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for LinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LinkState {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "up" => Ok(LinkState::Up),
            "down" => Ok(LinkState::Down),
            "unknown" => Ok(LinkState::Unknown),
            other => Err(ParseEnumError::new("link state", other)),
        }
    }
}

/// Packet-engine personality for a port group.
///
/// Stateless (`Stl`) drives raw stream templates and is what RFC 2544 uses.
/// Stateful (`Astf`) drives L4-7 client/server flows and arrives in milestone 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EngineMode {
    /// Stateless streams.
    Stl,
    /// Advanced stateful (L4-7) profiles.
    Astf,
}

impl EngineMode {
    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineMode::Stl => "stl",
            EngineMode::Astf => "astf",
        }
    }
}

impl std::fmt::Display for EngineMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EngineMode {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stl" => Ok(EngineMode::Stl),
            "astf" => Ok(EngineMode::Astf),
            other => Err(ParseEnumError::new("engine mode", other)),
        }
    }
}

/// Lifecycle of the engine instance backing a port group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortGroupState {
    /// No engine process is running.
    Stopped,
    /// Engine process spawned, not yet answering health checks.
    Starting,
    /// Engine is up and its ports are acquired.
    Ready,
    /// Engine failed to start or crashed past its restart budget.
    Error,
}

impl PortGroupState {
    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            PortGroupState::Stopped => "stopped",
            PortGroupState::Starting => "starting",
            PortGroupState::Ready => "ready",
            PortGroupState::Error => "error",
        }
    }
}

impl std::fmt::Display for PortGroupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PortGroupState {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stopped" => Ok(PortGroupState::Stopped),
            "starting" => Ok(PortGroupState::Starting),
            "ready" => Ok(PortGroupState::Ready),
            "error" => Ok(PortGroupState::Error),
            other => Err(ParseEnumError::new("port group state", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// The orchestrator's run state machine.
///
/// Milestone 1 only persists the enum; the transitions themselves land with the
/// orchestrator in milestone 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// Accepted, queued, not yet touched.
    Pending,
    /// Configuration being checked against current port/engine reality.
    Validating,
    /// Engine being configured with streams.
    Preparing,
    /// Traffic flowing.
    Running,
    /// Traffic stopped, results being reduced.
    Analyzing,
    /// Terminal: finished normally.
    Complete,
    /// Terminal: aborted by an error.
    Failed,
    /// Terminal: stopped by a user.
    Cancelled,
}

impl RunState {
    /// True when no further transition is possible.
    ///
    /// Used on daemon startup to sweep runs that were interrupted mid-flight.
    pub fn is_terminal(self) -> bool {
        matches!(self, RunState::Complete | RunState::Failed | RunState::Cancelled)
    }

    /// Stable lowercase token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Pending => "pending",
            RunState::Validating => "validating",
            RunState::Preparing => "preparing",
            RunState::Running => "running",
            RunState::Analyzing => "analyzing",
            RunState::Complete => "complete",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunState {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(RunState::Pending),
            "validating" => Ok(RunState::Validating),
            "preparing" => Ok(RunState::Preparing),
            "running" => Ok(RunState::Running),
            "analyzing" => Ok(RunState::Analyzing),
            "complete" => Ok(RunState::Complete),
            "failed" => Ok(RunState::Failed),
            "cancelled" => Ok(RunState::Cancelled),
            other => Err(ParseEnumError::new("run state", other)),
        }
    }
}

/// Kind of test a `tests` row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestType {
    /// Operator-driven start/stop of a set of flows.
    Manual,
    /// RFC 2544 section 26.1 throughput binary search.
    Rfc2544Throughput,
    /// RFC 2544 section 26.2 latency at the throughput rate.
    Rfc2544Latency,
    /// RFC 2544 section 26.3 frame-loss rate ladder.
    Rfc2544Frameloss,
    /// RFC 2544 section 26.4 back-to-back burst search.
    Rfc2544B2b,
}

impl TestType {
    /// Stable snake_case token used in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            TestType::Manual => "manual",
            TestType::Rfc2544Throughput => "rfc2544_throughput",
            TestType::Rfc2544Latency => "rfc2544_latency",
            TestType::Rfc2544Frameloss => "rfc2544_frameloss",
            TestType::Rfc2544B2b => "rfc2544_b2b",
        }
    }
}

impl std::fmt::Display for TestType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TestType {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "manual" => Ok(TestType::Manual),
            "rfc2544_throughput" => Ok(TestType::Rfc2544Throughput),
            "rfc2544_latency" => Ok(TestType::Rfc2544Latency),
            "rfc2544_frameloss" => Ok(TestType::Rfc2544Frameloss),
            "rfc2544_b2b" => Ok(TestType::Rfc2544B2b),
            other => Err(ParseEnumError::new("test type", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Database bridging
// ---------------------------------------------------------------------------

/// Derives `TryFrom<String>` from the type's `FromStr`.
///
/// These enums are stored as `TEXT` with a `CHECK` constraint rather than as
/// Postgres enum types, so that adding a variant is a one-line migration. sqlx's
/// `#[sqlx(try_from = "String")]` needs exactly this impl to decode such a column
/// straight into the domain type.
macro_rules! try_from_string {
    ($($t:ty),* $(,)?) => {
        $(
            impl TryFrom<String> for $t {
                type Error = ParseEnumError;

                // Spelled out rather than `Self::Error`, which is ambiguous for
                // any enum that also has a variant named `Error`.
                fn try_from(s: String) -> Result<Self, ParseEnumError> {
                    s.parse()
                }
            }
        )*
    };
}

try_from_string!(Role, PortMode, LinkState, EngineMode, PortGroupState, RunState, TestType);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_text_decodes_into_domain_enums() {
        assert_eq!(Role::try_from("operator".to_string()).unwrap(), Role::Operator);
        assert_eq!(PortMode::try_from("dpdk".to_string()).unwrap(), PortMode::Dpdk);
        // A row that somehow violated its CHECK constraint must surface as an
        // error rather than being silently coerced to a default.
        assert!(Role::try_from("root".to_string()).is_err());
    }

    #[test]
    fn role_ordering_reflects_privilege() {
        assert!(Role::Admin.has_at_least(Role::Viewer));
        assert!(Role::Admin.has_at_least(Role::Admin));
        assert!(Role::Operator.has_at_least(Role::Viewer));
        assert!(!Role::Viewer.has_at_least(Role::Operator));
        assert!(!Role::Operator.has_at_least(Role::Admin));
    }

    #[test]
    fn enum_tokens_round_trip_through_strings() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
        for mode in [PortMode::Kernel, PortMode::Dpdk] {
            assert_eq!(mode.as_str().parse::<PortMode>().unwrap(), mode);
        }
        for state in [LinkState::Up, LinkState::Down, LinkState::Unknown] {
            assert_eq!(state.as_str().parse::<LinkState>().unwrap(), state);
        }
        for state in [
            RunState::Pending,
            RunState::Validating,
            RunState::Preparing,
            RunState::Running,
            RunState::Analyzing,
            RunState::Complete,
            RunState::Failed,
            RunState::Cancelled,
        ] {
            assert_eq!(state.as_str().parse::<RunState>().unwrap(), state);
        }
        for ty in [
            TestType::Manual,
            TestType::Rfc2544Throughput,
            TestType::Rfc2544Latency,
            TestType::Rfc2544Frameloss,
            TestType::Rfc2544B2b,
        ] {
            assert_eq!(ty.as_str().parse::<TestType>().unwrap(), ty);
        }
    }

    #[test]
    fn json_tokens_match_database_tokens() {
        // The wire form and the CHECK-constraint form must not drift apart.
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), "\"admin\"");
        assert_eq!(serde_json::to_string(&PortMode::Dpdk).unwrap(), "\"dpdk\"");
        assert_eq!(
            serde_json::to_string(&TestType::Rfc2544Throughput).unwrap(),
            "\"rfc2544_throughput\""
        );
    }

    #[test]
    fn only_the_three_end_states_are_terminal() {
        assert!(RunState::Complete.is_terminal());
        assert!(RunState::Failed.is_terminal());
        assert!(RunState::Cancelled.is_terminal());
        assert!(!RunState::Running.is_terminal());
        assert!(!RunState::Pending.is_terminal());
    }
}
