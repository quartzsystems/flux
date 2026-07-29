/**
 * Starting values for the flow editor.
 *
 * These mirror the `Default` impls in `flux_core::flow`. They are duplicated
 * rather than fetched because the editor needs them synchronously when an
 * operator adds a layer, and a round trip there would put a spinner inside a
 * click. The Rust side remains authoritative — anything saved is validated
 * against it.
 */

import type { FlowConfig, HeaderLayer, HeaderProto, Modifier, ModifierField } from './api-types';

/** Builds a header layer of `proto` with sensible starting values. */
export function defaultLayer(proto: HeaderProto): HeaderLayer {
  switch (proto) {
    case 'ethernet':
      return {
        proto: 'ethernet',
        fields: { src: '00:00:00:00:00:01', dst: '00:00:00:00:00:02' },
      };
    case 'vlan':
      return { proto: 'vlan', fields: { id: 100, pcp: 0, dei: false, tpid: 0x8100 } };
    case 'ipv4':
      return {
        proto: 'ipv4',
        fields: {
          src: '10.0.0.1',
          dst: '10.0.0.2',
          ttl: 64,
          dscp: 0,
          ecn: 0,
          identification: 0,
          dontFragment: false,
        },
      };
    case 'ipv6':
      return {
        proto: 'ipv6',
        fields: {
          src: '2001:db8::1',
          dst: '2001:db8::2',
          hopLimit: 64,
          trafficClass: 0,
          flowLabel: 0,
        },
      };
    case 'tcp':
      return {
        proto: 'tcp',
        fields: { srcPort: 1024, dstPort: 80, seq: 0, ack: 0, flags: 0x02, window: 8192 },
      };
    case 'udp':
      return { proto: 'udp', fields: { srcPort: 1024, dstPort: 53 } };
    case 'custom':
      return { proto: 'custom', fields: { hex: 'deadbeef' } };
  }
}

/** A new flow: Ethernet over IPv4 over UDP at 64 bytes, ten percent of line. */
export function defaultFlow(txPort: string, rxPort: string): FlowConfig {
  return {
    txPort,
    rxPort,
    headers: [defaultLayer('ethernet'), defaultLayer('ipv4'), defaultLayer('udp')],
    size: { type: 'fixed', bytes: 64 },
    // Ten percent rather than a hundred: an operator who presses run without
    // reading the form should not immediately saturate a production link.
    rate: { type: 'percent', value: 10 },
    modifiers: [],
    durationSecs: null,
    latencyTrack: false,
  };
}

/** A new modifier over `field`. */
export function defaultModifier(field: ModifierField): Modifier {
  return { field, mode: 'increment', count: 10, step: 1 };
}

/** Byte length of a header layer, matching `HeaderLayer::byte_len`. */
export function layerBytes(layer: HeaderLayer): number {
  switch (layer.proto) {
    case 'ethernet':
      return 14;
    case 'vlan':
      return 4;
    case 'ipv4':
      return 20;
    case 'ipv6':
      return 40;
    case 'tcp':
      return 20;
    case 'udp':
      return 8;
    case 'custom':
      return hexBytes(layer.fields.hex);
  }
}

/** How many bytes a hex string decodes to, ignoring separators. */
export function hexBytes(hex: string): number {
  const cleaned = hex.replace(/[\s:-]/g, '');
  return Math.floor(cleaned.length / 2);
}

/** Display name for a protocol. */
export const PROTO_LABELS: Record<HeaderProto, string> = {
  ethernet: 'Ethernet',
  vlan: '802.1Q VLAN',
  ipv4: 'IPv4',
  ipv6: 'IPv6',
  tcp: 'TCP',
  udp: 'UDP',
  custom: 'Custom hex',
};

/** Display name for a modifier target. */
export const MODIFIER_LABELS: Record<ModifierField, string> = {
  eth_src: 'Ethernet source',
  eth_dst: 'Ethernet destination',
  vlan_id: 'VLAN id',
  ipv4_src: 'IPv4 source',
  ipv4_dst: 'IPv4 destination',
  ipv6_src: 'IPv6 source',
  ipv6_dst: 'IPv6 destination',
  l4_src_port: 'L4 source port',
  l4_dst_port: 'L4 destination port',
};

/**
 * Which layer a modifier needs.
 *
 * Mirrors `ModifierField::layer_name`, and lets the editor grey out targets the
 * current header stack cannot support instead of offering a choice the server
 * will reject.
 */
export function modifierRequires(field: ModifierField): HeaderProto[] {
  switch (field) {
    case 'eth_src':
    case 'eth_dst':
      return ['ethernet'];
    case 'vlan_id':
      return ['vlan'];
    case 'ipv4_src':
    case 'ipv4_dst':
      return ['ipv4'];
    case 'ipv6_src':
    case 'ipv6_dst':
      return ['ipv6'];
    case 'l4_src_port':
    case 'l4_dst_port':
      return ['tcp', 'udp'];
  }
}

/** True when `headers` contains a layer the modifier can apply to. */
export function modifierApplies(field: ModifierField, headers: HeaderLayer[]): boolean {
  const needed = modifierRequires(field);
  return headers.some((h) => needed.includes(h.proto));
}
