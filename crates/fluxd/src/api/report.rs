//! The printable run report.
//!
//! Rendered server-side into one self-contained HTML document: no scripts, no
//! external assets, nothing to fetch. That matters because a report is an
//! artefact people archive, email, and print months later — a page that needs
//! the appliance to still be running to render itself is not a record.
//!
//! The document prints on white with a print stylesheet, so "save as PDF" from a
//! browser produces the deliverable. The green mark holds up on white; the
//! wordmark is set in ink for print, per the brand rules.

use flux_core::rfc2544::Rfc2544Config;
use flux_core::types::Id;
use serde_json::Value;

use crate::store::models::{Run, RunResult};

/// Renders the report for a run.
pub fn render(run: &Run, results: &[RunResult], appliance_version: &str) -> String {
    let config = benchmark_config(run);

    let mut html = String::with_capacity(16 * 1024);
    html.push_str(&document_head(run));
    html.push_str(&header(run));
    html.push_str(&summary(run, config.as_ref()));
    html.push_str(&caveats(config.as_ref()));
    html.push_str(&results_section(run, results));
    html.push_str(&trials_section(results));
    html.push_str(&dut_section(run));
    html.push_str(&footer(run, appliance_version));
    html.push_str("</body></html>");
    html
}

/// The benchmark configuration this run was started with, if it had one.
///
/// Read from the run's own snapshot rather than from the test, so a report
/// stays accurate after the test has been edited.
fn benchmark_config(run: &Run) -> Option<Rfc2544Config> {
    run.config_snapshot.get("rfc2544").cloned().and_then(|v| serde_json::from_value(v).ok())
}

/// The document head, including the print stylesheet.
fn document_head(run: &Run) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Flux report — {test}</title>
<style>
  /* Screen and print share one layout; only colour and pagination differ. */
  :root {{
    --ink:       #0f1117;
    --ink-soft:  #4a4f5c;
    --rule:      #d7d9de;
    --rule-soft: #eceef1;
    --green:     #00b07a;
    --danger:    #c02b3a;
    --sans: "Manrope", ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    --mono: "JetBrains Mono", ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; padding: 32px 40px; background: #fff; color: var(--ink);
    font-family: var(--sans); font-size: 13px; line-height: 1.5;
    -webkit-print-color-adjust: exact; print-color-adjust: exact;
  }}
  header.report {{ display: flex; align-items: flex-start; justify-content: space-between;
                   gap: 24px; border-bottom: 2px solid var(--ink); padding-bottom: 16px; }}
  h1 {{ font-size: 22px; font-weight: 700; letter-spacing: -0.015em; margin: 0 0 2px; }}
  h2 {{ font-size: 14px; font-weight: 700; margin: 28px 0 10px;
        text-transform: uppercase; letter-spacing: 0.06em; color: var(--ink-soft); }}
  .subtitle {{ font-family: var(--mono); font-size: 12px; color: var(--ink-soft); margin: 0; }}
  dl.meta {{ display: grid; grid-template-columns: max-content 1fr; gap: 4px 18px; margin: 0; }}
  dl.meta dt {{ font-size: 11.5px; color: var(--ink-soft); }}
  dl.meta dd {{ margin: 0; font-family: var(--mono); font-size: 12px; }}
  table {{ width: 100%; border-collapse: collapse; margin-top: 6px; }}
  th {{ text-align: left; font-family: var(--mono); font-size: 10.5px; font-weight: 700;
        text-transform: uppercase; letter-spacing: 0.06em; color: var(--ink-soft);
        border-bottom: 1.5px solid var(--ink); padding: 6px 8px; white-space: nowrap; }}
  td {{ padding: 5px 8px; border-bottom: 1px solid var(--rule-soft); font-size: 12px; }}
  td.mono, td.num {{ font-family: var(--mono); font-variant-numeric: tabular-nums; }}
  td.num {{ text-align: right; }}
  .pass {{ color: var(--green); font-weight: 700; }}
  .fail {{ color: var(--danger); font-weight: 700; }}
  .note {{ border-left: 3px solid #e0a800; background: #fffaf0; padding: 8px 12px;
           margin: 8px 0; font-size: 12px; }}
  .note strong {{ display: block; margin-bottom: 2px; }}
  footer.report {{ margin-top: 32px; padding-top: 12px; border-top: 1px solid var(--rule);
                   display: flex; justify-content: space-between; gap: 16px;
                   font-family: var(--mono); font-size: 10.5px; color: var(--ink-soft); }}
  /* Long trial tables break across pages; a repeated header keeps every page
     readable on its own. */
  thead {{ display: table-header-group; }}
  tr {{ break-inside: avoid; }}
  section {{ break-inside: avoid-page; }}
  @page {{ margin: 14mm; }}
  @media print {{
    body {{ padding: 0; font-size: 11.5px; }}
    h2 {{ margin-top: 18px; }}
  }}
</style>
</head><body>"#,
        test = escape(&run.test_name)
    )
}

/// The report header: lockup, title, and identity.
fn header(run: &Run) -> String {
    format!(
        r#"<header class="report">
  <div>{lockup}</div>
  <div style="text-align:right">
    <h1>{test}</h1>
    <p class="subtitle">{kind} · run {id}</p>
  </div>
</header>"#,
        lockup = LOCKUP_PRINT,
        test = escape(&run.test_name),
        kind = escape(&run.test_type),
        id = run.id
    )
}

/// The Flux lockup, with the wordmark in ink for printing on white.
const LOCKUP_PRINT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 100" width="120" height="40" fill="none" role="img" aria-label="Flux">
  <g stroke="#00d992" stroke-linecap="round">
    <g stroke-width="9">
      <path d="M77.9 30.5 A34 34 0 0 1 77.9 69.5"></path>
      <path d="M77.9 30.5 A34 34 0 0 1 77.9 69.5" transform="rotate(90 50 50)"></path>
      <path d="M77.9 30.5 A34 34 0 0 1 77.9 69.5" transform="rotate(180 50 50)"></path>
      <path d="M77.9 30.5 A34 34 0 0 1 77.9 69.5" transform="rotate(270 50 50)"></path>
    </g>
    <g stroke-width="7" opacity="0.5">
      <path d="M65.4 42.8 A17 17 0 0 1 65.4 57.2" transform="rotate(45 50 50)"></path>
      <path d="M65.4 42.8 A17 17 0 0 1 65.4 57.2" transform="rotate(135 50 50)"></path>
      <path d="M65.4 42.8 A17 17 0 0 1 65.4 57.2" transform="rotate(225 50 50)"></path>
      <path d="M65.4 42.8 A17 17 0 0 1 65.4 57.2" transform="rotate(315 50 50)"></path>
    </g>
  </g>
  <text x="99" y="50" dominant-baseline="central" font-family="Manrope, system-ui, sans-serif" font-weight="800" font-size="52" letter-spacing="-1" fill="#0f1117">Flux</text>
</svg>"##;

/// Run and configuration summary.
fn summary(run: &Run, config: Option<&Rfc2544Config>) -> String {
    let duration = match &run.finished_at {
        Some(end) => format!("{:.0} s", (*end - run.started_at).as_seconds_f64()),
        None => "in progress".to_string(),
    };

    let mut rows = format!(
        r#"<dt>State</dt><dd>{state}</dd>
<dt>Started</dt><dd>{started}</dd>
<dt>Duration</dt><dd>{duration}</dd>"#,
        state = run.state,
        started = run
            .started_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        duration = duration,
    );

    if let Some(config) = config {
        rows.push_str(&format!(
            r#"
<dt>Frame sizes</dt><dd>{sizes}</dd>
<dt>Trial duration</dt><dd>{trial} s</dd>
<dt>Loss tolerance</dt><dd>{tolerance} %</dd>
<dt>Resolution</dt><dd>{resolution} % of line rate</dd>"#,
            sizes = config.frame_sizes.iter().map(u32::to_string).collect::<Vec<_>>().join(", "),
            trial = config.trial_seconds,
            tolerance = config.loss_tolerance_pct,
            resolution = config.resolution_pct,
        ));
    }

    if let Some(error) = &run.error {
        rows.push_str(&format!("\n<dt>Error</dt><dd>{}</dd>", escape(error)));
    }

    format!("<section><h2>Run</h2><dl class=\"meta\">{rows}</dl></section>")
}

/// Anything that stops this being a conformant RFC 2544 result.
///
/// Printed prominently rather than in a footnote: a report that quietly presents
/// a ten-second trial as an RFC 2544 throughput figure is misleading, and the
/// person reading it months later has no other way to know.
fn caveats(config: Option<&Rfc2544Config>) -> String {
    let Some(config) = config else { return String::new() };

    let notes = config.reportability_notes();
    if notes.is_empty() {
        return String::new();
    }

    let items: String =
        notes.iter().map(|n| format!("<li>{}</li>", escape(n))).collect::<Vec<_>>().join("");

    format!(
        r#"<div class="note">
  <strong>This is not a conformant RFC 2544 result.</strong>
  <ul style="margin:4px 0 0 16px;padding:0">{items}</ul>
</div>"#
    )
}

/// The headline per-frame-size table.
fn results_section(run: &Run, results: &[RunResult]) -> String {
    // The summary rows are the ones the search flagged as the result for their
    // frame size; the rest are the working that produced them.
    let summaries: Vec<&RunResult> = results
        .iter()
        .filter(|r| {
            r.params.get("resultRatePct").is_some() || r.params.get("resultBurstFrames").is_some()
        })
        .collect();

    if summaries.is_empty() {
        return String::new();
    }

    let is_burst = summaries.iter().any(|r| r.params.get("resultBurstFrames").is_some());

    let head = if is_burst {
        "<tr><th>Frame size</th><th>Burst (frames)</th><th>Burst (µs)</th><th>Trials</th><th>Outcome</th></tr>"
    } else {
        "<tr><th>Frame size</th><th>Throughput (%)</th><th>Throughput (pps)</th><th>Throughput (bit/s L1)</th><th>Latency p50 (µs)</th><th>Latency p99 (µs)</th><th>Trials</th><th>Outcome</th></tr>"
    };

    let rows: String = summaries
        .iter()
        .map(|r| {
            let conclusive =
                r.params.get("conclusive").and_then(Value::as_bool).unwrap_or(true);
            let reason = r
                .params
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("—");
            let trials = r.params.get("trialsRun").and_then(Value::as_u64).unwrap_or(0);
            let outcome = format!(
                "<span class=\"{}\">{}</span>",
                if conclusive { "pass" } else { "fail" },
                escape(reason)
            );

            if is_burst {
                format!(
                    "<tr><td class=\"mono\">{size}</td><td class=\"num\">{frames}</td><td class=\"num\">{micros}</td><td class=\"num\">{trials}</td><td>{outcome}</td></tr>",
                    size = r.frame_size.map(|s| s.to_string()).unwrap_or_else(|| "—".into()),
                    frames = number(r.metrics.get("resultBurstFrames")),
                    micros = decimal(r.metrics.get("resultBurstMicros"), 1),
                )
            } else {
                format!(
                    "<tr><td class=\"mono\">{size}</td><td class=\"num\">{pct}</td><td class=\"num\">{pps}</td><td class=\"num\">{bps}</td><td class=\"num\">{p50}</td><td class=\"num\">{p99}</td><td class=\"num\">{trials}</td><td>{outcome}</td></tr>",
                    size = r.frame_size.map(|s| s.to_string()).unwrap_or_else(|| "—".into()),
                    pct = decimal(r.metrics.get("resultRatePct"), 3),
                    pps = number(r.metrics.get("resultPps")),
                    bps = number(r.metrics.get("resultBpsL1")),
                    p50 = decimal(r.metrics.get("latP50"), 1),
                    p99 = decimal(r.metrics.get("latP99"), 1),
                )
            }
        })
        .collect();

    format!(
        "<section><h2>{title}</h2><table><thead>{head}</thead><tbody>{rows}</tbody></table></section>",
        title = match run.test_type.as_str() {
            "rfc2544_throughput" => "Throughput (RFC 2544 §26.1)",
            "rfc2544_latency" => "Latency (RFC 2544 §26.2)",
            "rfc2544_frameloss" => "Frame loss rate (RFC 2544 §26.3)",
            "rfc2544_b2b" => "Back-to-back frames (RFC 2544 §26.4)",
            _ => "Results",
        }
    )
}

/// Every trial, which is the working behind the headline figures.
fn trials_section(results: &[RunResult]) -> String {
    // A stateful load measures connections, not frames. Rendering it through
    // the frame table would print a row of em-dashes and call it a trial.
    if results.iter().any(|r| r.params.get("profileId").is_some()) {
        return load_section(results);
    }

    let trials: Vec<&RunResult> = results
        .iter()
        .filter(|r| {
            r.params.get("resultRatePct").is_none() && r.params.get("resultBurstFrames").is_none()
        })
        .collect();

    if trials.is_empty() {
        return String::new();
    }

    let rows: String = trials
        .iter()
        .map(|r| {
            format!(
                r#"<tr>
<td class="num">{iteration}</td>
<td class="mono">{size}</td>
<td class="num">{rate}</td>
<td class="num">{tx}</td>
<td class="num">{rx}</td>
<td class="num">{lost}</td>
<td class="num">{loss}</td>
<td><span class="{class}">{verdict}</span></td>
</tr>"#,
                iteration = r.iteration,
                size = r.frame_size.map(|s| s.to_string()).unwrap_or_else(|| "—".into()),
                rate = decimal(r.params.get("ratePct").or_else(|| r.params.get("burstFrames")), 3),
                tx = number(r.metrics.get("txPackets")),
                rx = number(r.metrics.get("rxPackets")),
                lost = number(r.metrics.get("lostPackets")),
                loss = decimal(r.metrics.get("lossPct"), 4),
                class = if r.passed { "pass" } else { "fail" },
                verdict = if r.passed { "pass" } else { "fail" },
            )
        })
        .collect();

    format!(
        r#"<section><h2>Trials</h2><table><thead>
<tr><th>#</th><th>Frame</th><th>Rate / burst</th><th>Transmitted</th><th>Received</th><th>Lost</th><th>Loss %</th><th>Result</th></tr>
</thead><tbody>{rows}</tbody></table></section>"#
    )
}

/// What a stateful load actually did, one row per profile.
///
/// Connections attempted and established rather than frames transmitted and
/// received: the question a load answers is whether the device kept up with the
/// connection rate, and how many attempts it dropped doing so.
fn load_section(results: &[RunResult]) -> String {
    let rows: String = results
        .iter()
        .map(|r| {
            format!(
                r#"<tr>
<td class="mono">{profile}</td>
<td class="num">{cps}</td>
<td class="num">{attempted}</td>
<td class="num">{established}</td>
<td class="num">{errors}</td>
<td class="num">{failure}</td>
<td class="num">{peak}</td>
<td><span class="{class}">{verdict}</span></td>
</tr>"#,
                profile =
                    escape(r.params.get("profileName").and_then(|v| v.as_str()).unwrap_or("—")),
                cps = decimal(r.params.get("targetCps"), 0),
                attempted = number(r.metrics.get("attempted")),
                established = number(r.metrics.get("established")),
                errors = number(r.metrics.get("connectErrors")),
                failure = decimal(r.metrics.get("failurePct"), 4),
                peak = number(r.metrics.get("active")),
                class = if r.passed { "pass" } else { "fail" },
                verdict = if r.passed { "pass" } else { "fail" },
            )
        })
        .collect();

    format!(
        r#"<section><h2>Load</h2><table><thead>
<tr><th>Profile</th><th>Target conn/s</th><th>Attempted</th><th>Established</th><th>Errors</th><th>Failure %</th><th>Open at end</th><th>Result</th></tr>
</thead><tbody>{rows}</tbody></table></section>"#
    )
}

/// Operator-supplied notes about the device under test.
fn dut_section(run: &Run) -> String {
    let Some(map) = run.dut_meta.as_object().filter(|m| !m.is_empty()) else {
        return String::new();
    };

    let rows: String = map
        .iter()
        .map(|(key, value)| {
            format!(
                "<dt>{}</dt><dd>{}</dd>",
                escape(key),
                escape(&match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            )
        })
        .collect();

    format!("<section><h2>Device under test</h2><dl class=\"meta\">{rows}</dl></section>")
}

/// Appliance identity.
fn footer(run: &Run, appliance_version: &str) -> String {
    format!(
        r#"<footer class="report">
  <span>Flux {version} · run {id}</span>
  <span>Generated {now}</span>
</footer>"#,
        version = escape(appliance_version),
        id = run.id,
        now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
    )
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Formats a JSON number with thousands separators.
fn number(value: Option<&Value>) -> String {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => group(n.round() as i64),
        _ => "—".into(),
    }
}

/// Formats a JSON number to a fixed number of decimal places.
fn decimal(value: Option<&Value>, places: usize) -> String {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => format!("{n:.places$}"),
        _ => "—".into(),
    }
}

/// Inserts thousands separators.
fn group(n: i64) -> String {
    let negative = n < 0;
    let digits = n.abs().to_string();

    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if negative {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Escapes text for HTML.
///
/// Every operator-supplied string in this document — test names, DUT notes,
/// error messages — passes through here. The report is emailed and archived, so
/// a name containing markup must not become markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Builds a run report from its identifier, for the handler.
pub struct ReportInput<'a> {
    /// The run.
    pub run: &'a Run,
    /// Its trials, in order.
    pub results: &'a [RunResult],
    /// The daemon version, for the footer.
    pub version: &'a str,
}

impl ReportInput<'_> {
    /// Renders the document.
    pub fn render(&self) -> String {
        render(self.run, self.results, self.version)
    }

    /// A filename a browser should offer when downloading.
    pub fn filename(&self) -> String {
        let safe: String = self
            .run
            .test_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        format!("flux-{}-{}.html", safe.trim_matches('-'), short_id(self.run.id))
    }
}

/// The first segment of a UUID, which is enough to distinguish runs by eye.
fn short_id(id: Id) -> String {
    id.to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    /// A completed throughput run.
    fn run() -> Run {
        Run {
            id: Id::nil(),
            test_id: None,
            test_name: "edge-router throughput".into(),
            test_type: "rfc2544_throughput".into(),
            state: flux_core::types::RunState::Complete,
            started_by: None,
            started_at: OffsetDateTime::UNIX_EPOCH,
            finished_at: Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(420)),
            dut_meta: json!({ "vendor": "Acme", "model": "AR-9000", "firmware": "4.2.1" }),
            config_snapshot: json!({ "rfc2544": Rfc2544Config::default() }),
            error: None,
        }
    }

    /// One trial row.
    fn trial(iteration: i32, frame_size: i32, rate: f64, loss: f64, passed: bool) -> RunResult {
        RunResult {
            id: Id::nil(),
            run_id: Id::nil(),
            iteration,
            frame_size: Some(frame_size),
            params: json!({ "frameSize": frame_size, "ratePct": rate }),
            metrics: json!({
                "txPackets": 1_000_000,
                "rxPackets": 990_000,
                "lostPackets": 10_000,
                "lossPct": loss,
            }),
            passed,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// A per-frame-size summary row.
    fn summary_row(iteration: i32, frame_size: i32, rate: f64) -> RunResult {
        RunResult {
            id: Id::nil(),
            run_id: Id::nil(),
            iteration,
            frame_size: Some(frame_size),
            params: json!({
                "frameSize": frame_size,
                "resultRatePct": rate,
                "stopReason": "converged to the configured resolution",
                "conclusive": true,
                "trialsRun": 7,
            }),
            metrics: json!({
                "resultRatePct": rate,
                "resultPps": 14_880_952.0,
                "resultBpsL1": 10_000_000_000.0,
                "latP50": 24.0,
                "latP99": 54.2,
            }),
            passed: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_report_is_one_self_contained_document() {
        // No scripts and no external references: a report is archived and read
        // long after the appliance that made it is gone.
        let html = render(&run(), &[summary_row(0, 64, 87.5)], "0.1.0");

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.ends_with("</body></html>"));
        assert!(!html.contains("<script"), "a report must not carry script");
        assert!(!html.contains("src=\"http"), "a report must not fetch anything");
        assert!(!html.contains("@import"), "a report must not fetch stylesheets");
    }

    #[test]
    fn the_headline_table_reports_throughput_in_absolute_units() {
        // A percentage of an unstated line rate is not comparable with another
        // tester's result.
        let html = render(&run(), &[summary_row(0, 64, 87.5)], "0.1.0");

        assert!(html.contains("87.500"), "the percentage");
        assert!(html.contains("14,880,952"), "the packet rate");
        assert!(html.contains("10,000,000,000"), "the bit rate");
    }

    #[test]
    fn latency_columns_appear_for_a_latency_run() {
        let html = render(&run(), &[summary_row(0, 64, 87.5)], "0.1.0");
        assert!(html.contains("24.0"), "p50");
        assert!(html.contains("54.2"), "p99");
    }

    #[test]
    fn trials_are_listed_separately_from_the_headline_result() {
        let results = vec![
            trial(0, 64, 100.0, 5.0, false),
            trial(1, 64, 50.0, 0.0, true),
            summary_row(2, 64, 87.5),
        ];
        let html = render(&run(), &results, "0.1.0");

        assert!(html.contains("<h2>Trials</h2>"), "the working is shown");
        assert!(html.contains("Throughput (RFC 2544 §26.1)"), "and the headline separately");
    }

    #[test]
    fn a_non_conformant_configuration_is_declared_prominently() {
        // Quietly presenting a ten-second trial as an RFC 2544 result would
        // mislead whoever reads the report months later.
        let mut r = run();
        r.config_snapshot = json!({
            "rfc2544": Rfc2544Config { trial_seconds: 10.0, ..Default::default() }
        });

        let html = render(&r, &[summary_row(0, 64, 87.5)], "0.1.0");
        assert!(html.contains("not a conformant RFC 2544 result"));
        assert!(html.contains("section 24"));
    }

    #[test]
    fn a_conformant_configuration_carries_no_caveat() {
        let html = render(&run(), &[summary_row(0, 64, 87.5)], "0.1.0");
        assert!(!html.contains("not a conformant"));
    }

    #[test]
    fn an_inconclusive_search_is_marked_as_such() {
        let mut row = summary_row(0, 64, 87.5);
        row.params = json!({
            "frameSize": 64,
            "resultRatePct": 87.5,
            "stopReason": "stopped at the iteration limit",
            "conclusive": false,
            "trialsRun": 20,
        });

        let html = render(&run(), &[row], "0.1.0");
        assert!(html.contains("class=\"fail\">stopped at the iteration limit"));
    }

    #[test]
    fn device_metadata_is_reproduced_verbatim() {
        let html = render(&run(), &[], "0.1.0");
        assert!(html.contains("Acme"));
        assert!(html.contains("AR-9000"));
        assert!(html.contains("4.2.1"));
    }

    #[test]
    fn operator_supplied_text_cannot_become_markup() {
        // Test names and DUT notes are free text and the report is emailed on.
        let mut r = run();
        r.test_name = "<script>alert('x')</script>".into();
        r.dut_meta = json!({ "note": "<img src=x onerror=alert(1)>" });

        let html = render(&r, &[], "0.1.0");
        assert!(!html.contains("<script>alert"), "the name was not escaped");
        assert!(!html.contains("<img src=x"), "the note was not escaped");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_footer_identifies_the_appliance_and_the_run() {
        let html = render(&run(), &[], "1.2.3");
        assert!(html.contains("Flux 1.2.3"));
        assert!(html.contains(&run().id.to_string()));
    }

    #[test]
    fn the_wordmark_prints_in_ink_and_the_mark_stays_green() {
        let html = render(&run(), &[], "0.1.0");
        assert!(html.contains(r##"fill="#0f1117">Flux"##), "the wordmark is ink for print");
        assert!(html.contains(r##"stroke="#00d992""##), "the mark keeps the brand green");
    }

    #[test]
    fn a_burst_run_reports_burst_length_rather_than_a_rate() {
        let mut r = run();
        r.test_type = "rfc2544_b2b".into();

        let row = RunResult {
            params: json!({
                "frameSize": 64,
                "resultBurstFrames": 37_400,
                "stopReason": "converged to the configured resolution",
                "conclusive": true,
                "trialsRun": 12,
            }),
            metrics: json!({ "resultBurstFrames": 37_400, "resultBurstMicros": 2513.2 }),
            ..summary_row(0, 64, 0.0)
        };

        let html = render(&r, &[row], "0.1.0");
        assert!(html.contains("Back-to-back frames"));
        assert!(html.contains("37,400"));
        assert!(html.contains("2513.2"));
    }

    #[test]
    fn a_load_run_reports_connections_rather_than_frames() {
        let mut r = run();
        r.test_type = "manual".into();
        r.config_snapshot = json!({});

        let row = RunResult {
            id: Id::nil(),
            run_id: Id::nil(),
            iteration: 0,
            frame_size: None,
            params: json!({
                "profileId": Id::nil(),
                "profileName": "http-ramp",
                "targetCps": 2000.0,
                "maxConcurrent": 20_000,
                "warmupSecs": 5.0,
            }),
            metrics: json!({
                "attempted": 35_000,
                "established": 34_900,
                "connectErrors": 100,
                "failurePct": 0.2857,
                "active": 400,
            }),
            passed: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        };

        let html = render(&r, &[row], "0.1.0");

        assert!(html.contains("<h2>Load</h2>"), "{html}");
        assert!(html.contains("http-ramp"), "{html}");
        assert!(html.contains("35,000"), "{html}");
        assert!(html.contains("0.2857"), "{html}");
        // And not through the frame table, which would render a row of dashes.
        assert!(!html.contains("<h2>Trials</h2>"), "{html}");
    }

    #[test]
    fn a_failed_run_shows_its_error() {
        let mut r = run();
        r.state = flux_core::types::RunState::Failed;
        r.error = Some("no link on ens1f0; check the cabling".into());

        let html = render(&r, &[], "0.1.0");
        assert!(html.contains("no link on ens1f0"));
    }

    #[test]
    fn a_run_with_no_results_still_renders() {
        // A run that failed during preparation has nothing to tabulate, and the
        // report is still how an operator sees why.
        let html = render(&run(), &[], "0.1.0");
        assert!(html.contains("<h2>Run</h2>"));
        assert!(!html.contains("<h2>Trials</h2>"));
    }

    #[test]
    fn missing_metrics_render_as_a_dash_rather_than_nan() {
        assert_eq!(number(None), "—");
        assert_eq!(decimal(None, 2), "—");
        assert_eq!(number(Some(&json!(f64::NAN))), "—");
        assert_eq!(decimal(Some(&json!(f64::INFINITY)), 2), "—");
    }

    #[test]
    fn numbers_are_grouped_for_reading() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(14_880_952), "14,880,952");
        assert_eq!(group(-1_234), "-1,234");
    }

    #[test]
    fn the_download_filename_is_safe_and_identifiable() {
        let r = run();
        let input = ReportInput { run: &r, results: &[], version: "0.1.0" };

        let name = input.filename();
        assert!(name.starts_with("flux-edge-router-throughput-"));
        assert!(name.ends_with(".html"));
        assert!(!name.contains(' ') && !name.contains('/') && !name.contains('\\'), "got {name}");
    }
}
