//! Port persistence, including reconciliation against the hardware inventory.

use flux_core::port::{NicInfo, PciAddr};
use flux_core::types::{Id, LinkState, PortMode};
use sqlx::PgPool;

use super::models::{Port, PortRowJoined};

/// Columns selected wherever a full [`Port`] is returned.
const COLUMNS: &str = "id, name, pci_addr, description, driver, ifname, mac, speed_mbps, \
                       numa_node, mode, link_state, group_id, group_index, present, \
                       created_at, updated_at";

/// The one query that backs the ports page.
///
/// Group and reservation data are left-joined rather than fetched per row, so
/// listing stays a single round trip no matter how many ports are installed.
const LIST_JOINED: &str = "
    SELECT
        p.id, p.name, p.pci_addr, p.description, p.driver, p.ifname, p.mac,
        p.speed_mbps, p.numa_node, p.mode, p.link_state, p.group_id,
        p.group_index, p.present, p.created_at, p.updated_at,
        g.name        AS group_name,
        g.engine_mode AS group_engine_mode,
        g.state       AS group_state,
        r.id          AS reservation_id,
        r.user_id     AS reservation_user_id,
        u.username    AS reservation_username,
        r.note        AS reservation_note,
        r.expires_at  AS reservation_expires_at
    FROM ports p
    LEFT JOIN port_groups  g ON g.id = p.group_id
    LEFT JOIN reservations r ON r.port_id = p.id AND r.expires_at > now()
    LEFT JOIN users        u ON u.id = r.user_id
";

/// Every port with its group and reservation, ordered for stable display.
///
/// Ordering by PCI address rather than by name keeps physically adjacent ports
/// adjacent in the table, which is what an operator matching cables expects.
pub async fn list_joined(pool: &PgPool) -> sqlx::Result<Vec<PortRowJoined>> {
    sqlx::query_as::<_, PortRowJoined>(&format!("{LIST_JOINED} ORDER BY p.pci_addr"))
        .fetch_all(pool)
        .await
}

/// One port with its group and reservation.
pub async fn get_joined(pool: &PgPool, id: Id) -> sqlx::Result<Option<PortRowJoined>> {
    sqlx::query_as::<_, PortRowJoined>(&format!("{LIST_JOINED} WHERE p.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// One port by primary key.
pub async fn get(pool: &PgPool, id: Id) -> sqlx::Result<Option<Port>> {
    sqlx::query_as::<_, Port>(&format!("SELECT {COLUMNS} FROM ports WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Several ports by primary key, in the order requested.
///
/// The caller's order is the engine's port numbering, so it has to survive the
/// round trip rather than being whatever Postgres finds convenient.
pub async fn get_many_ordered(pool: &PgPool, ids: &[Id]) -> sqlx::Result<Vec<Port>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, Port>(&format!(
        "SELECT {COLUMNS} FROM ports WHERE id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;

    let mut by_id: std::collections::HashMap<Id, Port> =
        rows.into_iter().map(|p| (p.id, p)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// Renames a port.
pub async fn rename(pool: &PgPool, id: Id, name: &str) -> sqlx::Result<Option<Port>> {
    sqlx::query_as::<_, Port>(&format!(
        "UPDATE ports SET name = $2, updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(name)
    .fetch_optional(pool)
    .await
}

/// Records the outcome of a driver rebind.
pub async fn set_binding(
    pool: &PgPool,
    id: Id,
    mode: PortMode,
    driver: Option<&str>,
    ifname: Option<&str>,
    link_state: LinkState,
) -> sqlx::Result<Option<Port>> {
    sqlx::query_as::<_, Port>(&format!(
        "UPDATE ports
         SET mode = $2, driver = $3, ifname = $4, link_state = $5, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(mode.as_str())
    .bind(driver)
    .bind(ifname)
    .bind(link_state.as_str())
    .fetch_optional(pool)
    .await
}

/// Reconciles one hardware inventory entry into the `ports` table.
///
/// Inserts on first sight and refreshes the hardware-derived columns thereafter.
/// The operator-owned columns — `name`, `group_id`, `group_index` — are never
/// touched by a refresh: a card that is rebound or briefly disappears must not
/// lose the label an operator gave it or the group it belongs to.
pub async fn upsert_from_inventory(pool: &PgPool, nic: &NicInfo) -> sqlx::Result<Port> {
    sqlx::query_as::<_, Port>(&format!(
        "INSERT INTO ports
             (name, pci_addr, description, driver, ifname, mac,
              speed_mbps, numa_node, mode, link_state, present)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, TRUE)
         ON CONFLICT (pci_addr) DO UPDATE SET
             description = EXCLUDED.description,
             driver      = EXCLUDED.driver,
             ifname      = EXCLUDED.ifname,
             -- A DPDK-bound card stops reporting its MAC and speed through the
             -- kernel; keep the last known values rather than blanking the table.
             mac         = COALESCE(EXCLUDED.mac, ports.mac),
             speed_mbps  = COALESCE(EXCLUDED.speed_mbps, ports.speed_mbps),
             numa_node   = COALESCE(EXCLUDED.numa_node, ports.numa_node),
             mode        = EXCLUDED.mode,
             link_state  = EXCLUDED.link_state,
             present     = TRUE,
             updated_at  = now()
         RETURNING {COLUMNS}"
    ))
    .bind(default_name(nic))
    .bind(nic.pci_addr.as_str())
    .bind(&nic.description)
    .bind(nic.driver.as_deref())
    .bind(nic.ifname.as_deref())
    .bind(nic.mac.as_deref())
    .bind(nic.speed_mbps)
    .bind(nic.numa_node.map(|n| n as i32))
    .bind(nic.mode.as_str())
    .bind(nic.link_state.as_str())
    .fetch_one(pool)
    .await
}

/// Flags every port whose PCI address is absent from `seen`.
///
/// Rows are kept rather than deleted so a card pulled for maintenance comes back
/// with its name, group, and reservation history intact.
pub async fn mark_absent_except(pool: &PgPool, seen: &[PciAddr]) -> sqlx::Result<u64> {
    let addrs: Vec<String> = seen.iter().map(|a| a.as_str().to_string()).collect();
    let result = sqlx::query(
        "UPDATE ports
         SET present = FALSE, link_state = 'unknown', updated_at = now()
         WHERE present = TRUE AND NOT (pci_addr = ANY($1))",
    )
    .bind(&addrs)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// The label a newly discovered port gets before an operator renames it.
///
/// Prefers the kernel interface name because that is what an operator sees in
/// `ip link` and on the cable label. A DPDK-bound card has no such name, so it
/// falls back to its PCI address with the separators flattened — still unique,
/// still recognisable.
fn default_name(nic: &NicInfo) -> String {
    match &nic.ifname {
        Some(name) if !name.is_empty() => name.clone(),
        _ => format!(
            "p{}",
            nic.pci_addr
                .as_str()
                .trim_start_matches("0000:")
                .replace([':', '.'], "-")
        ),
    }
}

/// Counts ports by link state, for the dashboard health cards.
pub async fn count_by_link_state(pool: &PgPool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT link_state, count(*) FROM ports WHERE present GROUP BY link_state",
    )
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use flux_core::port::NicInfo;
    use flux_core::types::{LinkState, PortMode};

    use super::*;

    /// Builds an inventory entry for naming tests.
    fn nic(pci: &str, ifname: Option<&str>) -> NicInfo {
        NicInfo {
            pci_addr: PciAddr::parse(pci).unwrap(),
            description: "test".into(),
            driver: None,
            ifname: ifname.map(str::to_string),
            mac: None,
            speed_mbps: None,
            mode: if ifname.is_some() { PortMode::Kernel } else { PortMode::Dpdk },
            link_state: LinkState::Unknown,
            numa_node: None,
        }
    }

    #[test]
    fn a_kernel_bound_card_is_named_after_its_interface() {
        assert_eq!(default_name(&nic("0000:81:00.0", Some("ens1f0"))), "ens1f0");
    }

    #[test]
    fn a_dpdk_bound_card_falls_back_to_a_unique_pci_derived_name() {
        assert_eq!(default_name(&nic("0000:81:00.0", None)), "p81-00-0");
        assert_eq!(default_name(&nic("0000:81:00.1", None)), "p81-00-1");
        // A non-zero PCI domain must not collapse onto the domain-zero name.
        assert_eq!(default_name(&nic("0001:81:00.0", None)), "p0001-81-00-0");
    }

    #[test]
    fn an_empty_interface_name_does_not_become_an_empty_port_name() {
        assert_eq!(default_name(&nic("0000:81:00.0", Some(""))), "p81-00-0");
    }
}
