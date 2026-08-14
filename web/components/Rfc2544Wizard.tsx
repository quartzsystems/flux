'use client';

/**
 * The RFC 2544 test wizard.
 *
 * Opens populated with the standard frame sizes and a sixty-second trial,
 * because those are what make a result comparable with anybody else's. Anything
 * an operator changes that would break conformance is called out here, in the
 * wizard, rather than waiting until they read the report — a sixty-minute run is
 * an expensive way to learn that the trial was too short.
 *
 * The configuration is grouped into tabs, every one of them always reachable:
 * a tab with something wrong on it says so with a mark rather than by refusing
 * to open.
 */

import { useMemo, useState } from 'react';

import { Alert } from '@/components/ui';
import { Tabs, type TabItem } from '@/components/ui/Tabs';
import { CheckList, CheckRow, Field, TextInput } from '@/components/ui/formkit';
import {
  REPORTABLE_TRIAL_SECONDS,
  STANDARD_FRAME_SIZES,
  defaultRfc2544Config,
  reportabilityNotes,
  type Flow,
  type Rfc2544Config,
  type TestType,
} from '@/lib/api-types';
import { formatDuration } from '@/lib/format';

/** The four benchmarks, with what each one measures. */
export const BENCHMARKS: {
  type: TestType;
  label: string;
  section: string;
  describes: string;
}[] = [
  {
    type: 'rfc2544_throughput',
    label: 'Throughput',
    section: '§26.1',
    describes:
      'The highest rate at which the device forwards every frame. A binary search on rate, per frame size.',
  },
  {
    type: 'rfc2544_latency',
    label: 'Latency',
    section: '§26.2',
    describes:
      'Forwarding delay measured at the throughput rate. Runs the throughput search first, then one timestamped trial at the rate it found.',
  },
  {
    type: 'rfc2544_frameloss',
    label: 'Frame loss rate',
    section: '§26.3',
    describes:
      'Loss at each rate on a descending ladder, stopping once two successive trials lose nothing.',
  },
  {
    type: 'rfc2544_b2b',
    label: 'Back-to-back frames',
    section: '§26.4',
    describes:
      'The longest burst the device absorbs without dropping a frame. A binary search on burst length at full line rate.',
  },
];

/** The wizard's tabs, in the order the decisions usually get made. */
type WizardTab = 'benchmark' | 'sizes' | 'search' | 'flows';

/**
 * Which tab a rejected field belongs to, so a validation error from the
 * backend lands on the tab that can fix it rather than wherever the operator
 * happened to be.
 */
function tabOfError(path: string): WizardTab {
  if (path === 'name' || path === 'config.trialSeconds' || path === 'config.lossTolerancePct') {
    return 'benchmark';
  }
  if (path === 'config.frameSizes') return 'sizes';
  if (path === 'flowIds') return 'flows';
  return 'search';
}

/** Props for {@link Rfc2544Wizard}. */
export interface WizardProps {
  /** Which benchmark is being configured. */
  type: TestType;
  /** Flows available to drive. */
  flows: Flow[];
  /** Called with the finished definition. */
  onSubmit: (input: {
    name: string;
    type: TestType;
    config: Record<string, unknown>;
    flowIds: string[];
  }) => void;
  /** Whether a submission is in flight. */
  busy: boolean;
  /** Field errors from a rejected submission, keyed by path. */
  errors: Record<string, string>;
}

/** Configures one RFC 2544 benchmark. */
export function Rfc2544Wizard({ type, flows, onSubmit, busy, errors }: WizardProps) {
  const benchmark = BENCHMARKS.find((b) => b.type === type) ?? BENCHMARKS[0]!;

  const [tab, setTab] = useState<WizardTab>('benchmark');
  const [name, setName] = useState('');
  const [config, setConfig] = useState<Rfc2544Config>(defaultRfc2544Config);
  const [selected, setSelected] = useState<string[]>([]);

  const notes = useMemo(() => reportabilityNotes(config), [config]);

  // Worst case: every frame size runs to the iteration limit. Operators
  // consistently underestimate this, and a seven-size run at sixty seconds is
  // over two hours even when it converges early.
  const estimate = useMemo(() => {
    const searchesPerSize =
      type === 'rfc2544_frameloss'
        ? Math.ceil((config.initialRatePct - config.minRatePct) / config.ladderStepPct) + 1
        : Math.min(config.maxIterations, 12);
    const trialSeconds = type === 'rfc2544_b2b' ? 5 : config.trialSeconds;
    return config.frameSizes.length * searchesPerSize * trialSeconds;
  }, [config, type]);

  // Every tab stays reachable; one with a rejected field carries the mark. An
  // empty size list is marked too — the list opens populated, so emptiness is
  // a deliberate act worth flagging before a submission bounces.
  const tabItems: TabItem[] = useMemo(() => {
    const marked = new Set(Object.keys(errors).map(tabOfError));
    if (config.frameSizes.length === 0) marked.add('sizes');
    return [
      { value: 'benchmark', label: 'Benchmark', invalid: marked.has('benchmark') },
      {
        value: 'sizes',
        label: 'Frame sizes',
        count: config.frameSizes.length > 0 ? config.frameSizes.length : undefined,
        invalid: marked.has('sizes'),
      },
      { value: 'search', label: 'Search', invalid: marked.has('search') },
      {
        value: 'flows',
        label: 'Flows',
        count: selected.length > 0 ? selected.length : undefined,
        invalid: marked.has('flows'),
      },
    ];
  }, [errors, config.frameSizes.length, selected.length]);

  /** Updates one field of the configuration. */
  const set = <K extends keyof Rfc2544Config>(key: K, value: Rfc2544Config[K]) => {
    setConfig((previous) => ({ ...previous, [key]: value }));
  };

  /** Adds or removes a frame size, keeping the list ascending. */
  const toggleSize = (size: number) => {
    setConfig((previous) => ({
      ...previous,
      frameSizes: previous.frameSizes.includes(size)
        ? previous.frameSizes.filter((s) => s !== size)
        : [...previous.frameSizes, size].sort((a, b) => a - b),
    }));
  };

  /** Adds or removes a flow, preserving selection order. */
  const toggleFlow = (id: string) => {
    setSelected((previous) =>
      previous.includes(id) ? previous.filter((f) => f !== id) : [...previous, id],
    );
  };

  return (
    <div className="stack gap-18">
      <Alert tone="info">
        <strong>
          {benchmark.label} <span className="muted">RFC 2544 {benchmark.section}</span>
        </strong>
        <p style={{ margin: '2px 0 0' }}>{benchmark.describes}</p>
      </Alert>

      <Tabs items={tabItems} value={tab} onChange={(v) => setTab(v as WizardTab)} />

      {tab === 'benchmark' ? (
        <div className="field-grid">
          <Field label="Name" error={errors.name}>
            <TextInput
              value={name}
              autoFocus
              invalid={Boolean(errors.name)}
              onChange={setName}
            />
          </Field>

          <Field
            label="Trial duration (seconds)"
            error={errors['config.trialSeconds']}
            hint={
              type === 'rfc2544_b2b'
                ? 'A burst trial runs for as long as the burst takes.'
                : `§24 requires ${REPORTABLE_TRIAL_SECONDS}s for a reportable result.`
            }
          >
            <TextInput
              mono
              value={String(config.trialSeconds)}
              disabled={type === 'rfc2544_b2b'}
              invalid={Boolean(errors['config.trialSeconds'])}
              onChange={(v) => set('trialSeconds', Number(v) || 0)}
            />
          </Field>

          <Field
            label="Loss tolerance (%)"
            error={errors['config.lossTolerancePct']}
            hint="Zero is the RFC definition."
          >
            <TextInput
              mono
              value={String(config.lossTolerancePct)}
              disabled={type === 'rfc2544_b2b'}
              invalid={Boolean(errors['config.lossTolerancePct'])}
              onChange={(v) => set('lossTolerancePct', Number(v) || 0)}
            />
          </Field>
        </div>
      ) : null}

      {tab === 'sizes' ? (
        <Field label="Frame sizes (bytes, including FCS)" error={errors['config.frameSizes']}>
          <div className="stack gap-8">
            <CheckList>
              {STANDARD_FRAME_SIZES.map((size) => (
                <CheckRow
                  key={size}
                  checked={config.frameSizes.includes(size)}
                  onChange={() => toggleSize(size)}
                >
                  <span className="mono">{size}</span>
                </CheckRow>
              ))}
            </CheckList>
            <div>
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={() => set('frameSizes', [...STANDARD_FRAME_SIZES])}
              >
                All seven
              </button>
            </div>
          </div>
        </Field>
      ) : null}

      {tab === 'search' ? (
        <div className="stack gap-14">
          {type === 'rfc2544_frameloss' ? (
            <div className="field-grid">
              <NumField
                label="Ladder step (%)"
                value={config.ladderStepPct}
                onChange={(v) => set('ladderStepPct', v)}
              />
              <NumField
                label="Lowest rate (%)"
                value={config.minRatePct}
                onChange={(v) => set('minRatePct', v)}
              />
            </div>
          ) : null}

          {type === 'rfc2544_b2b' ? (
            <div className="field-grid">
              <NumField
                label="Longest burst (frames)"
                value={config.maxBurstFrames}
                onChange={(v) => set('maxBurstFrames', v)}
              />
              <NumField
                label="Burst resolution (frames)"
                value={config.burstResolutionFrames}
                onChange={(v) => set('burstResolutionFrames', v)}
              />
            </div>
          ) : (
            <div className="field-grid">
              <NumField
                label="Starting rate (%)"
                hint="Also the highest rate the search will try."
                value={config.initialRatePct}
                onChange={(v) => set('initialRatePct', v)}
              />
              <NumField
                label="Resolution (%)"
                hint="The search stops once its window is this narrow."
                error={errors['config.resolutionPct']}
                value={config.resolutionPct}
                onChange={(v) => set('resolutionPct', v)}
              />
              <NumField
                label="Iteration limit"
                value={config.maxIterations}
                onChange={(v) => set('maxIterations', v)}
              />
            </div>
          )}
        </div>
      ) : null}

      {tab === 'flows' ? (
        <FlowPicker
          label="Flows"
          options={flows}
          selected={selected}
          onToggle={toggleFlow}
          error={errors.flowIds}
          hint="The benchmark supplies the frame size and rate; each flow contributes its header stack, ports, and modifiers."
        />
      ) : null}

      {notes.length > 0 ? (
        <Alert tone="warn">
          <strong>This will not produce a conformant RFC 2544 result.</strong>
          <ul style={{ margin: '4px 0 0 16px', padding: 0 }}>
            {notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>
          <p style={{ margin: '4px 0 0' }}>
            The run will still work, and its report will say the same thing.
          </p>
        </Alert>
      ) : null}

      <div className="row between gap-12" style={{ flexWrap: 'wrap' }}>
        <span className="note mono">
          Worst case about {formatDuration(estimate)} · {config.frameSizes.length} frame size
          {config.frameSizes.length === 1 ? '' : 's'}
        </span>
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !name.trim() || selected.length === 0 || config.frameSizes.length === 0}
          onClick={() =>
            onSubmit({
              name,
              type,
              config: config as unknown as Record<string, unknown>,
              flowIds: selected,
            })
          }
        >
          {busy ? 'Creating…' : 'Create test'}
        </button>
      </div>
    </div>
  );
}

/**
 * The ordered multi-select shared by this wizard and the manual test form.
 *
 * Selection order is the order flows are programmed, so each chosen row shows
 * its ordinal.
 */
export function FlowPicker({
  label,
  options,
  selected,
  onToggle,
  hint,
  error,
  emptyText = 'None defined yet.',
}: {
  /** Field label, without the selection count — that is appended here. */
  label: string;
  /** What can be chosen. */
  options: { id: string; name: string }[];
  /** Chosen ids, in selection order. */
  selected: string[];
  /** Called with an id to add or remove it. */
  onToggle: (id: string) => void;
  /** Explains what the selection drives. */
  hint?: string;
  /** Field error from a rejected submission. */
  error?: string;
  /** Shown when there is nothing to offer. */
  emptyText?: string;
}) {
  const suffix = selected.length > 0 ? ` (${selected.length} selected, in order)` : '';

  return (
    <Field label={`${label}${suffix}`} hint={hint} error={error}>
      {options.length === 0 ? (
        <p className="note">{emptyText}</p>
      ) : (
        <CheckList>
          {options.map((option) => {
            const position = selected.indexOf(option.id);
            return (
              <CheckRow
                key={option.id}
                checked={position >= 0}
                onChange={() => onToggle(option.id)}
              >
                <span className="mono">{option.name}</span>
                {position >= 0 ? <span className="mono muted">#{position}</span> : null}
              </CheckRow>
            );
          })}
        </CheckList>
      )}
    </Field>
  );
}

/** One numeric configuration field: mono, with the shared field chrome. */
function NumField({
  label,
  value,
  onChange,
  hint,
  error,
  disabled,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
  hint?: string;
  error?: string;
  disabled?: boolean;
}) {
  return (
    <Field label={label} hint={hint} error={error}>
      <TextInput
        mono
        value={String(value)}
        disabled={disabled}
        invalid={Boolean(error)}
        onChange={(v) => onChange(Number(v) || 0)}
      />
    </Field>
  );
}
