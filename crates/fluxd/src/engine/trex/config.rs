//! Generating a TRex platform configuration file.
//!
//! TRex reads its port list, core assignment, and memory settings from a YAML
//! file at startup rather than over RPC, so launching an instance means writing
//! one first. Flux generates it from the port group rather than shipping a
//! static file, because the ports, their NUMA node, and the RPC ports differ per
//! group and per appliance.

use flux_core::config::EngineInstanceConfig;
use flux_core::port::PciAddr;
use serde::Serialize;

/// A generated `/etc/trex_cfg.yaml`-style document.
///
/// TODO(trex-verify): key names throughout. These follow the TRex manual's
/// platform configuration section; `port_info`, `c`, and `platform` are the ones
/// most likely to have moved between v2 and v3.
#[derive(Debug, Clone, Serialize)]
pub struct TrexPlatformConfig {
    /// Instance index within the host. One per port group.
    pub port_limit: u8,
    /// Version of the configuration schema.
    pub version: u8,
    /// PCI addresses of the ports this instance drives, in port-number order.
    pub interfaces: Vec<String>,
    /// Worker threads per interface.
    pub c: u8,
    /// Per-port source and destination addressing.
    pub port_info: Vec<PortInfo>,
    /// CPU and NUMA placement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Memory pool sizing, in megabytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<Memory>,
}

/// Addressing for one port.
///
/// TRex needs a source and destination for the ARP it does on startup. For a
/// stateless test whose frames carry their own addresses this is largely
/// ceremonial, but the file will not load without it.
#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    /// Address this port claims.
    pub src_mac: String,
    /// Address it sends to.
    pub dest_mac: String,
}

/// CPU placement.
#[derive(Debug, Clone, Serialize)]
pub struct Platform {
    /// Core the control plane runs on.
    pub master_thread_id: u16,
    /// Core the latency measurement thread runs on.
    pub latency_thread_id: u16,
    /// Worker cores, grouped by the socket they belong to.
    pub dual_if: Vec<DualIf>,
}

/// Worker cores for one interface pair.
#[derive(Debug, Clone, Serialize)]
pub struct DualIf {
    /// NUMA socket these cores sit on.
    pub socket: u32,
    /// Core ids.
    pub threads: Vec<u16>,
}

/// Memory pool sizing.
#[derive(Debug, Clone, Serialize)]
pub struct Memory {
    /// Buffers of 2048 bytes, which is what most frame sizes land in.
    pub mbuf_2048: u32,
}

/// Builds a platform configuration for a port group.
///
/// `pci_addrs` must be in port-number order: TRex numbers its ports by their
/// position in this list, and that numbering is what the orchestrator uses to
/// address them afterwards.
pub fn build(
    pci_addrs: &[PciAddr],
    numa_node: Option<u32>,
    cfg: &EngineInstanceConfig,
) -> TrexPlatformConfig {
    let port_count = pci_addrs.len() as u8;

    let port_info = pci_addrs
        .iter()
        .enumerate()
        .map(|(i, _)| PortInfo {
            // Locally administered addresses derived from the port index, so two
            // instances on one host cannot collide.
            src_mac: format!("02:00:00:00:00:{:02x}", i * 2),
            dest_mac: format!("02:00:00:00:00:{:02x}", i * 2 + 1),
        })
        .collect();

    // TRex reserves core 0 for its control plane and one more for latency
    // measurement, so worker cores start at 2. When no explicit list is given,
    // hand it a contiguous block sized to the port count.
    let platform = if cfg.cores.is_empty() {
        let threads: Vec<u16> =
            (0..u16::from(cfg.threads_per_port) * u16::from(port_count.max(1))).map(|i| i + 2).collect();
        Some(Platform {
            master_thread_id: 0,
            latency_thread_id: 1,
            dual_if: vec![DualIf { socket: numa_node.unwrap_or(0), threads }],
        })
    } else {
        Some(Platform {
            master_thread_id: 0,
            latency_thread_id: 1,
            dual_if: vec![DualIf {
                socket: numa_node.unwrap_or(0),
                threads: cfg.cores.clone(),
            }],
        })
    };

    TrexPlatformConfig {
        port_limit: port_count,
        version: 2,
        interfaces: pci_addrs.iter().map(|a| a.as_str().to_owned()).collect(),
        c: cfg.threads_per_port,
        port_info,
        platform,
        memory: cfg.memory_mb.map(|mb| Memory {
            // Roughly 500 buffers per megabyte, which is the ratio the TRex
            // manual's sizing table implies.
            mbuf_2048: mb.saturating_mul(500),
        }),
    }
}

/// Renders the configuration as the YAML document TRex expects.
///
/// TRex wants a list containing one platform mapping, not a bare mapping.
pub fn to_yaml(config: &TrexPlatformConfig) -> Result<String, serde_yaml::Error> {
    serde_yaml::to_string(&vec![config])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ports on one card.
    fn addrs() -> Vec<PciAddr> {
        vec![
            PciAddr::parse("0000:81:00.0").unwrap(),
            PciAddr::parse("0000:81:00.1").unwrap(),
        ]
    }

    #[test]
    fn interfaces_keep_the_order_they_were_given() {
        // TRex numbers ports by position in this list, and that numbering is
        // what every later call addresses.
        let config = build(&addrs(), Some(1), &EngineInstanceConfig::default());
        assert_eq!(config.interfaces, vec!["0000:81:00.0", "0000:81:00.1"]);
        assert_eq!(config.port_limit, 2);
    }

    #[test]
    fn every_port_gets_addressing() {
        let config = build(&addrs(), None, &EngineInstanceConfig::default());
        assert_eq!(config.port_info.len(), 2);
        assert_ne!(config.port_info[0].src_mac, config.port_info[1].src_mac);
        // Locally administered: the second-least-significant bit of the first
        // octet is set, so these cannot collide with real hardware addresses.
        assert!(config.port_info.iter().all(|p| p.src_mac.starts_with("02:")));
    }

    #[test]
    fn worker_cores_avoid_the_control_and_latency_threads() {
        let config = build(&addrs(), Some(0), &EngineInstanceConfig::default());
        let platform = config.platform.unwrap();

        assert_eq!(platform.master_thread_id, 0);
        assert_eq!(platform.latency_thread_id, 1);
        assert!(
            platform.dual_if[0].threads.iter().all(|t| *t >= 2),
            "workers must not share a core with the control plane"
        );
    }

    #[test]
    fn an_explicit_core_list_is_used_verbatim() {
        let cfg = EngineInstanceConfig { cores: vec![8, 9, 10, 11], ..Default::default() };
        let config = build(&addrs(), Some(1), &cfg);
        assert_eq!(config.platform.unwrap().dual_if[0].threads, vec![8, 9, 10, 11]);
    }

    #[test]
    fn the_numa_node_follows_the_card() {
        // Workers on the wrong socket cross the interconnect for every packet,
        // which costs more than any other placement mistake.
        let config = build(&addrs(), Some(1), &EngineInstanceConfig::default());
        assert_eq!(config.platform.unwrap().dual_if[0].socket, 1);
    }

    #[test]
    fn memory_is_omitted_unless_configured() {
        let config = build(&addrs(), None, &EngineInstanceConfig::default());
        assert!(config.memory.is_none());

        let cfg = EngineInstanceConfig { memory_mb: Some(2048), ..Default::default() };
        assert!(build(&addrs(), None, &cfg).memory.is_some());
    }

    #[test]
    fn the_document_renders_as_a_yaml_list() {
        // TRex expects a list containing one mapping, not a bare mapping.
        let config = build(&addrs(), Some(1), &EngineInstanceConfig::default());
        let yaml = to_yaml(&config).unwrap();

        assert!(yaml.starts_with("- "), "expected a list, got:\n{yaml}");
        assert!(yaml.contains("0000:81:00.0"));
        assert!(yaml.contains("port_limit: 2"));
    }

    #[test]
    fn a_single_port_group_still_produces_a_usable_document() {
        let one = vec![PciAddr::parse("0000:81:00.0").unwrap()];
        let config = build(&one, None, &EngineInstanceConfig::default());
        assert_eq!(config.port_limit, 1);
        assert!(!config.platform.unwrap().dual_if[0].threads.is_empty());
    }
}
