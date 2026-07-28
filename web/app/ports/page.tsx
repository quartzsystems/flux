'use client';

/**
 * The ports page.
 *
 * The inventory table plus the two actions an operator performs on a port:
 * moving it between the kernel and DPDK, and reserving it so a colleague does
 * not start a test over the same cable.
 *
 * Both actions are gated by role, and the controls are disabled rather than
 * hidden when the account cannot use them — a viewer should be able to see that
 * rebinding exists and that it needs an admin, not wonder why the page looks
 * different from the one in the documentation.
 */

import {
  IconAlertTriangle,
  IconLock,
  IconLockOpen,
  IconPencil,
  IconRefresh,
} from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';

import { AppShell } from '@/components/AppShell';
import {
  Alert,
  Badge,
  EmptyRow,
  LinkBadge,
  ModeBadge,
  PageHeader,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import type { Port, PortMode } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatRelative, formatSpeed } from '@/lib/format';

export default function PortsPage() {
  return (
    <AppShell>
      <Ports />
    </AppShell>
  );
}

function Ports() {
  const queryClient = useQueryClient();
  const { can, user } = useAuth();
  const [error, setError] = useState<string | null>(null);

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
    refetchInterval: 15_000,
  });

  /** Refetches ports and health — a rebind changes both. */
  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.ports });
    void queryClient.invalidateQueries({ queryKey: queryKeys.health });
  };

  const refresh = useMutation({
    mutationFn: () => api.ports.refresh(),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const setMode = useMutation({
    mutationFn: ({ id, mode }: { id: string; mode: PortMode }) =>
      api.ports.update(id, { mode }),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const rename = useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) => api.ports.update(id, { name }),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const reserve = useMutation({
    mutationFn: (id: string) => api.ports.reserve(id, { note: '' }),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const release = useMutation({
    mutationFn: (id: string) => api.ports.release(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const busy =
    setMode.isPending || rename.isPending || reserve.isPending || release.isPending;

  const rows = ports.data ?? [];
  const upCount = rows.filter((p) => p.linkState === 'up').length;

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Ports"
        subtitle={
          ports.data
            ? `${rows.length} discovered · ${upCount} up · ${rows.filter((p) => p.mode === 'dpdk').length} bound to DPDK`
            : 'Hardware inventory'
        }
        actions={
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => {
              setError(null);
              refresh.mutate();
            }}
            disabled={!can('admin') || refresh.isPending}
            title={can('admin') ? 'Re-read the hardware inventory' : 'Requires an admin account'}
          >
            <IconRefresh size={15} stroke={1.8} />
            {refresh.isPending ? 'Refreshing…' : 'Refresh inventory'}
          </button>
        }
      />

      {error ? (
        <Alert tone="danger">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>{error}</span>
        </Alert>
      ) : null}

      <Surface padded={false}>
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>PCI address</th>
                <th>Description</th>
                <th>MAC</th>
                <th>Speed</th>
                <th>NUMA</th>
                <th>Link</th>
                <th>Mode</th>
                <th>Group</th>
                <th>Reservation</th>
                <th style={{ textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            {ports.isLoading ? (
              <TableSkeleton columns={11} rows={4} />
            ) : (
              <tbody>
                {rows.length === 0 ? (
                  <EmptyRow columns={11}>
                    No ports discovered yet.
                    {can('admin')
                      ? ' Use “Refresh inventory” to read the chassis.'
                      : ' An administrator can refresh the inventory.'}
                  </EmptyRow>
                ) : (
                  rows.map((port) => (
                    <PortRow
                      key={port.id}
                      port={port}
                      busy={busy}
                      canBind={can('admin')}
                      canReserve={can('operator')}
                      isMine={port.reservation?.userId === user?.id}
                      onRename={(name) => {
                        setError(null);
                        rename.mutate({ id: port.id, name });
                      }}
                      onSetMode={(mode) => {
                        setError(null);
                        setMode.mutate({ id: port.id, mode });
                      }}
                      onReserve={() => {
                        setError(null);
                        reserve.mutate(port.id);
                      }}
                      onRelease={() => {
                        setError(null);
                        release.mutate(port.id);
                      }}
                    />
                  ))
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>

      <p className="dim" style={{ fontSize: 12.5, margin: 0 }}>
        Binding a port to DPDK hands it to a userspace driver: the kernel loses its interface, and
        link state stops being observable until an engine instance owns the port. A port belonging
        to a running port group cannot be rebound — stop the group first.
      </p>
    </div>
  );
}

/** One row of the inventory table. */
function PortRow({
  port,
  busy,
  canBind,
  canReserve,
  isMine,
  onRename,
  onSetMode,
  onReserve,
  onRelease,
}: {
  port: Port;
  busy: boolean;
  canBind: boolean;
  canReserve: boolean;
  isMine: boolean;
  onRename: (name: string) => void;
  onSetMode: (mode: PortMode) => void;
  onReserve: () => void;
  onRelease: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(port.name);

  const commitRename = () => {
    const next = draft.trim();
    setEditing(false);
    if (next && next !== port.name) onRename(next);
    else setDraft(port.name);
  };

  const held = port.reservation !== null;
  // A hold belonging to someone else blocks release; an admin can still take it
  // over, which the API enforces and reports.
  const releasable = held && (isMine || canBind);

  return (
    <tr aria-selected={held && isMine}>
      <td className="mono">
        {editing ? (
          <input
            className="input"
            style={{ width: 140, padding: '4px 8px', fontSize: 12.5 }}
            value={draft}
            autoFocus
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commitRename}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commitRename();
              if (e.key === 'Escape') {
                setDraft(port.name);
                setEditing(false);
              }
            }}
          />
        ) : (
          <span className="row gap-6">
            {port.name}
            {!port.present ? <Badge tone="crit">absent</Badge> : null}
          </span>
        )}
      </td>

      <td className="mono">{port.pciAddr}</td>
      <td className="dim" style={{ maxWidth: 260 }}>
        {port.description}
      </td>
      <td className="mono">{port.mac ?? '—'}</td>
      <td className="mono">{formatSpeed(port.speedMbps)}</td>
      <td className="mono">{port.numaNode ?? '—'}</td>

      <td>
        <LinkBadge state={port.linkState} />
      </td>
      <td>
        <ModeBadge mode={port.mode} />
      </td>

      <td>
        {port.group ? (
          <span className="mono">
            {port.group.name}
            <span className="muted"> #{port.group.index}</span>
          </span>
        ) : (
          <span className="muted">—</span>
        )}
      </td>

      <td>
        {port.reservation ? (
          <span className="stack" style={{ gap: 2 }}>
            <span className="mono">{port.reservation.username}</span>
            <span className="muted" style={{ fontSize: 11 }}>
              expires {formatRelative(port.reservation.expiresAt)}
            </span>
          </span>
        ) : (
          <span className="muted">—</span>
        )}
      </td>

      <td>
        <div className="row gap-6" style={{ justifyContent: 'flex-end' }}>
          <button
            type="button"
            className="btn btn-ghost btn-sm btn-icon"
            title={canBind ? 'Rename port' : 'Renaming requires an admin account'}
            disabled={!canBind || busy || editing}
            onClick={() => {
              setDraft(port.name);
              setEditing(true);
            }}
          >
            <IconPencil size={14} stroke={1.8} />
          </button>

          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={!canBind || busy || !port.present}
            title={
              !canBind
                ? 'Rebinding requires an admin account'
                : !port.present
                  ? 'The device is not present in the chassis'
                  : port.mode === 'dpdk'
                    ? 'Return this port to the kernel driver'
                    : 'Bind this port to vfio-pci for DPDK'
            }
            onClick={() => onSetMode(port.mode === 'dpdk' ? 'kernel' : 'dpdk')}
          >
            {port.mode === 'dpdk' ? 'To kernel' : 'To DPDK'}
          </button>

          {held ? (
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              disabled={!releasable || busy}
              title={
                releasable
                  ? 'Release this reservation'
                  : `Held by ${port.reservation?.username}; only they or an admin can release it`
              }
              onClick={onRelease}
            >
              <IconLockOpen size={14} stroke={1.8} />
              Release
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              disabled={!canReserve || busy}
              title={canReserve ? 'Reserve for 8 hours' : 'Reserving requires an operator account'}
              onClick={onReserve}
            >
              <IconLock size={14} stroke={1.8} />
              Reserve
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

/** Turns any thrown value into a line an operator can act on. */
function describe(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.fieldErrors.length > 0) {
      return error.fieldErrors.map((e) => `${e.path}: ${e.msg}`).join('; ');
    }
    if (error.status === 403) return 'Your account is not permitted to do that.';
    return error.message;
  }
  return 'The request failed.';
}
