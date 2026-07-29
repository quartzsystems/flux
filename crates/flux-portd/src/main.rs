//! `flux-portd` — the only part of Flux that runs as root.
//!
//! It exposes a deliberately tiny surface: a unix socket at `/run/flux/portd.sock`
//! speaking newline-delimited JSON, with five operations. Everything it will act on
//! must appear in an allowlist read from `/etc/flux/portd.yaml`, which is what
//! prevents an operator (or a compromised `fluxd`) from unbinding the management
//! NIC and taking the appliance off the network.
//!
//! The helper is Linux-only in substance — it drives `dpdk-devbind.py` and `/sys`.
//! It still compiles on other platforms so the workspace builds on a developer
//! machine; the operations simply report that they are unsupported there.

use std::path::PathBuf;

use anyhow::Context;

// On a non-unix host `server::serve` refuses to start, so nothing reaches the
// allowlist or the operation layer and every item in them reads as dead code.
// They still compile and their unit tests still run, which is the point: a
// developer on Windows or macOS can work on this crate without a Linux box.
#[cfg_attr(not(unix), allow(dead_code))]
mod allowlist;
#[cfg_attr(not(unix), allow(dead_code))]
mod ops;
mod server;

/// Where the allowlist lives unless `FLUX_PORTD_CONFIG` says otherwise.
const DEFAULT_CONFIG: &str = "/etc/flux/portd.yaml";
/// Where the control socket is created unless `FLUX_PORTD_SOCKET` says otherwise.
const DEFAULT_SOCKET: &str = "/run/flux/portd.sock";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Answered before the allowlist is read, so the installer can ask a freshly
    // placed binary what it is on a machine that has no configuration yet.
    if std::env::args().skip(1).any(|a| matches!(a.as_str(), "--version" | "-V")) {
        println!("flux-portd {}", flux_core::VERSION);
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FLUX_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path: PathBuf =
        std::env::var("FLUX_PORTD_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.into()).into();
    let socket_path: PathBuf =
        std::env::var("FLUX_PORTD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.into()).into();

    let allowlist = allowlist::Allowlist::load(&config_path)
        .with_context(|| format!("loading allowlist from {}", config_path.display()))?;

    tracing::info!(
        config = %config_path.display(),
        socket = %socket_path.display(),
        allowed = allowlist.len(),
        "flux-portd starting"
    );

    // An empty allowlist is not an error — refusing everything is a legitimate,
    // if useless, policy — but it is almost never what the operator meant, and
    // it presents as hardware that will not bind. `load` already refuses a
    // missing file for exactly this reason; an empty `allow:` produces the same
    // symptom and needs saying just as loudly.
    if allowlist.is_empty() {
        tracing::warn!(
            config = %config_path.display(),
            "the allowlist is empty: every bind and unbind will be refused. \
             List the data-plane NICs under `allow:` — but never the management NIC"
        );
    }

    server::serve(socket_path, allowlist).await
}
