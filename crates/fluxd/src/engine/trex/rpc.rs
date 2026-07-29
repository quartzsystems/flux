//! JSON-RPC 2.0 against TRex.
//!
//! TRex speaks a batched JSON-RPC dialect: a request is either one object or an
//! array of them, and the reply mirrors that shape. Most of what this module
//! does is envelope handling; the method names and parameter spellings are
//! TRex's, and each is marked where it needs checking against a live instance.
//!
//! Every `TODO(trex-verify)` below is a field name or a semantic taken from the
//! TRex Python client and the RPC specification rather than from a running
//! server. They are deliberately confined to this file and `stream.rs`, so
//! verifying them is a read of two files rather than an audit of the daemon.

use flux_core::engine::EngineError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::transport::RpcTransport;

/// The JSON-RPC version TRex expects in every envelope.
const JSONRPC_VERSION: &str = "2.0";

/// The API version this client negotiates.
///
/// TRex refuses calls from a client whose major version differs. TODO(trex-verify):
/// confirm against `get_version` on the deployed build; 4.x is what TRex v3
/// reports for the `core` API.
const API_MAJOR: u32 = 4;
/// Minor component of the negotiated API version.
const API_MINOR: u32 = 0;

/// One outgoing call.
#[derive(Debug, Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

/// One reply. `result` and `error` are mutually exclusive per the spec.
#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// The error object TRex returns for a failed call.
#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
    /// TRex puts a human-readable explanation here for most failures.
    #[serde(default)]
    data: Option<Value>,
}

impl RpcError {
    /// The most informative message available.
    fn describe(&self) -> String {
        match &self.data {
            Some(Value::String(s)) if !s.is_empty() => format!("{} ({})", self.message, s),
            Some(Value::Null) | None => self.message.clone(),
            Some(other) => format!("{} ({other})", self.message),
        }
    }
}

/// A JSON-RPC client for one TRex instance.
pub struct RpcClient {
    transport: Box<dyn RpcTransport>,
    next_id: u64,
    /// Handle returned by `api_sync_v2`, required on most later calls.
    api_handle: Option<String>,
}

impl RpcClient {
    /// Wraps a transport.
    pub fn new(transport: Box<dyn RpcTransport>) -> Self {
        Self { transport, next_id: 1, api_handle: None }
    }

    /// Allocates the next request id.
    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Issues one call and returns its result as raw JSON.
    #[tracing::instrument(skip(self, params), fields(endpoint = %self.transport.endpoint()))]
    pub async fn call_raw(&mut self, method: &str, params: Value) -> Result<Value, EngineError> {
        let id = self.next_id();
        let request = Request { jsonrpc: JSONRPC_VERSION, id, method, params };

        let body = serde_json::to_string(&request)
            .map_err(|e| EngineError::Protocol(format!("encoding {method}: {e}")))?;

        let reply = self.transport.round_trip(&body).await?;
        let response: Response = serde_json::from_str(&reply).map_err(|e| {
            EngineError::Protocol(format!("reply to {method} is not JSON-RPC: {e}"))
        })?;

        // A mismatched id means replies and requests have desynchronised, which
        // on a strictly alternating REQ socket means every later reply is also
        // wrong. Better to fail loudly than to attribute one call's answer to
        // another.
        if let Some(returned) = &response.id {
            if returned != &json!(id) {
                return Err(EngineError::Protocol(format!(
                    "reply id {returned} does not match request id {id}"
                )));
            }
        }

        if let Some(error) = response.error {
            return Err(classify(error));
        }

        response
            .result
            .ok_or_else(|| EngineError::Protocol(format!("reply to {method} has no result")))
    }

    /// Issues several calls in one round trip.
    ///
    /// TRex accepts a JSON array of requests. Programming a hundred streams as a
    /// hundred round trips is the single slowest thing a naive client does, and
    /// on a REQ socket they cannot be pipelined.
    pub async fn call_batch(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Value>, EngineError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::with_capacity(calls.len());
        let requests: Vec<Value> = calls
            .into_iter()
            .map(|(method, params)| {
                let id = self.next_id();
                ids.push(id);
                json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "method": method, "params": params })
            })
            .collect();

        let body = serde_json::to_string(&requests)
            .map_err(|e| EngineError::Protocol(format!("encoding a batch: {e}")))?;

        let reply = self.transport.round_trip(&body).await?;
        let responses: Vec<Response> = serde_json::from_str(&reply)
            .map_err(|e| EngineError::Protocol(format!("batch reply is not JSON-RPC: {e}")))?;

        if responses.len() != ids.len() {
            return Err(EngineError::Protocol(format!(
                "sent {} calls but received {} replies",
                ids.len(),
                responses.len()
            )));
        }

        let mut results = Vec::with_capacity(responses.len());
        for response in responses {
            if let Some(error) = response.error {
                return Err(classify(error));
            }
            results.push(response.result.unwrap_or(Value::Null));
        }

        Ok(results)
    }

    /// Negotiates the API version and stores the handle later calls need.
    ///
    /// TODO(trex-verify): the parameter shape is `{"api_vers": [{"type": "core",
    /// "major": M, "minor": N}]}` and the reply carries `api_vers[0].api_h`.
    /// Older builds used `"type": "stl"` instead of `"core"`.
    pub async fn api_sync(&mut self) -> Result<String, EngineError> {
        let result = self
            .call_raw(
                "api_sync_v2",
                json!({
                    "api_vers": [{ "type": "core", "major": API_MAJOR, "minor": API_MINOR }]
                }),
            )
            .await?;

        let handle = result
            .get("api_vers")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("api_h"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                EngineError::Protocol(format!(
                    "api_sync_v2 reply has no api_vers[0].api_h: {result}"
                ))
            })?
            .to_owned();

        tracing::info!(handle = %handle, "negotiated the TRex API version");
        self.api_handle = Some(handle.clone());
        Ok(handle)
    }
}

/// Maps a TRex error object onto the engine's error taxonomy.
///
/// The distinction matters upstream: a rejected call is the operator's problem,
/// an unavailable instance is the appliance's.
///
/// TODO(trex-verify): the numeric codes. TRex uses the JSON-RPC reserved range
/// for protocol errors and its own codes above it; the ownership error in
/// particular is worth pinning down, since it is the one an operator will hit.
fn classify(error: RpcError) -> EngineError {
    let described = error.describe();
    let lowered = described.to_ascii_lowercase();

    if lowered.contains("not owned") || lowered.contains("must acquire") {
        return EngineError::Rejected(described);
    }

    match error.code {
        // Method not found. Named separately because it almost always means a
        // version mismatch rather than a malformed call.
        -32601 => {
            EngineError::Protocol(format!("TRex does not implement this method: {described}"))
        }
        // The rest of the JSON-RPC reserved range: malformed request, bad params.
        -32700..=-32600 => EngineError::Protocol(described),
        _ => EngineError::Rejected(described),
    }
}

#[cfg(test)]
mod tests {
    use super::super::transport::testing::FakeTransport;
    use super::*;

    /// A client over a transport with canned replies.
    fn client(replies: &[&str]) -> (RpcClient, FakeTransport) {
        let fake = FakeTransport::with_replies(replies.iter().map(|s| (*s).to_owned()));
        (RpcClient::new(Box::new(fake.clone())), fake)
    }

    #[tokio::test]
    async fn a_call_is_wrapped_in_a_json_rpc_envelope() {
        let (mut rpc, fake) = client(&[r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#]);
        let _: Value = rpc.call_raw("get_version", json!({})).await.unwrap();

        let sent: Value = serde_json::from_str(&fake.sent()[0]).unwrap();
        assert_eq!(sent["jsonrpc"], "2.0");
        assert_eq!(sent["method"], "get_version");
        assert_eq!(sent["id"], 1);
    }

    #[tokio::test]
    async fn request_ids_advance_so_replies_can_be_matched() {
        let (mut rpc, fake) = client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
        ]);

        let _: Value = rpc.call_raw("a", json!({})).await.unwrap();
        let _: Value = rpc.call_raw("b", json!({})).await.unwrap();

        let first: Value = serde_json::from_str(&fake.sent()[0]).unwrap();
        let second: Value = serde_json::from_str(&fake.sent()[1]).unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
    }

    #[tokio::test]
    async fn a_mismatched_reply_id_is_a_protocol_error() {
        // On a strictly alternating REQ socket, a desynchronised reply means
        // every later one is misattributed too.
        let (mut rpc, _) = client(&[r#"{"jsonrpc":"2.0","id":99,"result":{}}"#]);
        let result: Result<Value, _> = rpc.call_raw("get_version", json!({})).await;
        assert!(matches!(result, Err(EngineError::Protocol(_))), "got {result:?}");
    }

    #[tokio::test]
    async fn an_error_reply_becomes_an_engine_error_carrying_its_detail() {
        let (mut rpc, _) = client(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"bad","data":"port is busy"}}"#,
        ]);
        let result: Result<Value, _> = rpc.call_raw("start_traffic", json!({})).await;

        match result {
            Err(EngineError::Rejected(message)) => {
                assert!(message.contains("bad"), "got {message}");
                assert!(message.contains("port is busy"), "got {message}");
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_ownership_error_is_a_rejection_rather_than_a_protocol_failure() {
        let (mut rpc, _) = client(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"port is not owned by you"}}"#,
        ]);
        let result: Result<Value, _> = rpc.call_raw("add_stream", json!({})).await;
        assert!(matches!(result, Err(EngineError::Rejected(_))), "got {result:?}");
    }

    #[tokio::test]
    async fn an_unknown_method_is_a_protocol_failure() {
        let (mut rpc, _) = client(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
        ]);
        let result: Result<Value, _> = rpc.call_raw("nonexistent", json!({})).await;
        assert!(matches!(result, Err(EngineError::Protocol(_))), "got {result:?}");
    }

    #[tokio::test]
    async fn a_reply_that_is_not_json_rpc_is_reported_as_such() {
        let (mut rpc, _) = client(&["this is not json"]);
        let result: Result<Value, _> = rpc.call_raw("get_version", json!({})).await;
        assert!(matches!(result, Err(EngineError::Protocol(_))));
    }

    #[tokio::test]
    async fn a_reply_with_neither_result_nor_error_is_rejected() {
        let (mut rpc, _) = client(&[r#"{"jsonrpc":"2.0","id":1}"#]);
        let result: Result<Value, _> = rpc.call_raw("get_version", json!({})).await;
        assert!(matches!(result, Err(EngineError::Protocol(_))));
    }

    #[tokio::test]
    async fn api_sync_extracts_and_remembers_the_handle() {
        let (mut rpc, fake) = client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{"api_vers":[{"api_h":"ABCD1234","type":"core"}]}}"#,
        ]);

        assert_eq!(rpc.api_sync().await.unwrap(), "ABCD1234");
        assert_eq!(rpc.api_handle.as_deref(), Some("ABCD1234"), "the handle is remembered");

        let sent: Value = serde_json::from_str(&fake.sent()[0]).unwrap();
        assert_eq!(sent["method"], "api_sync_v2");
        assert_eq!(sent["params"]["api_vers"][0]["type"], "core");
    }

    #[tokio::test]
    async fn api_sync_reports_a_reply_missing_its_handle() {
        let (mut rpc, _) = client(&[r#"{"jsonrpc":"2.0","id":1,"result":{"api_vers":[]}}"#]);
        assert!(matches!(rpc.api_sync().await, Err(EngineError::Protocol(_))));
    }

    #[tokio::test]
    async fn a_batch_is_one_round_trip_carrying_every_call() {
        let (mut rpc, fake) = client(&[
            r#"[{"jsonrpc":"2.0","id":1,"result":1},{"jsonrpc":"2.0","id":2,"result":2}]"#,
        ]);

        let results = rpc
            .call_batch(vec![
                ("add_stream".into(), json!({ "stream_id": 1 })),
                ("add_stream".into(), json!({ "stream_id": 2 })),
            ])
            .await
            .unwrap();

        assert_eq!(results, vec![json!(1), json!(2)]);
        assert_eq!(fake.sent().len(), 1, "a batch must not become several round trips");

        let sent: Value = serde_json::from_str(&fake.sent()[0]).unwrap();
        assert!(sent.is_array());
        assert_eq!(sent.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_short_batch_reply_is_a_protocol_error() {
        let (mut rpc, _) = client(&[r#"[{"jsonrpc":"2.0","id":1,"result":1}]"#]);
        let result = rpc
            .call_batch(vec![("a".into(), json!({})), ("b".into(), json!({}))])
            .await;
        assert!(matches!(result, Err(EngineError::Protocol(_))), "got {result:?}");
    }

    #[tokio::test]
    async fn an_empty_batch_does_not_reach_the_transport() {
        let (mut rpc, fake) = client(&[]);
        assert!(rpc.call_batch(Vec::new()).await.unwrap().is_empty());
        assert!(fake.sent().is_empty());
    }

    #[tokio::test]
    async fn one_failed_call_fails_the_whole_batch() {
        let (mut rpc, _) = client(&[
            r#"[{"jsonrpc":"2.0","id":1,"result":1},{"jsonrpc":"2.0","id":2,"error":{"code":-1,"message":"nope"}}]"#,
        ]);
        let result = rpc
            .call_batch(vec![("a".into(), json!({})), ("b".into(), json!({}))])
            .await;
        assert!(result.is_err());
    }
}
