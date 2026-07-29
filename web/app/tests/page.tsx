'use client';

/**
 * The tests page.
 *
 * Milestone 2 delivers the manual test type: pick flows, press run, watch them.
 * The RFC 2544 wizards land in milestone 3, and the type selector says so rather
 * than offering choices that would be rejected.
 */

import { IconAlertTriangle, IconPlayerPlay, IconPlus, IconTrash } from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRouter } from 'next/navigation';
import { useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { Alert, Badge, EmptyRow, PageHeader, Surface, TableSkeleton } from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import type { Flow, Test, TestType } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatTimestamp } from '@/lib/format';

/** Which test types can actually run today. */
const RUNNABLE: TestType[] = ['manual'];

/** Display names for each type. */
const TYPE_LABELS: Record<TestType, string> = {
  manual: 'Manual',
  rfc2544_throughput: 'RFC 2544 throughput',
  rfc2544_latency: 'RFC 2544 latency',
  rfc2544_frameloss: 'RFC 2544 frame loss',
  rfc2544_b2b: 'RFC 2544 back-to-back',
};

export default function TestsPage() {
  return (
    <AppShell>
      <Tests />
    </AppShell>
  );
}

function Tests() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { can } = useAuth();
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const tests = useQuery({
    queryKey: queryKeys.tests,
    queryFn: ({ signal }) => api.tests.list(signal),
  });

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const start = useMutation({
    mutationFn: (id: string) => api.tests.run(id),
    onSuccess: ({ runId }) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.runs });
      // Straight to the live view: an operator who pressed run wants to watch it.
      router.push(`/runs/${runId}/`);
    },
    onError: (e) => setError(describe(e)),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.tests.remove(id),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.tests }),
    onError: (e) => setError(describe(e)),
  });

  const rows = tests.data ?? [];

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Tests"
        subtitle={tests.data ? `${rows.length} defined` : 'Test definitions'}
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={!can('operator') || (flows.data ?? []).length === 0}
            title={
              (flows.data ?? []).length === 0
                ? 'Define a flow first'
                : 'Create a test'
            }
            onClick={() => {
              setError(null);
              setCreating((v) => !v);
            }}
          >
            <IconPlus size={15} stroke={2} />
            {creating ? 'Cancel' : 'New test'}
          </button>
        }
      />

      {error ? (
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>{error}</span>
        </Alert>
      ) : null}

      {creating ? (
        <CreateTest
          flows={flows.data ?? []}
          onDone={() => {
            setCreating(false);
            void queryClient.invalidateQueries({ queryKey: queryKeys.tests });
          }}
          onError={setError}
        />
      ) : null}

      <Surface padded={false}>
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Type</th>
                <th>Flows</th>
                <th>Created</th>
                <th style={{ textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            {tests.isLoading ? (
              <TableSkeleton columns={5} rows={3} />
            ) : (
              <tbody>
                {rows.length === 0 ? (
                  <EmptyRow columns={5}>
                    No tests yet. A test names the flows to drive and how to drive them.
                  </EmptyRow>
                ) : (
                  rows.map((test) => (
                    <TestRow
                      key={test.id}
                      test={test}
                      flows={flows.data ?? []}
                      canRun={can('operator')}
                      busy={start.isPending || remove.isPending}
                      onRun={() => {
                        setError(null);
                        start.mutate(test.id);
                      }}
                      onRemove={() => {
                        setError(null);
                        remove.mutate(test.id);
                      }}
                    />
                  ))
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>
    </div>
  );
}

/** One test row. */
function TestRow({
  test,
  flows,
  canRun,
  busy,
  onRun,
  onRemove,
}: {
  test: Test;
  flows: Flow[];
  canRun: boolean;
  busy: boolean;
  onRun: () => void;
  onRemove: () => void;
}) {
  const names = test.flowIds
    .map((id) => flows.find((f) => f.id === id)?.name ?? 'missing')
    .join(', ');
  const runnable = RUNNABLE.includes(test.type);

  return (
    <tr>
      <td className="mono">{test.name}</td>
      <td>
        <Badge tone={runnable ? 'info' : 'muted'}>{TYPE_LABELS[test.type]}</Badge>
      </td>
      <td className="dim" style={{ maxWidth: 320 }}>
        {names}
      </td>
      <td className="dim" style={{ fontSize: 12 }}>
        {formatTimestamp(test.createdAt)}
      </td>
      <td>
        <div className="row gap-6" style={{ justifyContent: 'flex-end' }}>
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={!canRun || busy || !runnable}
            title={
              !runnable
                ? 'This test type arrives in milestone 3'
                : canRun
                  ? 'Start a run'
                  : 'Running requires an operator account'
            }
            onClick={onRun}
          >
            <IconPlayerPlay size={13} stroke={2} />
            Run
          </button>
          <button
            type="button"
            className="btn btn-ghost btn-sm btn-icon"
            title="Delete this test"
            disabled={!canRun || busy}
            onClick={onRemove}
          >
            <IconTrash size={14} stroke={1.8} />
          </button>
        </div>
      </td>
    </tr>
  );
}

/** Inline form for defining a test. */
function CreateTest({
  flows,
  onDone,
  onError,
}: {
  flows: Flow[];
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [name, setName] = useState('');
  const [selected, setSelected] = useState<string[]>([]);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  const create = useMutation({
    mutationFn: () =>
      api.tests.create({ name, type: 'manual', config: {}, flowIds: selected }),
    onSuccess: () => {
      setName('');
      setSelected([]);
      setFieldErrors({});
      onDone();
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError && e.fieldErrors.length > 0) {
        setFieldErrors(Object.fromEntries(e.fieldErrors.map((f) => [f.path, f.msg])));
      } else {
        onError(describe(e));
      }
    },
  });

  /** Adds or removes a flow, preserving selection order. */
  const toggle = (id: string) => {
    setSelected((previous) =>
      previous.includes(id) ? previous.filter((f) => f !== id) : [...previous, id],
    );
  };

  return (
    <Surface title="New test">
      <div className="stack gap-14">
        <div className="field-grid">
          <label className="field">
            <span className="field-label">Name</span>
            <input
              className="input"
              value={name}
              autoFocus
              aria-invalid={Boolean(fieldErrors.name)}
              onChange={(e) => setName(e.target.value)}
            />
            {fieldErrors.name ? <span className="field-error">{fieldErrors.name}</span> : null}
          </label>

          <label className="field">
            <span className="field-label">Type</span>
            <select className="select" value="manual" disabled>
              <option value="manual">Manual — start and stop flows</option>
            </select>
            <span className="muted" style={{ fontSize: 11.5 }}>
              RFC 2544 wizards arrive in milestone 3.
            </span>
          </label>
        </div>

        <div className="field">
          <span className="field-label">
            Flows {selected.length > 0 ? `(${selected.length} selected, in order)` : ''}
          </span>
          {fieldErrors.flowIds ? (
            <span className="field-error">{fieldErrors.flowIds}</span>
          ) : null}

          <div className="stack gap-6" style={{ marginTop: 4 }}>
            {flows.map((flow) => {
              const position = selected.indexOf(flow.id);
              return (
                <label
                  key={flow.id}
                  className="row gap-8"
                  style={{ fontSize: 13, cursor: 'pointer' }}
                >
                  <input
                    type="checkbox"
                    checked={position >= 0}
                    onChange={() => toggle(flow.id)}
                  />
                  <span className="mono">{flow.name}</span>
                  {position >= 0 ? <Badge tone="ok">#{position}</Badge> : null}
                </label>
              );
            })}
          </div>
        </div>

        <div>
          <button
            type="button"
            className="btn btn-primary"
            disabled={create.isPending || !name.trim() || selected.length === 0}
            onClick={() => create.mutate()}
          >
            {create.isPending ? 'Creating…' : 'Create test'}
          </button>
        </div>
      </div>
    </Surface>
  );
}

/** Turns any thrown value into a line an operator can act on. */
function describe(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.fieldErrors.length > 0) {
      return error.fieldErrors.map((e) => e.msg).join('; ');
    }
    return error.message;
  }
  return 'The request failed.';
}
