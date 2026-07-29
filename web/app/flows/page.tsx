'use client';

/**
 * The flows page: a list on the left, an editor on the right.
 *
 * The editor previews continuously. Every change is sent to `/flows/preview`,
 * which returns the frame bytes, the resolved rate, and the summary line — all
 * computed by the same code that will programme the engine, so what an operator
 * sees here is what will go on the wire.
 */

import {
  IconAlertTriangle,
  IconDeviceFloppy,
  IconFileImport,
  IconPlus,
  IconTrash,
} from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState } from 'react';

import { AppShell } from '@/components/AppShell';
import { HeaderStack } from '@/components/HeaderStack';
import { Alert, Badge, EmptyRow, PageHeader, Skeleton, Surface } from '@/components/ui';
import { ApiError, api, queryKeys } from '@/lib/api';
import {
  flowConfigSchema,
  type Flow,
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

export default function FlowsPage() {
  return (
    <AppShell>
      <Flows />
    </AppShell>
  );
}

function Flows() {
  const { can } = useAuth();
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState<{ name: string; config: FlowConfig } | null>(null);

  const flows = useQuery({
    queryKey: queryKeys.flows,
    queryFn: ({ signal }) => api.flows.list(signal),
  });

  const ports = useQuery({
    queryKey: queryKeys.ports,
    queryFn: ({ signal }) => api.ports.list(signal),
  });

  /** Loads a stored flow into the editor. */
  const open = (flow: Flow) => {
    const parsed = flowConfigSchema.safeParse(flow.config);
    if (!parsed.success) {
      // A flow this build cannot read is better refused than silently
      // reinterpreted into something the operator did not configure.
      setSelected(flow.id);
      setDraft(null);
      return;
    }
    setSelected(flow.id);
    setDraft({ name: flow.name, config: parsed.data });
  };

  /** Starts a new flow using the first two available ports. */
  const startNew = () => {
    const available = ports.data ?? [];
    const tx = available[0]?.id ?? '';
    const rx = available[1]?.id ?? available[0]?.id ?? '';
    setSelected(null);
    setDraft({ name: '', config: defaultFlow(tx, rx) });
  };

  return (
    <div className="page stack gap-18">
      <PageHeader
        title="Flows"
        subtitle={
          flows.data
            ? `${flows.data.length} defined`
            : 'Traffic definitions'
        }
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={startNew}
            disabled={!can('operator') || (ports.data ?? []).length === 0}
            title={
              !can('operator')
                ? 'Creating flows requires an operator account'
                : (ports.data ?? []).length === 0
                  ? 'No ports have been discovered yet'
                  : 'Create a flow'
            }
          >
            <IconPlus size={15} stroke={2} />
            New flow
          </button>
        }
      />

      <div className="flow-layout">
        <Surface title="Defined flows" padded={false}>
          <div className="qz-table-wrap">
            <table className="qz-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Updated</th>
                </tr>
              </thead>
              {flows.isLoading ? (
                <tbody>
                  <tr>
                    <td colSpan={2}>
                      <Skeleton height={14} />
                    </td>
                  </tr>
                </tbody>
              ) : (
                <tbody>
                  {(flows.data ?? []).length === 0 ? (
                    <EmptyRow columns={2}>No flows yet.</EmptyRow>
                  ) : (
                    (flows.data ?? []).map((flow) => (
                      <tr
                        key={flow.id}
                        aria-selected={flow.id === selected}
                        onClick={() => open(flow)}
                        style={{ cursor: 'pointer' }}
                      >
                        <td className="mono">{flow.name}</td>
                        <td className="dim" style={{ fontSize: 12 }}>
                          {new Date(flow.updatedAt).toLocaleDateString()}
                        </td>
                      </tr>
                    ))
                  )}
                </tbody>
              )}
            </table>
          </div>
        </Surface>

        {draft ? (
          <FlowEditor
            key={selected ?? 'new'}
            flowId={selected}
            initial={draft}
            ports={ports.data ?? []}
            onClose={() => {
              setDraft(null);
              setSelected(null);
            }}
          />
        ) : (
          <Surface>
            <p className="dim" style={{ margin: 0, fontSize: 13 }}>
              {selected
                ? 'This flow was written by a different version of Flux and cannot be shown here.'
                : 'Select a flow to edit it, or create a new one.'}
            </p>
          </Surface>
        )}
      </div>
    </div>
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
                onClick={() => remove.mutate()}
              >
                <IconTrash size={14} stroke={1.8} />
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
              <IconDeviceFloppy size={14} stroke={1.8} />
              {save.isPending ? 'Saving…' : 'Save'}
            </button>
          </>
        }
      >
        <div className="stack gap-18">
          {banner ? (
            <Alert tone="danger">
              <IconAlertTriangle size={16} stroke={1.8} />
              <span>{banner}</span>
            </Alert>
          ) : null}

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
                className="input"
                style={{ fontFamily: 'var(--qz-font-mono)' }}
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

            <label className="field" style={{ justifyContent: 'flex-end' }}>
              <span className="row gap-8" style={{ fontSize: 13 }}>
                <input
                  type="checkbox"
                  checked={config.latencyTrack}
                  onChange={(e) => set('latencyTrack', e.target.checked)}
                />
                Measure latency
              </span>
              <span className="muted" style={{ fontSize: 11.5 }}>
                Adds a timestamp to each frame; costs a little transmit capacity.
              </span>
            </label>
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

      <SummaryBar preview={preview} error={previewError} />

      {preview ? <HexPreview preview={preview} /> : null}
    </div>
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
            className="input"
            style={{ fontFamily: 'var(--qz-font-mono)' }}
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
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={size.min}
              onChange={(e) =>
                onChange({ type: 'random', min: globalThis.Number(e.target.value) || 0, max: size.max })
              }
            />
          </label>
          <label className="field">
            <span className="field-label">Maximum</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
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
          className="input"
          style={{ fontFamily: 'var(--qz-font-mono)' }}
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
                      className="select"
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
                      className="select"
                      style={{ width: 120 }}
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
                      className="input"
                      style={{ width: 100, fontFamily: 'var(--qz-font-mono)' }}
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
                      className="input"
                      style={{ width: 80, fontFamily: 'var(--qz-font-mono)' }}
                      value={modifier.step}
                      onChange={(e) =>
                        replace(index, { ...modifier, step: globalThis.Number(e.target.value) || 1 })
                      }
                    />
                  </td>
                  <td style={{ textAlign: 'right' }}>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm btn-icon"
                      title="Remove modifier"
                      onClick={() => onChange(modifiers.filter((_, i) => i !== index))}
                    >
                      <IconTrash size={14} stroke={1.8} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="dim" style={{ margin: 0, fontSize: 13 }}>
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
          <IconPlus size={14} stroke={2} />
          Add modifier
        </button>
      </div>
    </div>
  );
}

/** The computed summary bar under the editor. */
function SummaryBar({ preview, error }: { preview: FlowPreview | null; error: string | null }) {
  if (error) {
    return (
      <Alert tone="danger">
        <IconAlertTriangle size={16} stroke={1.8} />
        <span>{error}</span>
      </Alert>
    );
  }

  if (!preview) {
    return (
      <div className="summary-bar">
        <Skeleton height={16} width={340} />
      </div>
    );
  }

  return (
    <div className="summary-bar">
      <div className="row gap-14" style={{ flexWrap: 'wrap' }}>
        <span className="mono" style={{ color: 'var(--qz-fg-1)', fontSize: 13 }}>
          {preview.summary}
        </span>

        {preview.exceedsLineRate ? (
          <Badge tone="crit">exceeds line rate</Badge>
        ) : preview.portSpeedMbps > 0 ? (
          <Badge tone={preview.rate.linePct > 90 ? 'warn' : 'ok'}>
            {preview.rate.linePct.toFixed(1)}% of line
          </Badge>
        ) : (
          <Badge tone="muted">port speed unknown</Badge>
        )}
      </div>

      <div className="row gap-14 mono" style={{ fontSize: 11.5, color: 'var(--qz-fg-4)' }}>
        <span>{formatPps(preview.rate.pps)}</span>
        <span>L1 {formatBitrate(preview.rate.bpsL1)}</span>
        <span>L2 {formatBitrate(preview.rate.bpsL2)}</span>
        <span>{preview.headerBytes}B headers</span>
      </div>
    </div>
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
            className="select"
            style={{ width: 140 }}
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
      <p className="muted" style={{ margin: 0, padding: '0 18px 14px', fontSize: 11.5 }}>
        {frame.bytes.length} bytes transmitted; the NIC appends a 4-byte FCS for {frame.wireLen}{' '}
        on the wire.
      </p>
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
    <div className="stack" style={{ alignItems: 'flex-end', gap: 4 }}>
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
        <IconFileImport size={14} stroke={1.8} />
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
