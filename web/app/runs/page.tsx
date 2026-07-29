'use client';

/**
 * Run history.
 *
 * Refetches while anything is in flight and stops when nothing is, so an idle
 * appliance is not polled every few seconds for a table that cannot change.
 */

import { IconRefresh } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';
import { useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { Badge, EmptyRow, PageHeader, Surface, TableSkeleton, type BadgeTone } from '@/components/ui';
import { api, queryKeys } from '@/lib/api';
import { isTerminal, type Run, type RunState } from '@/lib/api-types';
import { formatDuration, formatTimestamp } from '@/lib/format';

/** How often to refetch while a run is in flight. */
const LIVE_REFETCH_MS = 3_000;

/** Rows per page. */
const PAGE_SIZE = 50;

/** Visual weight for each run state. */
export function runStateTone(state: RunState): BadgeTone {
  switch (state) {
    case 'complete':
      return 'ok';
    case 'failed':
      return 'crit';
    case 'cancelled':
      return 'muted';
    case 'running':
      return 'info';
    default:
      return 'warn';
  }
}

export default function RunsPage() {
  return (
    <AppShell>
      <Runs />
    </AppShell>
  );
}

function Runs() {
  const [filter, setFilter] = useState<RunState | ''>('');
  const [offset, setOffset] = useState(0);

  const runs = useQuery({
    queryKey: [...queryKeys.runs, filter, offset],
    queryFn: ({ signal }) =>
      api.runs.list(
        { state: filter || undefined, limit: PAGE_SIZE, offset },
        signal,
      ),
    // Poll only while something can still change.
    refetchInterval: (query) =>
      (query.state.data?.runs ?? []).some((r) => !isTerminal(r.state))
        ? LIVE_REFETCH_MS
        : false,
  });

  const page = runs.data;
  const rows = page?.runs ?? [];

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Runs"
        subtitle={page ? `${page.total} recorded` : 'Execution history'}
        actions={
          <>
            <select
              className="select"
              style={{ width: 160 }}
              value={filter}
              onChange={(e) => {
                setFilter(e.target.value as RunState | '');
                setOffset(0);
              }}
            >
              <option value="">All states</option>
              <option value="running">Running</option>
              <option value="complete">Complete</option>
              <option value="failed">Failed</option>
              <option value="cancelled">Cancelled</option>
            </select>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => void runs.refetch()}
              disabled={runs.isFetching}
            >
              <IconRefresh size={15} stroke={1.8} />
              Refresh
            </button>
          </>
        }
      />

      <Surface padded={false}>
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Test</th>
                <th>Type</th>
                <th>State</th>
                <th>Started</th>
                <th>Duration</th>
                <th>Detail</th>
              </tr>
            </thead>
            {runs.isLoading ? (
              <TableSkeleton columns={6} rows={5} />
            ) : (
              <tbody>
                {rows.length === 0 ? (
                  <EmptyRow columns={6}>
                    {filter ? `No ${filter} runs.` : 'No runs yet. Start one from the tests page.'}
                  </EmptyRow>
                ) : (
                  rows.map((run) => <RunRow key={run.id} run={run} />)
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>

      {page && page.total > PAGE_SIZE ? (
        <div className="row gap-10" style={{ justifyContent: 'flex-end' }}>
          <span className="muted mono" style={{ fontSize: 12 }}>
            {offset + 1}–{Math.min(offset + PAGE_SIZE, page.total)} of {page.total}
          </span>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
          >
            Previous
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={offset + PAGE_SIZE >= page.total}
            onClick={() => setOffset(offset + PAGE_SIZE)}
          >
            Next
          </button>
        </div>
      ) : null}
    </div>
  );
}

/** One run row. */
function RunRow({ run }: { run: Run }) {
  const duration = run.finishedAt
    ? (new Date(run.finishedAt).getTime() - new Date(run.startedAt).getTime()) / 1000
    : (Date.now() - new Date(run.startedAt).getTime()) / 1000;

  return (
    <tr>
      <td className="mono">
        <Link href={`/runs/${run.id}/`} style={{ color: 'var(--qz-accent)' }}>
          {run.testName}
        </Link>
      </td>
      <td className="dim">{run.type}</td>
      <td>
        <Badge tone={runStateTone(run.state)}>{run.state}</Badge>
      </td>
      <td className="dim" style={{ fontSize: 12 }}>
        {formatTimestamp(run.startedAt)}
      </td>
      <td className="mono">{formatDuration(duration)}</td>
      <td className="dim" style={{ maxWidth: 320, fontSize: 12 }}>
        {run.error ?? '—'}
      </td>
    </tr>
  );
}
