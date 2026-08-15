'use client';

/**
 * The flow editor, on its own page.
 *
 * The editor previews continuously. Every change is sent to `/flows/preview`,
 * which returns the frame bytes, the resolved rate, and the summary line — all
 * computed by the same code that will programme the engine, so what an operator
 * sees here is what will go on the wire.
 *
 * The id comes from the path (`/flows/<id>/`), with `new` meaning a flow that
 * does not exist yet — the same placeholder-document arrangement the run view
 * uses, so a flow is linkable.
 */

import { ArrowLeft, Save, Import, Plus, Trash2 } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { usePathname, useRouter } from 'next/navigation';
import { useEffect, useMemo, useRef, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { HeaderStack } from '@/components/HeaderStack';
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
import { CheckRow } from '@/components/ui/formkit';
import { ModalHeader, ModalShell } from '@/components/ui/Modal';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  flowConfigSchema,
  type FlowConfig,
  type FlowPreview,
  type Modifier,
  type ModifierField,
  type Port,
} from '@/lib/api-types';
import { useAuth } from '@/lib/auth';
import {
  MODIFIER_LABELS,
  defaultFlow,
  defaultModifier,
  modifierApplies,
} from '@/lib/flow-defaults';
import { formatBitrate, formatPps } from '@/lib/format';

/** How long to wait after a keystroke before previewing. */
const PREVIEW_DEBOUNCE_MS = 250;

/**
 * Extracts the flow id from `/flows/<id>/`.
 *
 * Returns an empty string for a path that carries no id, which makes the query
 * fail cleanly with "not found" rather than requesting `/flows/undefined`.
 */
function flowIdFrom(pathname: string): string {
  const segments = pathname.split('/').filter(Boolean);
  const index = segments.indexOf('flows');
  return index >= 0 ? (segments[index + 1] ?? '') : '';
}

export function FlowEditorView() {
  return (
    <AppShell>
      <FlowEditorPage />
    </AppShell>
  );
}

function FlowEditorPage() {
  const pathname = usePathname();
  const router = useRouter();

  // Read the id from the path rather than from the router params: this route
  // is exported once under a placeholder segment, so the URL is the truth.
  const id = flowIdFrom(pathname);
  const isNew = id === 'new';

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  const flow = useQuery({
    queryKey: queryKeys.flow(id),
    queryFn: ({ signal }) => api.flows.get(id, signal),
    enabled: !isNew && id !== '',
  });

  const close = () => router.push('/flows/');

  /** The editor's starting value, or a reason there is none. */
  const initial = useMemo(() => {
    if (isNew) {
      const available = ports.data ?? [];
      const tx = available[0]?.id ?? '';
      const rx = available[1]?.id ?? available[0]?.id ?? '';
      if (!tx) return null;
      return { name: '', config: defaultFlow(tx, rx) };
    }
    if (!flow.data) return null;
    const parsed = flowConfigSchema.safeParse(flow.data.config);
    // A flow this build cannot read is better refused than silently
    // reinterpreted into something the operator did not configure.
    if (!parsed.success) return null;
    return { name: flow.data.name, config: parsed.data };
  }, [isNew, ports.data, flow.data]);

  const loading = ports.isLoading || (!isNew && flow.isLoading);

  return (
    <Page>
      <PageHeader
        title={isNew ? 'New flow' : (flow.data?.name ?? 'Edit flow')}
        subtitle="Traffic definition"
        actions={
          <button type="button" className="btn btn-ghost btn-sm" onClick={close}>
            <ArrowLeft size={14} />
            All flows
          </button>
        }
      />
      <PageBody>
        {loading ? (
          <Skeleton height={220} />
        ) : flow.error ? (
          <Alert tone="danger">
            {flow.error instanceof ApiError ? flow.error.message : 'The flow could not be loaded.'}
          </Alert>
        ) : !initial ? (
          <Alert tone="warn">
            {isNew
              ? 'No ports have been discovered yet, so a flow has nothing to transmit from.'
              : 'This flow was written by a different version of Flux and cannot be shown here.'}
          </Alert>
        ) : (
          <FlowEditor
            key={isNew ? 'new' : id}
            flowId={isNew ? null : id}
            initial={initial}
            ports={ports.data ?? []}
            onClose={close}
          />
        )}
      </PageBody>
    </Page>
  );
}

/** Props for {@link FlowEditor}. */
interface EditorProps {
  flowId: string | null;
  initial: { name: string; config: FlowConfig };
  ports: Port[];
  onClose: () => void;
}

/** The flow editor. */
function FlowEditor({ flowId, initial, ports, onClose }: EditorProps) {
  const queryClient = useQueryClient();
  const { can } = useAuth();

  const [name, setName] = useState(initial.name);
  const [config, setConfig] = useState<FlowConfig>(initial.config);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [banner, setBanner] = useState<string | null>(null);
  const [preview, setPreview] = useState<FlowPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  // Previewing on every keystroke would send a request per character; the
  // debounce keeps it to one per pause without making the summary feel stale.
  const body = useMemo(() => ({ name: name || 'unnamed', config }), [name, config]);

  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      api.flows
        .preview(body, controller.signal)
        .then((result) => {
          setPreview(result);
          setPreviewError(null);
          setErrors({});
        })
        .catch((err: unknown) => {
          if (err instanceof DOMException && err.name === 'AbortError') return;
          if (err instanceof ApiError) {
            setErrors(Object.fromEntries(err.fieldErrors.map((f) => [f.path, f.msg])));
            setPreviewError(err.fieldErrors.length > 0 ? null : err.message);
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
      flowId
        ? api.flows.update(flowId, { name, config })
        : api.flows.create({ name, config }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.flows });
      setBanner(null);
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
    mutationFn: () => api.flows.remove(flowId ?? ''),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.flows });
      onClose();
    },
    onError: (err: unknown) => {
      // Close the confirm dialog so the banner explaining the refusal shows.
      setConfirmingDelete(false);
      if (err instanceof ApiError) setBanner(err.message);
    },
  });

  /** Updates one top-level field of the config. */
  const set = <K extends keyof FlowConfig>(key: K, value: FlowConfig[K]) => {
    setConfig((previous) => ({ ...previous, [key]: value }));
  };

  return (
    <div className="stack gap-14">
      <Surface
        title={flowId ? 'Edit flow' : 'New flow'}
        actions={
          <>
            {flowId && can('operator') ? (
              <button
                type="button"
                className="btn btn-ghost btn-sm btn-icon"
                title="Delete this flow"
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
              <span className="field-label">Transmit port</span>
              <select
                className="select"
                value={config.txPort}
                onChange={(e) => set('txPort', e.target.value)}
              >
                {ports.map((port) => (
                  <option key={port.id} value={port.id}>
                    {port.name} — {port.pciAddr}
                  </option>
                ))}
              </select>
              {errors['config.txPort'] ? (
                <span className="field-error">{errors['config.txPort']}</span>
              ) : null}
            </label>

            <label className="field">
              <span className="field-label">Receive port</span>
              <select
                className="select"
                value={config.rxPort}
                onChange={(e) => set('rxPort', e.target.value)}
              >
                {ports.map((port) => (
                  <option key={port.id} value={port.id}>
                    {port.name} — {port.pciAddr}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </div>
      </Surface>

      <Surface
        title="Header stack"
        actions={
          <PcapImport
            onImported={(headers) => set('headers', headers)}
            onError={setBanner}
          />
        }
      >
        <HeaderStack
          headers={config.headers}
          onChange={(headers) => set('headers', headers)}
          errors={errors}
        />
      </Surface>

      <Surface title="Size and rate">
        <div className="stack gap-14">
          <SizeEditor
            size={config.size}
            onChange={(size) => set('size', size)}
            errors={errors}
          />
          <RateEditor
            rate={config.rate}
            onChange={(rate) => set('rate', rate)}
            errors={errors}
          />

          <div className="field-grid">
            <label className="field">
              <span className="field-label">Duration (seconds)</span>
              <input
                className="input input-mono"
                placeholder="until stopped"
                value={config.durationSecs ?? ''}
                aria-invalid={Boolean(errors['config.durationSecs'])}
                onChange={(e) => {
                  const raw = e.target.value.trim();
                  set('durationSecs', raw === '' ? null : globalThis.Number(raw));
                }}
              />
              {errors['config.durationSecs'] ? (
                <span className="field-error">{errors['config.durationSecs']}</span>
              ) : null}
            </label>

            <div className="field">
              <CheckRow
                checked={config.latencyTrack}
                onChange={() => set('latencyTrack', !config.latencyTrack)}
              >
                Measure latency
              </CheckRow>
              <span className="field-hint">
                Adds a timestamp to each frame; costs a little transmit capacity.
              </span>
            </div>
          </div>
        </div>
      </Surface>

      <Surface title="Modifiers">
        <ModifierTable
          modifiers={config.modifiers}
          headers={config.headers}
          onChange={(modifiers) => set('modifiers', modifiers)}
          errors={errors}
        />
      </Surface>

      <FlowSummary preview={preview} error={previewError} />

      {preview ? <HexPreview preview={preview} /> : null}

      {confirmingDelete ? (
        <DeleteFlowDialog
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
function DeleteFlowDialog({
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
        title={`Delete ${name.trim() || 'this flow'}?`}
        subtitle="Stored flow definition."
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
            {pending ? 'Deleting…' : 'Delete flow'}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}

/** Frame size controls. */
function SizeEditor({
  size,
  onChange,
  errors,
}: {
  size: FlowConfig['size'];
  onChange: (size: FlowConfig['size']) => void;
  errors: Record<string, string>;
}) {
  return (
    <div className="field-grid">
      <label className="field">
        <span className="field-label">Frame size</span>
        <select
          className="select"
          value={size.type}
          onChange={(e) => {
            const type = e.target.value as FlowConfig['size']['type'];
            if (type === 'fixed') onChange({ type: 'fixed', bytes: 64 });
            else if (type === 'imix') onChange({ type: 'imix', preset: 'simple' });
            else onChange({ type: 'random', min: 64, max: 1518 });
          }}
        >
          <option value="fixed">Fixed</option>
          <option value="imix">IMIX preset</option>
          <option value="random">Random range</option>
        </select>
      </label>

      {size.type === 'fixed' ? (
        <label className="field">
          <span className="field-label">Bytes (including FCS)</span>
          <input
            className="input input-mono"
            value={size.bytes}
            aria-invalid={Boolean(errors['config.size.bytes'] ?? errors['config.size'])}
            onChange={(e) => onChange({ type: 'fixed', bytes: globalThis.Number(e.target.value) || 0 })}
          />
          {errors['config.size.bytes'] ?? errors['config.size'] ? (
            <span className="field-error">
              {errors['config.size.bytes'] ?? errors['config.size']}
            </span>
          ) : null}
        </label>
      ) : null}

      {size.type === 'imix' ? (
        <label className="field">
          <span className="field-label">Mixture</span>
          <select
            className="select"
            value={size.preset}
            onChange={(e) =>
              onChange({ type: 'imix', preset: e.target.value as 'simple' | 'tolly' })
            }
          >
            <option value="simple">Simple — 7:4:1 of 64, 570, 1518</option>
            <option value="tolly">Tolly — 55:5:40 of 64, 594, 1518</option>
          </select>
        </label>
      ) : null}

      {size.type === 'random' ? (
        <>
          <label className="field">
            <span className="field-label">Minimum</span>
            <input
              className="input input-mono"
              value={size.min}
              onChange={(e) =>
                onChange({ type: 'random', min: globalThis.Number(e.target.value) || 0, max: size.max })
              }
            />
          </label>
          <label className="field">
            <span className="field-label">Maximum</span>
            <input
              className="input input-mono"
              value={size.max}
              aria-invalid={Boolean(errors['config.size.max'])}
              onChange={(e) =>
                onChange({ type: 'random', min: size.min, max: globalThis.Number(e.target.value) || 0 })
              }
            />
            {errors['config.size.max'] ? (
              <span className="field-error">{errors['config.size.max']}</span>
            ) : null}
          </label>
        </>
      ) : null}
    </div>
  );
}

/** Rate controls. */
function RateEditor({
  rate,
  onChange,
  errors,
}: {
  rate: FlowConfig['rate'];
  onChange: (rate: FlowConfig['rate']) => void;
  errors: Record<string, string>;
}) {
  const unit = rate.type === 'percent' ? '% of line rate' : rate.type === 'pps' ? 'packets/s' : 'bits/s (L1)';

  return (
    <div className="field-grid">
      <label className="field">
        <span className="field-label">Rate</span>
        <select
          className="select"
          value={rate.type}
          onChange={(e) => {
            const type = e.target.value as FlowConfig['rate']['type'];
            if (type === 'percent') onChange({ type: 'percent', value: 10 });
            else if (type === 'pps') onChange({ type: 'pps', value: 1_000_000 });
            else onChange({ type: 'bps', value: 1e9 });
          }}
        >
          <option value="percent">Percent of line</option>
          <option value="pps">Packets per second</option>
          <option value="bps">Bits per second</option>
        </select>
      </label>

      <label className="field">
        <span className="field-label">{unit}</span>
        <input
          className="input input-mono"
          value={rate.value}
          aria-invalid={Boolean(errors['config.rate.value'])}
          onChange={(e) =>
            onChange({ type: rate.type, value: globalThis.Number(e.target.value) || 0 })
          }
        />
        {errors['config.rate.value'] ? (
          <span className="field-error">{errors['config.rate.value']}</span>
        ) : null}
      </label>
    </div>
  );
}

/** The modifiers table. */
function ModifierTable({
  modifiers,
  headers,
  onChange,
  errors,
}: {
  modifiers: Modifier[];
  headers: FlowConfig['headers'];
  onChange: (modifiers: Modifier[]) => void;
  errors: Record<string, string>;
}) {
  const fields = Object.keys(MODIFIER_LABELS) as ModifierField[];
  const available = fields.filter((f) => modifierApplies(f, headers));

  /** Replaces one modifier. */
  const replace = (index: number, modifier: Modifier) => {
    onChange(modifiers.map((m, i) => (i === index ? modifier : m)));
  };

  return (
    <div className="stack gap-10">
      {modifiers.length > 0 ? (
        <div className="qz-table-wrap">
          <table className="qz-table">
            <thead>
              <tr>
                <th>Field</th>
                <th>Mode</th>
                <th>Count</th>
                <th>Step</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {modifiers.map((modifier, index) => (
                <tr key={index}>
                  <td>
                    <select
                      className="select input-sm"
                      style={{ minWidth: 180 }}
                      value={modifier.field}
                      onChange={(e) =>
                        replace(index, { ...modifier, field: e.target.value as ModifierField })
                      }
                    >
                      {fields.map((field) => (
                        <option
                          key={field}
                          value={field}
                          disabled={!modifierApplies(field, headers)}
                        >
                          {MODIFIER_LABELS[field]}
                          {modifierApplies(field, headers) ? '' : ' — no such layer'}
                        </option>
                      ))}
                    </select>
                    {errors[`config.modifiers.${index}.field`] ? (
                      <div className="field-error">
                        {errors[`config.modifiers.${index}.field`]}
                      </div>
                    ) : null}
                  </td>
                  <td>
                    <select
                      className="select input-sm"
                      value={modifier.mode}
                      onChange={(e) =>
                        replace(index, {
                          ...modifier,
                          mode: e.target.value as Modifier['mode'],
                        })
                      }
                    >
                      <option value="increment">Increment</option>
                      <option value="random">Random</option>
                    </select>
                  </td>
                  <td>
                    <input
                      className="input input-sm input-mono"
                      style={{ width: 100 }}
                      value={modifier.count}
                      aria-invalid={Boolean(errors[`config.modifiers.${index}.count`])}
                      onChange={(e) =>
                        replace(index, { ...modifier, count: globalThis.Number(e.target.value) || 0 })
                      }
                    />
                    {errors[`config.modifiers.${index}.count`] ? (
                      <div className="field-error">
                        {errors[`config.modifiers.${index}.count`]}
                      </div>
                    ) : null}
                  </td>
                  <td>
                    <input
                      className="input input-sm input-mono"
                      style={{ width: 100 }}
                      value={modifier.step}
                      onChange={(e) =>
                        replace(index, { ...modifier, step: globalThis.Number(e.target.value) || 1 })
                      }
                    />
                  </td>
                  <td className="right">
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm btn-icon"
                      title="Remove modifier"
                      onClick={() => onChange(modifiers.filter((_, i) => i !== index))}
                    >
                      <Trash2 size={14} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="note">
          No modifiers. Add one to vary a field across generated frames — this is how one
          flow emulates many hosts.
        </p>
      )}

      <div>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={available.length === 0}
          title={
            available.length === 0
              ? 'Add a header layer first'
              : 'Vary a field across generated frames'
          }
          onClick={() => {
            const first = available[0];
            if (first) onChange([...modifiers, defaultModifier(first)]);
          }}
        >
          <Plus size={14} />
          Add modifier
        </button>
      </div>
    </div>
  );
}

/** The computed summary bar under the editor, on the shared chassis. */
function FlowSummary({ preview, error }: { preview: FlowPreview | null; error: string | null }) {
  if (error) {
    return <Alert tone="danger">{error}</Alert>;
  }

  if (!preview) {
    return <SummaryBar loading />;
  }

  // The percentage badge only means something when the port speed is known and
  // the rate fits on the wire; the other two states get their own badge.
  const badge = preview.exceedsLineRate ? (
    <Badge tone="crit">exceeds line rate</Badge>
  ) : preview.portSpeedMbps > 0 ? undefined : (
    <Badge tone="muted">port speed unknown</Badge>
  );

  return (
    <SummaryBar
      items={[
        {
          value: preview.summary,
          meta: (
            <>
              <span>{formatPps(preview.rate.pps)}</span>
              <span>L1 {formatBitrate(preview.rate.bpsL1)}</span>
              <span>L2 {formatBitrate(preview.rate.bpsL2)}</span>
              <span>{preview.headerBytes}B headers</span>
            </>
          ),
        },
      ]}
      linePct={badge ? undefined : preview.rate.linePct}
      badge={badge}
    />
  );
}

/** The hex dump of the generated frames. */
function HexPreview({ preview }: { preview: FlowPreview }) {
  const [index, setIndex] = useState(0);
  const frame = preview.frames[Math.min(index, preview.frames.length - 1)];

  if (!frame) return null;

  return (
    <Surface
      title="Frame preview"
      actions={
        preview.frames.length > 1 ? (
          <select
            className="select input-sm"
            value={index}
            onChange={(e) => setIndex(globalThis.Number(e.target.value))}
          >
            {preview.frames.map((f, i) => (
              <option key={f.wireLen} value={i}>
                {f.wireLen} bytes
              </option>
            ))}
          </select>
        ) : null
      }
      padded={false}
    >
      <pre className="hex-dump">{frame.hexDump}</pre>
      <div className="surface-body">
        <p className="note">
          {frame.bytes.length} bytes transmitted; the NIC appends a 4-byte FCS for {frame.wireLen}{' '}
          on the wire.
        </p>
      </div>
    </Surface>
  );
}

/**
 * Replaces the header stack from a captured packet.
 *
 * The import lands in the editor rather than saving a flow: the operator still
 * has to pick ports and a rate, and will usually want to adjust an address.
 * Anything the decoder approximated or dropped is reported rather than left for
 * them to notice in a capture later.
 */
function PcapImport({
  onImported,
  onError,
}: {
  onImported: (headers: FlowConfig['headers']) => void;
  onError: (message: string) => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [notes, setNotes] = useState<string[]>([]);

  const load = useMutation({
    mutationFn: (file: File) => api.flows.importPcap(file),
    onSuccess: (result) => {
      onImported(result.headers);
      setNotes(result.notes);
      onError('');
    },
    onError: (e: unknown) => {
      setNotes([]);
      onError(e instanceof ApiError ? e.fieldErrors[0]?.msg ?? e.message : 'The import failed.');
    },
  });

  return (
    <div className="stack gap-6" style={{ alignItems: 'flex-end' }}>
      <input
        ref={input}
        type="file"
        accept=".pcap,.cap,application/vnd.tcpdump.pcap"
        style={{ display: 'none' }}
        onChange={(e) => {
          const file = e.target.files?.[0];
          if (file) load.mutate(file);
          // Clear the value so selecting the same file twice fires again.
          e.target.value = '';
        }}
      />
      <button
        type="button"
        className="btn btn-secondary btn-sm"
        disabled={load.isPending}
        title="Read the header stack from the first packet of a capture"
        onClick={() => input.current?.click()}
      >
        <Import size={14} />
        {load.isPending ? 'Reading…' : 'Import from pcap'}
      </button>

      {notes.length > 0 ? (
        <ul className="import-notes">
          {notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
