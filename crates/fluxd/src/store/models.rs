//! Row types and the API views built from them.
//!
//! Two kinds of struct live here. A *record* mirrors a table and derives
//! `FromRow`; a *view* is what the REST API emits and derives `Serialize` with
//! camelCase field names. They are usually the same shape, and where they are
//! not it is because the record holds something the API must never see — the
//! split between [`User`] and [`UserView`] exists solely so a password hash
//! cannot be serialised by accident.
//!
//! ## `#[schema(value_type = String, format = Uuid)]`
//!
//! utoipa's derive recognises `Uuid` by the token written in the field type, so
//! it does not see through the `Id` alias and would demand a `ToSchema` impl that
//! cannot be written for a foreign type. The annotation tells it what the alias
//! resolves to. It appears on every `Id` field of a `ToSchema` struct, here and
//! in `api::`.
//!
//! ## `#[allow(dead_code)]` on record structs
//!
//! A record mirrors its table. Some columns have no reader yet — nothing renders
//! `users.updated_at` — but dropping them from the struct would mean the next
//! feature that wants one has to re-derive the mapping and risk getting a column
//! name wrong. Keeping the record complete is worth the allow.

use flux_core::port::PciAddr;
use flux_core::types::{
    EngineMode, Id, LinkState, PortGroupState, PortMode, Role, RunState, TestType,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// A `users` row.
///
/// Deliberately not `Serialize`. Every path that returns a user to a client goes
/// through [`UserView`], which makes leaking `pw_hash` a compile error rather
/// than a code-review question.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code, reason = "record mirrors the table; not every column has a reader yet")]
pub struct User {
    /// Primary key.
    pub id: Id,
    /// Login name, stored with the operator's original casing.
    pub username: String,
    /// Argon2id PHC string.
    pub pw_hash: String,
    /// Access level.
    #[sqlx(try_from = "String")]
    pub role: Role,
    /// When the account was created.
    pub created_at: OffsetDateTime,
    /// When the account was last modified.
    pub updated_at: OffsetDateTime,
    /// When the account last authenticated successfully.
    pub last_login_at: Option<OffsetDateTime>,
}

/// A user as the API presents it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserView {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Login name.
    pub username: String,
    /// Access level.
    pub role: Role,
    /// When the account was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the account last authenticated successfully.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            role: u.role,
            created_at: u.created_at,
            last_login_at: u.last_login_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// A `sessions` row joined with the role of its owner.
///
/// The join is done in the lookup query so that authenticating a request is a
/// single round trip rather than a session read followed by a user read.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code, reason = "record mirrors the query; not every column has a reader yet")]
pub struct SessionWithUser {
    /// Session primary key.
    pub id: Id,
    /// Owning account.
    pub user_id: Id,
    /// Owning account's login name.
    pub username: String,
    /// Owning account's access level.
    #[sqlx(try_from = "String")]
    pub role: Role,
    /// When the session stops being valid.
    pub expires_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Port groups
// ---------------------------------------------------------------------------

/// A `port_groups` row, also the API view — nothing here is sensitive.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortGroup {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Operator-assigned label.
    pub name: String,
    /// Stateless or stateful engine personality.
    #[sqlx(try_from = "String")]
    pub engine_mode: EngineMode,
    /// Lifecycle of the backing engine instance.
    #[sqlx(try_from = "String")]
    pub state: PortGroupState,
    /// Serialised `EngineInstanceConfig`.
    pub trex_cfg: serde_json::Value,
    /// Why the group is in `error`, when it is.
    pub error: Option<String>,
    /// When the group was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the group was last modified.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The minimal group identity embedded in a port view.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortGroupRef {
    /// Group primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Group label.
    pub name: String,
    /// Engine personality, which determines what tests may use the port.
    pub engine_mode: EngineMode,
    /// Group lifecycle state.
    pub state: PortGroupState,
    /// This port's index within the group.
    pub index: i16,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// A `ports` row.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code, reason = "record mirrors the table; not every column has a reader yet")]
pub struct Port {
    /// Primary key.
    pub id: Id,
    /// Operator-assigned label.
    pub name: String,
    /// Hardware identity.
    #[sqlx(try_from = "String")]
    pub pci_addr: PciAddr,
    /// Product description from the PCI ID database.
    pub description: String,
    /// Currently bound driver module.
    pub driver: Option<String>,
    /// Kernel interface name, when kernel-bound.
    pub ifname: Option<String>,
    /// Permanent hardware address.
    pub mac: Option<String>,
    /// Link speed in megabits per second.
    pub speed_mbps: Option<i32>,
    /// NUMA node the card sits on.
    pub numa_node: Option<i32>,
    /// Kernel or DPDK ownership.
    #[sqlx(try_from = "String")]
    pub mode: PortMode,
    /// Carrier state.
    #[sqlx(try_from = "String")]
    pub link_state: LinkState,
    /// Owning port group, if any.
    pub group_id: Option<Id>,
    /// Index within the owning group.
    pub group_index: Option<i16>,
    /// Whether the device was seen in the last inventory refresh.
    pub present: bool,
    /// When the row was created.
    pub created_at: OffsetDateTime,
    /// When the row was last refreshed.
    pub updated_at: OffsetDateTime,
}

/// The flat shape returned by the ports list query, before nesting.
///
/// A single left-joined query keeps listing ports at one round trip regardless
/// of how many ports, groups, and reservations exist.
#[derive(Debug, Clone, FromRow)]
pub struct PortRowJoined {
    /// The port itself.
    #[sqlx(flatten)]
    pub port: Port,
    /// Group label, present when `port.group_id` is set.
    pub group_name: Option<String>,
    /// Group engine mode.
    pub group_engine_mode: Option<String>,
    /// Group lifecycle state.
    pub group_state: Option<String>,
    /// Reservation primary key, present when the port is held.
    pub reservation_id: Option<Id>,
    /// Who holds the reservation.
    pub reservation_user_id: Option<Id>,
    /// Their login name.
    pub reservation_username: Option<String>,
    /// Their note.
    pub reservation_note: Option<String>,
    /// When the hold lapses.
    pub reservation_expires_at: Option<OffsetDateTime>,
}

/// A port as the API presents it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortView {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Operator-assigned label.
    pub name: String,
    /// Hardware identity.
    pub pci_addr: PciAddr,
    /// Product description.
    pub description: String,
    /// Currently bound driver module.
    pub driver: Option<String>,
    /// Kernel interface name, when kernel-bound.
    pub ifname: Option<String>,
    /// Permanent hardware address.
    pub mac: Option<String>,
    /// Link speed in megabits per second.
    pub speed_mbps: Option<i32>,
    /// NUMA node.
    pub numa_node: Option<i32>,
    /// Kernel or DPDK ownership.
    pub mode: PortMode,
    /// Carrier state.
    pub link_state: LinkState,
    /// Whether the device was present at the last inventory refresh.
    pub present: bool,
    /// Owning group, when the port belongs to one.
    pub group: Option<PortGroupRef>,
    /// Current hold, when the port is reserved.
    pub reservation: Option<ReservationView>,
    /// When the row was last refreshed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl TryFrom<PortRowJoined> for PortView {
    type Error = anyhow::Error;

    /// Nests the joined columns.
    ///
    /// This is fallible because the group and reservation columns are decoded as
    /// `String` — a `LEFT JOIN` yields `NULL` for them, which the strongly typed
    /// enums cannot represent, so parsing is deferred to here.
    fn try_from(row: PortRowJoined) -> Result<Self, Self::Error> {
        let PortRowJoined {
            port,
            group_name,
            group_engine_mode,
            group_state,
            reservation_id,
            reservation_user_id,
            reservation_username,
            reservation_note,
            reservation_expires_at,
        } = row;

        let group = match (port.group_id, group_name, group_engine_mode, group_state) {
            (Some(id), Some(name), Some(mode), Some(state)) => Some(PortGroupRef {
                id,
                name,
                engine_mode: mode.parse()?,
                state: state.parse()?,
                index: port.group_index.unwrap_or(0),
            }),
            _ => None,
        };

        let reservation = match (
            reservation_id,
            reservation_user_id,
            reservation_username,
            reservation_expires_at,
        ) {
            (Some(id), Some(user_id), Some(username), Some(expires_at)) => Some(ReservationView {
                id,
                port_id: port.id,
                user_id,
                username,
                note: reservation_note.unwrap_or_default(),
                expires_at,
            }),
            _ => None,
        };

        Ok(PortView {
            id: port.id,
            name: port.name,
            pci_addr: port.pci_addr,
            description: port.description,
            driver: port.driver,
            ifname: port.ifname,
            mac: port.mac,
            speed_mbps: port.speed_mbps,
            numa_node: port.numa_node,
            mode: port.mode,
            link_state: port.link_state,
            present: port.present,
            group,
            reservation,
            updated_at: port.updated_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Reservations
// ---------------------------------------------------------------------------

/// A port hold, with the holder's name resolved.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReservationView {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Held port.
    #[schema(value_type = String, format = Uuid)]
    pub port_id: Id,
    /// Holder.
    #[schema(value_type = String, format = Uuid)]
    pub user_id: Id,
    /// Holder's login name.
    pub username: String,
    /// Free-text reason shown to other operators.
    pub note: String,
    /// When the hold lapses.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

/// A `flows` row.
///
/// `config` is stored as JSONB and deserialised into `flux_core::flow::FlowConfig`
/// on use. It is kept as `Value` here so a row written by a newer version can be
/// listed rather than making the whole page fail to load.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Operator-assigned label.
    pub name: String,
    /// Serialised `FlowConfig`.
    pub config: serde_json::Value,
    /// Who created it.
    #[schema(value_type = Option<String>, format = Uuid)]
    pub created_by: Option<Id>,
    /// When it was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A `tests` row.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Test {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Operator-assigned label.
    pub name: String,
    /// Which kind of test this is.
    #[sqlx(rename = "type", try_from = "String")]
    #[serde(rename = "type")]
    pub test_type: TestType,
    /// Type-specific configuration.
    pub config: serde_json::Value,
    /// Flows this test drives, in order.
    #[schema(value_type = Vec<String>)]
    pub flow_ids: Vec<Id>,
    /// Who created it.
    #[schema(value_type = Option<String>, format = Uuid)]
    pub created_by: Option<Id>,
    /// When it was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// A `runs` row.
///
/// `test_name` and `type` are copied in rather than joined, so a run survives the
/// deletion of the test that produced it. A result nobody can trace back to a
/// configuration is worthless, which is also why `config_snapshot` exists.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// The test this run came from, if it still exists.
    #[schema(value_type = Option<String>, format = Uuid)]
    pub test_id: Option<Id>,
    /// The test's name at the time the run started.
    pub test_name: String,
    /// The test's type.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub test_type: String,
    /// Where the run is in its lifecycle.
    #[sqlx(try_from = "String")]
    pub state: RunState,
    /// Who started it.
    #[schema(value_type = Option<String>, format = Uuid)]
    pub started_by: Option<Id>,
    /// When it started.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// When it reached a terminal state.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Operator-supplied notes about the device under test.
    pub dut_meta: serde_json::Value,
    /// The complete resolved configuration at the moment the run started.
    pub config_snapshot: serde_json::Value,
    /// Why the run failed, when it did.
    pub error: Option<String>,
}

/// A `run_results` row: one trial.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    /// Primary key.
    #[schema(value_type = String, format = Uuid)]
    pub id: Id,
    /// Owning run.
    #[schema(value_type = String, format = Uuid)]
    pub run_id: Id,
    /// Trial number within the run.
    pub iteration: i32,
    /// Frame size this trial used, when the test varies it.
    pub frame_size: Option<i32>,
    /// Trial inputs.
    pub params: serde_json::Value,
    /// Trial outputs.
    pub metrics: serde_json::Value,
    /// Whether this trial established the reported result for its frame size.
    pub passed: bool,
    /// When the trial was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// A `settings` row.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    /// Setting name.
    pub key: String,
    /// Arbitrary JSON payload, shaped by the key.
    pub value: serde_json::Value,
    /// When it last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
