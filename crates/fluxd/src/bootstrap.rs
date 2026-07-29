//! First-boot setup.
//!
//! A freshly imaged appliance has no accounts, and an appliance nobody can log
//! into is a brick. This module creates exactly one administrator when the users
//! table is empty, and never touches it again.

use anyhow::Context;
use flux_core::types::Role;

use crate::auth;
use crate::config::Config;
use crate::store::{users, Store};

/// Name of the account created on first boot.
const BOOTSTRAP_USERNAME: &str = "admin";

/// Creates the first administrator if there are no accounts at all.
///
/// The password comes from `FLUX_BOOTSTRAP_ADMIN_PASSWORD` when set. Otherwise
/// one is generated and written to the log once — the installer captures it, and
/// an operator who missed it can reset the database rather than being handed a
/// well-known default password that will still be in place a year later.
pub async fn ensure_admin_account(store: &Store, config: &Config) -> anyhow::Result<()> {
    if users::count(store.pool()).await.context("counting accounts")? > 0 {
        return Ok(());
    }

    let (password, generated) = match &config.bootstrap_admin_password {
        Some(p) => (p.clone(), false),
        None => (generate_passphrase(), true),
    };

    auth::check_password_policy(&password)
        .context("FLUX_BOOTSTRAP_ADMIN_PASSWORD does not meet the password policy")?;

    let hash = auth::hash_password(&password).context("hashing the bootstrap password")?;
    let user = users::create(store.pool(), BOOTSTRAP_USERNAME, &hash, Role::Admin)
        .await
        .context("creating the bootstrap administrator")?;

    if generated {
        // Deliberately at warn so it survives a default log filter, and formatted
        // to be obvious in a wall of startup output.
        tracing::warn!(
            "──────────────────────────────────────────────────────────────\n\
             Flux created its first administrator account.\n\
             \n\
                 username: {BOOTSTRAP_USERNAME}\n\
                 password: {password}\n\
             \n\
             This is shown once. Change it after signing in.\n\
             ──────────────────────────────────────────────────────────────"
        );
    } else {
        tracing::info!(
            user_id = %user.id,
            "created the bootstrap administrator from FLUX_BOOTSTRAP_ADMIN_PASSWORD"
        );
    }

    Ok(())
}

/// Builds a readable high-entropy passphrase.
///
/// Grouped hex rather than a wordlist: it is transcribable over a KVM console,
/// carries 128 bits, and needs no dictionary shipped with the appliance.
fn generate_passphrase() -> String {
    let token = auth::generate_token();
    token
        .as_bytes()
        .chunks(6)
        .take(6)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generated_passphrase_satisfies_the_policy_it_will_be_checked_against() {
        // If these two ever disagreed, first boot would fail on a fresh appliance
        // and there would be no way in at all.
        for _ in 0..32 {
            let p = generate_passphrase();
            assert!(
                auth::check_password_policy(&p).is_ok(),
                "generated passphrase {p:?} violates the policy"
            );
        }
    }

    #[test]
    fn the_passphrase_is_grouped_for_transcription_and_unique_per_call() {
        let p = generate_passphrase();
        assert_eq!(p.split('-').count(), 6);
        assert!(p.split('-').all(|g| g.len() == 6));
        assert_ne!(p, generate_passphrase());
    }
}
