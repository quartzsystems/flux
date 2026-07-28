//! The unix-socket client for `flux-portd`.
//!
//! One request opens one connection. Port operations happen a handful of times
//! per appliance lifetime — a bind, a hugepage setup, a periodic inventory
//! refresh — so connection reuse would buy nothing and would cost the reconnect
//! and stale-socket handling that a long-lived connection needs after the helper
//! restarts.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use flux_core::port::{
    DriverKind, HugepageSize, HugepagesStatus, NicInfo, PciAddr, PortController, PortError,
    PortdOk, PortdRequest,
};

/// How long to wait for the helper to answer.
///
/// A driver rebind can genuinely take a couple of seconds — the kernel tears
/// down and re-probes the device — so this is generous. Anything past it means
/// the helper is wedged, and a request path must not hang forever on that.
#[cfg_attr(not(unix), allow(dead_code, reason = "only the unix implementation reads it"))]
const TIMEOUT: Duration = Duration::from_secs(30);

/// Talks to `flux-portd` over its control socket.
#[derive(Debug, Clone)]
pub struct UnixPortdClient {
    #[cfg_attr(not(unix), allow(dead_code, reason = "only the unix implementation reads it"))]
    socket_path: PathBuf,
}

impl UnixPortdClient {
    /// Points a client at a socket path. Nothing is connected until a call is made.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self { socket_path: socket_path.into() }
    }
}

#[cfg(unix)]
impl UnixPortdClient {
    /// Sends one request and reads one response.
    async fn call(&self, request: &PortdRequest) -> Result<PortdOk, PortError> {
        use flux_core::port::PortdResponse;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        // The address the request targets, so error codes can be turned back into
        // the address-carrying `PortError` variants.
        let target = match request {
            PortdRequest::Bind { pci, .. } | PortdRequest::Unbind { pci } => Some(pci.clone()),
            _ => None,
        };

        let exchange = async {
            let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
                PortError::Unavailable(format!("{}: {e}", self.socket_path.display()))
            })?;
            let (read_half, mut write_half) = stream.into_split();

            let mut line = serde_json::to_vec(request)
                .map_err(|e| PortError::Invalid(format!("encoding request: {e}")))?;
            line.push(b'\n');
            write_half
                .write_all(&line)
                .await
                .map_err(|e| PortError::Unavailable(format!("writing request: {e}")))?;
            write_half
                .flush()
                .await
                .map_err(|e| PortError::Unavailable(format!("flushing request: {e}")))?;

            let mut response_line = String::new();
            BufReader::new(read_half)
                .read_line(&mut response_line)
                .await
                .map_err(|e| PortError::Unavailable(format!("reading response: {e}")))?;

            if response_line.trim().is_empty() {
                return Err(PortError::Unavailable(
                    "flux-portd closed the connection without replying".into(),
                ));
            }

            let response: PortdResponse = serde_json::from_str(&response_line)
                .map_err(|e| PortError::Failed(format!("malformed response: {e}")))?;

            response.into_result(target.as_ref())
        };

        tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| PortError::Unavailable(format!("no response within {TIMEOUT:?}")))?
    }
}

#[cfg(not(unix))]
impl UnixPortdClient {
    /// There are no unix domain sockets here.
    async fn call(&self, _request: &PortdRequest) -> Result<PortdOk, PortError> {
        Err(PortError::Unavailable(
            "flux-portd requires a unix platform; set FLUX_PORTD=mock".into(),
        ))
    }
}

#[async_trait]
impl PortController for UnixPortdClient {
    async fn list(&self) -> Result<Vec<NicInfo>, PortError> {
        match self.call(&PortdRequest::List).await? {
            PortdOk::Nics { nics } => Ok(nics),
            other => Err(unexpected("list", &other)),
        }
    }

    async fn bind(&self, pci: &PciAddr, driver: DriverKind) -> Result<NicInfo, PortError> {
        match self.call(&PortdRequest::Bind { pci: pci.clone(), driver }).await? {
            PortdOk::Nic { nic } => Ok(*nic),
            other => Err(unexpected("bind", &other)),
        }
    }

    async fn unbind(&self, pci: &PciAddr) -> Result<NicInfo, PortError> {
        match self.call(&PortdRequest::Unbind { pci: pci.clone() }).await? {
            PortdOk::Nic { nic } => Ok(*nic),
            other => Err(unexpected("unbind", &other)),
        }
    }

    async fn hugepages_status(&self) -> Result<HugepagesStatus, PortError> {
        match self.call(&PortdRequest::HugepagesStatus).await? {
            PortdOk::Hugepages { hugepages } => Ok(hugepages),
            other => Err(unexpected("hugepages_status", &other)),
        }
    }

    async fn hugepages_setup(
        &self,
        count: u64,
        size: HugepageSize,
    ) -> Result<HugepagesStatus, PortError> {
        match self.call(&PortdRequest::HugepagesSetup { count, size }).await? {
            PortdOk::Hugepages { hugepages } => Ok(hugepages),
            other => Err(unexpected("hugepages_setup", &other)),
        }
    }
}

/// The helper answered with a payload that does not match the operation.
///
/// This means the two sides disagree about the protocol — a version skew between
/// `fluxd` and `flux-portd` — so it names both the operation and what came back.
fn unexpected(op: &str, got: &PortdOk) -> PortError {
    PortError::Failed(format!(
        "flux-portd answered {op} with an unexpected payload: {}",
        match got {
            PortdOk::Nics { .. } => "nics",
            PortdOk::Nic { .. } => "nic",
            PortdOk::Hugepages { .. } => "hugepages",
        }
    ))
}
