'use client';

/**
 * The RFC 2544 test wizard.
 *
 * Opens populated with the standard frame sizes and a sixty-second trial,
 * because those are what make a result comparable with anybody else's. Anything
 * an operator changes that would break conformance is called out here, in the
 * wizard, rather than waiting until they read the report — a sixty-minute run is
 * an expensive way to learn that the trial was too short.
 */

import { IconAlertTriangle, IconInfoCircle } from '@tabler/icons-react';
import { useMemo, useState } from 'react';

import { Alert, Badge } from '@/components/ui';
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
      <div className="wizard-intro">
        <IconInfoCircle size={16} stroke={1.8} style={{ flexShrink: 0, marginTop: 1 }} />
        <div>
          <strong>
            {benchmark.label} <span className="muted">RFC 2544 {benchmark.section}</span>
          </strong>
          <p style={{ margin: '2px 0 0', fontSize: 12.5 }}>{benchmark.describes}</p>
        </div>
      </div>

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
          <span className="field-label">Trial duration (seconds)</span>
          <input
            className="input"
            style={{ fontFamily: 'var(--qz-font-mono)' }}
            value={config.trialSeconds}
            disabled={type === 'rfc2544_b2b'}
            aria-invalid={Boolean(errors['config.trialSeconds'])}
            onChange={(e) => set('trialSeconds', Number(e.target.value) || 0)}
          />
          <span className="muted" style={{ fontSize: 11.5 }}>
            {type === 'rfc2544_b2b'
              ? 'A burst trial runs for as long as the burst takes.'
              : `§24 requires ${REPORTABLE_TRIAL_SECONDS}s for a reportable result.`}
          </span>
        </label>

        <label className="field">
          <span className="field-label">Loss tolerance (%)</span>
          <input
            className="input"
            style={{ fontFamily: 'var(--qz-font-mono)' }}
            value={config.lossTolerancePct}
            disabled={type === 'rfc2544_b2b'}
            aria-invalid={Boolean(errors['config.lossTolerancePct'])}
            onChange={(e) => set('lossTolerancePct', Number(e.target.value) || 0)}
          />
          <span className="muted" style={{ fontSize: 11.5 }}>
            Zero is the RFC definition.
          </span>
        </label>
      </div>

      <div className="field">
        <span className="field-label">Frame sizes (bytes, including FCS)</span>
        <div className="row gap-6" style={{ flexWrap: 'wrap', marginTop: 4 }}>
          {STANDARD_FRAME_SIZES.map((size) => {
            const on = config.frameSizes.includes(size);
            return (
              <button
                key={size}
                type="button"
                className={`btn btn-sm ${on ? 'btn-primary' : 'btn-secondary'}`}
                style={{ fontFamily: 'var(--qz-font-mono)', minWidth: 62 }}
                onClick={() => toggleSize(size)}
              >
                {size}
              </button>
            );
          })}
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={() => set('frameSizes', [...STANDARD_FRAME_SIZES])}
          >
            All seven
          </button>
        </div>
        {errors['config.frameSizes'] ? (
          <span className="field-error">{errors['config.frameSizes']}</span>
        ) : null}
      </div>

      {type === 'rfc2544_frameloss' ? (
        <div className="field-grid">
          <label className="field">
            <span className="field-label">Ladder step (%)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.ladderStepPct}
              onChange={(e) => set('ladderStepPct', Number(e.target.value) || 0)}
            />
          </label>
          <label className="field">
            <span className="field-label">Lowest rate (%)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.minRatePct}
              onChange={(e) => set('minRatePct', Number(e.target.value) || 0)}
            />
          </label>
        </div>
      ) : null}

      {type === 'rfc2544_b2b' ? (
        <div className="field-grid">
          <label className="field">
            <span className="field-label">Longest burst (frames)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.maxBurstFrames}
              onChange={(e) => set('maxBurstFrames', Number(e.target.value) || 0)}
            />
          </label>
          <label className="field">
            <span className="field-label">Burst resolution (frames)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.burstResolutionFrames}
              onChange={(e) => set('burstResolutionFrames', Number(e.target.value) || 0)}
            />
          </label>
        </div>
      ) : (
        <div className="field-grid">
          <label className="field">
            <span className="field-label">Starting rate (%)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.initialRatePct}
              onChange={(e) => set('initialRatePct', Number(e.target.value) || 0)}
            />
            <span className="muted" style={{ fontSize: 11.5 }}>
              Also the highest rate the search will try.
            </span>
          </label>
          <label className="field">
            <span className="field-label">Resolution (%)</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.resolutionPct}
              aria-invalid={Boolean(errors['config.resolutionPct'])}
              onChange={(e) => set('resolutionPct', Number(e.target.value) || 0)}
            />
            <span className="muted" style={{ fontSize: 11.5 }}>
              The search stops once its window is this narrow.
            </span>
          </label>
          <label className="field">
            <span className="field-label">Iteration limit</span>
            <input
              className="input"
              style={{ fontFamily: 'var(--qz-font-mono)' }}
              value={config.maxIterations}
              onChange={(e) => set('maxIterations', Number(e.target.value) || 0)}
            />
          </label>
        </div>
      )}

      <div className="field">
        <span className="field-label">
          Flows {selected.length > 0 ? `(${selected.length} selected, in order)` : ''}
        </span>
        {errors.flowIds ? <span className="field-error">{errors.flowIds}</span> : null}
        <p className="muted" style={{ margin: '2px 0 6px', fontSize: 11.5 }}>
          The benchmark supplies the frame size and rate; each flow contributes its header
          stack, ports, and modifiers.
        </p>
        <div className="stack gap-6">
          {flows.map((flow) => {
            const position = selected.indexOf(flow.id);
            return (
              <label key={flow.id} className="row gap-8" style={{ fontSize: 13, cursor: 'pointer' }}>
                <input
                  type="checkbox"
                  checked={position >= 0}
                  onChange={() => toggleFlow(flow.id)}
                />
                <span className="mono">{flow.name}</span>
                {position >= 0 ? <Badge tone="ok">#{position}</Badge> : null}
              </label>
            );
          })}
        </div>
      </div>

      {notes.length > 0 ? (
        <Alert tone="warn">
          <IconAlertTriangle size={16} stroke={1.8} />
          <div>
            <strong>This will not produce a conformant RFC 2544 result.</strong>
            <ul style={{ margin: '4px 0 0 16px', padding: 0, fontSize: 12.5 }}>
              {notes.map((note) => (
                <li key={note}>{note}</li>
              ))}
            </ul>
            <p style={{ margin: '4px 0 0', fontSize: 12.5 }}>
              The run will still work, and its report will say the same thing.
            </p>
          </div>
        </Alert>
      ) : null}

      <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 12 }}>
        <span className="muted mono" style={{ fontSize: 12 }}>
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
