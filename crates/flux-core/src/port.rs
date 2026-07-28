//! The port-control boundary: NIC inventory, driver binding, and hugepages.
//!
//! `fluxd` runs unprivileged and therefore cannot rebind a NIC or write to
//! `/sys`. Everything in this module is either a validated value type or the
//! [`PortController`] trait, whose real implementation forwards to the `flux-portd`
//! helper over a unix socket. The mock implementation satisfies the same trait so
//! the whole daemon runs on a developer laptop.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::{LinkState, PortMode};

// ---------------------------------------------------------------------------
// PCI addresses
// ---------------------------------------------------------------------------

/// A validated PCI address in canonical `DDDD:BB:DD.F` form.
///
/// This is a newtype rather than a `String` on purpose: the value is passed to a
/// root-privileged helper that shells out to `dpdk-devbind.py`, so it must never
/// be possible to construct one containing shell metacharacters or a path. The
/// only way in is [`PciAddr::parse`], which enforces the exact hex layout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, ToSchema)]
#[serde(transparent)]
pub struct PciAddr(String);

impl PciAddr {
    /// Parses a PCI address, normalising it to lowercase canonical form.
    ///
    /// Accepts both the full `0000:81:00.0` form and the short `81:00.0` form,
    /// expanding the latter to domain `0000`.
    pub fn parse(raw: &str) -> Result<Self, PciAddrError> {
        let raw = raw.trim();
        let (domain, rest) = match raw.split(':').count() {
            3 => {
                let (d, r) = raw.split_once(':').expect("3 segments implies a colon");
                (d, r)
            }
            2 => ("0000", raw),
            _ => return Err(PciAddrError(raw.to_string())),
        };

        let (bus, devfn) = rest.split_once(':').ok_or_else(|| PciAddrError(raw.to_string()))?;
        let (device, function) =
            devfn.split_once('.').ok_or_else(|| PciAddrError(raw.to_string()))?;

        let ok = is_hex(domain, 4) && is_hex(bus, 2) && is_hex(device, 2) && is_hex(function, 1);
        if !ok {
            return Err(PciAddrError(raw.to_string()));
        }

        Ok(PciAddr(format!(
            "{}:{}:{}.{}",
            domain.to_ascii_lowercase(),
            bus.to_ascii_lowercase(),
            device.to_ascii_lowercase(),
            function.to_ascii_lowercase()
        )))
    }

    /// The canonical string form, safe to hand to the privileged helper.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exactly `len` characters, all ASCII hex digits.
fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl fmt::Display for PciAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PciAddr {
    type Err = PciAddrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PciAddr::parse(s)
    }
}

impl TryFrom<String> for PciAddr {
    type Error = PciAddrError;

    /// Lets sqlx decode a `TEXT` column straight into a validated address.
    fn try_from(s: String) -> Result<Self, Self::Error> {
        PciAddr::parse(&s)
    }
}

impl<'de> Deserialize<'de> for PciAddr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        PciAddr::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// A string that does not parse as a canonical PCI address.
#[derive(Debug, Clone, thiserror::Error)]
#[error("`{0}` is not a valid PCI address (expected DDDD:BB:DD.F)")]
pub struct PciAddrError(pub String);

// ---------------------------------------------------------------------------
// Drivers and hugepages
// ---------------------------------------------------------------------------

/// The driver a NIC can be bound to.
///
/// `Kernel` is a request to restore whatever in-tree driver the device
/// advertises; the two userspace variants are the DPDK-capable options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DriverKind {
    /// Restore the device's native in-tree kernel driver.
    Kernel,
    /// IOMMU-backed userspace driver. The default and the only one we recommend.
    VfioPci,
    /// Legacy userspace driver, for hosts without a usable IOMMU.
    UioPciGeneric,
}

impl DriverKind {
    /// The module name as `dpdk-devbind.py` expects it.
    pub fn module_name(self) -> &'static str {
        match self {
            DriverKind::Kernel => "kernel",
            DriverKind::VfioPci => "vfio-pci",
            DriverKind::UioPciGeneric => "uio_pci_generic",
        }
    }

    /// The port mode a device ends up in once bound to this driver.
    pub fn resulting_mode(self) -> PortMode {
        match self {
            DriverKind::Kernel => PortMode::Kernel,
            DriverKind::VfioPci | DriverKind::UioPciGeneric => PortMode::Dpdk,
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.module_name())
    }
}

/// Hugepage size class. DPDK wants 1G pages for line-rate work; 2M is the
/// fallback when the kernel was booted without `hugepagesz=1G`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HugepageSize {
    /// 2 MiB pages.
    #[serde(rename = "2M")]
    TwoMb,
    /// 1 GiB pages.
    #[serde(rename = "1G")]
    OneGb,
}

impl HugepageSize {
    /// Size in kilobytes, which is the unit `/sys/kernel/mm/hugepages` uses.
    pub fn kb(self) -> u64 {
        match self {
            HugepageSize::TwoMb => 2048,
            HugepageSize::OneGb => 1_048_576,
        }
    }

    /// The `hugepages-<n>kB` sysfs directory name for this size.
    pub fn sysfs_dir(self) -> String {
        format!("hugepages-{}kB", self.kb())
    }
}

impl fmt::Display for HugepageSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HugepageSize::TwoMb => f.write_str("2M"),
            HugepageSize::OneGb => f.write_str("1G"),
        }
    }
}

/// Hugepage allocation as currently reported by the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HugepagesStatus {
    /// Per-size allocation, one entry per size class the kernel exposes.
    pub pools: Vec<HugepagePool>,
    /// True when at least one pool has free pages, i.e. an engine could start.
    pub sufficient: bool,
}

/// One hugepage size class on one NUMA node (or system-wide when `node` is `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HugepagePool {
    /// Page size for this pool.
    pub size: HugepageSize,
    /// NUMA node, or `None` for the system-wide aggregate.
    pub node: Option<u32>,
    /// Pages currently allocated to the pool.
    pub total: u64,
    /// Pages in the pool not yet handed to a process.
    pub free: u64,
}

// ---------------------------------------------------------------------------
// NIC inventory
// ---------------------------------------------------------------------------

/// A NIC as the privileged helper sees it.
///
/// This is the *hardware* view. It is reconciled against the `ports` table on
/// every inventory refresh; the database row carries the operator-assigned name
/// and group membership, this carries the physical truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NicInfo {
    /// Canonical PCI address, the stable identity of the device.
    pub pci_addr: PciAddr,
    /// Human-readable device description from the PCI ID database.
    pub description: String,
    /// Currently bound driver module name, if any.
    pub driver: Option<String>,
    /// Kernel interface name, present only while bound to a kernel driver.
    pub ifname: Option<String>,
    /// Permanent hardware address, lowercase colon-separated.
    pub mac: Option<String>,
    /// Negotiated (or nominal) link speed in megabits per second.
    pub speed_mbps: Option<i32>,
    /// Whether the kernel or userspace driver owns the device.
    pub mode: PortMode,
    /// Carrier state, `Unknown` when bound to DPDK and no engine is attached.
    pub link_state: LinkState,
    /// NUMA node the device is attached to, for engine core pinning.
    pub numa_node: Option<u32>,
}

/// Failures the port-control boundary can produce.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// The helper is not reachable (socket missing, or it died).
    #[error("flux-portd is unavailable: {0}")]
    Unavailable(String),

    /// The requested PCI address is not in the helper's allowlist.
    ///
    /// This is the guard that stops an operator from unbinding the management NIC.
    #[error("port {0} is not permitted by the flux-portd allowlist")]
    NotAllowed(PciAddr),

    /// No device exists at that address.
    #[error("no device at PCI address {0}")]
    NotFound(PciAddr),

    /// The helper accepted the request but the operation failed.
    #[error("port operation failed: {0}")]
    Failed(String),

    /// The request was rejected before it reached the helper.
    #[error("invalid port request: {0}")]
    Invalid(String),
}

/// The privileged operations `fluxd` needs but cannot perform itself.
///
/// Implemented by the unix-socket client (production) and by an in-memory fake
/// (development and tests). Keeping this a trait is what makes `FLUX_PORTD=mock`
/// a one-line swap at startup rather than a compile-time fork of the daemon.
#[async_trait::async_trait]
pub trait PortController: Send + Sync + 'static {
    /// Enumerates every NIC the helper is allowed to talk about.
    async fn list(&self) -> Result<Vec<NicInfo>, PortError>;

    /// Binds a device to `driver`, returning its refreshed inventory entry.
    async fn bind(&self, pci: &PciAddr, driver: DriverKind) -> Result<NicInfo, PortError>;

    /// Returns a device to its in-tree kernel driver.
    async fn unbind(&self, pci: &PciAddr) -> Result<NicInfo, PortError>;

    /// Reads the current hugepage allocation.
    async fn hugepages_status(&self) -> Result<HugepagesStatus, PortError>;

    /// Requests `count` pages of `size`, returning the resulting allocation.
    async fn hugepages_setup(
        &self,
        count: u64,
        size: HugepageSize,
    ) -> Result<HugepagesStatus, PortError>;
}

// ---------------------------------------------------------------------------
// Wire protocol between fluxd and flux-portd
// ---------------------------------------------------------------------------

/// A single newline-delimited JSON request sent to `flux-portd`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PortdRequest {
    /// Enumerate allowlisted NICs.
    List,
    /// Bind `pci` to `driver`.
    Bind {
        /// Target device.
        pci: PciAddr,
        /// Driver to bind.
        driver: DriverKind,
    },
    /// Restore `pci` to its kernel driver.
    Unbind {
        /// Target device.
        pci: PciAddr,
    },
    /// Read hugepage pools.
    HugepagesStatus,
    /// Allocate hugepages.
    HugepagesSetup {
        /// Number of pages requested.
        count: u64,
        /// Page size class.
        size: HugepageSize,
    },
}

/// A single newline-delimited JSON response from `flux-portd`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PortdResponse {
    /// The operation succeeded.
    Ok(PortdOk),
    /// The operation failed; `code` maps back onto a [`PortError`] variant.
    Error {
        /// Machine-readable failure class.
        code: PortdErrorCode,
        /// Human-readable detail for logs and the UI.
        message: String,
    },
}

/// Successful payloads, discriminated by the operation that produced them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortdOk {
    /// Response to [`PortdRequest::List`].
    Nics {
        /// Every allowlisted device.
        nics: Vec<NicInfo>,
    },
    /// Response to [`PortdRequest::Bind`] or [`PortdRequest::Unbind`].
    Nic {
        /// The device after the operation.
        nic: Box<NicInfo>,
    },
    /// Response to either hugepages operation.
    Hugepages {
        /// Resulting allocation.
        hugepages: HugepagesStatus,
    },
}

/// Failure classes the helper can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortdErrorCode {
    /// Address absent from `/etc/flux/portd.yaml`.
    NotAllowed,
    /// No such device.
    NotFound,
    /// Underlying command or sysfs write failed.
    Failed,
    /// Malformed request.
    Invalid,
}

impl PortdResponse {
    /// Converts a helper response into the trait-level `Result`.
    ///
    /// `pci` is the address the request targeted, used to build the address-carrying
    /// error variants that the wire format only identifies by code.
    pub fn into_result(self, pci: Option<&PciAddr>) -> Result<PortdOk, PortError> {
        match self {
            PortdResponse::Ok(ok) => Ok(ok),
            PortdResponse::Error { code, message } => Err(match (code, pci) {
                (PortdErrorCode::NotAllowed, Some(p)) => PortError::NotAllowed(p.clone()),
                (PortdErrorCode::NotFound, Some(p)) => PortError::NotFound(p.clone()),
                (PortdErrorCode::Invalid, _) => PortError::Invalid(message),
                _ => PortError::Failed(message),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_short_pci_addresses() {
        assert_eq!(PciAddr::parse("0000:81:00.0").unwrap().as_str(), "0000:81:00.0");
        assert_eq!(PciAddr::parse("81:00.1").unwrap().as_str(), "0000:81:00.1");
        assert_eq!(PciAddr::parse("  0000:0A:1F.7 ").unwrap().as_str(), "0000:0a:1f.7");
    }

    #[test]
    fn rejects_addresses_that_could_reach_a_shell() {
        for bad in [
            "0000:81:00.0; rm -rf /",
            "../../etc/passwd",
            "0000:81:00",
            "81:00",
            "gggg:81:00.0",
            "0000:81:00.",
            "0000:181:00.0",
            "",
            "$(whoami)",
        ] {
            assert!(PciAddr::parse(bad).is_err(), "should have rejected {bad:?}");
        }
    }

    #[test]
    fn pci_addresses_deserialize_through_validation() {
        let ok: Result<PciAddr, _> = serde_json::from_str("\"0000:81:00.0\"");
        assert!(ok.is_ok());
        let bad: Result<PciAddr, _> = serde_json::from_str("\"not-a-pci-address\"");
        assert!(bad.is_err());
    }

    #[test]
    fn driver_choice_determines_port_mode() {
        assert_eq!(DriverKind::VfioPci.resulting_mode(), PortMode::Dpdk);
        assert_eq!(DriverKind::UioPciGeneric.resulting_mode(), PortMode::Dpdk);
        assert_eq!(DriverKind::Kernel.resulting_mode(), PortMode::Kernel);
    }

    #[test]
    fn portd_requests_round_trip_over_the_wire() {
        let req = PortdRequest::Bind {
            pci: PciAddr::parse("0000:81:00.0").unwrap(),
            driver: DriverKind::VfioPci,
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"op\":\"bind\""), "unexpected encoding: {line}");
        assert!(line.contains("\"driver\":\"vfio-pci\""), "unexpected encoding: {line}");

        let back: PortdRequest = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, PortdRequest::Bind { .. }));
    }

    #[test]
    fn error_responses_map_onto_address_carrying_errors() {
        let pci = PciAddr::parse("0000:81:00.0").unwrap();
        let resp = PortdResponse::Error {
            code: PortdErrorCode::NotAllowed,
            message: "management interface".into(),
        };
        assert!(matches!(resp.into_result(Some(&pci)), Err(PortError::NotAllowed(_))));
    }

    #[test]
    fn hugepage_sysfs_names_match_the_kernel_layout() {
        assert_eq!(HugepageSize::TwoMb.sysfs_dir(), "hugepages-2048kB");
        assert_eq!(HugepageSize::OneGb.sysfs_dir(), "hugepages-1048576kB");
    }
}
