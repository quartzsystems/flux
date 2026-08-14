'use client';

/**
 * The ordered header-stack builder.
 *
 * Order is the point: it is what determines the bytes on the wire, so layers are
 * reorderable and each shows its byte offset. The offsets are computed the same
 * way the frame builder computes them, which is what lets an operator match a
 * field here against a capture.
 */

import { ChevronDown, ChevronUp, Plus, Trash2 } from 'lucide-react';

import { CheckRow, Field, SelectInput, TextInput } from '@/components/ui/formkit';
import type { HeaderLayer, HeaderProto } from '@/lib/api-types';
import { defaultLayer, layerBytes, PROTO_LABELS } from '@/lib/flow-defaults';

/** Protocols offered by the add menu, in the order they usually stack. */
const ADDABLE: HeaderProto[] = ['ethernet', 'vlan', 'ipv4', 'ipv6', 'tcp', 'udp', 'custom'];

/** Props for {@link HeaderStack}. */
export interface HeaderStackProps {
  /** The current stack, outermost first. */
  headers: HeaderLayer[];
  /** Called with the new stack whenever it changes. */
  onChange: (headers: HeaderLayer[]) => void;
  /** Field-level errors keyed by path, e.g. `config.headers.1.src`. */
  errors: Record<string, string>;
}

/** Renders the header stack editor. */
export function HeaderStack({ headers, onChange, errors }: HeaderStackProps) {
  /** Replaces one layer. */
  const replace = (index: number, layer: HeaderLayer) => {
    onChange(headers.map((h, i) => (i === index ? layer : h)));
  };

  /** Moves a layer one position, if there is somewhere to move it. */
  const move = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= headers.length) return;

    const next = [...headers];
    const [layer] = next.splice(index, 1);
    if (layer) next.splice(target, 0, layer);
    onChange(next);
  };

  // Offsets accumulate down the stack, matching how the builder writes bytes.
  let offset = 0;

  return (
    <div className="stack gap-10">
      {headers.map((layer, index) => {
        const start = offset;
        offset += layerBytes(layer);

        return (
          <div key={index} className="layer">
            <div className="layer-head">
              <span className="layer-index mono">{index}</span>
              <strong>{PROTO_LABELS[layer.proto]}</strong>
              <span className="mono layer-offset">
                +{start} · {layerBytes(layer)}B
              </span>

              <div className="row gap-6 ml-auto">
                <button
                  type="button"
                  className="btn btn-ghost btn-sm btn-icon"
                  title="Move outward"
                  disabled={index === 0}
                  onClick={() => move(index, -1)}
                >
                  <ChevronUp size={14} />
                </button>
                <button
                  type="button"
                  className="btn btn-ghost btn-sm btn-icon"
                  title="Move inward"
                  disabled={index === headers.length - 1}
                  onClick={() => move(index, 1)}
                >
                  <ChevronDown size={14} />
                </button>
                <button
                  type="button"
                  className="btn btn-ghost btn-sm btn-icon"
                  title="Remove layer"
                  onClick={() => onChange(headers.filter((_, i) => i !== index))}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            </div>

            <div className="layer-body">
              <LayerFields
                layer={layer}
                onChange={(next) => replace(index, next)}
                errors={errors}
                path={`config.headers.${index}`}
              />
            </div>
          </div>
        );
      })}

      <div className="row gap-6" style={{ flexWrap: 'wrap' }}>
        <span className="note">
          <Plus size={12} style={{ verticalAlign: -2 }} /> Add layer
        </span>
        {ADDABLE.map((proto) => (
          <button
            key={proto}
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => onChange([...headers, defaultLayer(proto)])}
          >
            {PROTO_LABELS[proto]}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Props shared by every layer field editor. */
interface FieldsProps {
  layer: HeaderLayer;
  onChange: (layer: HeaderLayer) => void;
  errors: Record<string, string>;
  path: string;
}

/** Dispatches to the right field set for a layer. */
function LayerFields({ layer, onChange, errors, path }: FieldsProps) {
  /** Builds a change handler for one field of the current layer. */
  const set = <K extends string>(key: K, value: unknown) => {
    onChange({
      ...layer,
      fields: { ...(layer.fields as Record<string, unknown>), [key]: value },
    } as HeaderLayer);
  };

  const field = (name: string) => errors[`${path}.${name}`];

  switch (layer.proto) {
    case 'ethernet':
      return (
        <div className="field-grid">
          <Text label="Source MAC" value={layer.fields.src} error={field('src')} onChange={(v) => set('src', v)} mono />
          <Text label="Destination MAC" value={layer.fields.dst} error={field('dst')} onChange={(v) => set('dst', v)} mono />
          <Number
            label="EtherType"
            hint="Derived from the next layer when empty"
            value={layer.fields.ethertype}
            hex
            optional
            onChange={(v) => set('ethertype', v)}
          />
        </div>
      );

    case 'vlan':
      return (
        <div className="field-grid">
          <Number label="VLAN id" value={layer.fields.id} error={field('id')} onChange={(v) => set('id', v ?? 0)} />
          <Number label="Priority (PCP)" value={layer.fields.pcp} error={field('pcp')} onChange={(v) => set('pcp', v ?? 0)} />
          <Select
            label="Tag protocol"
            value={String(layer.fields.tpid)}
            error={field('tpid')}
            options={[
              ['33024', '0x8100 — 802.1Q'],
              ['34984', '0x88a8 — 802.1ad outer'],
              ['37120', '0x9100 — legacy QinQ'],
            ]}
            onChange={(v) => set('tpid', globalThis.Number(v))}
          />
          <Check label="Drop eligible" checked={layer.fields.dei} onChange={(v) => set('dei', v)} />
        </div>
      );

    case 'ipv4':
      return (
        <div className="field-grid">
          <Text label="Source" value={layer.fields.src} error={field('src')} onChange={(v) => set('src', v)} mono />
          <Text label="Destination" value={layer.fields.dst} error={field('dst')} onChange={(v) => set('dst', v)} mono />
          <Number label="TTL" value={layer.fields.ttl} error={field('ttl')} onChange={(v) => set('ttl', v ?? 64)} />
          <Number label="DSCP" value={layer.fields.dscp} error={field('dscp')} onChange={(v) => set('dscp', v ?? 0)} />
          <Number label="ECN" value={layer.fields.ecn} error={field('ecn')} onChange={(v) => set('ecn', v ?? 0)} />
          <Check
            label="Don't fragment"
            checked={layer.fields.dontFragment}
            onChange={(v) => set('dontFragment', v)}
          />
        </div>
      );

    case 'ipv6':
      return (
        <div className="field-grid">
          <Text label="Source" value={layer.fields.src} error={field('src')} onChange={(v) => set('src', v)} mono />
          <Text label="Destination" value={layer.fields.dst} error={field('dst')} onChange={(v) => set('dst', v)} mono />
          <Number label="Hop limit" value={layer.fields.hopLimit} error={field('hopLimit')} onChange={(v) => set('hopLimit', v ?? 64)} />
          <Number label="Traffic class" value={layer.fields.trafficClass} onChange={(v) => set('trafficClass', v ?? 0)} />
          <Number label="Flow label" value={layer.fields.flowLabel} error={field('flowLabel')} onChange={(v) => set('flowLabel', v ?? 0)} />
        </div>
      );

    case 'tcp':
      return (
        <div className="field-grid">
          <Number label="Source port" value={layer.fields.srcPort} onChange={(v) => set('srcPort', v ?? 0)} />
          <Number label="Destination port" value={layer.fields.dstPort} onChange={(v) => set('dstPort', v ?? 0)} />
          <Number label="Sequence" value={layer.fields.seq} onChange={(v) => set('seq', v ?? 0)} />
          <Number label="Flags" value={layer.fields.flags} error={field('flags')} hex onChange={(v) => set('flags', v ?? 0)} />
          <Number label="Window" value={layer.fields.window} onChange={(v) => set('window', v ?? 0)} />
        </div>
      );

    case 'udp':
      return (
        <div className="field-grid">
          <Number label="Source port" value={layer.fields.srcPort} onChange={(v) => set('srcPort', v ?? 0)} />
          <Number label="Destination port" value={layer.fields.dstPort} onChange={(v) => set('dstPort', v ?? 0)} />
        </div>
      );

    case 'custom':
      return (
        <div className="field-grid">
          <Text
            label="Bytes (hex)"
            value={layer.fields.hex}
            error={field('hex')}
            hint="Separators are ignored"
            onChange={(v) => set('hex', v)}
            mono
            wide
          />
        </div>
      );
  }
}

// ---------------------------------------------------------------------------
// Field primitives — thin wrappers over formkit's shared controls, keeping
// this file's conveniences (`hex`, `optional`, `wide`) local to the layers
// that need them.
// ---------------------------------------------------------------------------

/** A text input with the shared field chrome. */
function Text({
  label,
  value,
  onChange,
  error,
  hint,
  mono,
  wide,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  hint?: string;
  mono?: boolean;
  /** Spans the full field grid — for values too long for one column. */
  wide?: boolean;
}) {
  const field = (
    <Field label={label} hint={hint} error={error}>
      <TextInput value={value} onChange={onChange} mono={mono} invalid={Boolean(error)} />
    </Field>
  );
  return wide ? <div style={{ gridColumn: '1 / -1' }}>{field}</div> : field;
}

/**
 * A numeric input.
 *
 * `hex` displays the value in hex, which is how EtherTypes and TCP flags are
 * written everywhere else; the underlying value stays a number.
 */
function Number({
  label,
  value,
  onChange,
  error,
  hint,
  hex,
  optional,
}: {
  label: string;
  value: number | undefined;
  onChange: (value: number | undefined) => void;
  error?: string;
  hint?: string;
  hex?: boolean;
  optional?: boolean;
}) {
  const display = value === undefined ? '' : hex ? `0x${value.toString(16)}` : String(value);

  return (
    <Field label={label} hint={hint} error={error}>
      <TextInput
        mono
        value={display}
        invalid={Boolean(error)}
        onChange={(next) => {
          const raw = next.trim();
          if (raw === '') {
            onChange(optional ? undefined : 0);
            return;
          }
          const parsed = raw.startsWith('0x') ? parseInt(raw.slice(2), 16) : globalThis.Number(raw);
          if (!globalThis.isNaN(parsed)) onChange(parsed);
        }}
      />
    </Field>
  );
}

/** A select with the shared field chrome. */
function Select({
  label,
  value,
  options,
  onChange,
  error,
  hint,
}: {
  label: string;
  value: string;
  options: [string, string][];
  onChange: (value: string) => void;
  error?: string;
  hint?: string;
}) {
  return (
    <Field label={label} hint={hint} error={error}>
      <SelectInput value={value} onChange={onChange} invalid={Boolean(error)}>
        {options.map(([v, text]) => (
          <option key={v} value={v}>
            {text}
          </option>
        ))}
      </SelectInput>
    </Field>
  );
}

/** A checkbox. */
function Check({
  label,
  checked,
  onChange,
  error,
  hint,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  error?: string;
  hint?: string;
}) {
  return (
    <div>
      <CheckRow checked={checked} onChange={() => onChange(!checked)}>
        <span className="text-[13px]">{label}</span>
      </CheckRow>
      {error ? <span className="field-error">{error}</span> : null}
      {!error && hint ? <span className="field-hint">{hint}</span> : null}
    </div>
  );
}
