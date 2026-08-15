//! The privileged operations themselves.
//!
//! Reads come from sysfs, writes go through `dpdk-devbind.py` / `driverctl` or
//! directly to `/sys`. Every entry point re-checks the allowlist: the server
//! checks it too, but this is the layer that actually touches hardware and it
//! does not get to assume its caller was careful.
//!
//! Sysfs reads are ordinary blocking file reads. They are single-digit
//! microseconds against an in-kernel filesystem, so they run inline on the async
//! task rather than paying for a `spawn_blocking` hop.

use flux_core::port::{
    DriverKind, HugepageSize, HugepagesStatus, NicInfo, PciAddr, PortdErrorCode, PortdOk,
};

use crate::allowlist::Allowlist;

/// A failure to report back over the socket.
#[derive(Debug, Clone)]
pub struct OpError {
    /// Machine-readable class, mapped back to a `PortError` by the client.
    pub code: PortdErrorCode,
    /// Detail for the operator.
    pub message: String,
}

impl OpError {
    /// The request named a device outside the allowlist.
    fn not_allowed(pci: &PciAddr) -> Self {
        Self {
            code: PortdErrorCode::NotAllowed,
            message: format!("{pci} is not in the flux-portd allowlist"),
        }
    }

    /// The device does not exist on this machine.
    fn not_found(pci: &PciAddr) -> Self {
        Self { code: PortdErrorCode::NotFound, message: format!("no PCI device at {pci}") }
    }

    /// The operation was attempted and failed.
    fn failed(message: impl Into<String>) -> Self {
        Self { code: PortdErrorCode::Failed, message: message.into() }
    }
}

/// Result alias for every operation.
pub type OpResult = Result<PortdOk, OpError>;

/// Executes port operations against the host, bounded by an allowlist.
pub struct Ops {
    allowlist: Allowlist,
}

impl Ops {
    /// Wraps an allowlist in an executor.
    pub fn new(allowlist: Allowlist) -> Self {
        Self { allowlist }
    }

    /// Enumerates every allowlisted device that is actually present.
    ///
    /// Devices in the allowlist but absent from the machine are skipped rather
    /// than reported as errors: a chassis may be configured for more NICs than
    /// are currently installed.
    pub async fn list(&self) -> OpResult {
        let mut nics = Vec::new();
        for pci in self.allowlist.iter() {
            match platform::read_nic(pci) {
                Ok(nic) => nics.push(nic),
                Err(err) => {
                    tracing::debug!(%pci, error = %err.message, "skipping absent device");
                }
            }
        }
        Ok(PortdOk::Nics { nics })
    }

    /// Binds `pci` to `driver` and returns its refreshed state.
    pub async fn bind(&self, pci: &PciAddr, driver: DriverKind) -> OpResult {
        self.check(pci)?;
        platform::bind(pci, driver).await?;
        let nic = platform::read_nic(pci)?;
        tracing::info!(%pci, %driver, "bound device");
        Ok(PortdOk::Nic { nic: Box::new(nic) })
    }

    /// Returns `pci` to its in-tree kernel driver.
    pub async fn unbind(&self, pci: &PciAddr) -> OpResult {
        self.check(pci)?;
        platform::unbind(pci).await?;
        let nic = platform::read_nic(pci)?;
        tracing::info!(%pci, "restored device to kernel driver");
        Ok(PortdOk::Nic { nic: Box::new(nic) })
    }

    /// Reads the current hugepage allocation.
    pub async fn hugepages_status(&self) -> OpResult {
        Ok(PortdOk::Hugepages { hugepages: platform::hugepages_status()? })
    }

    /// Requests `count` pages of `size` and returns the resulting allocation.
    ///
    /// The kernel may satisfy less than the request when memory is fragmented,
    /// so the caller is expected to compare the returned totals against what it
    /// asked for rather than assuming success.
    pub async fn hugepages_setup(&self, count: u64, size: HugepageSize) -> OpResult {
        platform::hugepages_setup(count, size)?;
        let status = platform::hugepages_status()?;
        tracing::info!(count, %size, total = ?status.pools, "hugepage allocation updated");
        Ok(PortdOk::Hugepages { hugepages: status })
    }

    /// Rejects any address the allowlist does not cover.
    fn check(&self, pci: &PciAddr) -> Result<(), OpError> {
        if self.allowlist.permits(pci) {
            Ok(())
        } else {
            tracing::warn!(%pci, "refused operation on non-allowlisted device");
            Err(OpError::not_allowed(pci))
        }
    }
}

// ---------------------------------------------------------------------------
// Linux implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod platform {
    use std::path::{Path, PathBuf};

    use flux_core::port::HugepagePool;
    use flux_core::types::{LinkState, PortMode};

    use super::*;

    /// Root of the PCI device tree in sysfs.
    const PCI_DEVICES: &str = "/sys/bus/pci/devices";
    /// Root of the network class tree in sysfs.
    const NET_CLASS: &str = "/sys/class/net";
    /// System-wide hugepage pools.
    const HUGEPAGES: &str = "/sys/kernel/mm/hugepages";
    /// Per-NUMA-node tree, used to report hugepages per node.
    const NUMA_NODES: &str = "/sys/devices/system/node";
    /// Where we remember the kernel driver a device had before we took it away.
    ///
    /// Inside this helper's own state directory. It used to live under
    /// `/var/lib/flux`, but that directory belongs to `fluxd`: systemd chowns a
    /// StateDirectory to its unit's user, and systemd ≥ 256 refuses to spawn a
    /// service whose state directory is owned by someone unexpected — so the
    /// root helper stopped starting the moment `fluxd` had run once.
    const ORIGINAL_DRIVERS: &str = "/var/lib/flux-portd/original-drivers";
    /// Where records lived before 0.1.5. Read as a fallback so a port bound to
    /// DPDK by an older version still knows its way back to the kernel driver.
    const LEGACY_ORIGINAL_DRIVERS: &str = "/var/lib/flux/original-drivers";

    /// Reads one file and trims trailing whitespace.
    fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
        std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    /// Builds the sysfs directory for a device.
    fn device_dir(pci: &PciAddr) -> PathBuf {
        Path::new(PCI_DEVICES).join(pci.as_str())
    }

    /// Reads the full inventory entry for one device.
    pub fn read_nic(pci: &PciAddr) -> Result<NicInfo, OpError> {
        let dir = device_dir(pci);
        if !dir.exists() {
            return Err(OpError::not_found(pci));
        }

        // The `driver` symlink points at the module currently bound, if any.
        let driver = std::fs::read_link(dir.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));

        // A kernel driver publishes the device under `net/<ifname>`; a userspace
        // (DPDK) driver does not, which is how we tell the two apart.
        let ifname = std::fs::read_dir(dir.join("net"))
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned());

        let mode = if ifname.is_some() { PortMode::Kernel } else { PortMode::Dpdk };

        let (mac, speed_mbps, link_state) = match &ifname {
            Some(name) => {
                let net = Path::new(NET_CLASS).join(name);
                let mac = read_trimmed(net.join("address"));
                // `speed` reads -1 or EINVAL when the link is down.
                let speed = read_trimmed(net.join("speed"))
                    .and_then(|s| s.parse::<i32>().ok())
                    .filter(|s| *s > 0);
                let link = match read_trimmed(net.join("operstate")).as_deref() {
                    Some("up") => LinkState::Up,
                    Some("down") => LinkState::Down,
                    _ => LinkState::Unknown,
                };
                (mac, speed, link)
            }
            // Bound to DPDK: the kernel has no view of carrier state. The engine
            // reports it instead, once an instance owns the port.
            None => (None, None, LinkState::Unknown),
        };

        let numa_node = read_trimmed(dir.join("numa_node"))
            .and_then(|s| s.parse::<i32>().ok())
            .and_then(|n| u32::try_from(n).ok());

        Ok(NicInfo {
            pci_addr: pci.clone(),
            description: describe(pci, &dir),
            driver,
            ifname,
            mac,
            speed_mbps,
            mode,
            link_state,
            numa_node,
        })
    }

    /// Best-effort human-readable device name.
    ///
    /// `lspci` gives a real product name when `pciutils` and its ID database are
    /// installed; otherwise we fall back to raw vendor/device ids, which are at
    /// least unambiguous.
    fn describe(pci: &PciAddr, dir: &Path) -> String {
        if let Ok(out) = std::process::Command::new("lspci").arg("-s").arg(pci.as_str()).output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(line) = text.lines().next() {
                    // "81:00.0 Ethernet controller: Intel Corporation E810-C"
                    if let Some((_, rest)) = line.split_once(both_colon_space()) {
                        return rest.trim().to_string();
                    }
                }
            }
        }
        let vendor = read_trimmed(dir.join("vendor")).unwrap_or_else(|| "?".into());
        let device = read_trimmed(dir.join("device")).unwrap_or_else(|| "?".into());
        format!("PCI device {vendor}:{device}")
    }

    /// The `": "` separator `lspci` puts between the slot and the description.
    fn both_colon_space() -> &'static str {
        ": "
    }

    /// Binds a device to the requested driver.
    pub async fn bind(pci: &PciAddr, driver: DriverKind) -> Result<(), OpError> {
        if driver == DriverKind::Kernel {
            return unbind(pci).await;
        }

        // Remember where it came from so `unbind` can put it back. Recording this
        // before the bind means a crash mid-operation still leaves a recoverable
        // note on disk.
        if let Some(current) = read_nic(pci)?.driver {
            if current != driver.module_name() {
                remember_original_driver(pci, &current);
            }
        }

        run("dpdk-devbind.py", &["--force", "--bind", driver.module_name(), pci.as_str()]).await
    }

    /// Restores a device to the kernel driver it had before Flux took it.
    pub async fn unbind(pci: &PciAddr) -> Result<(), OpError> {
        match recall_original_driver(pci) {
            Some(original) => run("dpdk-devbind.py", &["--bind", &original, pci.as_str()]).await,
            // Nothing recorded — ask driverctl to drop any override and let the
            // kernel re-probe using its own matching rules.
            None => run("driverctl", &["unset-override", pci.as_str()]).await,
        }
    }

    /// Persists the pre-Flux driver for a device.
    fn remember_original_driver(pci: &PciAddr, driver: &str) {
        let dir = Path::new(ORIGINAL_DRIVERS);
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!(%pci, %err, "could not record original driver");
            return;
        }
        if let Err(err) = std::fs::write(dir.join(pci.as_str()), driver) {
            tracing::warn!(%pci, %err, "could not record original driver");
        }
    }

    /// Reads back the pre-Flux driver for a device, if we recorded one.
    fn recall_original_driver(pci: &PciAddr) -> Option<String> {
        [ORIGINAL_DRIVERS, LEGACY_ORIGINAL_DRIVERS].iter().find_map(|dir| {
            read_trimmed(Path::new(dir).join(pci.as_str())).filter(|s| !s.is_empty())
        })
    }

    /// Runs a command, turning a non-zero exit into an `OpError`.
    async fn run(program: &str, args: &[&str]) -> Result<(), OpError> {
        let output = tokio::process::Command::new(program)
            .args(args)
            .output()
            .await
            .map_err(|e| OpError::failed(format!("could not execute {program}: {e}")))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(OpError::failed(format!(
            "{program} {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { "no output".into() } else { stderr }
        )))
    }

    /// Reads every hugepage pool the kernel exposes, system-wide and per NUMA node.
    pub fn hugepages_status() -> Result<HugepagesStatus, OpError> {
        let mut pools = Vec::new();

        for size in [HugepageSize::TwoMb, HugepageSize::OneGb] {
            let dir = Path::new(HUGEPAGES).join(size.sysfs_dir());
            if let Some(pool) = read_pool(&dir, size, None) {
                pools.push(pool);
            }
        }

        if let Ok(entries) = std::fs::read_dir(NUMA_NODES) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(node) = name.strip_prefix("node").and_then(|n| n.parse::<u32>().ok())
                else {
                    continue;
                };
                for size in [HugepageSize::TwoMb, HugepageSize::OneGb] {
                    let dir = entry.path().join("hugepages").join(size.sysfs_dir());
                    if let Some(pool) = read_pool(&dir, size, Some(node)) {
                        pools.push(pool);
                    }
                }
            }
        }

        // "Sufficient" deliberately looks at free pages, not total: a pool fully
        // consumed by an already-running engine cannot start another one.
        let sufficient = pools.iter().any(|p| p.node.is_none() && p.free > 0);
        Ok(HugepagesStatus { pools, sufficient })
    }

    /// Reads one hugepage pool directory, or `None` if the kernel lacks that size.
    fn read_pool(dir: &Path, size: HugepageSize, node: Option<u32>) -> Option<HugepagePool> {
        let total = read_trimmed(dir.join("nr_hugepages"))?.parse().ok()?;
        let free = read_trimmed(dir.join("free_hugepages"))?.parse().unwrap_or(0);
        Some(HugepagePool { size, node, total, free })
    }

    /// Asks the kernel to resize a hugepage pool.
    pub fn hugepages_setup(count: u64, size: HugepageSize) -> Result<(), OpError> {
        let path = Path::new(HUGEPAGES).join(size.sysfs_dir()).join("nr_hugepages");
        std::fs::write(&path, count.to_string())
            .map_err(|e| OpError::failed(format!("writing {}: {e}", path.display())))
    }
}

// ---------------------------------------------------------------------------
// Everything else
// ---------------------------------------------------------------------------

/// Stubs so the workspace builds on non-Linux developer machines.
///
/// The helper has no meaning off Linux — there is no sysfs and no DPDK — but the
/// workspace must still compile, and a clear runtime error beats a build failure
/// for someone who only wanted to work on `fluxd`.
#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    /// The message every unsupported operation returns.
    fn unsupported() -> OpError {
        OpError::failed("flux-portd requires Linux; use FLUX_PORTD=mock for development")
    }

    pub fn read_nic(_pci: &PciAddr) -> Result<NicInfo, OpError> {
        Err(unsupported())
    }

    pub async fn bind(_pci: &PciAddr, _driver: DriverKind) -> Result<(), OpError> {
        Err(unsupported())
    }

    pub async fn unbind(_pci: &PciAddr) -> Result<(), OpError> {
        Err(unsupported())
    }

    pub fn hugepages_status() -> Result<HugepagesStatus, OpError> {
        Err(unsupported())
    }

    pub fn hugepages_setup(_count: u64, _size: HugepageSize) -> Result<(), OpError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operations_on_unlisted_devices_are_refused_before_touching_hardware() {
        let ops = Ops::new(Allowlist::from_addrs([PciAddr::parse("0000:81:00.0").unwrap()]));
        let forbidden = PciAddr::parse("0000:02:00.0").unwrap();

        let err = ops.bind(&forbidden, DriverKind::VfioPci).await.unwrap_err();
        assert_eq!(err.code, PortdErrorCode::NotAllowed);

        let err = ops.unbind(&forbidden).await.unwrap_err();
        assert_eq!(err.code, PortdErrorCode::NotAllowed);
    }

    #[tokio::test]
    async fn listing_never_fails_on_absent_hardware() {
        // The allowlist may name NICs that are not installed in this chassis.
        let ops = Ops::new(Allowlist::from_addrs([PciAddr::parse("0000:ff:1f.7").unwrap()]));
        let out = ops.list().await.expect("list should succeed with absent devices");
        match out {
            PortdOk::Nics { nics } => assert!(nics.is_empty()),
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
