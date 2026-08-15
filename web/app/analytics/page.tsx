'use client';

/**
 * The analytics page: a small chart builder over recorded time series.
 *
 * Deliberately not a query language. An operator picks a metric, narrows it by
 * the labels Flux itself writes, and chooses a range; the daemon does the rest.
 * A free-form PromQL box would be a better tool for the two people who know
 * PromQL and a worse one for everybody else — and a denial-of-service primitive
 * on an appliance.
 */

import { ChartLine, RefreshCw } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
// Structural, not cosmetic — see the note in components/LiveChart.tsx.
import 'uplot/dist/uPlot.min.css';

import uPlot from 'uplot';
import { useEffect, useMemo, useRef, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { Alert, Dash, Empty, Page, PageBody, PageHeader, Skeleton, Surface } from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import type { AnalyticsResult } from '@/lib/api-types';
import { CHART_HEIGHT, SERIES_COLOURS, baseOptions } from '@/lib/chart-theme';
import { formatBitrate, formatCount, formatPps, formatTimestamp } from '@/lib/format';

/** Selectable ranges, as seconds back from now. */
const RANGES: { label: string; seconds: number }[] = [
  { label: 'Last 15 minutes', seconds: 15 * 60 },
  { label: 'Last hour', seconds: 3600 },
  { label: 'Last 6 hours', seconds: 6 * 3600 },
  { label: 'Last 24 hours', seconds: 24 * 3600 },
  { label: 'Last 7 days', seconds: 7 * 86_400 },
  { label: 'Last 30 days', seconds: 30 * 86_400 },
];

export default function AnalyticsPage() {
  return (
    <AppShell>
      <Analytics />
    </AppShell>
  );
}

function Analytics() {
  const [metric, setMetric] = useState('flux_port_tx_pps');
  const [rangeSecs, setRangeSecs] = useState(3600);
  const [port, setPort] = useState('');
  const [stream, setStream] = useState('');
  const [runId, setRunId] = useState('');

  // Pinned per query rather than read from the clock on each render, so the
  // chart does not silently shift under the cursor while it is being read.
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));

  const metrics = useQuery({
    queryKey: queryKeys.analyticsMetrics,
    queryFn: ({ signal }) => api.analytics.metrics(signal),
    staleTime: Infinity,
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const runs = useQuery({
    queryKey: [...queryKeys.runs, 'analytics'],
    queryFn: ({ signal }) => api.runs.list({ limit: 25 }, signal),
  });

  const result = useQuery({
    queryKey: ['analytics', metric, rangeSecs, port, stream, runId, now],
    queryFn: ({ signal }) =>
      api.analytics.query(
        {
          metric,
          port: port || undefined,
          stream: stream || undefined,
          runId: runId || undefined,
          start: now - rangeSecs,
          end: now,
        },
        signal,
      ),
  });

  const info = metrics.data?.find((m) => m.name === metric);
  const isPortMetric = metric.startsWith('flux_port_');

  return (
    <Page>
      <PageHeader
        title="Analytics"
        subtitle="Recorded time series"
        actions={
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={result.isFetching}
            onClick={() => setNow(Math.floor(Date.now() / 1000))}
          >
            <RefreshCw size={15} />
            Refresh
          </button>
        }
      />
      <PageBody>
        <Surface title="Query">
          <div className="field-grid">
            <label className="field">
              <span className="field-label">Metric</span>
              <select
                className="select"
                value={metric}
                onChange={(e) => {
                  setMetric(e.target.value);
                  // Labels do not carry across metric families: a port filter
                  // means nothing to a flow series.
                  setPort('');
                  setStream('');
                }}
              >
                {(metrics.data ?? []).map((m) => (
                  <option key={m.name} value={m.name}>
                    {m.label}
                  </option>
                ))}
              </select>
            </label>

            <label className="field">
              <span className="field-label">Range</span>
              <select
                className="select"
                value={rangeSecs}
                onChange={(e) => {
                  setRangeSecs(Number(e.target.value));
                  setNow(Math.floor(Date.now() / 1000));
                }}
              >
                {RANGES.map((range) => (
                  <option key={range.seconds} value={range.seconds}>
                    {range.label}
                  </option>
                ))}
              </select>
            </label>

            {isPortMetric ? (
              <label className="field">
                <span className="field-label">Port</span>
                <select className="select" value={port} onChange={(e) => setPort(e.target.value)}>
                  <option value="">All ports</option>
                  {(ports.data ?? []).map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <label className="field">
                <span className="field-label">Flow</span>
                <select
                  className="select"
                  value={stream}
                  onChange={(e) => setStream(e.target.value)}
                >
                  <option value="">All flows</option>
                  {(flows.data ?? []).map((f) => (
                    <option key={f.id} value={f.id}>
                      {f.name}
                    </option>
                  ))}
                </select>
              </label>
            )}

            <label className="field">
              <span className="field-label">Run</span>
              <select className="select" value={runId} onChange={(e) => setRunId(e.target.value)}>
                <option value="">Any run</option>
                {(runs.data?.runs ?? []).map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.testName} — {formatTimestamp(r.startedAt)}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </Surface>

        {result.error ? (
          <Alert tone="danger">
            {result.error instanceof ApiError
              ? result.error.message
              : 'The query could not be run.'}
          </Alert>
        ) : null}

        <Surface
          title={info?.label ?? 'Series'}
          actions={
            result.data ? (
              <span className="note mono">
                {result.data.series.length} series · {result.data.step}s step
              </span>
            ) : null
          }
        >
          {result.isLoading ? (
            <Skeleton height={CHART_HEIGHT} />
          ) : result.data && result.data.series.length > 0 ? (
            <HistoryChart result={result.data} />
          ) : (
            <Empty icon={<ChartLine size={28} />}>
              Nothing recorded for this query.
              <br />
              Series are written while a run is in flight, so start a test and come back.
            </Empty>
          )}
        </Surface>

        {result.data && result.data.series.length > 0 ? (
          <Surface title="Series" padded={false}>
            <div className="qz-table-wrap">
              <table className="qz-table">
                <thead>
                  <tr>
                    <th>Labels</th>
                    <th className="num">Points</th>
                    <th className="num">Latest</th>
                    <th className="num">Peak</th>
                  </tr>
                </thead>
                <tbody>
                  {result.data.series.map((series, i) => {
                    const present = series.values.filter((v): v is number => v !== null);
                    const latest = present.at(-1);
                    const peak = present.length > 0 ? Math.max(...present) : undefined;

                    return (
                      <tr key={i}>
                        <td className="mono">{describeLabels(series.labels)}</td>
                        <td className="num">{formatCount(present.length)}</td>
                        <td className="num">
                          {latest === undefined ? <Dash /> : formatValue(latest, result.data.unit)}
                        </td>
                        <td className="num">
                          {peak === undefined ? <Dash /> : formatValue(peak, result.data.unit)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </Surface>
        ) : null}
      </PageBody>
    </Page>
  );
}

/** A historical chart. Rebuilt whenever the query result changes. */
function HistoryChart({ result }: { result: AnalyticsResult }) {
  const container = useRef<HTMLDivElement>(null);

  // uPlot wants one shared time axis, but each series carries its own
  // timestamps. Merging them into one sorted axis and aligning every series
  // onto it is what lets several series share a chart.
  const data = useMemo(() => {
    const axis = [...new Set(result.series.flatMap((s) => s.timestamps))].sort((a, b) => a - b);
    const index = new Map(axis.map((t, i) => [t, i]));

    const columns = result.series.map((series) => {
      const column: (number | null)[] = new Array(axis.length).fill(null);
      series.timestamps.forEach((t, i) => {
        const at = index.get(t);
        if (at !== undefined) column[at] = series.values[i] ?? null;
      });
      return column;
    });

    return [axis, ...columns];
  }, [result]);

  useEffect(() => {
    const element = container.current;
    if (!element || data[0]?.length === 0) return;

    const chart = new uPlot(
      {
        ...baseOptions({
          width: element.clientWidth || 600,
          unit: result.unit,
          format: (v) => formatValue(v, result.unit),
          // On for historical charts: zooming into a recorded range is the
          // point of them, and there is no incoming sample to snap the scale
          // back (see BaseOptionsArgs).
          setScale: true,
        }),
        series: [
          { label: 'time' },
          ...result.series.map((series, i) => ({
            label: describeLabels(series.labels),
            stroke: SERIES_COLOURS[i % SERIES_COLOURS.length],
            width: 1.5,
            // A gap in the data is drawn as a gap, not bridged: joining across
            // a period with no measurement would invent one.
            spanGaps: false,
            value: (_self: uPlot, v: number | null) =>
              v === null ? '—' : formatValue(v, result.unit),
          })),
        ],
      },
      data as unknown as uPlot.AlignedData,
      element,
    );

    const observer = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width;
      if (width) chart.setSize({ width, height: CHART_HEIGHT });
    });
    observer.observe(element);

    return () => {
      observer.disconnect();
      chart.destroy();
    };
  }, [data, result]);

  return <div ref={container} className="live-chart" />;
}

/**
 * A readable name for a series.
 *
 * `__name__` is dropped because it is the metric, which the chart title already
 * says.
 */
function describeLabels(labels: Record<string, string>): string {
  const parts = Object.entries(labels)
    .filter(([key]) => key !== '__name__')
    .map(([key, value]) => `${key}=${shorten(value)}`);

  return parts.length > 0 ? parts.join(' ') : 'all';
}

/**
 * Shortens a UUID to its first segment.
 *
 * A legend of full identifiers is unreadable, and the first eight characters
 * are enough to tell two ports apart by eye.
 */
function shorten(value: string): string {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-/i.test(value) ? value.slice(0, 8) : value;
}

/** Formats a value for its unit. */
function formatValue(value: number, unit: string): string {
  switch (unit) {
    case 'pps':
      return formatPps(value);
    case 'bit/s':
      return formatBitrate(value);
    case 'µs':
      return `${value.toFixed(1)}`;
    default:
      return formatCount(Math.round(value));
  }
}
