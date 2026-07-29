//! The live statistics WebSocket.
//!
//! A client connects, says what it wants, and receives one message a second.
//! Nothing is polled per connection: every session reads the same broadcast
//! channel the collector publishes to, and filters locally.
//!
//! ## Backfill
//!
//! The first message after a subscription carries the collector's ring buffer,
//! so a client that opens the run view three minutes in renders a full chart
//! immediately rather than drawing itself in from the right.
//!
//! ## Slow clients are dropped, not buffered
//!
//! A subscriber that cannot keep up with one message a second is not going to
//! catch up, and buffering for it would grow without bound. The broadcast
//! channel drops it; this module tells it why before closing.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use super::extract::Auth;
use crate::collector::StatsBatch;
use crate::state::AppState;

/// Mounts the stream route.
///
/// Both spellings are registered. Nesting a route at `/` matches the parent
/// path without a trailing slash only, and the UI is built with
/// `trailingSlash: true` — so a client that appends one to every path would
/// otherwise get a 404 that looks like a routing bug rather than a slash.
pub fn router() -> Router<AppState> {
    Router::new().route("/", get(upgrade)).route("/{*rest}", get(upgrade))
}

/// What a client asks for after connecting.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Subscribe {
    /// Selectors, e.g. `port:*`, `stream:<flowId>`, `run:<runId>`.
    subscribe: Vec<String>,
}

/// What the server sends that is not a statistics batch.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Control {
    /// Confirms a subscription and says how many backfill samples follow.
    Subscribed {
        /// Selectors the server understood.
        selectors: Vec<String>,
        /// How many historical samples are being sent.
        backfill: usize,
    },
    /// The connection is being closed, with a reason.
    Error {
        /// What went wrong.
        message: String,
    },
}

/// A parsed subscription set.
#[derive(Debug, Default)]
struct Filter {
    all_ports: bool,
    ports: HashSet<String>,
    all_streams: bool,
    streams: HashSet<String>,
    runs: HashSet<String>,
}

impl Filter {
    /// Parses selectors, ignoring any that are not understood.
    ///
    /// Ignoring rather than rejecting means a client built against a newer
    /// server still gets the series it asked for that this build does know.
    fn parse(selectors: &[String]) -> Self {
        let mut filter = Filter::default();

        for selector in selectors {
            match selector.split_once(':') {
                Some(("port", "*")) => filter.all_ports = true,
                Some(("port", id)) => {
                    filter.ports.insert(id.to_string());
                }
                Some(("stream", "*")) => filter.all_streams = true,
                // `stream:run:<id>` means every stream belonging to a run. The
                // collector already tags batches with their run, so this is the
                // run filter plus all streams.
                Some(("stream", rest)) => match rest.split_once(':') {
                    Some(("run", run_id)) => {
                        filter.all_streams = true;
                        filter.runs.insert(run_id.to_string());
                    }
                    _ => {
                        filter.streams.insert(rest.to_string());
                    }
                },
                Some(("run", id)) => {
                    filter.runs.insert(id.to_string());
                }
                _ => tracing::debug!(%selector, "ignoring an unrecognised stream selector"),
            }
        }

        filter
    }

    /// True when nothing was selected.
    fn is_empty(&self) -> bool {
        !self.all_ports
            && !self.all_streams
            && self.ports.is_empty()
            && self.streams.is_empty()
            && self.runs.is_empty()
    }

    /// Narrows a batch to what this client asked for.
    ///
    /// Returns `None` when nothing survives, so an idle subscriber is not woken
    /// once a second to be handed an empty object.
    fn apply(&self, batch: &StatsBatch) -> Option<StatsBatch> {
        // A run filter scopes everything: a client watching one run should not
        // receive another run's ports just because it also asked for `port:*`.
        if !self.runs.is_empty() {
            let matches = batch.run.as_ref().is_some_and(|r| self.runs.contains(&r.run_id));
            if !matches {
                return None;
            }
        }

        let ports = if self.all_ports {
            batch.ports.clone()
        } else {
            batch
                .ports
                .iter()
                .filter(|(id, _)| self.ports.contains(*id))
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        };

        let streams = if self.all_streams {
            batch.streams.clone()
        } else {
            batch
                .streams
                .iter()
                .filter(|(id, _)| self.streams.contains(*id))
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        };

        if ports.is_empty() && streams.is_empty() && batch.run.is_none() {
            return None;
        }

        Some(StatsBatch { ts: batch.ts, ports, streams, run: batch.run.clone() })
    }
}

/// Upgrades an authenticated request to a WebSocket.
///
/// Authentication happens here, before the upgrade: a WebSocket cannot carry a
/// 401 once it is open, and the cookie is available on the upgrade request like
/// any other.
async fn upgrade(
    State(state): State<AppState>,
    Auth(identity): Auth,
    upgrade: WebSocketUpgrade,
) -> Response {
    tracing::debug!(user = %identity.username, "statistics stream opening");
    upgrade.on_upgrade(move |socket| session(socket, state))
}

/// Serves one connection.
async fn session(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    // Wait for the subscription before subscribing to the broadcast channel, so
    // a client that connects and says nothing costs nothing.
    let filter = match stream.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<Subscribe>(&text) {
            Ok(request) => {
                let filter = Filter::parse(&request.subscribe);
                if filter.is_empty() {
                    send_control(
                        &mut sink,
                        Control::Error {
                            message: "no recognised selectors in the subscription".into(),
                        },
                    )
                    .await;
                    return;
                }

                let backfill = state.collector.backfill().await;
                let matching: Vec<Arc<StatsBatch>> = backfill
                    .iter()
                    .filter_map(|b| filter.apply(b).map(Arc::new))
                    .collect();

                send_control(
                    &mut sink,
                    Control::Subscribed {
                        selectors: request.subscribe.clone(),
                        backfill: matching.len(),
                    },
                )
                .await;

                for batch in matching {
                    if send_batch(&mut sink, &batch).await.is_err() {
                        return;
                    }
                }

                filter
            }
            Err(err) => {
                send_control(
                    &mut sink,
                    Control::Error { message: format!("could not read the subscription: {err}") },
                )
                .await;
                return;
            }
        },
        // A client that closes or sends binary before subscribing gets nothing.
        _ => return,
    };

    let mut samples = state.collector.subscribe();

    loop {
        tokio::select! {
            received = samples.recv() => match received {
                Ok(batch) => {
                    if let Some(filtered) = filter.apply(&batch) {
                        if send_batch(&mut sink, &filtered).await.is_err() {
                            break;
                        }
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    // Say so rather than silently skipping: a chart with an
                    // invisible gap is worse than one that reports a gap.
                    tracing::warn!(missed, "a statistics subscriber fell behind");
                    send_control(
                        &mut sink,
                        Control::Error {
                            message: format!("dropped {missed} samples; this client is too slow"),
                        },
                    )
                    .await;
                }
                Err(RecvError::Closed) => break,
            },

            // Read the client side so that a close frame is noticed promptly
            // rather than on the next failed send.
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }

    tracing::debug!("statistics stream closed");
}

/// Sends one statistics batch.
async fn send_batch<S>(sink: &mut S, batch: &StatsBatch) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let Ok(text) = serde_json::to_string(batch) else {
        // A batch that cannot be serialised is a bug, but it must not take the
        // connection down — the next one will probably be fine.
        tracing::error!("could not serialise a statistics batch");
        return Ok(());
    };

    sink.send(Message::Text(text.into())).await.map_err(|_| ())
}

/// Sends a control message, ignoring a failure to write.
async fn send_control<S>(sink: &mut S, control: Control)
where
    S: SinkExt<Message> + Unpin,
{
    if let Ok(text) = serde_json::to_string(&control) {
        let _ = sink.send(Message::Text(text.into())).await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::collector::{PortSample, RunProgress, StreamSample};

    use super::*;

    /// A batch with two ports, two flows, and optionally a run.
    fn batch(run_id: Option<&str>) -> StatsBatch {
        let mut ports = BTreeMap::new();
        ports.insert("port-a".to_string(), PortSample { tx_pps: 1.0, ..Default::default() });
        ports.insert("port-b".to_string(), PortSample { tx_pps: 2.0, ..Default::default() });

        let mut streams = BTreeMap::new();
        streams.insert("flow-a".to_string(), StreamSample { tx_pps: 3.0, ..Default::default() });
        streams.insert("flow-b".to_string(), StreamSample { tx_pps: 4.0, ..Default::default() });

        StatsBatch {
            ts: 1,
            ports,
            streams,
            run: run_id.map(|id| RunProgress {
                run_id: id.to_string(),
                state: "running".into(),
                iteration: None,
                frame_size: None,
                trial_rate_pct: None,
                trial_remaining_secs: None,
                progress: None,
                message: None,
            }),
        }
    }

    /// Parses selectors from string literals.
    fn filter(selectors: &[&str]) -> Filter {
        Filter::parse(&selectors.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_wildcard_port_selector_keeps_every_port() {
        let filtered = filter(&["port:*"]).apply(&batch(None)).unwrap();
        assert_eq!(filtered.ports.len(), 2);
        assert!(filtered.streams.is_empty(), "streams were not asked for");
    }

    #[test]
    fn a_named_port_selector_keeps_only_that_port() {
        let filtered = filter(&["port:port-a"]).apply(&batch(None)).unwrap();
        assert_eq!(filtered.ports.len(), 1);
        assert!(filtered.ports.contains_key("port-a"));
    }

    #[test]
    fn several_named_selectors_accumulate() {
        let filtered = filter(&["port:port-a", "stream:flow-b"]).apply(&batch(None)).unwrap();
        assert_eq!(filtered.ports.len(), 1);
        assert_eq!(filtered.streams.len(), 1);
        assert!(filtered.streams.contains_key("flow-b"));
    }

    #[test]
    fn a_run_selector_rejects_batches_from_other_runs() {
        let f = filter(&["run:abc"]);
        assert!(f.apply(&batch(Some("abc"))).is_some());
        assert!(f.apply(&batch(Some("xyz"))).is_none());
        assert!(f.apply(&batch(None)).is_none(), "a batch with no run cannot match one");
    }

    #[test]
    fn a_run_filter_scopes_ports_that_were_also_requested() {
        // A client watching one run should not receive another run's ports just
        // because it also said `port:*`.
        let f = filter(&["run:abc", "port:*"]);
        assert!(f.apply(&batch(Some("xyz"))).is_none());
        assert_eq!(f.apply(&batch(Some("abc"))).unwrap().ports.len(), 2);
    }

    #[test]
    fn the_run_scoped_stream_selector_means_every_stream_of_that_run() {
        let f = filter(&["stream:run:abc"]);

        let filtered = f.apply(&batch(Some("abc"))).unwrap();
        assert_eq!(filtered.streams.len(), 2);
        assert!(f.apply(&batch(Some("other"))).is_none());
    }

    #[test]
    fn an_unrecognised_selector_is_ignored_rather_than_failing_the_subscription() {
        // A client built against a newer server still gets what this build knows.
        let f = filter(&["port:*", "quantum:entangled", "nonsense"]);
        assert!(f.apply(&batch(None)).is_some());
    }

    #[test]
    fn a_subscription_with_nothing_recognisable_is_empty() {
        assert!(filter(&["nonsense"]).is_empty());
        assert!(filter(&[]).is_empty());
        assert!(!filter(&["port:*"]).is_empty());
    }

    #[test]
    fn a_batch_matching_nothing_is_not_sent_at_all() {
        // An idle subscriber should not be woken once a second for an empty
        // object.
        let f = filter(&["port:not-present"]);
        assert!(f.apply(&batch(None)).is_none());
    }

    #[test]
    fn run_progress_alone_is_enough_to_deliver_a_batch() {
        // Between trials there may be no traffic, but the countdown still has to
        // reach the run view.
        let mut b = batch(Some("abc"));
        b.ports.clear();
        b.streams.clear();

        let filtered = filter(&["run:abc"]).apply(&b).unwrap();
        assert!(filtered.run.is_some());
    }

    #[test]
    fn a_subscription_message_parses_from_the_documented_shape() {
        let request: Subscribe = serde_json::from_str(
            r#"{"subscribe":["port:*","stream:run:abc","run:abc"]}"#,
        )
        .unwrap();
        assert_eq!(request.subscribe.len(), 3);

        let f = Filter::parse(&request.subscribe);
        assert!(f.all_ports);
        assert!(f.all_streams);
        assert!(f.runs.contains("abc"));
    }
}
