'use client';

/**
 * The dashboard: is this appliance ready, and what are its ports doing.
 *
 * Everything here answers a question an operator has before starting a test.
 * Sections that belong to later milestones are present but say so explicitly —
 * an empty panel with no explanation reads as a failure.
 */

import {
  Clock,
  Cpu,
  Database,
  EthernetPort,
  RefreshCw,
  Server,
} from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Badge,
  Dash,
  Empty,
  EmptyRow,
  Kpi,
  KpiSkeleton,
  LinkBadge,
  ModeBadge,
  Page,
  PageBody,
  PageHeader,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { runStateTone } from './runs/page';
import { api, queryKeys } from '@/lib/api';
import { isTerminal, type Health, type Run, type SubsystemHealth } from '@/lib/api-types';
import {
  formatBytes,
  formatDuration,
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
    <Page>
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
            <RefreshCw size={15} />
            Refresh
          </button>
        }
      />

      <PageBody>
        {health.data?.mocked ? (
          <Alert tone="warn">
            This appliance is running in <strong>mock mode</strong>. No hardware is being driven
            and every statistic is simulated.
          </Alert>
        ) : null}

        {health.error ? (
          <Alert tone="danger">Could not read appliance health. {String(health.error)}</Alert>
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
                        <td>{port.group ? <span className="mono">{port.group.name}</span> : <Dash />}</td>
                        <td>
                          {port.reservation ? (
                            <span className="mono">{port.reservation.username}</span>
                          ) : (
                            <Dash />
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
      </PageBody>
    </Page>
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
  if (!loading && runs.length === 0) {
    return <Empty>{empty}</Empty>;
  }

  return (
    <div className="qz-table-wrap">
      <table className="qz-table">
        <thead>
          <tr>
            <th>Test</th>
            <th>State</th>
            <th className="right">Started</th>
          </tr>
        </thead>
        {loading ? (
          <TableSkeleton columns={3} rows={2} />
        ) : (
          <tbody>
            {runs.map((run) => (
              <tr key={run.id}>
                <td className="mono">
                  <Link href={`/runs/${run.id}/`} className="link">
                    {run.testName}
                  </Link>
                </td>
                <td>
                  <Badge tone={runStateTone(run.state)}>{run.state}</Badge>
                </td>
                <td className="dim right">{formatTimestamp(run.startedAt)}</td>
              </tr>
            ))}
          </tbody>
        )}
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
          <KpiSkeleton key={i} />
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
      {/* The 32px slot carries a number; the health verdict lives in the foot
          as a badge, where a word belongs. */}
      <Kpi
        label="Appliance"
        icon={<Server size={12} />}
        value={formatDuration(health.uptimeSecs)}
        foot={
          <span className="row gap-6">
            <Badge tone={health.healthy ? 'ok' : 'crit'}>
              {health.healthy ? 'ready' : 'degraded'}
            </Badge>
            {health.healthy ? 'all subsystems responding' : 'a subsystem is not responding'}
          </span>
        }
      />
      <Kpi
        label="Ports up"
        icon={<EthernetPort size={12} />}
        value={health.ports.up}
        unit={`/ ${health.ports.total}`}
        foot={`${health.ports.down} down · ${health.ports.unknown} unknown`}
      />
      <Kpi
        label="Memory free"
        icon={<Cpu size={12} />}
        value={formatBytes(health.memoryAvailableBytes)}
        foot={`of ${formatBytes(health.memoryTotalBytes)} total`}
      />
      <Kpi
        label="Disk free"
        icon={<Database size={12} />}
        value={disk ? formatBytes(disk.availableBytes) : <Dash />}
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
          <span className="row gap-6 mono muted">
            <Clock size={11} aria-hidden />
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
      <td className="dim">{sub.detail ?? <Dash />}</td>
    </tr>
  );
}
