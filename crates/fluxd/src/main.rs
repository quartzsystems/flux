//! `fluxd` — the Flux appliance daemon.
//!
//! One process serves the REST API, the WebSocket stream, and the static UI;
//! supervises the packet engines; and owns the test orchestrator. It runs
//! unprivileged, delegating the two things that need root — NIC binding and
//! hugepage allocation — to `flux-portd` over a unix socket.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use flux_core::port::PortController;
use tokio::sync::RwLock;

mod api;
mod auth;
mod bootstrap;
mod collector;
mod config;
mod engine;
mod orch;
mod portmgr;
mod state;
mod store;
mod tls;

use collector::Collector;
use config::{Config, PortdBackend};
use engine::EngineRegistry;
use orch::RunSupervisor;
use portmgr::{MockPortController, PortManager, UnixPortdClient};
use state::{AppState, MockControlRegistry};
use store::Store;

/// How often expired sessions and lapsed reservations are swept away.
const JANITOR_INTERVAL: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Answered before anything else is touched, so the installer can ask a
    // freshly placed binary what it is without a database, a config file, or a
    // port helper being in place yet.
    if report_version_and_exit() {
        return Ok(());
    }

    init_tracing();

    let config = Arc::new(Config::from_env().context("reading configuration")?);
    tracing::info!(
        version = api::system::VERSION,
        bind = %config.bind,
        engine = ?config.engine,
        portd = ?config.portd,
        web_root = %config.web_root.display(),
        "fluxd starting"
    );
    if config.is_fully_mocked() {
        tracing::warn!(
            "running fully mocked: no hardware is driven and all traffic statistics are simulated"
        );
    }

    let store = Store::connect(&config.database_url, config.database_max_connections).await?;
    store.migrate().await?;
    bootstrap::ensure_admin_account(&store, &config).await?;

    // A run that was in flight when the daemon stopped has no engine state left
    // to resume from. Failing it with a reason is the honest outcome; leaving it
    // `running` forever would make the dashboard lie about what the appliance is
    // doing.
    match store::runs::fail_interrupted(store.pool(), "daemon_restart").await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(count = n, "failed runs that were interrupted by a restart"),
        Err(err) => tracing::error!(%err, "could not sweep interrupted runs"),
    }

    let controller: Arc<dyn PortController> = match config.portd {
        PortdBackend::Mock => Arc::new(MockPortController::new()),
        PortdBackend::Unix => Arc::new(UnixPortdClient::new(&config.portd_socket)),
    };
    let ports = PortManager::new(controller, store.clone());

    // A refresh failure at startup is not fatal. The helper may still be coming
    // up under systemd, and an appliance that refuses to serve its own UI because
    // it cannot see a NIC gives an operator no way to diagnose that.
    if let Err(err) = ports.refresh_inventory().await {
        tracing::warn!(%err, "could not read the port inventory at startup");
    }

    // Time series are a convenience, not a dependency: a VictoriaMetrics that is
    // not up yet must not stop the appliance from running tests.
    let metrics = match collector::vm::MetricsWriter::new(&config.victoria_metrics_url) {
        Ok(writer) => Some(writer),
        Err(err) => {
            tracing::warn!(%err, "time series will not be recorded");
            None
        }
    };

    let engines = EngineRegistry::new();
    let stats = Collector::new(metrics);
    let mock_controls: MockControlRegistry = Arc::new(RwLock::new(HashMap::new()));
    let runs = RunSupervisor::new(
        store.clone(),
        engines.clone(),
        stats.clone(),
        Arc::clone(&config),
        Arc::clone(&mock_controls),
    );

    // One outbound client for the whole daemon: it holds the connection pool
    // the analytics proxy reuses.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the outbound HTTP client")?;

    let state = AppState::new(state::AppStateParts {
        store: store.clone(),
        ports,
        engines: engines.clone(),
        collector: stats.clone(),
        runs: runs.clone(),
        mock_controls,
        http,
        config: Arc::clone(&config),
    });
    tokio::spawn(janitor(store));

    let router = api::router(state);
    let tls_paths = tls::Paths::in_dir(&config.tls_dir);

    if tls_paths.present() {
        tracing::info!(
            address = %config.bind,
            certificate = %tls_paths.certificate.display(),
            "listening with TLS"
        );

        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &tls_paths.certificate,
            &tls_paths.private_key,
        )
        .await
        .context("loading the installed TLS certificate")?;

        // axum-server drives its own accept loop rather than taking a
        // TcpListener, so graceful shutdown goes through its handle.
        let handle = axum_server::Handle::new();
        let shutdown = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown.graceful_shutdown(Some(Duration::from_secs(10)));
        });

        axum_server::bind_rustls(config.bind, tls)
            .handle(handle)
            .serve(router.into_make_service())
            .await
            .context("running the HTTPS server")?;
    } else {
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("binding {}", config.bind))?;
        tracing::info!(address = %listener.local_addr()?, "listening");

        if !config.is_fully_mocked() {
            tracing::warn!(
                "serving plain HTTP; the session cookie is a bearer credential, so \
                 install a certificate under Settings before using this appliance on \
                 a shared network"
            );
        }

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("running the HTTP server")?;
    }

    // Order matters on the way out: stop the runs so they unwind and stop
    // traffic themselves, then stop collecting, then take the engines down.
    // Reversing it would leave an engine transmitting with nobody watching.
    tracing::info!("stopping active runs");
    runs.stop_all().await;
    stats.stop_all().await;
    engines.shutdown_all().await;

    tracing::info!("fluxd stopped");
    Ok(())
}

/// Prints the version if asked for one, and reports whether it did.
///
/// Deliberately hand-rolled rather than an argument parser: the daemon takes its
/// configuration from the environment and has no other flags, so a dependency
/// here would exist to parse exactly two spellings of one word.
fn report_version_and_exit() -> bool {
    let asked = std::env::args().skip(1).any(|arg| matches!(arg.as_str(), "--version" | "-V"));

    if asked {
        println!("fluxd {}", api::system::VERSION);
    }
    asked
}

/// Configures structured logging.
///
/// Defaults to JSON when not attached to a terminal, which is how the daemon runs
/// under systemd, and to human-readable output otherwise.
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("FLUX_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,fluxd=debug,tower_http=info"));

    let json = std::env::var("FLUX_LOG_FORMAT").as_deref() == Ok("json");

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry.with(tracing_subscriber::fmt::layer().json()).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer().with_target(false)).init();
    }
}

/// Periodically removes rows that have simply timed out.
///
/// Expiry is enforced in every query, so this is housekeeping rather than a
/// correctness requirement — which is why a failure logs and retries on the next
/// tick instead of taking the daemon down.
async fn janitor(store: Store) {
    let mut ticker = tokio::time::interval(JANITOR_INTERVAL);
    // The first tick fires immediately; skip it so startup is not competing with
    // migrations and the first inventory refresh for connections.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        match store::sessions::purge_expired(store.pool()).await {
            Ok(n) if n > 0 => tracing::info!(count = n, "purged expired sessions"),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, "could not purge expired sessions"),
        }

        match store::reservations::purge_expired(store.pool()).await {
            Ok(n) if n > 0 => tracing::info!(count = n, "released lapsed port reservations"),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, "could not purge lapsed reservations"),
        }
    }
}

/// Resolves when the process is asked to stop.
///
/// systemd sends `SIGTERM`; an operator at a console sends `SIGINT`. Both need to
/// drain in-flight requests rather than cutting them off.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "could not listen for SIGINT");
            // Never resolve, rather than shutting down on a listener failure.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
