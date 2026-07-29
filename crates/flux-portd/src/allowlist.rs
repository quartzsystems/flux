//! The allowlist that bounds what the privileged helper will touch.
//!
//! This is the single most security-relevant file in the product. `fluxd` runs
//! unprivileged and can ask for anything; the helper's job is to say no. The
//! policy is deliberately allow-only — there is no wildcard and no "deny" list to
//! get wrong, because a rule that is absent means "refuse".

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use flux_core::port::PciAddr;
use serde::Deserialize;

/// On-disk form of `/etc/flux/portd.yaml`.
#[derive(Debug, Deserialize)]
struct AllowlistFile {
    /// PCI addresses the helper may bind, unbind, and report on.
    ///
    /// The management NIC must never appear here. The installer writes this file
    /// with the data-plane NICs it detected, excluding the interface holding the
    /// default route.
    #[serde(default)]
    allow: Vec<String>,
}

/// The parsed, validated set of addresses this helper will act on.
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    allowed: BTreeSet<PciAddr>,
}

impl Allowlist {
    /// Reads and validates the allowlist.
    ///
    /// A missing file is a hard error rather than an empty allowlist, because
    /// silently starting with "refuse everything" would look like a hardware
    /// fault to the operator and send them debugging the wrong thing.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: AllowlistFile =
            serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

        let mut allowed = BTreeSet::new();
        for entry in &file.allow {
            let addr = PciAddr::parse(entry)
                .with_context(|| format!("invalid PCI address {entry:?} in {}", path.display()))?;
            allowed.insert(addr);
        }

        Ok(Self { allowed })
    }

    /// Builds an allowlist directly, without a file.
    ///
    /// Test-only. The running helper has exactly one way to learn what it may
    /// touch — reading the file at a path it was told — and a second constructor
    /// reachable from the binary would be a second way for that policy to be
    /// set.
    #[cfg(test)]
    pub fn from_addrs(addrs: impl IntoIterator<Item = PciAddr>) -> Self {
        Self { allowed: addrs.into_iter().collect() }
    }

    /// True when the helper is permitted to act on `pci`.
    pub fn permits(&self, pci: &PciAddr) -> bool {
        self.allowed.contains(pci)
    }

    /// Every permitted address, in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = &PciAddr> {
        self.allowed.iter()
    }

    /// How many addresses are permitted.
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    /// True when nothing is permitted.
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_listed_addresses() {
        let list = Allowlist::from_addrs([PciAddr::parse("0000:81:00.0").unwrap()]);
        assert!(list.permits(&PciAddr::parse("0000:81:00.0").unwrap()));
        assert!(!list.permits(&PciAddr::parse("0000:81:00.1").unwrap()));
    }

    #[test]
    fn short_and_long_forms_refer_to_the_same_device() {
        // The installer may write either form; normalisation happens at parse time
        // so a short-form allow entry still matches a long-form request.
        let list = Allowlist::from_addrs([PciAddr::parse("81:00.0").unwrap()]);
        assert!(list.permits(&PciAddr::parse("0000:81:00.0").unwrap()));
    }

    #[test]
    fn an_empty_allowlist_permits_nothing() {
        let list = Allowlist::default();
        assert!(!list.permits(&PciAddr::parse("0000:81:00.0").unwrap()));
        assert!(list.is_empty());
    }
}
