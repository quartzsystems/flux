'use client';

/**
 * Settings: users, engines, TLS, retention, and configuration transfer.
 *
 * Admin-only. The whole page is behind the role check rather than individual
 * controls, because nothing on it is meaningful to read without the ability to
 * act on it. Each concern is a tab; Configuration transfer and About share the
 * System tab because both describe the appliance as a whole.
 */

import {
  Download,
  Lock,
  Play,
  Square,
  Plus,
  Trash2,
  Upload,
} from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState, type FormEvent } from 'react';

import { AppShell } from '@/components/AppShell';
import { FluxLockup, TAGLINE } from '@/components/Brand';
import {
  Alert,
  Badge,
  Dash,
  EmptyRow,
  GroupStateBadge,
  Page,
  PageBody,
  PageHeader,
  RoleBadge,
  Surface,
  TableSkeleton,
} from '@/components/ui';
import { ModalHeader, ModalShell } from '@/components/ui/Modal';
import { Tabs } from '@/components/ui/Tabs';
import {
  ErrorText,
  Field,
  ModalFooter,
  SelectInput,
  TextInput,
} from '@/components/ui/formkit';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  applianceSettingSchema,
  retentionSettingSchema,
  tlsSettingSchema,
  type ImportSummary,
  type PortGroup,
  type Role,
  type User,
} from '@/lib/api-types';
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
  const [tab, setTab] = useState('users');

  if (!can('admin')) {
    return (
      <Page>
        <PageHeader title="Settings" subtitle="Appliance administration" />
        <PageBody>
          <Alert tone="warn">Settings are available to administrators only.</Alert>
        </PageBody>
      </Page>
    );
  }

  return (
    <Page>
      <PageHeader title="Settings" subtitle="Users, appliance, and configuration" />
      <PageBody>
        <Tabs
          items={[
            { value: 'users', label: 'Users' },
            { value: 'groups', label: 'Port groups' },
            { value: 'tls', label: 'TLS' },
            { value: 'appliance', label: 'Appliance' },
            { value: 'system', label: 'System' },
          ]}
          value={tab}
          onChange={setTab}
        />

        {tab === 'users' ? <UsersPanel /> : null}
        {tab === 'groups' ? <PortGroupsPanel /> : null}
        {tab === 'tls' ? <TlsPanel /> : null}
        {tab === 'appliance' ? <AppliancePanel /> : null}
        {tab === 'system' ? (
          <>
            <TransferPanel />
            <AboutPanel />
          </>
        ) : null}
      </PageBody>
    </Page>
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
  const [deleting, setDeleting] = useState<User | null>(null);

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
    onSuccess: () => {
      setDeleting(null);
      invalidate();
    },
    onError: (e) => {
      setDeleting(null);
      setError(describe(e));
    },
  });

  const rows = users.data ?? [];

  return (
    <>
      {error ? <Alert tone="danger">{error}</Alert> : null}

      <Surface
        title="Users"
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => {
              setError(null);
              setCreating(true);
            }}
          >
            <Plus size={14} />
            Add user
          </button>
        }
        padded={false}
      >
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Username</th>
                <th>Role</th>
                <th>Created</th>
                <th>Last sign-in</th>
                <th className="right">Actions</th>
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
                      onDelete={() => {
                        setError(null);
                        setDeleting(user);
                      }}
                    />
                  ))
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>

      {creating ? (
        <CreateUserDialog
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            invalidate();
          }}
        />
      ) : null}

      {deleting ? (
        <DeleteUserDialog
          user={deleting}
          busy={remove.isPending}
          onClose={() => setDeleting(null)}
          onConfirm={() => remove.mutate(deleting.id)}
        />
      ) : null}
    </>
  );
}

/** One account row. */
function UserRow({
  user,
  isSelf,
  busy,
  onRole,
  onDelete,
}: {
  user: User;
  isSelf: boolean;
  busy: boolean;
  onRole: (role: Role) => void;
  onDelete: () => void;
}) {
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
            className="select input-sm"
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
        <div className="row gap-6 end">
          <button
            type="button"
            className="btn btn-ghost btn-sm btn-icon"
            title={`Delete ${user.username}`}
            disabled={busy}
            onClick={onDelete}
          >
            <Trash2 size={14} />
          </button>
        </div>
      </td>
    </tr>
  );
}

/** Dialog for adding an account. */
function CreateUserDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('operator');
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [general, setGeneral] = useState('');

  const create = useMutation({
    mutationFn: () => api.users.create({ username, password, role }),
    onSuccess: onCreated,
    onError: (e) => {
      // Field-level failures go next to their input; anything else is a line
      // above the footer.
      if (e instanceof ApiError && e.fieldErrors.length > 0) {
        setGeneral('');
        setFieldErrors(Object.fromEntries(e.fieldErrors.map((f) => [f.path, f.msg])));
      } else {
        setFieldErrors({});
        setGeneral(describe(e));
      }
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!create.isPending) create.mutate();
  };

  return (
    <ModalShell onClose={onClose}>
      <ModalHeader
        title="Add user"
        subtitle="A local account on this appliance."
        onClose={onClose}
      />
      <form onSubmit={submit} className="stack gap-14">
        <Field label="Username" htmlFor="new-user-name" required error={fieldErrors.username}>
          <TextInput
            id="new-user-name"
            value={username}
            onChange={setUsername}
            mono
            autoFocus
            invalid={Boolean(fieldErrors.username)}
          />
        </Field>

        <Field
          label="Password"
          htmlFor="new-user-password"
          required
          error={fieldErrors.password}
          hint="At least 12 characters."
        >
          <TextInput
            id="new-user-password"
            type="password"
            value={password}
            onChange={setPassword}
            invalid={Boolean(fieldErrors.password)}
          />
        </Field>

        <Field label="Role" htmlFor="new-user-role">
          <SelectInput id="new-user-role" value={role} onChange={(v) => setRole(v as Role)}>
            <option value="viewer">viewer</option>
            <option value="operator">operator</option>
            <option value="admin">admin</option>
          </SelectInput>
        </Field>

        <ErrorText msg={general} />
        <ModalFooter
          onCancel={onClose}
          saving={create.isPending}
          disabled={!username || !password}
          savingLabel="Creating…"
          submitLabel="Create user"
        />
      </form>
    </ModalShell>
  );
}

/** Confirms removal of an account before anything is sent. */
function DeleteUserDialog({
  user,
  busy,
  onClose,
  onConfirm,
}: {
  user: User;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <ModalShell onClose={onClose} maxWidth={440}>
      <ModalHeader
        title={`Delete ${user.username}?`}
        subtitle="Local account on this appliance."
        onClose={onClose}
      />
      <div className="stack gap-14">
        <p className="prose">
          The account is removed and can no longer sign in. This cannot be undone.
        </p>
        <div className="row gap-8 end">
          <button type="button" className="btn btn-secondary" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn btn-danger" disabled={busy} onClick={onConfirm}>
            {busy ? 'Deleting…' : 'Delete user'}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}

// ---------------------------------------------------------------------------
// Port groups
// ---------------------------------------------------------------------------

/**
 * Engine lifecycle.
 *
 * A group's engine is normally brought up on demand when a run starts, but an
 * operator who has just rebound a NIC wants to know the engine comes back
 * before committing a test to it.
 */
function PortGroupsPanel() {
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);

  const groups = useQuery({
    queryKey: queryKeys.portGroups,
    queryFn: ({ signal }) => api.portGroups.list(signal),
    // Bring-up is not instant; poll only while something is mid-transition.
    refetchInterval: (query) =>
      (query.state.data ?? []).some((g) => g.state === 'starting') ? 2_000 : false,
  });

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.portGroups });
    void queryClient.invalidateQueries({ queryKey: queryKeys.ports });
  };

  const start = useMutation({
    mutationFn: (id: string) => api.portGroups.start(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const stop = useMutation({
    mutationFn: (id: string) => api.portGroups.stop(id),
    onSuccess: invalidate,
    onError: (e) => setError(describe(e)),
  });

  const rows = groups.data ?? [];
  const busy = start.isPending || stop.isPending;

  return (
    <>
      {error ? <Alert tone="danger">{error}</Alert> : null}

      <Surface title="Port groups" padded={false}>
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Mode</th>
                <th>State</th>
                <th className="num">Ports</th>
                <th>Detail</th>
                <th className="right">Engine</th>
              </tr>
            </thead>
            {groups.isLoading ? (
              <TableSkeleton columns={6} rows={2} />
            ) : (
              <tbody>
                {rows.length === 0 ? (
                  <EmptyRow columns={6}>
                    No port groups. Create one from the ports page to give an engine something to
                    drive.
                  </EmptyRow>
                ) : (
                  rows.map((group) => (
                    <PortGroupRow
                      key={group.id}
                      group={group}
                      busy={busy}
                      onStart={() => {
                        setError(null);
                        start.mutate(group.id);
                      }}
                      onStop={() => {
                        setError(null);
                        stop.mutate(group.id);
                      }}
                    />
                  ))
                )}
              </tbody>
            )}
          </table>
        </div>
      </Surface>
    </>
  );
}

/** One port group row. */
function PortGroupRow({
  group,
  busy,
  onStart,
  onStop,
}: {
  group: PortGroup;
  busy: boolean;
  onStart: () => void;
  onStop: () => void;
}) {
  const up = group.state === 'ready' || group.state === 'starting';
  const empty = group.portIds.length === 0;

  return (
    <tr>
      <td className="mono">{group.name}</td>
      <td>
        <Badge tone={group.engineMode === 'astf' ? 'info' : 'muted'}>{group.engineMode}</Badge>
      </td>
      <td>
        <GroupStateBadge state={group.state} />
      </td>
      <td className="num">{group.portIds.length}</td>
      <td className="dim" style={{ maxWidth: 320 }}>
        {group.error ?? <Dash />}
      </td>
      <td>
        <div className="row gap-6 end">
          {up ? (
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={busy}
              onClick={onStop}
            >
              <Square size={13} />
              Stop
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={busy || empty}
              title={empty ? 'This group has no member ports' : 'Bring the engine up'}
              onClick={onStart}
            >
              <Play size={13} />
              Start
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

// ---------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------

/**
 * Certificate management.
 *
 * The listener is bound once at startup, so installing or removing a
 * certificate is a two-step operation and the panel says so rather than
 * pretending the change is live.
 */
function TlsPanel() {
  const queryClient = useQueryClient();
  const [certificate, setCertificate] = useState('');
  const [privateKey, setPrivateKey] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: ({ signal }) => api.settings.list(signal),
  });

  const tls = tlsSettingSchema.safeParse(
    settings.data?.find((s) => s.key === 'tls')?.value,
  ).data;

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: queryKeys.settings });

  const upload = useMutation({
    mutationFn: () => api.settings.uploadTls(certificate, privateKey),
    onSuccess: (result) => {
      setCertificate('');
      setPrivateKey('');
      setError(null);
      setNotice(`Installed (${result.subject}). Restart fluxd for it to take effect.`);
      invalidate();
    },
    onError: (e) => {
      setNotice(null);
      setError(describe(e));
    },
  });

  const remove = useMutation({
    mutationFn: () => api.settings.removeTls(),
    onSuccess: () => {
      setError(null);
      setNotice('Removed. Restart fluxd to return to plain HTTP.');
      invalidate();
    },
    onError: (e) => {
      setNotice(null);
      setError(describe(e));
    },
  });

  return (
    <Surface
      title="TLS"
      actions={
        tls?.enabled ? (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            disabled={remove.isPending}
            onClick={() => remove.mutate()}
          >
            Remove certificate
          </button>
        ) : null
      }
    >
      <div className="stack gap-14">
        {error ? <Alert tone="danger">{error}</Alert> : null}
        {notice ? <Alert tone="warn">{notice}</Alert> : null}

        {tls?.enabled ? (
          <div className="row gap-10">
            <Lock size={16} style={{ color: 'var(--qz-accent)' }} />
            <span className="prose">
              A certificate is installed
              {tls.subject ? (
                <>
                  {' for '}
                  <span className="mono">{tls.subject}</span>
                </>
              ) : null}
              {tls.notAfter ? `, valid until ${formatTimestamp(tls.notAfter)}` : ''}.
            </span>
          </div>
        ) : (
          <p className="prose">
            This appliance is serving plain HTTP. The session cookie it issues is a bearer
            credential, so install a certificate before putting it on a shared network.
          </p>
        )}

        <label className="field">
          <span className="field-label">Certificate chain (PEM, leaf first)</span>
          <textarea
            className="input input-mono"
            rows={5}
            spellCheck={false}
            style={{ resize: 'vertical' }}
            placeholder="-----BEGIN CERTIFICATE-----"
            value={certificate}
            onChange={(e) => setCertificate(e.target.value)}
          />
        </label>

        <label className="field">
          <span className="field-label">Private key (PEM)</span>
          <textarea
            className="input input-mono"
            rows={5}
            spellCheck={false}
            style={{ resize: 'vertical' }}
            placeholder="-----BEGIN PRIVATE KEY-----"
            value={privateKey}
            onChange={(e) => setPrivateKey(e.target.value)}
          />
          <span className="field-hint">
            Written owner-readable only and never returned by the API.
          </span>
        </label>

        <div>
          <button
            type="button"
            className="btn btn-primary"
            disabled={upload.isPending || !certificate.trim() || !privateKey.trim()}
            onClick={() => {
              setError(null);
              setNotice(null);
              upload.mutate();
            }}
          >
            <Upload size={14} />
            {upload.isPending ? 'Installing…' : 'Install certificate'}
          </button>
        </div>
      </div>
    </Surface>
  );
}

// ---------------------------------------------------------------------------
// Appliance identity and retention
// ---------------------------------------------------------------------------

/** Who owns this appliance, and how long its results are kept. */
function AppliancePanel() {
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: ({ signal }) => api.settings.list(signal),
  });

  const [form, setForm] = useState({
    hostname: '',
    location: '',
    contact: '',
    runDays: 90,
    seriesDays: 30,
  });

  // Seed the form from the server exactly once per fetch. Editing is local
  // until Save, so a background refetch must not overwrite typing — keying the
  // effect on `dataUpdatedAt` only reseeds when new data actually lands.
  const seededAt = useRef(0);
  useEffect(() => {
    if (!settings.data || settings.dataUpdatedAt === seededAt.current) return;
    seededAt.current = settings.dataUpdatedAt;

    const appliance =
      applianceSettingSchema.safeParse(settings.data.find((s) => s.key === 'appliance')?.value)
        .data ?? applianceSettingSchema.parse({});
    const retention =
      retentionSettingSchema.safeParse(settings.data.find((s) => s.key === 'retention')?.value)
        .data ?? retentionSettingSchema.parse({});

    setForm({
      hostname: appliance.hostname ?? '',
      location: appliance.location ?? '',
      contact: appliance.contact ?? '',
      runDays: retention.runDays,
      seriesDays: retention.seriesDays,
    });
  }, [settings.data, settings.dataUpdatedAt]);

  const save = useMutation({
    mutationFn: async () => {
      await api.settings.put('appliance', {
        hostname: form.hostname.trim() || null,
        location: form.location.trim() || null,
        contact: form.contact.trim() || null,
      });
      await api.settings.put('retention', {
        runDays: form.runDays,
        seriesDays: form.seriesDays,
      });
    },
    onSuccess: () => {
      setError(null);
      setSaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.settings });
    },
    onError: (e) => {
      setSaved(false);
      setError(describe(e));
    },
  });

  const field = (key: keyof typeof form, value: string | number) => {
    setSaved(false);
    setForm((f) => ({ ...f, [key]: value }));
  };

  return (
    <Surface title="Appliance">
      <div className="stack gap-14">
        {error ? <Alert tone="danger">{error}</Alert> : null}

        <div className="field-grid">
          <label className="field">
            <span className="field-label">Hostname</span>
            <input
              className="input"
              placeholder="not set"
              value={form.hostname}
              onChange={(e) => field('hostname', e.target.value)}
            />
          </label>

          <label className="field">
            <span className="field-label">Location</span>
            <input
              className="input"
              placeholder="not set"
              value={form.location}
              onChange={(e) => field('location', e.target.value)}
            />
          </label>

          <label className="field">
            <span className="field-label">Contact</span>
            <input
              className="input"
              placeholder="not set"
              value={form.contact}
              onChange={(e) => field('contact', e.target.value)}
            />
            <span className="field-hint">
              Printed on reports, so a result can be traced back to the rack that produced it.
            </span>
          </label>
        </div>

        <div className="field-grid">
          <label className="field">
            <span className="field-label">Keep runs for (days)</span>
            <input
              className="input input-mono"
              type="number"
              min={1}
              value={form.runDays}
              onChange={(e) => field('runDays', Number(e.target.value))}
            />
          </label>

          <label className="field">
            <span className="field-label">Keep time series for (days)</span>
            <input
              className="input input-mono"
              type="number"
              min={1}
              value={form.seriesDays}
              onChange={(e) => field('seriesDays', Number(e.target.value))}
            />
            <span className="field-hint">
              Series outlive nothing: a run whose samples have aged out still shows its summary.
            </span>
          </label>
        </div>

        <div className="row gap-12">
          <button
            type="button"
            className="btn btn-primary"
            disabled={save.isPending}
            onClick={() => save.mutate()}
          >
            {save.isPending ? 'Saving…' : 'Save'}
          </button>
          {saved ? <p className="note">Saved.</p> : null}
        </div>
      </div>
    </Surface>
  );
}

// ---------------------------------------------------------------------------
// Configuration transfer
// ---------------------------------------------------------------------------

/** Export and import of the appliance's configuration. */
function TransferPanel() {
  const queryClient = useQueryClient();
  const picker = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [summary, setSummary] = useState<ImportSummary | null>(null);

  const load = useMutation({
    mutationFn: async (file: File) => {
      const text = await file.text();
      return api.settings.import(JSON.parse(text) as unknown);
    },
    onSuccess: (result) => {
      setError(null);
      setSummary(result);
      for (const key of [queryKeys.flows, queryKeys.loadProfiles, queryKeys.tests, queryKeys.settings]) {
        void queryClient.invalidateQueries({ queryKey: key });
      }
    },
    onError: (e) => {
      setSummary(null);
      setError(e instanceof SyntaxError ? 'That file is not valid JSON.' : describe(e));
    },
  });

  const skipped = summary
    ? summary.flowsSkipped + summary.profilesSkipped + summary.testsSkipped
    : 0;

  return (
    <Surface title="Configuration">
      <div className="stack gap-14">
        <p className="prose">
          Flows, load profiles, tests, and settings travel as one JSON bundle, cross-referenced
          by name. Accounts, sessions, run history, and TLS material are deliberately left out —
          none of them are safe or meaningful to copy between appliances.
        </p>

        {error ? <Alert tone="danger">{error}</Alert> : null}

        {summary ? (
          <Alert tone={summary.problems.length > 0 ? 'warn' : 'info'}>
            <div>
              <strong>
                Imported {summary.flowsCreated} flows, {summary.profilesCreated} profiles,{' '}
                {summary.testsCreated} tests.
              </strong>
              {skipped > 0 ? (
                <p className="note" style={{ marginTop: 2 }}>
                  {skipped} skipped because that name already exists — import never overwrites.
                </p>
              ) : null}
              {summary.problems.length > 0 ? (
                <ul style={{ margin: '4px 0 0 16px', padding: 0 }}>
                  {summary.problems.map((problem) => (
                    <li key={problem} className="note">
                      {problem}
                    </li>
                  ))}
                </ul>
              ) : null}
            </div>
          </Alert>
        ) : null}

        <div className="row gap-10">
          <a
            className="btn btn-secondary"
            href={api.settings.exportUrl()}
            download="flux-config.json"
          >
            <Download size={14} />
            Export
          </a>

          <input
            ref={picker}
            type="file"
            accept="application/json,.json"
            style={{ display: 'none' }}
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) load.mutate(file);
              // Cleared so re-picking the same file fires `change` again.
              e.target.value = '';
            }}
          />
          <button
            type="button"
            className="btn btn-secondary"
            disabled={load.isPending}
            onClick={() => picker.current?.click()}
          >
            <Upload size={14} />
            {load.isPending ? 'Importing…' : 'Import'}
          </button>
        </div>
      </div>
    </Surface>
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
          <p className="note">{TAGLINE}</p>
        </div>

        <dl className="qz-facts">
          <dt>Daemon version</dt>
          <dd className="mono">{health.data?.version ?? <Dash />}</dd>

          <dt>Uptime</dt>
          <dd className="mono">{health.data ? formatDuration(health.data.uptimeSecs) : <Dash />}</dd>

          <dt>Packet engine</dt>
          <dd className="mono">{health.data?.engine.backend ?? <Dash />}</dd>

          <dt>Port controller</dt>
          <dd className="mono">{health.data?.portd.backend ?? <Dash />}</dd>
        </dl>
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
