'use client';

/**
 * Settings: user administration and appliance identity.
 *
 * Admin-only. The whole page is behind the role check rather than individual
 * controls, because nothing on it is meaningful to read without the ability to
 * act on it.
 */

import { IconAlertTriangle, IconPlus, IconTrash } from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type FormEvent } from 'react';

import { AppShell } from '@/components/AppShell';
import { FluxLockup, TAGLINE } from '@/components/Brand';
import {
  Alert,
  EmptyRow,
  PageHeader,
  RoleBadge,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import type { Role, User } from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatDuration, formatTimestamp } from '@/lib/format';

export default function SettingsPage() {
  return (
    <AppShell>
      <Settings />
    </AppShell>
  );
}

function Settings() {
  const { can } = useAuth();

  if (!can('admin')) {
    return (
      <div className="page stack gap-18">
        <PageHeader title="Settings" subtitle="Appliance administration" />
        <Alert tone="warn">
          <IconAlertTriangle size={16} stroke={1.8} />
          <span>Settings are available to administrators only.</span>
        </Alert>
      </div>
    );
  }

  return (
    <div className="page stack gap-18">
      <PageHeader title="Settings" subtitle="Users and appliance identity" />
      <UsersPanel />
      <AboutPanel />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

function UsersPanel() {
  const queryClient = useQueryClient();
  const { user: me } = useAuth();
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const users = useQuery({
    queryKey: queryKeys.users,
    queryFn: ({ signal }) => api.users.list(signal),
  });

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.users });
  };

  const setRole = useMutation({
    mutationFn: ({ id, role }: { id: string; role: Role }) => api.users.update(id, { role }),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.users.remove(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const rows = users.data ?? [];

  return (
    <Surface
      title="Users"
      actions={
        <button
          type="button"
          className="btn btn-primary btn-sm"
          onClick={() => {
            setError(null);
            setCreating((v) => !v);
          }}
        >
          <IconPlus size={14} stroke={2} />
          {creating ? 'Cancel' : 'Add user'}
        </button>
      }
      padded={false}
    >
      {error ? (
        <div style={{ padding: '14px 18px 0' }}>
          <Alert tone="danger">
            <IconAlertTriangle size={16} stroke={1.8} />
            <span>{error}</span>
          </Alert>
        </div>
      ) : null}

      {creating ? (
        <CreateUserForm
          onDone={() => {
            setCreating(false);
            invalidate();
          }}
          onError={setError}
        />
      ) : null}

      <div className="qz-table-wrap">
        <table className="qz-table">
          <thead>
            <tr>
              <th>Username</th>
              <th>Role</th>
              <th>Created</th>
              <th>Last sign-in</th>
              <th style={{ textAlign: 'right' }}>Actions</th>
            </tr>
          </thead>
          {users.isLoading ? (
            <TableSkeleton columns={5} rows={3} />
          ) : (
            <tbody>
              {rows.length === 0 ? (
                <EmptyRow columns={5}>No accounts exist.</EmptyRow>
              ) : (
                rows.map((user) => (
                  <UserRow
                    key={user.id}
                    user={user}
                    isSelf={user.id === me?.id}
                    busy={setRole.isPending || remove.isPending}
                    onRole={(role) => {
                      setError(null);
                      setRole.mutate({ id: user.id, role });
                    }}
                    onRemove={() => {
                      setError(null);
                      remove.mutate(user.id);
                    }}
                  />
                ))
              )}
            </tbody>
          )}
        </table>
      </div>
    </Surface>
  );
}

/** One account row. */
function UserRow({
  user,
  isSelf,
  busy,
  onRole,
  onRemove,
}: {
  user: User;
  isSelf: boolean;
  busy: boolean;
  onRole: (role: Role) => void;
  onRemove: () => void;
}) {
  const [confirming, setConfirming] = useState(false);

  return (
    <tr>
      <td className="mono">
        {user.username}
        {isSelf ? <span className="muted"> (you)</span> : null}
      </td>

      <td>
        <div className="row gap-8">
          <RoleBadge role={user.role} />
          <select
            className="select"
            style={{ width: 118, padding: '3px 8px', fontSize: 12 }}
            value={user.role}
            disabled={busy}
            onChange={(e) => onRole(e.target.value as Role)}
            aria-label={`Role for ${user.username}`}
          >
            <option value="viewer">viewer</option>
            <option value="operator">operator</option>
            <option value="admin">admin</option>
          </select>
        </div>
      </td>

      <td className="dim">{formatTimestamp(user.createdAt)}</td>
      <td className="dim">{user.lastLoginAt ? formatTimestamp(user.lastLoginAt) : 'never'}</td>

      <td>
        <div className="row gap-6" style={{ justifyContent: 'flex-end' }}>
          {confirming ? (
            <>
              <span className="muted" style={{ fontSize: 12 }}>
                Delete {user.username}?
              </span>
              <button
                type="button"
                className="btn btn-danger btn-sm"
                disabled={busy}
                onClick={() => {
                  setConfirming(false);
                  onRemove();
                }}
              >
                Confirm
              </button>
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={() => setConfirming(false)}
              >
                Cancel
              </button>
            </>
          ) : (
            <button
              type="button"
              className="btn btn-ghost btn-sm btn-icon"
              title={`Delete ${user.username}`}
              disabled={busy}
              onClick={() => setConfirming(true)}
            >
              <IconTrash size={14} stroke={1.8} />
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

/** Inline form for adding an account. */
function CreateUserForm({
  onDone,
  onError,
}: {
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('operator');
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  const create = useMutation({
    mutationFn: () => api.users.create({ username, password, role }),
    onSuccess: () => {
      setUsername('');
      setPassword('');
      setFieldErrors({});
      onDone();
    },
    onError: (e) => {
      // Field-level failures go next to their input; anything else is a banner.
      if (e instanceof ApiError && e.fieldErrors.length > 0) {
        setFieldErrors(Object.fromEntries(e.fieldErrors.map((f) => [f.path, f.msg])));
      } else {
        onError(describe(e));
      }
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!create.isPending) create.mutate();
  };

  return (
    <form
      onSubmit={submit}
      style={{
        display: 'grid',
        gridTemplateColumns: '1fr 1fr 150px auto',
        gap: 12,
        alignItems: 'start',
        padding: '16px 18px',
        borderBottom: '1px solid var(--qz-divider)',
      }}
    >
      <label className="field">
        <span className="field-label">Username</span>
        <input
          className="input"
          value={username}
          required
          autoFocus
          aria-invalid={Boolean(fieldErrors.username)}
          onChange={(e) => setUsername(e.target.value)}
        />
        {fieldErrors.username ? <span className="field-error">{fieldErrors.username}</span> : null}
      </label>

      <label className="field">
        <span className="field-label">Password</span>
        <input
          className="input"
          type="password"
          value={password}
          required
          autoComplete="new-password"
          aria-invalid={Boolean(fieldErrors.password)}
          onChange={(e) => setPassword(e.target.value)}
        />
        <span className={fieldErrors.password ? 'field-error' : 'muted'} style={{ fontSize: 12 }}>
          {fieldErrors.password ?? 'At least 12 characters.'}
        </span>
      </label>

      <label className="field">
        <span className="field-label">Role</span>
        <select
          className="select"
          value={role}
          onChange={(e) => setRole(e.target.value as Role)}
        >
          <option value="viewer">viewer</option>
          <option value="operator">operator</option>
          <option value="admin">admin</option>
        </select>
      </label>

      <button
        type="submit"
        className="btn btn-primary"
        style={{ marginTop: 22 }}
        disabled={create.isPending || !username || !password}
      >
        {create.isPending ? 'Creating…' : 'Create'}
      </button>
    </form>
  );
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

function AboutPanel() {
  const health = useQuery({
    queryKey: queryKeys.health,
    queryFn: ({ signal }) => api.system.health(signal),
  });

  return (
    <Surface title="About">
      <div className="stack gap-18">
        <div className="stack gap-10">
          <FluxLockup width={160} />
          <p className="login-tagline" style={{ textAlign: 'left' }}>
            {TAGLINE}
          </p>
        </div>

        <dl
          style={{
            display: 'grid',
            gridTemplateColumns: 'auto 1fr',
            gap: '8px 20px',
            margin: 0,
            fontSize: 13,
          }}
        >
          <dt className="muted">Daemon version</dt>
          <dd className="mono" style={{ margin: 0 }}>
            {health.data?.version ?? '—'}
          </dd>

          <dt className="muted">Uptime</dt>
          <dd className="mono" style={{ margin: 0 }}>
            {health.data ? formatDuration(health.data.uptimeSecs) : '—'}
          </dd>

          <dt className="muted">Packet engine</dt>
          <dd className="mono" style={{ margin: 0 }}>
            {health.data?.engine.backend ?? '—'}
          </dd>

          <dt className="muted">Port controller</dt>
          <dd className="mono" style={{ margin: 0 }}>
            {health.data?.portd.backend ?? '—'}
          </dd>
        </dl>

        <p className="dim" style={{ margin: 0, fontSize: 12.5 }}>
          TLS configuration, retention policy, and appliance hostname arrive in milestone 4.
        </p>
      </div>
    </Surface>
  );
}

/** Turns any thrown value into a line an operator can act on. */
function describe(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.fieldErrors.length > 0) {
      return error.fieldErrors.map((e) => `${e.path}: ${e.msg}`).join('; ');
    }
    return error.message;
  }
  return 'The request failed.';
}
