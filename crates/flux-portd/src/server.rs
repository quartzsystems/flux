//! The unix-socket server: one line of JSON in, one line of JSON out.
//!
//! There is no framing beyond the newline and no session state. Each connection
//! may issue any number of requests; each is answered in order on the same
//! connection. Keeping the protocol this dumb is what lets the helper stay small
//! enough to audit.

use std::path::PathBuf;

use crate::allowlist::Allowlist;

#[cfg(unix)]
pub use unix_impl::serve;

#[cfg(unix)]
mod unix_impl {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use anyhow::Context;
    use flux_core::port::{PortdRequest, PortdResponse};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    use super::*;
    use crate::ops::{OpResult, Ops};

    /// Socket mode: owner and group read/write, nothing for others.
    ///
    /// The unit runs as root and the socket's group is set to `flux` by the
    /// systemd unit, so `fluxd` can connect while no other account can.
    const SOCKET_MODE: u32 = 0o660;

    /// Binds the control socket and serves connections until cancelled.
    pub async fn serve(socket_path: PathBuf, allowlist: Allowlist) -> anyhow::Result<()> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        // A socket left behind by an unclean shutdown would make bind() fail with
        // EADDRINUSE even though nothing is listening.
        match std::fs::remove_file(&socket_path) {
            Ok(()) => tracing::warn!(path = %socket_path.display(), "removed stale socket"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", socket_path.display())),
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("binding {}", socket_path.display()))?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(SOCKET_MODE))
            .with_context(|| format!("setting mode on {}", socket_path.display()))?;

        let ops = Arc::new(Ops::new(allowlist));
        tracing::info!(path = %socket_path.display(), "listening");

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::error!(%err, "accept failed");
                    continue;
                }
            };

            let ops = Arc::clone(&ops);
            tokio::spawn(async move {
                if let Err(err) = handle(stream, ops).await {
                    tracing::warn!(%err, "connection ended with an error");
                }
            });
        }
    }

    /// Reads requests from one connection until the peer hangs up.
    async fn handle(stream: UnixStream, ops: Arc<Ops>) -> anyhow::Result<()> {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        while let Some(line) = lines.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let response = match serde_json::from_str::<PortdRequest>(line) {
                Ok(request) => dispatch(&ops, request).await,
                // A malformed request is answered, not fatal: the connection stays
                // usable so a client bug does not look like a helper crash.
                Err(err) => PortdResponse::Error {
                    code: flux_core::port::PortdErrorCode::Invalid,
                    message: format!("malformed request: {err}"),
                },
            };

            let mut encoded = serde_json::to_vec(&response)?;
            encoded.push(b'\n');
            write_half.write_all(&encoded).await?;
            write_half.flush().await?;
        }

        Ok(())
    }

    /// Routes one parsed request to the matching operation.
    async fn dispatch(ops: &Ops, request: PortdRequest) -> PortdResponse {
        let result: OpResult = match request {
            PortdRequest::List => ops.list().await,
            PortdRequest::Bind { pci, driver } => ops.bind(&pci, driver).await,
            PortdRequest::Unbind { pci } => ops.unbind(&pci).await,
            PortdRequest::HugepagesStatus => ops.hugepages_status().await,
            PortdRequest::HugepagesSetup { count, size } => ops.hugepages_setup(count, size).await,
        };

        match result {
            Ok(ok) => PortdResponse::Ok(ok),
            Err(err) => PortdResponse::Error { code: err.code, message: err.message },
        }
    }
}

#[cfg(not(unix))]
/// The helper cannot run without unix domain sockets.
pub async fn serve(_socket_path: PathBuf, _allowlist: Allowlist) -> anyhow::Result<()> {
    anyhow::bail!("flux-portd requires a unix platform; use FLUX_PORTD=mock for development")
}
