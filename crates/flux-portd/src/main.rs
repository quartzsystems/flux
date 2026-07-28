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

    server::serve(socket_path, allowlist).await
}
