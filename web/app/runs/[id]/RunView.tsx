'use client';

/**
 * The live run view.
 *
 * Three things happen here at once: a progress header driven by the run's own
 * transitions, charts fed straight from the statistics WebSocket, and a results
 * table that fills in as trials complete.
 *
 * The charts subscribe to the stream directly and never go through React state
 * — see `components/LiveChart`. Only the progress header re-renders, and only
 * when something about the run actually changed.
 */

import { FileText, Square } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { usePathname } from 'next/navigation';
import Link from 'next/link';
import { useEffect, useMemo, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { LiveChart, type ChartSeries } from '@/components/LiveChart';
import {
  Alert,
  Badge,
  Dash,
  EmptyRow,
  Kpi,
  Page,
  PageBody,
  PageHeader,
  Skeleton,
  Surface,
} from '@/components/ui';
import { runStateTone } from '../page';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  isTerminal,
  manualMetricsSchema,
  type RunDetail,
  type RunResult,
  type RunState,
  type StatsBatch,
} from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { useStatsStream, type StreamStatus } from '@/lib/stream';
import {
  formatBitrate,
  formatCount,
  formatDuration,
  formatPercent,
  formatPps,
  formatTimestamp,
} from '@/lib/format';

/** How often to refetch the run record while it is in flight. */
const LIVE_REFETCH_MS = 2_000;

/**
 * Extracts the run id from `/runs/<id>/`.
 *
 * Returns an empty string for a path that carries no id, which makes the query
 * fail cleanly with "not found" rather than requesting `/runs/undefined`.
 */
function runIdFrom(pathname: string): string {
  const segments = pathname.split('/').filter(Boolean);
  const index = segments.indexOf('runs');
  return index >= 0 ? (segments[index + 1] ?? '') : '';
}

export function RunView() {
  return (
    <AppShell>
      <RunViewInner />
    </AppShell>
  );
}

function RunViewInner() {
  // Read the id from the path rather than from the router: this route is
  // exported once under a placeholder segment, so `useParams` would return the
  // placeholder rather than the run being viewed.
  const pathname = usePathname();
  const runId = useMemo(() => runIdFrom(pathname), [pathname]);
  const queryClient = useQueryClient();
  const { can } = useAuth();

  const run = useQuery({
    queryKey: queryKeys.run(runId),
    queryFn: ({ signal }) => api.runs.get(runId, signal),
    refetchInterval: (query) =>
      query.state.data && !isTerminal(query.state.data.state) ? LIVE_REFETCH_MS : false,
  });

  const live = run.data ? !isTerminal(run.data.state) : false;

  // Subscribing to everything belonging to this run: the server scopes ports
  // and streams by the run filter, so this does not pick up another run's data.
  const selectors = useMemo(
    () => [`run:${runId}`, `stream:run:${runId}`, 'port:*'],
    [runId],
  );
  const stream = useStatsStream(selectors, live);

  const stop = useMutation({
    mutationFn: () => api.runs.stop(runId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.run(runId) }),
  });

  const detail = run.data;

  // The header is hoisted above the loading/error branch so the title block
  // never jumps in late; only the body below it changes shape.
  return (
    <Page>
      <PageHeader
        title={detail?.testName ?? 'Run'}
        subtitle={
          detail ? (
            <>
              {detail.type} · started {formatTimestamp(detail.startedAt)}
            </>
          ) : run.isLoading ? (
            'Loading…'
          ) : (
            'Not found'
          )
        }
        actions={
          detail ? (
            <>
              {live ? <StreamIndicator status={stream.status} /> : null}
              {detail.stoppable && can('operator') ? (
                <button
                  type="button"
                  className="btn btn-danger btn-sm"
                  disabled={stop.isPending}
                  onClick={() => stop.mutate()}
                >
                  <Square size={14} />
                  {stop.isPending ? 'Stopping…' : 'Stop run'}
                </button>
              ) : null}
              <Link href={`/runs/${runId}/report/`} className="btn btn-secondary btn-sm">
                <FileText size={14} />
                Report
              </Link>
              <Link href="/runs/" className="btn btn-secondary btn-sm">
                All runs
              </Link>
            </>
          ) : null
        }
      />

      <PageBody>
        {run.isLoading ? (
          <>
            <Skeleton height={120} />
            <Skeleton height={220} />
          </>
        ) : run.error || !detail ? (
          <>
            <Alert tone="danger">
              {run.error instanceof ApiError ? run.error.message : 'This run could not be loaded.'}
            </Alert>
            <div>
              <Link href="/runs/" className="btn btn-secondary btn-sm">
                Back to runs
              </Link>
            </div>
          </>
        ) : (
          <RunBody detail={detail} stream={stream} live={live} />
        )}
      </PageBody>
    </Page>
  );
}

/**
 * The stream's connection state: a coloured dot plus a sentence-case label.
 *
 * The dot pulses only while data is actually flowing — a pulsing "Connecting…"
 * would claim liveness the stream does not have.
 */
function StreamIndicator({ status }: { status: StreamStatus }) {
  const { colour, label, pulse } =
    status === 'open'
      ? { colour: 'var(--qz-success)', label: 'Live', pulse: true }
      : status === 'connecting'
        ? { colour: 'var(--qz-warn)', label: 'Connecting…', pulse: false }
        : { colour: 'var(--qz-ink-7)', label: 'Disconnected', pulse: false };

  return (
    <span className="row gap-6 note mono">
      <span className={pulse ? 'pulse' : 'link-dot'} style={{ background: colour }} aria-hidden />
      {label}
    </span>
  );
}

/** Everything below the header once the run record has loaded. */
function RunBody({
  detail,
  stream,
  live,
}: {
  detail: RunDetail;
  stream: ReturnType<typeof useStatsStream>;
  live: boolean;
}) {
  const duration = detail.finishedAt
    ? (new Date(detail.finishedAt).getTime() - new Date(detail.startedAt).getTime()) / 1000
    : (Date.now() - new Date(detail.startedAt).getTime()) / 1000;

  // Progress from the stream is fresher than the polled record, so it wins when
  // both are available. The stream reports states from the same lifecycle the
  // record does; the cast makes that contract explicit to the tone map.
  const state = (stream.progress?.state as RunState | undefined) ?? detail.state;

  return (
    <>
      {detail.error ? <Alert tone="danger">{detail.error}</Alert> : null}

      {stream.detail ? <Alert tone="warn">{stream.detail}</Alert> : null}

      <div className="kpi-grid">
        {/* The badge lives in the foot: the 32px tabular-nums slot is for
            numbers, and a run's state is not one. */}
        <Kpi
          label="State"
          value={<Dash />}
          foot={
            <span className="row gap-6">
              <Badge tone={runStateTone(state)}>{state}</Badge>
              {stream.progress?.message ?? detail.type}
            </span>
          }
        />
        <Kpi label="Duration" value={formatDuration(duration)} foot={live ? 'running' : 'final'} />
        <Kpi
          label="Trials"
          value={detail.results.length}
          foot={detail.results.length === 1 ? 'result recorded' : 'results recorded'}
        />
        <Kpi
          label="Frame size"
          value={stream.progress?.frameSize ?? <Dash />}
          unit={stream.progress?.frameSize ? 'B' : undefined}
          foot={
            stream.progress?.trialRatePct !== undefined
              ? `trialling ${stream.progress.trialRatePct.toFixed(1)}%`
              : 'set per trial'
          }
        />
      </div>

      {stream.progress?.progress !== undefined ? (
        <Surface title="Progress">
          <div className="stack gap-8">
            <div className="progress-track">
              <div
                className="progress-fill"
                style={{ width: `${Math.round(stream.progress.progress * 100)}%` }}
              />
            </div>
            <div className="row between">
              <span className="note mono">{Math.round(stream.progress.progress * 100)}%</span>
              {stream.progress.trialRemainingSecs !== undefined ? (
                <span className="note mono">
                  {formatDuration(stream.progress.trialRemainingSecs)} left in this trial
                </span>
              ) : null}
            </div>
          </div>
        </Surface>
      ) : null}

      {live ? <LiveCharts stream={stream} /> : null}

      <ResultsTable results={detail.results} />

      <SnapshotPanel detail={detail} />
    </>
  );
}

/**
 * True once a batch has carried connection-level statistics.
 *
 * Sticky: a stateful run is stateful for its whole life, and a single sample
 * arriving without connections — which happens between the engine starting and
 * the first load being programmed — must not flip the charts back.
 *
 * A boolean is the only thing this lifts into React state; the numbers behind
 * it stay in the charts' own arrays, as everywhere else.
 */
function useHasConnections(stream: ReturnType<typeof useStatsStream>): boolean {
  const [seen, setSeen] = useState(false);

  useEffect(
    () =>
      stream.subscribe((batch) => {
        if (batch.connections) setSeen(true);
      }),
    [stream],
  );

  return seen;
}

/**
 * One chart card: a titled surface around a stream-fed chart.
 *
 * Both chart grids build from this, so every panel carries the same head, the
 * same padding, and a series list declared the same way.
 */
function ChartPanel({
  stream,
  title,
  series,
  unit,
  format,
}: {
  stream: ReturnType<typeof useStatsStream>;
  title: string;
  series: ChartSeries[];
  unit: string;
  format: (value: number) => string;
}) {
  return (
    <Surface title={title}>
      <LiveChart stream={stream} series={series} unit={unit} format={format} />
    </Surface>
  );
}

// Series are derived from the batch each second rather than from a fixed list
// of ids, because a run's flows are known only once data starts arriving. They
// are module constants so a re-render never hands the chart a new identity.

const RATE_SERIES: ChartSeries[] = [
  { label: 'tx', value: (b) => sumStreams(b, (s) => s.txPps) },
  { label: 'rx', value: (b) => sumStreams(b, (s) => s.rxPps) },
];

const LOSS_SERIES: ChartSeries[] = [
  { label: 'loss', value: (b) => sumStreams(b, (s) => s.lossPps) },
];

const THROUGHPUT_SERIES: ChartSeries[] = [
  { label: 'tx', value: (b) => sumPorts(b, (p) => p.txBps) },
  { label: 'rx', value: (b) => sumPorts(b, (p) => p.rxBps) },
];

const LATENCY_SERIES: ChartSeries[] = [
  { label: 'p50', value: (b) => firstLatency(b, 'p50Us') },
  { label: 'p99', value: (b) => firstLatency(b, 'p99Us') },
  { label: 'max', value: (b) => firstLatency(b, 'maxUs') },
];

/** Reads one connection-level field, or a gap while none are reported. */
const conn =
  (pick: (c: NonNullable<StatsBatch['connections']>) => number) =>
  (batch: StatsBatch) =>
    batch.connections ? pick(batch.connections) : null;

const CONNECTION_RATE_SERIES: ChartSeries[] = [
  { label: 'established', value: conn((c) => c.cps) },
  { label: 'errors', value: conn((c) => c.errorsPerSec) },
];

const OPEN_CONNECTIONS_SERIES: ChartSeries[] = [
  { label: 'active', value: conn((c) => c.active) },
];

const APP_THROUGHPUT_SERIES: ChartSeries[] = [
  { label: 'tx', value: conn((c) => c.txBps) },
  { label: 'rx', value: conn((c) => c.rxBps) },
];

const FAILURE_SERIES: ChartSeries[] = [
  { label: 'failed', value: conn((c) => c.failurePct) },
];

/** The charts shown while a run is in flight. */
function LiveCharts({ stream }: { stream: ReturnType<typeof useStatsStream> }) {
  const stateful = useHasConnections(stream);

  if (stateful) {
    return <ConnectionCharts stream={stream} />;
  }

  return (
    <div className="chart-grid">
      <ChartPanel stream={stream} title="Packet rate" series={RATE_SERIES} unit="pps" format={formatPps} />
      <ChartPanel stream={stream} title="Loss" series={LOSS_SERIES} unit="pps" format={formatPps} />
      <ChartPanel
        stream={stream}
        title="Throughput"
        series={THROUGHPUT_SERIES}
        unit="bits/s"
        format={formatBitrate}
      />
      <ChartPanel
        stream={stream}
        title="Latency"
        series={LATENCY_SERIES}
        unit="µs"
        format={(v) => `${v.toFixed(1)}`}
      />
    </div>
  );
}

/**
 * What a stateful load looks like while it runs.
 *
 * Packets per second and per-frame loss say nothing useful about a connection
 * load — the questions are how fast connections are being established, how many
 * are open, and how many are failing — so these replace the stateless charts
 * rather than sitting alongside them.
 */
function ConnectionCharts({ stream }: { stream: ReturnType<typeof useStatsStream> }) {
  return (
    <div className="chart-grid">
      <ChartPanel
        stream={stream}
        title="Connection rate"
        series={CONNECTION_RATE_SERIES}
        unit="conn/s"
        format={(v) => formatCount(Math.round(v))}
      />
      <ChartPanel
        stream={stream}
        title="Open connections"
        series={OPEN_CONNECTIONS_SERIES}
        unit="connections"
        format={(v) => formatCount(Math.round(v))}
      />
      <ChartPanel
        stream={stream}
        title="Application throughput"
        series={APP_THROUGHPUT_SERIES}
        unit="bits/s"
        format={formatBitrate}
      />
      <ChartPanel
        stream={stream}
        title="Failure rate"
        series={FAILURE_SERIES}
        unit="%"
        format={(v) => `${v.toFixed(2)}%`}
      />
    </div>
  );
}

/** Adds one field across every flow in a batch. */
function sumStreams(batch: StatsBatch, pick: (s: StatsBatch['streams'][string]) => number): number {
  return Object.values(batch.streams).reduce((total, sample) => total + pick(sample), 0);
}

/** Adds one field across every port in a batch. */
function sumPorts(batch: StatsBatch, pick: (p: StatsBatch['ports'][string]) => number): number {
  return Object.values(batch.ports).reduce((total, sample) => total + pick(sample), 0);
}

/**
 * The first reported value of a latency percentile.
 *
 * Percentiles cannot be averaged across flows, so the chart shows the first
 * flow that reports one rather than a number that would not mean anything.
 */
function firstLatency(
  batch: StatsBatch,
  key: 'p50Us' | 'p99Us' | 'maxUs',
): number | null {
  for (const sample of Object.values(batch.streams)) {
    const value = sample.latency[key];
    if (value !== null) return value;
  }
  return null;
}

/** The per-trial results table. */
function ResultsTable({ results }: { results: RunResult[] }) {
  return (
    <Surface title="Results" padded={false}>
      <div className="qz-table-wrap">
        <table className="qz-table">
          <thead>
            <tr>
              <th>#</th>
              <th>Flow</th>
              <th>Frame</th>
              <th className="num">Tx</th>
              <th className="num">Rx</th>
              <th className="num">Lost</th>
              <th className="num">Loss</th>
              <th className="num">p50 µs</th>
              <th className="num">p99 µs</th>
              <th>Result</th>
            </tr>
          </thead>
          <tbody>
            {results.length === 0 ? (
              <EmptyRow columns={10}>
                No trials recorded yet. Results appear as each one completes.
              </EmptyRow>
            ) : (
              results.map((result) => <ResultRow key={result.id} result={result} />)
            )}
          </tbody>
        </table>
      </div>
    </Surface>
  );
}

/** One trial row. */
function ResultRow({ result }: { result: RunResult }) {
  const metrics = manualMetricsSchema.safeParse(result.metrics);
  const params = result.params as { flowName?: string } | null;

  if (!metrics.success) {
    return (
      <tr>
        <td className="mono">{result.iteration}</td>
        <td colSpan={9} className="empty">
          This result was recorded in a format this interface does not understand.
        </td>
      </tr>
    );
  }

  const m = metrics.data;

  return (
    <tr>
      <td className="mono">{result.iteration}</td>
      <td className="mono">{params?.flowName ?? <Dash />}</td>
      <td className="mono">{result.frameSize ?? 'mixed'}</td>
      <td className="num">{formatCount(m.txPackets)}</td>
      <td className="num">{formatCount(m.rxPackets)}</td>
      <td className="num">{formatCount(m.lostPackets)}</td>
      <td className="num">{formatPercent(m.lossPct / 100, 3)}</td>
      <td className="num">{m.latP50 !== null ? m.latP50.toFixed(1) : <Dash />}</td>
      <td className="num">{m.latP99 !== null ? m.latP99.toFixed(1) : <Dash />}</td>
      <td>
        <Badge tone={result.passed ? 'ok' : 'crit'}>{result.passed ? 'recorded' : 'failed'}</Badge>
      </td>
    </tr>
  );
}

/** The configuration the run was started with. */
function SnapshotPanel({ detail }: { detail: RunDetail }) {
  return (
    <Surface title="Configuration snapshot">
      <div className="stack gap-10">
        <p className="note">
          The resolved configuration at the moment this run started. It is kept with the run so a
          historical result stays interpretable after the test has moved on.
        </p>
        <pre className="code-block">{JSON.stringify(detail.configSnapshot, null, 2)}</pre>
      </div>
    </Surface>
  );
}
