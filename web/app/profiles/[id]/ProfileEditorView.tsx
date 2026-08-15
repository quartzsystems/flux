'use client';

/**
 * The load profile editor, on its own page — the L4-7 counterpart of the flow
 * editor.
 *
 * The editor previews continuously, and the figure that matters is the implied
 * throughput. A connection rate and a response size that each look reasonable
 * multiply into something that may be well past the link, and seeing that here
 * is far better than seeing a run quietly top out.
 *
 * The id comes from the path (`/profiles/<id>/`), with `new` meaning a profile
 * that does not exist yet.
 */

import { ArrowLeft, Save, Trash2 } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { usePathname, useRouter } from 'next/navigation';
import { useEffect, useMemo, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { SummaryBar } from '@/components/SummaryBar';
import {
  Alert,
  Badge,
  Page,
  PageBody,
  PageHeader,
  Skeleton,
  Surface,
} from '@/components/ui';
import { ModalHeader, ModalShell } from '@/components/ui/Modal';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  defaultLoadProfile,
  loadProfileConfigSchema,
  type AppSpec,
  type LoadProfileConfig,
  type Port,
  type ProfilePreview,
} from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import { formatBitrate, formatCount, formatDuration } from '@/lib/format';

/** How long to wait after a keystroke before previewing. */
const PREVIEW_DEBOUNCE_MS = 250;

/**
 * Extracts the profile id from `/profiles/<id>/`.
 *
 * Returns an empty string for a path that carries no id, which makes the query
 * fail cleanly with "not found" rather than requesting `/load-profiles/undefined`.
 */
function profileIdFrom(pathname: string): string {
  const segments = pathname.split('/').filter(Boolean);
  const index = segments.indexOf('profiles');
  return index >= 0 ? (segments[index + 1] ?? '') : '';
}

export function ProfileEditorView() {
  return (
    <AppShell>
      <ProfileEditorPage />
    </AppShell>
  );
}

function ProfileEditorPage() {
  const pathname = usePathname();
  const router = useRouter();

  const id = profileIdFrom(pathname);
  const isNew = id === 'new';

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  // The API has no single-profile read, so the record comes from the list —
  // which the table page has almost always already cached.
  const profiles = useQuery({
    queryKey: queryKeys.loadProfiles,
    queryFn: ({ signal }) => api.loadProfiles.list(signal),
    enabled: !isNew,
  });

  const profile = (profiles.data ?? []).find((p) => p.id === id) ?? null;

  const close = () => router.push('/profiles/');

  /** The editor's starting value, or a reason there is none. */
  const initial = useMemo(() => {
    if (isNew) {
      const available = ports.data ?? [];
      const client = available[0]?.id ?? '';
      const server = available[1]?.id ?? available[0]?.id ?? '';
      if (!client) return null;
      return { name: '', config: defaultLoadProfile(client, server) };
    }
    if (!profile) return null;
    const parsed = loadProfileConfigSchema.safeParse(profile.config);
    if (!parsed.success) return null;
    return { name: profile.name, config: parsed.data };
  }, [isNew, ports.data, profile]);

  const loading = ports.isLoading || (!isNew && profiles.isLoading);

  return (
    <Page>
      <PageHeader
        title={isNew ? 'New profile' : (profile?.name ?? 'Edit profile')}
        subtitle="Stateful L4-7 traffic"
        actions={
          <button type="button" className="btn btn-ghost btn-sm" onClick={close}>
            <ArrowLeft size={14} />
            All profiles
          </button>
        }
      />
      <PageBody>
        {loading ? (
          <Skeleton height={220} />
        ) : profiles.error ? (
          <Alert tone="danger">
            {profiles.error instanceof ApiError
              ? profiles.error.message
              : 'The profile could not be loaded.'}
          </Alert>
        ) : !isNew && !profile ? (
          <Alert tone="warn">No load profile with this id exists on this appliance.</Alert>
        ) : !initial ? (
          <Alert tone="warn">
            {isNew
              ? 'A profile needs a client port and a server port, and no ports have been discovered yet.'
              : 'This profile was written by a different version of Flux and cannot be shown here.'}
          </Alert>
        ) : (
          <ProfileEditor
            key={isNew ? 'new' : id}
            profileId={isNew ? null : id}
            initial={initial}
            ports={ports.data ?? []}
            onClose={close}
          />
        )}
      </PageBody>
    </Page>
  );
}

/** The profile editor. */
function ProfileEditor({
  profileId,
  initial,
  ports,
  onClose,
}: {
  profileId: string | null;
  initial: { name: string; config: LoadProfileConfig };
  ports: Port[];
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const { can } = useAuth();

  const [name, setName] = useState(initial.name);
  const [config, setConfig] = useState<LoadProfileConfig>(initial.config);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [banner, setBanner] = useState<string | null>(null);
  const [preview, setPreview] = useState<ProfilePreview | null>(null);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  const body = useMemo(() => ({ name: name || 'unnamed', config }), [name, config]);

  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      api.loadProfiles
        .preview(body, controller.signal)
        .then((result) => {
          setPreview(result);
          setErrors({});
        })
        .catch((err: unknown) => {
          if (err instanceof DOMException && err.name === 'AbortError') return;
          if (err instanceof ApiError) {
            setErrors(Object.fromEntries(err.fieldErrors.map((f) => [f.path, f.msg])));
          }
          setPreview(null);
        });
    }, PREVIEW_DEBOUNCE_MS);

    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [body]);

  const save = useMutation({
    mutationFn: () =>
      profileId
        ? api.loadProfiles.update(profileId, { name, config })
        : api.loadProfiles.create({ name, config }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.loadProfiles });
      onClose();
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError && err.fieldErrors.length > 0) {
        setErrors(Object.fromEntries(err.fieldErrors.map((f) => [f.path, f.msg])));
        setBanner('Some fields need attention.');
      } else if (err instanceof ApiError) {
        setBanner(err.message);
      }
    },
  });

  const remove = useMutation({
    mutationFn: () => api.loadProfiles.remove(profileId ?? ''),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.loadProfiles });
      onClose();
    },
    onError: (err: unknown) => {
      // Close the confirm dialog so the banner explaining the refusal shows.
      setConfirmingDelete(false);
      if (err instanceof ApiError) setBanner(err.message);
    },
  });

  /** Updates one top-level field. */
  const set = <K extends keyof LoadProfileConfig>(key: K, value: LoadProfileConfig[K]) => {
    setConfig((previous) => ({ ...previous, [key]: value }));
  };

  const error = (path: string) => errors[`config.${path}`];

  return (
    <div className="stack gap-14">
      <Alert tone="info">
        A profile emulates clients opening connections to servers Flux also emulates. It needs a
        port group in <strong>stateful</strong> mode — a stateless instance cannot run one.
      </Alert>

      <Surface
        title={profileId ? 'Edit profile' : 'New profile'}
        actions={
          <>
            {profileId && can('operator') ? (
              <button
                type="button"
                className="btn btn-ghost btn-sm btn-icon"
                title="Delete this profile"
                disabled={remove.isPending}
                onClick={() => setConfirmingDelete(true)}
              >
                <Trash2 size={14} />
              </button>
            ) : null}
            <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
              Cancel
            </button>
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={!can('operator') || save.isPending || !name.trim()}
              onClick={() => save.mutate()}
            >
              <Save size={14} />
              {save.isPending ? 'Saving…' : 'Save'}
            </button>
          </>
        }
      >
        <div className="stack gap-18">
          {banner ? <Alert tone="danger">{banner}</Alert> : null}

          <div className="field-grid">
            <label className="field">
              <span className="field-label">Name</span>
              <input
                className="input"
                value={name}
                autoFocus
                aria-invalid={Boolean(errors.name)}
                onChange={(e) => setName(e.target.value)}
              />
              {errors.name ? <span className="field-error">{errors.name}</span> : null}
            </label>

            <label className="field">
              <span className="field-label">Client port</span>
              <select
                className="select"
                value={config.clientPort}
                onChange={(e) => set('clientPort', e.target.value)}
              >
                {ports.map((port) => (
                  <option key={port.id} value={port.id}>
                    {port.name} — {port.pciAddr}
                  </option>
                ))}
              </select>
              {error('clientPort') ? (
                <span className="field-error">{error('clientPort')}</span>
              ) : null}
            </label>

            <label className="field">
              <span className="field-label">Server port</span>
              <select
                className="select"
                value={config.serverPort}
                onChange={(e) => set('serverPort', e.target.value)}
              >
                {ports.map((port) => (
                  <option key={port.id} value={port.id}>
                    {port.name} — {port.pciAddr}
                  </option>
                ))}
              </select>
              {error('serverPort') ? (
                <span className="field-error">{error('serverPort')}</span>
              ) : null}
            </label>
          </div>
        </div>
      </Surface>

      <Surface title="Address pools">
        <div className="stack gap-14">
          <PoolEditor
            label="Clients"
            pool={config.clientPool}
            onChange={(pool) => set('clientPool', pool)}
            error={error('clientPool') ?? error('clientPool.cidr')}
            hint={
              preview
                ? `${formatCount(preview.clientCapacity)} address/port pairs available`
                : undefined
            }
          />
          <PoolEditor
            label="Servers"
            pool={config.serverPool}
            onChange={(pool) => set('serverPool', pool)}
            error={error('serverPool') ?? error('serverPool.cidr')}
            hint={
              preview ? `${formatCount(preview.serverAddresses)} addresses available` : undefined
            }
          />
        </div>
      </Surface>

      <Surface title="Application">
        <AppEditor app={config.app} onChange={(app) => set('app', app)} errors={errors} />
      </Surface>

      <Surface title="Rate and ramp">
        <div className="field-grid">
          <label className="field">
            <span className="field-label">Connections per second</span>
            <input
              className="input input-mono"
              value={config.targetCps}
              aria-invalid={Boolean(error('targetCps'))}
              onChange={(e) => set('targetCps', Number(e.target.value) || 0)}
            />
            {error('targetCps') ? (
              <span className="field-error">{error('targetCps')}</span>
            ) : null}
          </label>

          <label className="field">
            <span className="field-label">Maximum concurrent</span>
            <input
              className="input input-mono"
              value={config.maxConcurrent}
              aria-invalid={Boolean(error('maxConcurrent'))}
              onChange={(e) => set('maxConcurrent', Number(e.target.value) || 0)}
            />
            {error('maxConcurrent') ? (
              <span className="field-error">{error('maxConcurrent')}</span>
            ) : null}
          </label>

          <label className="field">
            <span className="field-label">Warm-up (seconds)</span>
            <input
              className="input input-mono"
              value={config.ramp.warmupSecs}
              onChange={(e) =>
                set('ramp', { ...config.ramp, warmupSecs: Number(e.target.value) || 0 })
              }
            />
            <span className="field-hint">
              Climbing to the target rate rather than stepping to it.
            </span>
          </label>

          <label className="field">
            <span className="field-label">Settle (seconds)</span>
            <input
              className="input input-mono"
              value={config.ramp.settleSecs}
              onChange={(e) =>
                set('ramp', { ...config.ramp, settleSecs: Number(e.target.value) || 0 })
              }
            />
            <span className="field-hint">
              Ignored after the warm-up; measuring through it reports a transient.
            </span>
          </label>

          <label className="field">
            <span className="field-label">Duration (seconds)</span>
            <input
              className="input input-mono"
              placeholder="until stopped"
              value={config.durationSecs ?? ''}
              onChange={(e) => {
                const raw = e.target.value.trim();
                set('durationSecs', raw === '' ? null : Number(raw));
              }}
            />
          </label>
        </div>
      </Surface>

      <ProfileSummary preview={preview} />

      {confirmingDelete ? (
        <DeleteProfileDialog
          name={name}
          pending={remove.isPending}
          onCancel={() => setConfirmingDelete(false)}
          onConfirm={() => remove.mutate()}
        />
      ) : null}
    </div>
  );
}

/** The confirmation a delete has to pass through before anything is sent. */
function DeleteProfileDialog({
  name,
  pending,
  onCancel,
  onConfirm,
}: {
  name: string;
  pending: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <ModalShell onClose={onCancel} maxWidth={440}>
      <ModalHeader
        title={`Delete ${name.trim() || 'this profile'}?`}
        subtitle="Stored load profile."
        onClose={onCancel}
      />
      <div className="stack gap-14">
        <p className="text-[13px] text-[var(--qz-fg-3)] m-0">
          The definition is removed from this instance. A test that references it will no
          longer be able to run it, and there is no undo.
        </p>
        <div className="row end gap-8">
          <button type="button" className="btn btn-secondary" disabled={pending} onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="btn btn-danger" disabled={pending} onClick={onConfirm}>
            <Trash2 size={14} />
            {pending ? 'Deleting…' : 'Delete profile'}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}

/** One address pool, under a caption naming the side it emulates. */
function PoolEditor({
  label,
  pool,
  onChange,
  error,
  hint,
}: {
  label: string;
  pool: LoadProfileConfig['clientPool'];
  onChange: (pool: LoadProfileConfig['clientPool']) => void;
  error?: string;
  hint?: string;
}) {
  return (
    <div className="stack gap-8">
      <h4 className="field-label">{label}</h4>
      <div className="field-grid">
        <label className="field">
          <span className="field-label">Address block</span>
          <input
            className="input input-mono"
            value={pool.cidr}
            aria-invalid={Boolean(error)}
            onChange={(e) => onChange({ ...pool, cidr: e.target.value })}
          />
          {error ? <span className="field-error">{error}</span> : null}
          {!error && hint ? <span className="field-hint">{hint}</span> : null}
        </label>

        <label className="field">
          <span className="field-label">Lowest port</span>
          <input
            className="input input-mono"
            value={pool.portMin}
            onChange={(e) => onChange({ ...pool, portMin: Number(e.target.value) || 0 })}
          />
        </label>

        <label className="field">
          <span className="field-label">Highest port</span>
          <input
            className="input input-mono"
            value={pool.portMax}
            onChange={(e) => onChange({ ...pool, portMax: Number(e.target.value) || 0 })}
          />
        </label>
      </div>
    </div>
  );
}

/** The application the two sides speak. */
function AppEditor({
  app,
  onChange,
  errors,
}: {
  app: AppSpec;
  onChange: (app: AppSpec) => void;
  errors: Record<string, string>;
}) {
  return (
    <div className="field-grid">
      <label className="field">
        <span className="field-label">Application</span>
        <select
          className="select"
          value={app.type}
          onChange={(e) => {
            const type = e.target.value as AppSpec['type'];
            if (type === 'http_get') onChange({ type: 'http_get', path: '/', responseBytes: 32_768 });
            else if (type === 'raw')
              onChange({ type: 'raw', requestBytes: 200, responseBytes: 32_768 });
            else onChange({ type: 'pcap', pcapRef: '' });
          }}
        >
          <option value="http_get">HTTP GET</option>
          <option value="raw">Raw request and response</option>
          <option value="pcap">Replay a capture</option>
        </select>
      </label>

      {app.type === 'http_get' ? (
        <>
          <label className="field">
            <span className="field-label">Path</span>
            <input
              className="input input-mono"
              value={app.path}
              aria-invalid={Boolean(errors['config.app.path'])}
              onChange={(e) => onChange({ ...app, path: e.target.value })}
            />
            {errors['config.app.path'] ? (
              <span className="field-error">{errors['config.app.path']}</span>
            ) : null}
          </label>
          <label className="field">
            <span className="field-label">Response body (bytes)</span>
            <input
              className="input input-mono"
              value={app.responseBytes}
              onChange={(e) => onChange({ ...app, responseBytes: Number(e.target.value) || 0 })}
            />
          </label>
        </>
      ) : null}

      {app.type === 'raw' ? (
        <>
          <label className="field">
            <span className="field-label">Request (bytes)</span>
            <input
              className="input input-mono"
              value={app.requestBytes}
              onChange={(e) => onChange({ ...app, requestBytes: Number(e.target.value) || 0 })}
            />
          </label>
          <label className="field">
            <span className="field-label">Response (bytes)</span>
            <input
              className="input input-mono"
              value={app.responseBytes}
              onChange={(e) => onChange({ ...app, responseBytes: Number(e.target.value) || 0 })}
            />
          </label>
        </>
      ) : null}

      {app.type === 'pcap' ? (
        <label className="field" style={{ gridColumn: '1 / -1' }}>
          <span className="field-label">Capture name</span>
          <input
            className="input input-mono"
            value={app.pcapRef}
            aria-invalid={Boolean(errors['config.app.pcapRef'])}
            onChange={(e) => onChange({ type: 'pcap', pcapRef: e.target.value })}
          />
          <span className="field-hint">
            Replay is not implemented in this build; a run using it is refused by name rather
            than quietly substituting a synthetic exchange.
          </span>
        </label>
      ) : null}
    </div>
  );
}

/** The computed summary bar, on the shared chassis. */
function ProfileSummary({ preview }: { preview: ProfilePreview | null }) {
  if (!preview) {
    return <SummaryBar loading />;
  }

  // The preview does not carry a resolved percentage the way a flow's does,
  // so it is derived here; the badge tones come from the shared rule.
  const badge = preview.exceedsLineRate ? (
    <Badge tone="crit">exceeds the client port</Badge>
  ) : preview.clientPortSpeedMbps > 0 ? undefined : (
    <Badge tone="muted">port speed unknown</Badge>
  );

  return (
    <SummaryBar
      items={[
        {
          value: preview.summary,
          meta: (
            <>
              <span>{formatBitrate(preview.impliedBps)}</span>
              <span>{formatCount(preview.clientCapacity)} client tuples</span>
              <span>measure after {formatDuration(preview.measurementStartsAt)}</span>
            </>
          ),
        },
      ]}
      linePct={
        badge ? undefined : (preview.impliedBps / (preview.clientPortSpeedMbps * 1e6)) * 100
      }
      badge={badge}
    />
  );
}
