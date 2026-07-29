'use client';

/**
 * The dashboard: is this appliance ready, and what are its ports doing.
 *
 * Everything here answers a question an operator has before starting a test.
 * Sections that belong to later milestones are present but say so explicitly —
 * an empty panel with no explanation reads as a failure.
 */

import {
  IconAlertTriangle,
  IconClock,
  IconCpu,
  IconDatabase,
  IconPlugConnected,
  IconRefresh,
  IconServer2,
} from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Badge,
  EmptyRow,
  Kpi,
  LinkBadge,
  ModeBadge,
  PageHeader,
  Skeleton,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { runStateTone } from './runs/page';
import { api, queryKeys } from '@/lib/api';
import { isTerminal, type Health, type Run, type SubsystemHealth } from '@/lib/api-types';
import {
  formatBytes,
  formatDuration,
  formatRelative,
  formatSpeed,
  formatTimestamp,
} from '@/lib/format';

export default function DashboardPage() {
  return (
    <AppShell>
      <Dashboard />
    </AppShell>
  );
}

function Dashboard() {
  const health = useQuery({
    queryKey: queryKeys.health,
    queryFn: ({ signal }) => api.system.health(signal),
    // Health is the page's reason for existing; a stale answer is worse than a
    // little extra traffic on a local API.
    refetchInterval: 10_000,
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
    refetchInterval: 15_000,
  });

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Dashboard"
        subtitle={
          health.data
            ? `fluxd ${health.data.version} · up ${formatDuration(health.data.uptimeSecs)}`
            : 'Appliance status'
        }
        actions={
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => {
              void health.refetch();
              void ports.refetch();
            }}
            disabled={health.isFetching}
          >
            <IconRefresh size={15} stroke={1.8} />
            Refresh
          </button>
        }
      />

      {health.data?.mocked ? (
        <Alert tone="warn">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>
            This appliance is running in <strong>mock mode</strong>. No hardware is being driven
            and every statistic is simulated.
          </span>
        </Alert>
      ) : null}

      {health.error ? (
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>Could not read appliance health. {String(health.error)}</span>
        </Alert>
      ) : null}

      <HealthKpis health={health.data} loading={health.isLoading} />

      <SubsystemPanel health={health.data} loading={health.isLoading} />

      <Surface
        title="Ports"
        actions={
          <Link href="/ports" className="btn btn-ghost btn-sm">
            Manage ports
          </Link>
        }
        padded={false}
      >
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>PCI address</th>
                <th>Link</th>
                <th>Mode</th>
                <th>Speed</th>
                <th>Group</th>
                <th>Reserved by</th>
              </tr>
            </thead>
            {ports.isLoading ? (
              <TableSkeleton columns={7} />
            ) : (
              <tbody>
                {ports.data && ports.data.length > 0 ? (
                  ports.data.map((port) => (
                    <tr key={port.id}>
                      <td className="mono">{port.name}</td>
                      <td className="mono">{port.pciAddr}</td>
                      <td>
                        <LinkBadge state={port.linkState} />
                      </td>
                      <td>
                        <ModeBadge mode={port.mode} />
                      </td>
                      <td className="mono">{formatSpeed(port.speedMbps)}</td>
                      <td>
                        {port.group ? (
                          <span className="mono">{port.group.name}</span>
                        ) : (
                          <span className="muted">—</span>
                        )}
                      </td>
                      <td>
                        {port.reservation ? (
                          <span className="mono">{port.reservation.username}</span>
                        ) : (
                          <span className="muted">—</span>
                        )}
                      </td>
                    </tr>
                  ))
                ) : (
                  <EmptyRow columns={7}>
                    No ports have been discovered. Refresh the inventory from the ports page.
                  </EmptyRow>
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>

      <RunPanels />
    </div>
  );
}

/** Active runs on the left, recent history on the right. */
function RunPanels() {
  const recent = useQuery({
    queryKey: [...queryKeys.runs, 'dashboard'],
    queryFn: ({ signal }) => api.runs.list({ limit: 8 }, signal),
    // Poll while anything is in flight, and stop when nothing is.
    refetchInterval: (query) =>
      (query.state.data?.runs ?? []).some((r) => !isTerminal(r.state)) ? 3_000 : 20_000,
  });

  const runs = recent.data?.runs ?? [];
  const active = runs.filter((r) => !isTerminal(r.state));
  const finished = runs.filter((r) => isTerminal(r.state));

  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fit, minmax(340px, 1fr))',
        gap: 14,
      }}
    >
      <Surface
        title="Active runs"
        actions={
          <Link href="/runs/" className="btn btn-ghost btn-sm">
            All runs
          </Link>
        }
        padded={false}
      >
        <RunList
          runs={active}
          loading={recent.isLoading}
          empty="Nothing is running. Start a test from the tests page."
        />
      </Surface>

      <Surface title="Recent results" padded={false}>
        <RunList
          runs={finished}
          loading={recent.isLoading}
          empty="No completed runs yet."
        />
      </Surface>
    </div>
  );
}

/** A compact list of runs, shared by both dashboard panels. */
function RunList({
  runs,
  loading,
  empty,
}: {
  runs: Run[];
  loading: boolean;
  empty: string;
}) {
  if (loading) {
    return (
      <div className="stack gap-8" style={{ padding: 16 }}>
        <Skeleton height={14} />
        <Skeleton height={14} />
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <p className="dim" style={{ margin: 0, padding: 16, fontSize: 13 }}>
        {empty}
      </p>
    );
  }

  return (
    <div className="qz-table-wrap">
      <table className="qz-table">
        <tbody>
          {runs.map((run) => (
            <tr key={run.id}>
              <td className="mono">
                <Link href={`/runs/${run.id}/`} style={{ color: 'var(--qz-accent)' }}>
                  {run.testName}
                </Link>
              </td>
              <td>
                <Badge tone={runStateTone(run.state)}>{run.state}</Badge>
              </td>
              <td className="dim" style={{ fontSize: 12, textAlign: 'right' }}>
                {formatRelative(run.startedAt)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The four headline numbers. */
function HealthKpis({ health, loading }: { health: Health | undefined; loading: boolean }) {
  if (loading || !health) {
    return (
      <div className="kpi-grid">
        {Array.from({ length: 4 }, (_, i) => (
          <div className="kpi" key={i}>
            <Skeleton height={11} width={90} />
            <div style={{ marginTop: 12 }}>
              <Skeleton height={30} width={120} />
            </div>
          </div>
        ))}
      </div>
    );
  }

  // The root filesystem is the one that matters: it holds Postgres, the time
  // series, and the logs. Fall back to whichever is largest if "/" is not
  // reported, which is the case on a developer machine.
  const disk =
    health.disks.find((d) => d.mount === '/') ??
    [...health.disks].sort((a, b) => b.totalBytes - a.totalBytes)[0];

  return (
    <div className="kpi-grid">
      <Kpi
        label="Appliance"
        icon={<IconServer2 size={12} stroke={2} />}
        value={health.healthy ? 'Ready' : 'Degraded'}
        foot={
          health.healthy
            ? 'All subsystems responding'
            : 'One or more subsystems are not responding'
        }
      />
      <Kpi
        label="Ports up"
        icon={<IconPlugConnected size={12} stroke={2} />}
        value={health.ports.up}
        unit={`/ ${health.ports.total}`}
        foot={`${health.ports.down} down · ${health.ports.unknown} unknown`}
      />
      <Kpi
        label="Memory free"
        icon={<IconCpu size={12} stroke={2} />}
        value={formatBytes(health.memoryAvailableBytes)}
        foot={`of ${formatBytes(health.memoryTotalBytes)} total`}
      />
      <Kpi
        label="Disk free"
        icon={<IconDatabase size={12} stroke={2} />}
        value={disk ? formatBytes(disk.availableBytes) : '—'}
        foot={disk ? `${disk.mount} · ${formatBytes(disk.totalBytes)} total` : 'No filesystem reported'}
      />
    </div>
  );
}

/** Per-dependency status, plus hugepages. */
function SubsystemPanel({ health, loading }: { health: Health | undefined; loading: boolean }) {
  const hugepages = health?.hugepages;
  const oneGb = hugepages?.pools.find((p) => p.size === '1G' && p.node === null);

  return (
    <Surface
      title="Subsystems"
      actions={
        health ? (
          <span className="mono" style={{ fontSize: 11, color: 'var(--qz-fg-4)' }}>
            <IconClock size={11} stroke={2} style={{ verticalAlign: -1, marginRight: 4 }} />
            {formatTimestamp(new Date().toISOString())}
          </span>
        ) : null
      }
      padded={false}
    >
      <div className="qz-table-wrap">
        <table className="qz-table">
          <thead>
            <tr>
              <th>Subsystem</th>
              <th>Backend</th>
              <th>Status</th>
              <th>Detail</th>
            </tr>
          </thead>
          {loading ? (
            <TableSkeleton columns={4} rows={3} />
          ) : (
            <tbody>
              {health ? (
                <>
                  <SubsystemRow name="Database" sub={health.database} />
                  <SubsystemRow name="Port controller" sub={health.portd} />
                  <SubsystemRow name="Packet engine" sub={health.engine} />
                  <tr>
                    <td>Hugepages</td>
                    <td className="mono">1G</td>
                    <td>
                      {hugepages ? (
                        <Badge tone={hugepages.sufficient ? 'ok' : 'warn'}>
                          {hugepages.sufficient ? 'available' : 'none'}
                        </Badge>
                      ) : (
                        <Badge tone="muted">unknown</Badge>
                      )}
                    </td>
                    <td className="dim">
                      {oneGb
                        ? `${oneGb.free} of ${oneGb.total} pages free`
                        : 'No 1G pool reported'}
                    </td>
                  </tr>
                </>
              ) : (
                <EmptyRow columns={4}>Health is unavailable.</EmptyRow>
              )}
            </tbody>
          )}
        </table>
      </div>
    </Surface>
  );
}

/** One dependency row. */
function SubsystemRow({ name, sub }: { name: string; sub: SubsystemHealth }) {
  return (
    <tr>
      <td>{name}</td>
      <td className="mono">{sub.backend}</td>
      <td>
        <Badge tone={sub.ok ? 'ok' : 'crit'}>{sub.ok ? 'ok' : 'down'}</Badge>
      </td>
      <td className="dim">{sub.detail ?? '—'}</td>
    </tr>
  );
}
