//! Historical queries against VictoriaMetrics.
//!
//! `fluxd` proxies rather than letting the browser reach VictoriaMetrics
//! directly, for two reasons. VictoriaMetrics binds loopback and has no
//! authentication of its own, so exposing it would be exposing an unauthenticated
//! database; and proxying means the analytics page is subject to the same session
//! and role checks as everything else.
//!
//! The proxy is deliberately narrow: a fixed set of metric names, a bounded time
//! range, and a step floor. It is not a general PromQL endpoint, because one of
//! those on an appliance is a denial-of-service primitive.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json};
use crate::state::AppState;

/// Longest span a single query may cover.
///
/// Thirty days at the coarsest step is already tens of thousands of points; the
/// retention policy does not keep series longer than that either.
const MAX_RANGE_SECS: i64 = 30 * 24 * 3600;

/// Finest step permitted.
///
/// The collector samples at one hertz, so anything finer interpolates rather
/// than reveals.
const MIN_STEP_SECS: i64 = 1;

/// Most points a single response may carry.
///
/// A chart cannot usefully draw more than this, and the limit is what keeps a
/// thirty-day range at a one-second step from being asked for at all.
const MAX_POINTS: i64 = 5_000;

/// The metrics the analytics page may chart.
///
/// An allowlist rather than free-form PromQL: this is a query endpoint on an
/// appliance, and arbitrary expressions are how one becomes a way to pin a CPU.
pub const AVAILABLE_METRICS: &[MetricInfo] = &[
    MetricInfo { name: "flux_port_tx_pps", label: "Port transmit rate", unit: "pps" },
    MetricInfo { name: "flux_port_rx_pps", label: "Port receive rate", unit: "pps" },
    MetricInfo { name: "flux_port_tx_bps", label: "Port transmit throughput", unit: "bit/s" },
    MetricInfo { name: "flux_port_rx_bps", label: "Port receive throughput", unit: "bit/s" },
    MetricInfo { name: "flux_port_tx_errors", label: "Port transmit errors", unit: "errors" },
    MetricInfo { name: "flux_port_rx_errors", label: "Port receive errors", unit: "errors" },
    MetricInfo { name: "flux_stream_tx_pps", label: "Flow transmit rate", unit: "pps" },
    MetricInfo { name: "flux_stream_rx_pps", label: "Flow receive rate", unit: "pps" },
    MetricInfo { name: "flux_stream_loss_pps", label: "Flow loss rate", unit: "pps" },
    MetricInfo { name: "flux_stream_latency_us", label: "Flow latency", unit: "µs" },
];

/// One chartable metric.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricInfo {
    /// Series name in VictoriaMetrics.
    pub name: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Unit for the axis.
    pub unit: &'static str,
}

/// Mounts the analytics routes.
pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics)).route("/query", get(query))
}

/// The metrics available to chart.
async fn metrics(_auth: Auth) -> Json<Vec<MetricInfo>> {
    Json(AVAILABLE_METRICS.to_vec())
}

/// What to chart.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryParams {
    /// Which metric, from the allowlist.
    pub metric: String,
    /// Restrict to one port.
    #[serde(default)]
    pub port: Option<String>,
    /// Restrict to one flow.
    #[serde(default)]
    pub stream: Option<String>,
    /// Restrict to one run.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Restrict a latency series to one quantile.
    #[serde(default)]
    pub quantile: Option<String>,
    /// Range start, Unix seconds.
    pub start: i64,
    /// Range end, Unix seconds.
    pub end: i64,
    /// Sample interval in seconds.
    #[serde(default)]
    pub step: Option<i64>,
}

/// One series of points.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// The labels identifying this series.
    pub labels: std::collections::BTreeMap<String, String>,
    /// Timestamps, Unix seconds.
    pub timestamps: Vec<i64>,
    /// Values, aligned with the timestamps. `null` marks a gap.
    pub values: Vec<Option<f64>>,
}

/// A query result.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    /// The metric queried.
    pub metric: String,
    /// The unit for its axis.
    pub unit: String,
    /// Step actually used, which may be coarser than requested.
    pub step: i64,
    /// The series returned.
    pub series: Vec<Series>,
}

/// Runs a bounded range query.
#[tracing::instrument(skip(state), fields(metric = %params.metric))]
async fn query(
    State(state): State<AppState>,
    _auth: Auth,
    Query(params): Query<QueryParams>,
) -> ApiResult<Json<QueryResult>> {
    let info = AVAILABLE_METRICS
        .iter()
        .find(|m| m.name == params.metric)
        .ok_or_else(|| ApiError::field("metric", "not a metric this appliance records"))?;

    if params.end <= params.start {
        return Err(ApiError::field("end", "must be after the start"));
    }
    let span = params.end - params.start;
    if span > MAX_RANGE_SECS {
        return Err(ApiError::field(
            "start",
            format!("the range may cover at most {} days", MAX_RANGE_SECS / 86_400),
        ));
    }

    // Coarsen rather than refuse: an operator asking for a month at one-second
    // resolution wants the month, and the step is the part they do not care
    // about.
    let requested = params.step.unwrap_or(MIN_STEP_SECS).max(MIN_STEP_SECS);
    let step = requested.max((span / MAX_POINTS) + 1);

    let selector = build_selector(&params);
    let url = format!("{}/api/v1/query_range", state.config.victoria_metrics_url);

    let response = state
        .http
        .get(&url)
        .query(&[
            ("query", selector.as_str()),
            ("start", &params.start.to_string()),
            ("end", &params.end.to_string()),
            ("step", &format!("{step}s")),
        ])
        .send()
        .await
        .map_err(|e| {
            ApiError::Unavailable(format!("the time series database is unreachable: {e}"))
        })?;

    if !response.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "the time series database answered {}",
            response.status()
        )));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ApiError::Unavailable(format!("malformed reply: {e}")))?;

    Ok(Json(QueryResult {
        metric: info.name.to_string(),
        unit: info.unit.to_string(),
        step,
        series: decode(&body),
    }))
}

/// Builds the series selector from the validated parameters.
///
/// Label values are quoted and escaped, and only the labels Flux itself writes
/// are accepted — so a value cannot close the selector and append an expression.
fn build_selector(params: &QueryParams) -> String {
    let mut matchers: Vec<String> = Vec::new();

    for (label, value) in [
        ("port", &params.port),
        ("stream", &params.stream),
        ("run_id", &params.run_id),
        ("quantile", &params.quantile),
    ] {
        if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
            matchers.push(format!("{label}=\"{}\"", escape_label(value)));
        }
    }

    if matchers.is_empty() {
        params.metric.clone()
    } else {
        format!("{}{{{}}}", params.metric, matchers.join(","))
    }
}

/// Escapes a label value for a PromQL string literal.
fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "")
}

/// Reads VictoriaMetrics' range response into series.
fn decode(body: &serde_json::Value) -> Vec<Series> {
    let Some(results) = body.get("data").and_then(|d| d.get("result")).and_then(|r| r.as_array())
    else {
        return Vec::new();
    };

    results
        .iter()
        .map(|entry| {
            let labels = entry
                .get("metric")
                .and_then(|m| m.as_object())
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let mut timestamps = Vec::new();
            let mut values = Vec::new();

            if let Some(points) = entry.get("values").and_then(|v| v.as_array()) {
                for point in points {
                    let Some(pair) = point.as_array() else { continue };
                    let Some(ts) = pair.first().and_then(serde_json::Value::as_f64) else {
                        continue;
                    };

                    timestamps.push(ts as i64);
                    // Values arrive as strings, and "NaN" is how a gap is
                    // spelled — passed through as null so the chart draws a
                    // break rather than a line through zero.
                    values.push(
                        pair.get(1)
                            .and_then(serde_json::Value::as_str)
                            .and_then(|s| s.parse::<f64>().ok())
                            .filter(|v| v.is_finite()),
                    );
                }
            }

            Series { labels, timestamps, values }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Query parameters with only a metric and a range.
    fn params(metric: &str) -> QueryParams {
        QueryParams {
            metric: metric.into(),
            port: None,
            stream: None,
            run_id: None,
            quantile: None,
            start: 0,
            end: 3600,
            step: Some(1),
        }
    }

    #[test]
    fn a_bare_metric_selects_every_series() {
        assert_eq!(build_selector(&params("flux_port_tx_pps")), "flux_port_tx_pps");
    }

    #[test]
    fn labels_narrow_the_selector() {
        let mut p = params("flux_stream_latency_us");
        p.stream = Some("flow-a".into());
        p.quantile = Some("0.99".into());

        assert_eq!(
            build_selector(&p),
            r#"flux_stream_latency_us{stream="flow-a",quantile="0.99"}"#
        );
    }

    #[test]
    fn empty_labels_are_ignored_rather_than_matched_against_nothing() {
        // A form that submits blank fields should not select series whose label
        // is literally the empty string.
        let mut p = params("flux_port_tx_pps");
        p.port = Some(String::new());
        assert_eq!(build_selector(&p), "flux_port_tx_pps");
    }

    #[test]
    fn a_label_value_cannot_escape_its_quotes() {
        // This is a query endpoint on an appliance; a value that closed the
        // selector could append an arbitrary expression.
        let mut p = params("flux_port_tx_pps");
        p.port = Some(r#"a" or up{job="x"} or ""#.into());

        // Every quote in the value is escaped, so the whole thing stays one
        // string literal and cannot close the selector to append an expression.
        assert_eq!(
            build_selector(&p),
            r#"flux_port_tx_pps{port="a\" or up{job=\"x\"} or \""}"#
        );
    }

    #[test]
    fn a_newline_in_a_label_is_stripped() {
        let mut p = params("flux_port_tx_pps");
        p.port = Some("a\nb".into());
        assert!(!build_selector(&p).contains('\n'));
    }

    #[test]
    fn only_allowlisted_metrics_are_chartable() {
        assert!(AVAILABLE_METRICS.iter().any(|m| m.name == "flux_port_tx_pps"));
        assert!(!AVAILABLE_METRICS.iter().any(|m| m.name == "up"));
        assert!(!AVAILABLE_METRICS.iter().any(|m| m.name.contains('{')));
    }

    #[test]
    fn every_metric_carries_a_label_and_a_unit() {
        for metric in AVAILABLE_METRICS {
            assert!(!metric.label.is_empty(), "{} has no label", metric.name);
            assert!(!metric.unit.is_empty(), "{} has no unit", metric.name);
        }
    }

    #[test]
    fn a_range_query_response_decodes_into_series() {
        let body = json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [{
                    "metric": { "__name__": "flux_port_tx_pps", "port": "port-a" },
                    "values": [[1712345678, "1000"], [1712345679, "1010"]]
                }]
            }
        });

        let series = decode(&body);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].labels.get("port").map(String::as_str), Some("port-a"));
        assert_eq!(series[0].timestamps, vec![1712345678, 1712345679]);
        assert_eq!(series[0].values, vec![Some(1000.0), Some(1010.0)]);
    }

    #[test]
    fn a_gap_decodes_to_null_rather_than_zero() {
        // Drawing a line through zero would invent a measurement that says the
        // opposite of what a gap means.
        let body = json!({
            "data": { "result": [{
                "metric": {},
                "values": [[1, "10"], [2, "NaN"], [3, "12"]]
            }]}
        });

        assert_eq!(decode(&body)[0].values, vec![Some(10.0), None, Some(12.0)]);
    }

    #[test]
    fn an_empty_or_malformed_response_yields_no_series_rather_than_failing() {
        assert!(decode(&json!({})).is_empty());
        assert!(decode(&json!({ "data": {} })).is_empty());
        assert!(decode(&json!({ "data": { "result": [] } })).is_empty());
    }

    /// Applies the same step calculation the handler does.
    fn step_for(span: i64, requested: i64) -> i64 {
        requested.max(MIN_STEP_SECS).max((span / MAX_POINTS) + 1)
    }

    #[test]
    fn the_step_coarsens_rather_than_the_query_being_refused() {
        // An operator asking for a month wants the month; the step is the part
        // they do not care about.
        assert_eq!(step_for(3600, 1), 1, "an hour at one second fits");
        assert!(step_for(30 * 86_400, 1) > 500, "a month must coarsen considerably");
    }

    #[test]
    fn a_coarsened_query_stays_under_the_point_ceiling() {
        for span in [3600, 86_400, 7 * 86_400, 30 * 86_400] {
            let step = step_for(span, 1);
            assert!(span / step <= MAX_POINTS, "{span}s at {step}s exceeds the ceiling");
        }
    }

    #[test]
    fn a_requested_step_is_never_made_finer() {
        assert_eq!(step_for(3600, 60), 60);
        assert_eq!(step_for(3600, 0), 1, "zero is raised to the floor");
    }
}
