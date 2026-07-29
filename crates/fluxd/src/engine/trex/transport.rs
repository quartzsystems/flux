//! The ZeroMQ transport to a TRex instance.
//!
//! TRex exposes a synchronous JSON-RPC endpoint on a REQ/REP socket. Everything
//! that touches ZeroMQ is behind [`RpcTransport`], for two reasons: the RPC
//! layer above can then be tested against a fake without a running TRex, and the
//! choice of ZeroMQ crate is a single-file decision rather than one threaded
//! through the engine.
//!
//! A REQ socket is strictly alternating — send, receive, send, receive — so it
//! cannot be shared. That is the underlying reason the whole engine sits behind
//! an actor task.

use std::time::Duration;

use async_trait::async_trait;
use flux_core::engine::EngineError;

/// How long to wait for a reply before declaring the instance wedged.
///
/// TRex answers control RPCs in milliseconds. Anything approaching this means
/// the process is stuck, and a REQ socket that never receives its reply can
/// never send again — so timing out and rebuilding the socket is the only
/// recovery.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// A request/reply channel carrying JSON text.
#[async_trait]
pub trait RpcTransport: Send + Sync {
    /// Sends one request and returns the reply body.
    async fn round_trip(&mut self, request: &str) -> Result<String, EngineError>;

    /// Where this transport is pointed, for log messages.
    fn endpoint(&self) -> &str;
}

/// A REQ socket connected to a TRex RPC port.
pub struct ZmqTransport {
    endpoint: String,
    socket: Option<zeromq::ReqSocket>,
}

impl ZmqTransport {
    /// Builds a transport for `host:port`. Nothing connects until first use.
    pub fn new(host: &str, port: u16) -> Self {
        Self { endpoint: format!("tcp://{host}:{port}"), socket: None }
    }

    /// Returns the connected socket, dialling if necessary.
    async fn socket(&mut self) -> Result<&mut zeromq::ReqSocket, EngineError> {
        use zeromq::Socket;

        if self.socket.is_none() {
            let mut socket = zeromq::ReqSocket::new();
            socket.connect(&self.endpoint).await.map_err(|e| {
                EngineError::Unavailable(format!("connecting to {}: {e}", self.endpoint))
            })?;
            tracing::debug!(endpoint = %self.endpoint, "connected to the TRex RPC socket");
            self.socket = Some(socket);
        }

        Ok(self.socket.as_mut().expect("just populated"))
    }

    /// Drops the socket so the next call redials.
    ///
    /// Required after any failure mid-exchange: a REQ socket whose reply never
    /// arrived is stuck in the receive state and will refuse every later send.
    fn reset(&mut self) {
        if self.socket.take().is_some() {
            tracing::warn!(endpoint = %self.endpoint, "resetting the TRex RPC socket");
        }
    }
}

#[async_trait]
impl RpcTransport for ZmqTransport {
    async fn round_trip(&mut self, request: &str) -> Result<String, EngineError> {
        use zeromq::{SocketRecv, SocketSend};

        let endpoint = self.endpoint.clone();
        let socket = self.socket().await?;

        if let Err(err) = socket.send(request.to_owned().into()).await {
            self.reset();
            return Err(EngineError::Unavailable(format!("sending to {endpoint}: {err}")));
        }

        let reply = match tokio::time::timeout(RPC_TIMEOUT, socket.recv()).await {
            Ok(Ok(message)) => message,
            Ok(Err(err)) => {
                self.reset();
                return Err(EngineError::Unavailable(format!("reading from {endpoint}: {err}")));
            }
            Err(_) => {
                self.reset();
                return Err(EngineError::Timeout(RPC_TIMEOUT));
            }
        };

        // A ZeroMQ message is a sequence of frames; TRex sends the JSON body as
        // a single frame, so anything else means we are not talking to TRex.
        let Some(frame) = reply.get(0) else {
            self.reset();
            return Err(EngineError::Protocol("empty reply from TRex".into()));
        };

        String::from_utf8(frame.to_vec())
            .map_err(|e| EngineError::Protocol(format!("reply is not UTF-8: {e}")))
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! An in-memory transport, so the RPC layer can be tested without TRex.

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A transport that replays canned replies and records what it was sent.
    #[derive(Clone, Default)]
    pub struct FakeTransport {
        inner: Arc<Mutex<FakeState>>,
    }

    /// What the fake remembers.
    #[derive(Default)]
    struct FakeState {
        replies: VecDeque<Result<String, EngineError>>,
        sent: Vec<String>,
    }

    impl FakeTransport {
        /// A transport that will answer with `replies`, in order.
        pub fn with_replies(replies: impl IntoIterator<Item = String>) -> Self {
            let fake = Self::default();
            {
                let mut state = fake.inner.lock().expect("fresh mutex");
                state.replies = replies.into_iter().map(Ok).collect();
            }
            fake
        }

        /// Every request the transport has been handed.
        pub fn sent(&self) -> Vec<String> {
            self.inner.lock().expect("not poisoned").sent.clone()
        }
    }

    #[async_trait]
    impl RpcTransport for FakeTransport {
        async fn round_trip(&mut self, request: &str) -> Result<String, EngineError> {
            let mut state = self.inner.lock().expect("not poisoned");
            state.sent.push(request.to_owned());
            state
                .replies
                .pop_front()
                .unwrap_or_else(|| Err(EngineError::Unavailable("no canned reply".into())))
        }

        fn endpoint(&self) -> &str {
            "fake://transport"
        }
    }
}
