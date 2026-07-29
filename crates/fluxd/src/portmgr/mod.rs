//! Port management: the bridge between the hardware inventory and the `ports`
//! table.
//!
//! Everything that changes a port's binding goes through here rather than
//! calling the controller directly, because the database row and the hardware
//! must not disagree. The manager also owns the safety rules that the raw helper
//! has no context for — the helper knows an address is allowlisted, but only this
//! layer knows the port is currently a member of a running port group.

use std::sync::Arc;

use flux_core::port::{
    DriverKind, HugepageSize, HugepagesStatus, NicInfo, PortController, PortError,
};
use flux_core::types::{Id, PortGroupState, PortMode};

use crate::store::models::Port;
use crate::store::{port_groups, ports, Store};

pub mod mock;
pub mod unix;

pub use mock::MockPortController;
pub use unix::UnixPortdClient;

/// Failures the port manager can produce.
#[derive(Debug, thiserror::Error)]
pub enum PortMgrError {
    /// No such port row.
    #[error("no port with id {0}")]
    NotFound(Id),

    /// The operation is legal but not right now.
    #[error("{0}")]
    Busy(String),

    /// The privileged helper refused or failed.
    #[error(transparent)]
    Port(#[from] PortError),

    /// The database refused or failed.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What an inventory refresh changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InventorySummary {
    /// Devices the helper reported.
    pub seen: usize,
    /// Rows newly flagged as absent.
    pub went_absent: u64,
}

/// Reconciles hardware against the database and performs binding changes.
#[derive(Clone)]
pub struct PortManager {
    controller: Arc<dyn PortController>,
    store: Store,
}

impl PortManager {
    /// Wires a controller to a store.
    pub fn new(controller: Arc<dyn PortController>, store: Store) -> Self {
        Self { controller, store }
    }

    /// Pulls the hardware inventory and reconciles it into `ports`.
    ///
    /// Run at startup and on demand. Devices are upserted by PCI address, and
    /// rows for devices that have stopped appearing are flagged absent rather
    /// than deleted so their names and group membership survive a card swap.
    #[tracing::instrument(skip(self), err)]
    pub async fn refresh_inventory(&self) -> Result<InventorySummary, PortMgrError> {
        let nics: Vec<NicInfo> = self.controller.list().await?;

        for nic in &nics {
            ports::upsert_from_inventory(self.store.pool(), nic).await?;
        }

        let seen_addrs: Vec<_> = nics.iter().map(|n| n.pci_addr.clone()).collect();
        let went_absent = ports::mark_absent_except(self.store.pool(), &seen_addrs).await?;

        if went_absent > 0 {
            tracing::warn!(count = went_absent, "ports are no longer present in the chassis");
        }
        tracing::info!(seen = nics.len(), went_absent, "port inventory refreshed");

        Ok(InventorySummary { seen: nics.len(), went_absent })
    }

    /// Rebinds a port between kernel and DPDK ownership.
    ///
    /// Refuses while the port belongs to a group with a live engine: pulling the
    /// device out from under a running TRex instance does not stop traffic
    /// cleanly, it makes the instance fail in a way that needs a restart.
    #[tracing::instrument(skip(self), fields(port_id = %port_id, %mode), err)]
    pub async fn set_mode(&self, port_id: Id, mode: PortMode) -> Result<Port, PortMgrError> {
        let port =
            ports::get(self.store.pool(), port_id).await?.ok_or(PortMgrError::NotFound(port_id))?;

        if port.mode == mode {
            return Ok(port);
        }

        if !port.present {
            return Err(PortMgrError::Busy(format!(
                "port {} is not present in the chassis",
                port.name
            )));
        }

        self.ensure_group_is_stopped(&port).await?;

        let driver = match mode {
            PortMode::Kernel => DriverKind::Kernel,
            PortMode::Dpdk => DriverKind::VfioPci,
        };

        let nic = self.controller.bind(&port.pci_addr, driver).await?;

        // Trust the post-bind read from the helper over the requested mode: if the
        // bind silently landed somewhere else, the table should say so.
        let updated = ports::set_binding(
            self.store.pool(),
            port_id,
            nic.mode,
            nic.driver.as_deref(),
            nic.ifname.as_deref(),
            nic.link_state,
        )
        .await?
        .ok_or(PortMgrError::NotFound(port_id))?;

        tracing::info!(port = %updated.name, pci = %updated.pci_addr, mode = %updated.mode, "port rebound");
        Ok(updated)
    }

    /// Reads the hugepage allocation.
    pub async fn hugepages_status(&self) -> Result<HugepagesStatus, PortMgrError> {
        Ok(self.controller.hugepages_status().await?)
    }

    /// Requests a hugepage allocation.
    #[tracing::instrument(skip(self), fields(%size), err)]
    pub async fn hugepages_setup(
        &self,
        count: u64,
        size: HugepageSize,
    ) -> Result<HugepagesStatus, PortMgrError> {
        Ok(self.controller.hugepages_setup(count, size).await?)
    }

    /// Rejects the operation when the port's group has a live engine.
    async fn ensure_group_is_stopped(&self, port: &Port) -> Result<(), PortMgrError> {
        let Some(group_id) = port.group_id else {
            return Ok(());
        };
        let Some(group) = port_groups::get(self.store.pool(), group_id).await? else {
            // The row vanished between reads; nothing is holding the port.
            return Ok(());
        };

        if group.state == PortGroupState::Stopped || group.state == PortGroupState::Error {
            return Ok(());
        }

        Err(PortMgrError::Busy(format!(
            "port {} belongs to group {} which is {}; stop the group first",
            port.name, group.name, group.state
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_mock_controller_satisfies_the_trait_object_the_manager_holds() {
        // The manager stores `Arc<dyn PortController>`; this pins down that the
        // mock is usable through it, which is what `FLUX_PORTD=mock` relies on.
        let controller: Arc<dyn PortController> = Arc::new(MockPortController::new());
        assert_eq!(controller.list().await.unwrap().len(), 4);
    }
}
