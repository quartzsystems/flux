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

import { IconAlertTriangle, IconPlayerStop } from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { usePathname } from 'next/navigation';
import Link from 'next/link';
import { useMemo } from 'react';

import { AppShell } from '@/components/AppShell';
import { LiveChart, type ChartSeries } from '@/components/LiveChart';
import {
  Alert,
  Badge,
  EmptyRow,
  Kpi,
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
  type StatsBatch,
} from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { useStatsStream } from '@/lib/stream';
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

  if (run.isLoading) {
    return (
      <div className="page stack gap-18">
        <Skeleton height={36} width={320} />
        <Skeleton height={120} />
        <Skeleton height={220} />
      </div>
    );
  }

  if (run.error || !run.data) {
    return (
      <div className="page stack gap-18">
        <PageHeader title="Run" subtitle="Not found" />
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>
            {run.error instanceof ApiError ? run.error.message : 'This run could not be loaded.'}
          </span>
        </Alert>
        <div>
          <Link href="/runs/" className="btn btn-secondary btn-sm">
            Back to runs
          </Link>
        </div>
      </div>
    );
  }

  const detail = run.data;
  const duration = detail.finishedAt
    ? (new Date(detail.finishedAt).getTime() - new Date(detail.startedAt).getTime()) / 1000
    : (Date.now() - new Date(detail.startedAt).getTime()) / 1000;

  // Progress from the stream is fresher than the polled record, so it wins when
  // both are available.
  const state = stream.progress?.state ?? detail.state;

  return (
    <div className="page stack gap-18">
      <PageHeader
        title={detail.testName}
        subtitle={
          <>
            {detail.type} · started {formatTimestamp(detail.startedAt)}
          </>
        }
        actions={
          <>
            {live ? (
              <span className="row gap-6 mono" style={{ fontSize: 12, color: 'var(--qz-fg-3)' }}>
                <span className="pulse" aria-hidden />
                {stream.status === 'open' ? 'live' : stream.status}
              </span>
            ) : null}
            {detail.stoppable && can('operator') ? (
              <button
                type="button"
                className="btn btn-danger btn-sm"
                disabled={stop.isPending}
                onClick={() => stop.mutate()}
              >
                <IconPlayerStop size={14} stroke={2} />
                {stop.isPending ? 'Stopping…' : 'Stop run'}
              </button>
            ) : null}
            <Link href="/runs/" className="btn btn-secondary btn-sm">
              All runs
            </Link>
          </>
        }
      />

      {detail.error ? (
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>{detail.error}</span>
        </Alert>
      ) : null}

      {stream.detail ? (
        <Alert tone="warn">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>{stream.detail}</span>
        </Alert>
      ) : null}

      <div className="kpi-grid">
        <Kpi
          label="State"
          value={<Badge tone={runStateTone(detail.state)}>{state}</Badge>}
          foot={stream.progress?.message ?? detail.type}
        />
        <Kpi label="Duration" value={formatDuration(duration)} foot={live ? 'running' : 'final'} />
        <Kpi
          label="Trials"
          value={detail.results.length}
          foot={detail.results.length === 1 ? 'result recorded' : 'results recorded'}
        />
        <Kpi
          label="Frame size"
          value={stream.progress?.frameSize ?? '—'}
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
            <div className="row" style={{ justifyContent: 'space-between' }}>
              <span className="mono" style={{ fontSize: 12 }}>
                {Math.round(stream.progress.progress * 100)}%
              </span>
              {stream.progress.trialRemainingSecs !== undefined ? (
                <span className="mono muted" style={{ fontSize: 12 }}>
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
    </div>
  );
}

/** The three charts shown while a run is in flight. */
function LiveCharts({ stream }: { stream: ReturnType<typeof useStatsStream> }) {
  // Series are derived from the batch each second rather than from a fixed list
  // of ids, because a run's flows are known only once data starts arriving.
  const rateSeries: ChartSeries[] = useMemo(
    () => [
      { label: 'tx', value: (b: StatsBatch) => sumStreams(b, (s) => s.txPps) },
      { label: 'rx', value: (b: StatsBatch) => sumStreams(b, (s) => s.rxPps) },
    ],
    [],
  );

  const lossSeries: ChartSeries[] = useMemo(
    () => [{ label: 'loss', value: (b: StatsBatch) => sumStreams(b, (s) => s.lossPps) }],
    [],
  );

  const latencySeries: ChartSeries[] = useMemo(
    () => [
      { label: 'p50', value: (b: StatsBatch) => firstLatency(b, 'p50Us') },
      { label: 'p99', value: (b: StatsBatch) => firstLatency(b, 'p99Us') },
      { label: 'max', value: (b: StatsBatch) => firstLatency(b, 'maxUs') },
    ],
    [],
  );

  return (
    <div className="chart-grid">
      <Surface title="Packet rate">
        <LiveChart stream={stream} series={rateSeries} unit="pps" format={formatPps} />
      </Surface>

      <Surface title="Loss">
        <LiveChart stream={stream} series={lossSeries} unit="pps" format={formatPps} />
      </Surface>

      <Surface title="Throughput">
        <LiveChart
          stream={stream}
          series={[
            { label: 'tx', value: (b) => sumPorts(b, (p) => p.txBps) },
            { label: 'rx', value: (b) => sumPorts(b, (p) => p.rxBps) },
          ]}
          unit="bits/s"
          format={formatBitrate}
        />
      </Surface>

      <Surface title="Latency">
        <LiveChart
          stream={stream}
          series={latencySeries}
          unit="µs"
          format={(v) => `${v.toFixed(1)}`}
        />
      </Surface>
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
        <td colSpan={9} className="dim">
          This result was recorded in a format this interface does not understand.
        </td>
      </tr>
    );
  }

  const m = metrics.data;

  return (
    <tr>
      <td className="mono">{result.iteration}</td>
      <td className="mono">{params?.flowName ?? '—'}</td>
      <td className="mono">{result.frameSize ?? 'mixed'}</td>
      <td className="num">{formatCount(m.txPackets)}</td>
      <td className="num">{formatCount(m.rxPackets)}</td>
      <td className="num">{formatCount(m.lostPackets)}</td>
      <td className="num">{formatPercent(m.lossPct / 100, 3)}</td>
      <td className="num">{m.latP50 !== null ? m.latP50.toFixed(1) : '—'}</td>
      <td className="num">{m.latP99 !== null ? m.latP99.toFixed(1) : '—'}</td>
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
      <p className="dim" style={{ margin: '0 0 10px', fontSize: 12.5 }}>
        The resolved configuration at the moment this run started. It is kept with the run so a
        historical result stays interpretable after the test has moved on.
      </p>
      <pre className="hex-dump" style={{ borderRadius: 8, maxHeight: 320, overflow: 'auto' }}>
        {JSON.stringify(detail.configSnapshot, null, 2)}
      </pre>
    </Surface>
  );
}
