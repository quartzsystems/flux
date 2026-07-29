'use client';

/**
 * The tests page.
 *
 * Two shapes of test live here. A manual test names flows and runs them until
 * stopped; an RFC 2544 benchmark owns the frame size and the rate and searches
 * for an answer. They share a list, and the wizard differs per benchmark.
 */

import {
  IconAlertTriangle,
  IconChevronDown,
  IconPlayerPlay,
  IconTrash,
} from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRouter } from 'next/navigation';
import { useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { BENCHMARKS, Rfc2544Wizard } from '@/components/Rfc2544Wizard';
import { Alert, Badge, EmptyRow, PageHeader, Surface, TableSkeleton } from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import type { Flow, LoadProfile, Test, TestType } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatTimestamp } from '@/lib/format';

/** Every test type can now be run. */
const RUNNABLE: TestType[] = [
  'manual',
  'rfc2544_throughput',
  'rfc2544_latency',
  'rfc2544_frameloss',
  'rfc2544_b2b',
];

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
  const [creating, setCreating] = useState<TestType | null>(null);

  const tests = useQuery({
    queryKey: queryKeys.tests,
    queryFn: ({ signal }) => api.tests.list(signal),
  });

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const profiles = useQuery({
    queryKey: queryKeys.loadProfiles,
    queryFn: ({ signal }) => api.loadProfiles.list(signal),
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

  // A test needs something to drive. Either kind will do — the benchmarks need
  // flows specifically, and TypePicker gates those itself.
  const drivable = (flows.data ?? []).length + (profiles.data ?? []).length;

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Tests"
        subtitle={tests.data ? `${rows.length} defined` : 'Test definitions'}
        actions={
          creating ? (
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={() => {
                setError(null);
                setCreating(null);
              }}
            >
              Cancel
            </button>
          ) : (
            <TypePicker
              disabled={!can('operator') || drivable === 0}
              reason={
                drivable === 0
                  ? 'Define a flow or a load profile first'
                  : !can('operator')
                    ? 'Creating tests requires an operator account'
                    : undefined
              }
              onPick={(type) => {
                setError(null);
                setCreating(type);
              }}
            />
          )
        }
      />

      {error ? (
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>{error}</span>
        </Alert>
      ) : null}

      {creating === 'manual' ? (
        <CreateTest
          flows={flows.data ?? []}
          profiles={profiles.data ?? []}
          onDone={() => {
            setCreating(null);
            void queryClient.invalidateQueries({ queryKey: queryKeys.tests });
          }}
          onError={setError}
        />
      ) : null}

      {creating && creating !== 'manual' ? (
        <CreateBenchmark
          type={creating}
          flows={flows.data ?? []}
          onDone={() => {
            setCreating(null);
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
                <th>Drives</th>
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
                    No tests yet. A test names what to drive — flows or a load profile — and
                    how to drive it.
                  </EmptyRow>
                ) : (
                  rows.map((test) => (
                    <TestRow
                      key={test.id}
                      test={test}
                      flows={flows.data ?? []}
                      profiles={profiles.data ?? []}
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
  profiles,
  canRun,
  busy,
  onRun,
  onRemove,
}: {
  test: Test;
  flows: Flow[];
  profiles: LoadProfile[];
  canRun: boolean;
  busy: boolean;
  onRun: () => void;
  onRemove: () => void;
}) {
  const stateful = test.profileIds.length > 0;

  // "missing" rather than the raw id: a deleted flow is worth seeing, and the
  // id would say nothing to anyone reading the row.
  const names = stateful
    ? test.profileIds.map((id) => profiles.find((p) => p.id === id)?.name ?? 'missing').join(', ')
    : test.flowIds.map((id) => flows.find((f) => f.id === id)?.name ?? 'missing').join(', ');

  const runnable = RUNNABLE.includes(test.type);

  return (
    <tr>
      <td className="mono">{test.name}</td>
      <td>
        <Badge tone={runnable ? 'info' : 'muted'}>{TYPE_LABELS[test.type]}</Badge>
      </td>
      <td className="dim" style={{ maxWidth: 320 }}>
        <span className="row gap-8">
          <Badge tone="muted">{stateful ? 'load' : 'flows'}</Badge>
          <span>{names}</span>
        </span>
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
                ? 'This test type cannot be run by this build'
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

/**
 * Inline form for defining a manual test.
 *
 * A test drives stateless flows or a stateful load, never both: the two are
 * programmed through different engine calls and an instance is in one mode or
 * the other. Making that a choice up front is kinder than letting an operator
 * assemble a mixture the orchestrator will refuse.
 */
function CreateTest({
  flows,
  profiles,
  onDone,
  onError,
}: {
  flows: Flow[];
  profiles: LoadProfile[];
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [name, setName] = useState('');
  const [drives, setDrives] = useState<'flows' | 'profiles'>(
    flows.length > 0 ? 'flows' : 'profiles',
  );
  const [selected, setSelected] = useState<string[]>([]);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  const stateful = drives === 'profiles';
  const options: { id: string; name: string }[] = stateful ? profiles : flows;

  const create = useMutation({
    mutationFn: () =>
      api.tests.create({
        name,
        type: 'manual',
        config: {},
        flowIds: stateful ? [] : selected,
        profileIds: stateful ? selected : [],
      }),
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

  /** Adds or removes one, preserving selection order. */
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
            <span className="field-label">Drives</span>
            <select
              className="select"
              value={drives}
              onChange={(e) => {
                setDrives(e.target.value as 'flows' | 'profiles');
                // Different lists entirely; a selection cannot carry across.
                setSelected([]);
              }}
            >
              <option value="flows" disabled={flows.length === 0}>
                Flows — stateless traffic
              </option>
              <option value="profiles" disabled={profiles.length === 0}>
                Load profile — stateful connections
              </option>
            </select>
            <span className="muted" style={{ fontSize: 11.5 }}>
              {stateful ? 'Runs on an ASTF port group.' : 'Runs on a stateless port group.'}
            </span>
          </label>
        </div>

        <div className="field">
          <span className="field-label">
            {stateful ? 'Load profiles' : 'Flows'}{' '}
            {selected.length > 0 ? `(${selected.length} selected, in order)` : ''}
          </span>
          {fieldErrors.flowIds ? (
            <span className="field-error">{fieldErrors.flowIds}</span>
          ) : null}
          {fieldErrors.profileIds ? (
            <span className="field-error">{fieldErrors.profileIds}</span>
          ) : null}

          <div className="stack gap-6" style={{ marginTop: 4 }}>
            {options.length === 0 ? (
              <span className="muted" style={{ fontSize: 12.5 }}>
                None defined yet.
              </span>
            ) : (
              options.map((option) => {
                const position = selected.indexOf(option.id);
                return (
                  <label
                    key={option.id}
                    className="row gap-8"
                    style={{ fontSize: 13, cursor: 'pointer' }}
                  >
                    <input
                      type="checkbox"
                      checked={position >= 0}
                      onChange={() => toggle(option.id)}
                    />
                    <span className="mono">{option.name}</span>
                    {position >= 0 ? <Badge tone="ok">#{position}</Badge> : null}
                  </label>
                );
              })
            )}
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

/** The "new test" split button: manual, or one of the four benchmarks. */
function TypePicker({
  disabled,
  reason,
  onPick,
}: {
  disabled: boolean;
  reason?: string;
  onPick: (type: TestType) => void;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div style={{ position: 'relative' }}>
      <button
        type="button"
        className="btn btn-primary btn-sm"
        disabled={disabled}
        title={reason ?? 'Create a test'}
        onClick={() => setOpen((v) => !v)}
      >
        New test
        <IconChevronDown size={14} stroke={2} />
      </button>

      {open && !disabled ? (
        <>
          {/* Clicking anywhere else closes the menu without selecting. */}
          <div
            style={{ position: 'fixed', inset: 0, zIndex: 10 }}
            onClick={() => setOpen(false)}
            aria-hidden
          />
          <div className="menu">
            <button
              type="button"
              className="menu-item"
              onClick={() => {
                setOpen(false);
                onPick('manual');
              }}
            >
              <strong>Manual</strong>
              <span>Start and stop flows as configured</span>
            </button>

            <div className="menu-heading">RFC 2544</div>
            {BENCHMARKS.map((benchmark) => (
              <button
                key={benchmark.type}
                type="button"
                className="menu-item"
                onClick={() => {
                  setOpen(false);
                  onPick(benchmark.type);
                }}
              >
                <strong>
                  {benchmark.label} <span className="muted">{benchmark.section}</span>
                </strong>
                <span>{benchmark.describes}</span>
              </button>
            ))}
          </div>
        </>
      ) : null}
    </div>
  );
}

/** Wraps the wizard in a surface and submits it. */
function CreateBenchmark({
  type,
  flows,
  onDone,
  onError,
}: {
  type: TestType;
  flows: Flow[];
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  const create = useMutation({
    mutationFn: (input: {
      name: string;
      type: TestType;
      config: Record<string, unknown>;
      flowIds: string[];
    }) => api.tests.create(input),
    onSuccess: () => {
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

  return (
    <Surface title={`New ${BENCHMARKS.find((b) => b.type === type)?.label ?? 'benchmark'} test`}>
      <Rfc2544Wizard
        type={type}
        flows={flows}
        busy={create.isPending}
        errors={fieldErrors}
        onSubmit={(input) => create.mutate(input)}
      />
    </Surface>
  );
}
