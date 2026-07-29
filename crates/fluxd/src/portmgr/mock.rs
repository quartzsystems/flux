//! An in-process fake of the privileged helper.
//!
//! This is what makes `FLUX_PORTD=mock` a complete development environment: it
//! presents a plausible four-port appliance, honours bind/unbind, and tracks
//! hugepage allocation, all without root, DPDK, or a NIC.
//!
//! It simulates one thing a real kernel driver could not: link state stays
//! visible after a DPDK bind. On real hardware that information comes from the
//! engine once it owns the port, which milestone 2 wires up. Reporting it here
//! keeps the ports page meaningful in the meantime.

use std::sync::Mutex;

use async_trait::async_trait;
use flux_core::port::{
    DriverKind, HugepagePool, HugepageSize, HugepagesStatus, NicInfo, PciAddr, PortController,
    PortError,
};
use flux_core::types::{LinkState, PortMode};

/// The simulated chassis and its hugepage pools.
#[derive(Debug)]
pub struct MockPortController {
    state: Mutex<State>,
}

/// Everything the fake mutates.
#[derive(Debug)]
struct State {
    nics: Vec<NicInfo>,
    /// 1 GiB pages allocated. Zero until the operator sets them up, which is the
    /// state a freshly imaged appliance is actually in.
    hugepages_1g: u64,
    /// 1 GiB pages not yet consumed by an engine.
    hugepages_1g_free: u64,
}

impl Default for MockPortController {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPortController {
    /// Builds the simulated chassis.
    ///
    /// Four 100G ports on one card. The first pair is cabled together, the second
    /// pair is not — so the ports page shows both link states and the dashboard
    /// health card has something real to count.
    pub fn new() -> Self {
        let nics = (0..4)
            .map(|i| NicInfo {
                pci_addr: PciAddr::parse(&format!("0000:81:00.{i}")).expect("literal is valid"),
                description: "Intel Corporation Ethernet Controller E810-C for QSFP".into(),
                driver: Some("ice".into()),
                ifname: Some(format!("ens1f{i}")),
                mac: Some(format!("00:1b:21:aa:bb:{:02x}", 0xc0 + i)),
                speed_mbps: Some(100_000),
                mode: PortMode::Kernel,
                // Ports 0 and 1 are looped to the device under test; 2 and 3 are
                // spare and unpatched.
                link_state: if i < 2 { LinkState::Up } else { LinkState::Down },
                numa_node: Some(1),
            })
            .collect();

        Self { state: Mutex::new(State { nics, hugepages_1g: 0, hugepages_1g_free: 0 }) }
    }

    /// Locks the simulated state.
    ///
    /// A poisoned lock means a previous caller panicked mid-mutation. For a
    /// development fake the honest response is to recover the state and carry on
    /// rather than poison every subsequent request.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Finds a device, or reports it missing.
    fn index_of(state: &State, pci: &PciAddr) -> Result<usize, PortError> {
        state
            .nics
            .iter()
            .position(|n| &n.pci_addr == pci)
            .ok_or_else(|| PortError::NotFound(pci.clone()))
    }

    /// Current hugepage report.
    fn hugepages(state: &State) -> HugepagesStatus {
        HugepagesStatus {
            pools: vec![HugepagePool {
                size: HugepageSize::OneGb,
                node: None,
                total: state.hugepages_1g,
                free: state.hugepages_1g_free,
            }],
            sufficient: state.hugepages_1g_free > 0,
        }
    }
}

#[async_trait]
impl PortController for MockPortController {
    async fn list(&self) -> Result<Vec<NicInfo>, PortError> {
        Ok(self.state().nics.clone())
    }

    async fn bind(&self, pci: &PciAddr, driver: DriverKind) -> Result<NicInfo, PortError> {
        let mut state = self.state();
        let idx = Self::index_of(&state, pci)?;
        let nic = &mut state.nics[idx];

        match driver {
            DriverKind::Kernel => {
                nic.driver = Some("ice".into());
                nic.ifname =
                    Some(format!("ens1f{}", nic.pci_addr.as_str().chars().last().unwrap_or('0')));
                nic.mode = PortMode::Kernel;
            }
            DriverKind::VfioPci | DriverKind::UioPciGeneric => {
                nic.driver = Some(driver.module_name().to_string());
                // The kernel loses its interface for the device once a userspace
                // driver takes over.
                nic.ifname = None;
                nic.mode = PortMode::Dpdk;
            }
        }

        Ok(nic.clone())
    }

    async fn unbind(&self, pci: &PciAddr) -> Result<NicInfo, PortError> {
        self.bind(pci, DriverKind::Kernel).await
    }

    async fn hugepages_status(&self) -> Result<HugepagesStatus, PortError> {
        Ok(Self::hugepages(&self.state()))
    }

    async fn hugepages_setup(
        &self,
        count: u64,
        size: HugepageSize,
    ) -> Result<HugepagesStatus, PortError> {
        if size != HugepageSize::OneGb {
            return Err(PortError::Invalid("the mock chassis only offers 1G hugepages".into()));
        }
        let mut state = self.state();
        // Preserve how many pages are already in use across a resize, the way the
        // kernel does, instead of pretending a resize frees running engines.
        let in_use = state.hugepages_1g.saturating_sub(state.hugepages_1g_free);
        state.hugepages_1g = count;
        state.hugepages_1g_free = count.saturating_sub(in_use);
        Ok(Self::hugepages(&state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_mock_chassis_presents_four_kernel_bound_ports() {
        let mock = MockPortController::new();
        let nics = mock.list().await.unwrap();
        assert_eq!(nics.len(), 4);
        assert!(nics.iter().all(|n| n.mode == PortMode::Kernel));
        assert_eq!(nics.iter().filter(|n| n.link_state == LinkState::Up).count(), 2);
    }

    #[tokio::test]
    async fn binding_to_dpdk_takes_the_interface_away_from_the_kernel() {
        let mock = MockPortController::new();
        let pci = PciAddr::parse("0000:81:00.0").unwrap();

        let nic = mock.bind(&pci, DriverKind::VfioPci).await.unwrap();
        assert_eq!(nic.mode, PortMode::Dpdk);
        assert_eq!(nic.driver.as_deref(), Some("vfio-pci"));
        assert!(nic.ifname.is_none(), "a DPDK-bound card has no kernel interface");

        // And the change is visible to the next reader, not just the caller.
        let listed = mock.list().await.unwrap();
        assert_eq!(listed[0].mode, PortMode::Dpdk);
    }

    #[tokio::test]
    async fn unbinding_returns_the_interface_to_the_kernel() {
        let mock = MockPortController::new();
        let pci = PciAddr::parse("0000:81:00.2").unwrap();

        mock.bind(&pci, DriverKind::VfioPci).await.unwrap();
        let nic = mock.unbind(&pci).await.unwrap();

        assert_eq!(nic.mode, PortMode::Kernel);
        assert_eq!(nic.ifname.as_deref(), Some("ens1f2"));
    }

    #[tokio::test]
    async fn operating_on_an_absent_device_reports_not_found() {
        let mock = MockPortController::new();
        let pci = PciAddr::parse("0000:99:00.0").unwrap();
        assert!(matches!(mock.bind(&pci, DriverKind::VfioPci).await, Err(PortError::NotFound(_))));
    }

    #[tokio::test]
    async fn a_fresh_appliance_has_no_hugepages_until_they_are_set_up() {
        let mock = MockPortController::new();
        let before = mock.hugepages_status().await.unwrap();
        assert!(!before.sufficient);

        let after = mock.hugepages_setup(16, HugepageSize::OneGb).await.unwrap();
        assert!(after.sufficient);
        assert_eq!(after.pools[0].total, 16);
        assert_eq!(after.pools[0].free, 16);
    }
}
