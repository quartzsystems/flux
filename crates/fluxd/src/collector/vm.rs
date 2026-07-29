//! Writing samples to VictoriaMetrics.
//!
//! Uses the Prometheus exposition line format via `/api/v1/import/prometheus`,
//! which VictoriaMetrics accepts directly. Remote-write would mean protobuf and
//! snappy for no benefit over a loopback connection.
//!
//! Failures here are logged and dropped. Time series are for historical charts
//! and the analytics page; losing a second of them is a gap in a graph, and
//! stalling the collector — which also feeds the live WebSocket — would be a far
//! worse outcome than that gap.

use flux_core::types::Id;

use super::StatsBatch;

/// How long to wait for VictoriaMetrics before giving up on a batch.
///
/// Short: this is a loopback POST, and the collector has another sample to
/// deliver in a second either way.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Posts samples to a VictoriaMetrics instance.
#[derive(Clone)]
pub struct MetricsWriter {
    client: reqwest::Client,
    url: String,
}

impl MetricsWriter {
    /// Builds a writer for a VictoriaMetrics base URL.
    pub fn new(base_url: &str) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(WRITE_TIMEOUT).build()?;
        Ok(Self {
            client,
            url: format!("{}/api/v1/import/prometheus", base_url.trim_end_matches('/')),
        })
    }

    /// Writes one batch, best effort.
    pub async fn write(&self, batch: &StatsBatch, run_id: Option<Id>) {
        let body = encode(batch, run_id);
        if body.is_empty() {
            return;
        }

        match self.client.post(&self.url).body(body).send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(status = %response.status(), "VictoriaMetrics rejected a batch");
            }
            Err(err) => {
                tracing::warn!(%err, "could not write to VictoriaMetrics");
            }
        }
    }
}

/// Renders a batch in the Prometheus exposition format.
///
/// Timestamps are milliseconds, which is what the import endpoint expects; the
/// batch carries seconds because that is what the WebSocket clients want.
fn encode(batch: &StatsBatch, run_id: Option<Id>) -> String {
    let ts_ms = batch.ts * 1000;
    let run_label = run_id.map(|id| format!(",run_id=\"{id}\"")).unwrap_or_default();
    let mut out = String::new();

    for (port_id, sample) in &batch.ports {
        let labels = format!("port=\"{port_id}\"{run_label}");
        line(&mut out, "flux_port_tx_pps", &labels, sample.tx_pps, ts_ms);
        line(&mut out, "flux_port_rx_pps", &labels, sample.rx_pps, ts_ms);
        line(&mut out, "flux_port_tx_bps", &labels, sample.tx_bps, ts_ms);
        line(&mut out, "flux_port_rx_bps", &labels, sample.rx_bps, ts_ms);
        line(&mut out, "flux_port_tx_errors", &labels, sample.tx_errors as f64, ts_ms);
        line(&mut out, "flux_port_rx_errors", &labels, sample.rx_errors as f64, ts_ms);
    }

    for (flow_id, sample) in &batch.streams {
        let labels = format!("stream=\"{flow_id}\"{run_label}");
        line(&mut out, "flux_stream_tx_pps", &labels, sample.tx_pps, ts_ms);
        line(&mut out, "flux_stream_rx_pps", &labels, sample.rx_pps, ts_ms);
        line(&mut out, "flux_stream_loss_pps", &labels, sample.loss_pps, ts_ms);

        // Latency is published as a quantile-labelled series so the analytics
        // page can chart percentiles the way it charts anything else.
        for (quantile, value) in [
            ("0.5", sample.latency.p50_us),
            ("0.99", sample.latency.p99_us),
            ("0.999", sample.latency.p999_us),
        ] {
            if let Some(value) = value {
                line(
                    &mut out,
                    "flux_stream_latency_us",
                    &format!("{labels},quantile=\"{quantile}\""),
                    value,
                    ts_ms,
                );
            }
        }
    }

    // Connection-level series for a stateful run. Unlike ports and flows there
    // is one set per engine instance rather than per entity, so the run is all
    // that distinguishes them — which is enough, because a group runs one load
    // at a time. Without a run there is nothing to tell two groups' series
    // apart, so nothing is written.
    if let (Some(sample), Some(run_id)) = (&batch.connections, run_id) {
        let labels = format!("run_id=\"{run_id}\"");
        line(&mut out, "flux_conn_cps", &labels, sample.cps, ts_ms);
        line(&mut out, "flux_conn_active", &labels, sample.active as f64, ts_ms);
        line(&mut out, "flux_conn_errors_per_sec", &labels, sample.errors_per_sec, ts_ms);
        line(&mut out, "flux_conn_failure_pct", &labels, sample.failure_pct, ts_ms);
        line(&mut out, "flux_conn_tx_bps", &labels, sample.tx_bps, ts_ms);
        line(&mut out, "flux_conn_rx_bps", &labels, sample.rx_bps, ts_ms);
    }

    out
}

/// Appends one sample line.
///
/// Non-finite values are skipped rather than written: an ingested NaN poisons
/// every aggregate computed over the series afterwards.
fn line(out: &mut String, metric: &str, labels: &str, value: f64, ts_ms: i64) {
    if !value.is_finite() {
        return;
    }
    out.push_str(&format!("{metric}{{{labels}}} {value} {ts_ms}\n"));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use flux_core::engine::LatencyStats;

    use super::super::{ConnectionSample, PortSample, StreamSample};
    use super::*;

    /// A batch with one port and one flow.
    fn batch() -> StatsBatch {
        let mut ports = BTreeMap::new();
        ports.insert(
            "port-a".to_string(),
            PortSample {
                tx_pps: 1000.0,
                rx_pps: 990.0,
                tx_bps: 512_000.0,
                rx_bps: 506_880.0,
                tx_errors: 2,
                ..Default::default()
            },
        );

        let mut streams = BTreeMap::new();
        streams.insert(
            "flow-a".to_string(),
            StreamSample {
                tx_pps: 1000.0,
                rx_pps: 990.0,
                loss_pps: 10.0,
                latency: LatencyStats {
                    p50_us: Some(24.0),
                    p99_us: Some(60.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        StatsBatch { ts: 1_712_345_678, ports, streams, run: None, connections: None }
    }

    #[test]
    fn samples_are_labelled_by_port_and_stream() {
        let encoded = encode(&batch(), None);
        assert!(encoded.contains(r#"flux_port_tx_pps{port="port-a"} 1000"#), "{encoded}");
        assert!(encoded.contains(r#"flux_stream_tx_pps{stream="flow-a"} 1000"#), "{encoded}");
    }

    #[test]
    fn a_run_id_is_added_as_a_label_when_there_is_one() {
        let run_id = Id::new_v4();
        let encoded = encode(&batch(), Some(run_id));

        assert!(encoded.contains(&format!(r#"run_id="{run_id}""#)), "{encoded}");
        // And every line carries it, not just the first.
        assert!(encoded.lines().all(|l| l.contains("run_id=")), "{encoded}");
    }

    #[test]
    fn timestamps_are_converted_to_milliseconds() {
        // The batch carries seconds; the import endpoint wants milliseconds.
        let encoded = encode(&batch(), None);
        assert!(encoded.contains("1712345678000"), "{encoded}");
    }

    #[test]
    fn latency_percentiles_become_quantile_labelled_series() {
        let encoded = encode(&batch(), None);
        assert!(
            encoded.contains(r#"flux_stream_latency_us{stream="flow-a",quantile="0.5"} 24"#),
            "{encoded}"
        );
        assert!(encoded.contains(r#"quantile="0.99""#), "{encoded}");
        // The absent percentile is omitted rather than written as zero.
        assert!(!encoded.contains(r#"quantile="0.999""#), "{encoded}");
    }

    #[test]
    fn a_non_finite_value_is_never_written() {
        // An ingested NaN poisons every aggregate computed over the series.
        let mut b = batch();
        b.ports.get_mut("port-a").unwrap().tx_pps = f64::NAN;
        b.ports.get_mut("port-a").unwrap().rx_pps = f64::INFINITY;

        let encoded = encode(&b, None);
        assert!(!encoded.contains("NaN"), "{encoded}");
        assert!(!encoded.contains("inf"), "{encoded}");
        assert!(!encoded.contains("flux_port_tx_pps"), "{encoded}");
        // Other series in the same batch still come through.
        assert!(encoded.contains("flux_port_tx_bps"), "{encoded}");
    }

    #[test]
    fn connection_samples_are_written_against_their_run() {
        let run_id = Id::new_v4();
        let mut b = batch();
        b.connections = Some(ConnectionSample {
            cps: 2000.0,
            active: 400,
            failure_pct: 1.5,
            ..Default::default()
        });

        let encoded = encode(&b, Some(run_id));
        assert!(
            encoded.contains(&format!(r#"flux_conn_cps{{run_id="{run_id}"}} 2000"#)),
            "{encoded}"
        );
        assert!(
            encoded.contains(&format!(r#"flux_conn_active{{run_id="{run_id}"}} 400"#)),
            "{encoded}"
        );
    }

    #[test]
    fn connection_samples_without_a_run_are_dropped_rather_than_written_unlabelled() {
        // Ports and flows identify themselves; a connection series is only ever
        // distinguished by its run, so one without a run cannot be told apart
        // from another group's.
        let mut b = batch();
        b.connections = Some(ConnectionSample { cps: 2000.0, ..Default::default() });

        assert!(!encode(&b, None).contains("flux_conn_"));
    }

    #[test]
    fn an_empty_batch_produces_nothing_to_post() {
        let empty = StatsBatch {
            ts: 1,
            ports: BTreeMap::new(),
            streams: BTreeMap::new(),
            run: None,
            connections: None,
        };
        assert!(encode(&empty, None).is_empty());
    }

    #[test]
    fn every_line_is_terminated() {
        // A missing final newline makes the import endpoint drop the last sample.
        let encoded = encode(&batch(), None);
        assert!(encoded.ends_with('\n'), "{encoded:?}");
    }

    #[test]
    fn the_import_path_is_appended_to_the_base_url_exactly_once() {
        let writer = MetricsWriter::new("http://127.0.0.1:8428").unwrap();
        assert_eq!(writer.url, "http://127.0.0.1:8428/api/v1/import/prometheus");

        // A trailing slash in configuration must not produce a doubled one.
        let trailing = MetricsWriter::new("http://127.0.0.1:8428/").unwrap();
        assert_eq!(trailing.url, "http://127.0.0.1:8428/api/v1/import/prometheus");
    }
}
