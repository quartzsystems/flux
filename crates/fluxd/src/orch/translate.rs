//! Turning a flow document into programmed streams.
//!
//! Two things happen here. The header stack becomes bytes (delegated to
//! `flux_core::frame`), and the symbolic modifiers — "vary `ipv4_src` across
//! 10,000 values" — become byte offsets and widths, which is the form both
//! engines actually apply.
//!
//! Resolving offsets is the part worth care: it walks the same header stack the
//! frame builder walks, so the two agree by construction about where each field
//! lands. A modifier pointed at the wrong offset does not fail, it quietly
//! corrupts a different field.

use flux_core::engine::{PgId, StreamModifier, StreamSpec};
use flux_core::flow::{FlowConfig, FrameSize, HeaderLayer, Modifier, ModifierField};
use flux_core::frame::{self, FrameError};
use flux_core::rate;

/// A flow could not be turned into streams.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    /// The frame itself could not be built.
    #[error(transparent)]
    Frame(#[from] FrameError),

    /// A modifier names a field the header stack does not contain.
    ///
    /// Validation rejects this when a flow is saved, so reaching it means the
    /// stack changed underneath a stored flow.
    #[error("modifier {index} targets {field}, which this header stack does not contain")]
    NoSuchField {
        /// Position of the modifier in the flow.
        index: usize,
        /// The field it named.
        field: &'static str,
    },
}

/// Builds the streams that realise `flow`.
///
/// A fixed size yields one stream. A mixture yields one stream per component,
/// each carrying its share of the total rate, which is how the engine reproduces
/// the weighting — there is no "send a mixture" primitive.
///
/// `speed_mbps` is the transmitting port's line rate, needed only to resolve a
/// percentage into a packet rate.
pub fn to_streams(
    flow: &FlowConfig,
    first_pg_id: PgId,
    speed_mbps: u32,
) -> Result<Vec<StreamSpec>, TranslateError> {
    let offsets = resolve_offsets(flow)?;
    let total = rate::resolve_for_size(&flow.rate, &flow.size, speed_mbps);

    let components = components_of(&flow.size);
    let total_weight: f64 = components.iter().map(|c| f64::from(c.weight)).sum();

    let mut streams = Vec::with_capacity(components.len());
    for (i, component) in components.iter().enumerate() {
        let built = frame::build_with_size(flow, component.bytes)?;

        // Split the flow's packet rate across components by weight. Rate is
        // apportioned in packets, not bits, because that is what the operator's
        // "N pps" means and what the mixture weights describe.
        let share =
            if total_weight > 0.0 { f64::from(component.weight) / total_weight } else { 0.0 };

        streams.push(StreamSpec {
            // Components of one flow share consecutive ids so per-flow
            // statistics can be summed back together.
            pg_id: PgId(first_pg_id.0 + i as u32),
            packet: built.bytes,
            wire_len: built.wire_len,
            pps: total.pps * share,
            modifiers: offsets.clone(),
            latency: flow.latency_track,
            total_packets: None,
        });
    }

    Ok(streams)
}

/// One frame length a flow will emit, with its share of the traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Component {
    bytes: u32,
    weight: u32,
}

/// The distinct frame lengths a size setting produces.
///
/// A random range is approximated by its endpoints rather than modelled
/// exactly: TRex generates the range itself, and for rate accounting the mean of
/// two equally weighted endpoints is the mean of the uniform distribution.
fn components_of(size: &FrameSize) -> Vec<Component> {
    match size {
        FrameSize::Fixed { bytes } => vec![Component { bytes: *bytes, weight: 1 }],
        FrameSize::Imix { preset } => preset
            .entries()
            .iter()
            .map(|e| Component { bytes: e.bytes, weight: e.weight })
            .collect(),
        FrameSize::Random { min, max } => {
            if min == max {
                vec![Component { bytes: *min, weight: 1 }]
            } else {
                vec![Component { bytes: *min, weight: 1 }, Component { bytes: *max, weight: 1 }]
            }
        }
    }
}

/// Where each header layer starts, walking the stack the way the builder does.
#[derive(Debug, Default, Clone, Copy)]
struct LayerOffsets {
    ethernet: Option<u16>,
    /// The innermost VLAN tag, which is the one a modifier means.
    vlan: Option<u16>,
    ipv4: Option<u16>,
    ipv6: Option<u16>,
    l4: Option<u16>,
}

/// Computes layer offsets for a header stack.
///
/// This mirrors `frame::build`'s walk exactly. Where the builder writes bytes,
/// this accumulates lengths; both consume the same list in the same order, so a
/// layer added to one without the other shows up as a failing offset test rather
/// than as silently corrupted frames.
fn layer_offsets(headers: &[HeaderLayer]) -> LayerOffsets {
    let mut offsets = LayerOffsets::default();
    let mut position: u16 = 0;

    for layer in headers {
        match layer {
            // The outermost of each kind wins, except for VLANs.
            HeaderLayer::Ethernet(_) => {
                offsets.ethernet.get_or_insert(position);
            }
            // Later tags overwrite earlier ones: a modifier on a QinQ stack
            // means the customer tag, which is the inner one.
            HeaderLayer::Vlan(_) => offsets.vlan = Some(position),
            HeaderLayer::Ipv4(_) => {
                offsets.ipv4.get_or_insert(position);
            }
            HeaderLayer::Ipv6(_) => {
                offsets.ipv6.get_or_insert(position);
            }
            HeaderLayer::Tcp(_) | HeaderLayer::Udp(_) => {
                offsets.l4.get_or_insert(position);
            }
            HeaderLayer::Custom(_) => {}
        }
        position += layer.byte_len() as u16;
    }

    offsets
}

/// Resolves every modifier to an offset, width, and value range.
fn resolve_offsets(flow: &FlowConfig) -> Result<Vec<StreamModifier>, TranslateError> {
    let layers = layer_offsets(&flow.headers);
    let mut out = Vec::with_capacity(flow.modifiers.len());

    for (index, modifier) in flow.modifiers.iter().enumerate() {
        let resolved = resolve_one(flow, modifier, &layers)
            .ok_or(TranslateError::NoSuchField { index, field: modifier.field.as_str() })?;
        out.push(resolved);
    }

    Ok(out)
}

/// Resolves one modifier against the layer offsets.
///
/// Two constraints shape the choices here.
///
/// **Widths are 1, 2, or 4 bytes.** TRex flow variables come in those sizes
/// only, so a modifier over the low three bytes of an IPv4 address is expressed
/// as a four-byte variable whose range happens to span three. The bit budget
/// `ModifierField::width_bits` enforces at validation time is what keeps that
/// range from reaching into the network portion.
///
/// **Addresses are varied from their low end.** The top of a MAC is the OUI and
/// the top of an IPv4 address is the network; a host-emulation modifier that
/// walked those would generate traffic for a different network than the operator
/// configured. The offsets below therefore point at the last four bytes of each
/// address rather than its start.
fn resolve_one(
    flow: &FlowConfig,
    modifier: &Modifier,
    layers: &LayerOffsets,
) -> Option<StreamModifier> {
    // (layer start, offset within the layer, field width in bytes)
    let (base, within, width) = match modifier.field {
        // A MAC is six bytes; the low four are the ones worth varying.
        ModifierField::EthDst => (layers.ethernet?, 2, 4),
        ModifierField::EthSrc => (layers.ethernet?, 8, 4),
        // The VLAN id is the low twelve bits of the two-byte TCI, so the write
        // has to be masked or it would clobber the priority and DEI bits.
        ModifierField::VlanId => (layers.vlan?, 0, 2),
        // IPv4 addresses are four bytes, so the whole field is the variable.
        ModifierField::Ipv4Src => (layers.ipv4?, 12, 4),
        ModifierField::Ipv4Dst => (layers.ipv4?, 16, 4),
        // An IPv6 header is 8 bytes before its 16-byte source; the interface
        // identifier's low word sits 12 bytes into each address.
        ModifierField::Ipv6Src => (layers.ipv6?, 8 + 12, 4),
        ModifierField::Ipv6Dst => (layers.ipv6?, 24 + 12, 4),
        ModifierField::L4SrcPort => (layers.l4?, 0, 2),
        ModifierField::L4DstPort => (layers.l4?, 2, 2),
    };

    let offset = base + within;
    let base_value = read_base_value(flow, offset, width)?;
    let span = u64::from(modifier.count.saturating_sub(1)) * u64::from(modifier.step);

    Some(StreamModifier {
        offset,
        width,
        mode: modifier.mode,
        min: base_value,
        max: base_value.saturating_add(span),
        step: u64::from(modifier.step),
    })
}

/// Reads the modifier's starting value out of the built frame.
///
/// Starting from the configured value rather than from zero is what makes a
/// modifier mean "10,000 addresses starting at 10.0.0.1" rather than "10,000
/// addresses starting at 0.0.0.0".
fn read_base_value(flow: &FlowConfig, offset: u16, width: u8) -> Option<u64> {
    // Build at the smallest size the flow can produce; every size shares the
    // same header bytes, so any of them would give the same answer.
    let built = frame::build_with_size(flow, flow.size.min_bytes()).ok()?;
    let start = usize::from(offset);
    let end = start + usize::from(width);
    let bytes = built.bytes.get(start..end)?;

    Some(bytes.iter().fold(0u64, |value, b| (value << 8) | u64::from(*b)))
}

#[cfg(test)]
mod tests {
    use flux_core::flow::{
        EthernetFields, FrameSize, ImixPreset, Ipv4Fields, Ipv6Fields, ModifierMode, Rate,
        UdpFields, VlanFields,
    };
    use flux_core::types::Id;

    use super::*;

    /// Ethernet + IPv4 + UDP at a fixed size, 100% of line.
    fn sample() -> FlowConfig {
        FlowConfig {
            tx_port: Id::nil(),
            rx_port: Id::nil(),
            headers: vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            size: FrameSize::Fixed { bytes: 64 },
            rate: Rate::Percent { value: 100.0 },
            modifiers: Vec::new(),
            duration_secs: None,
            latency_track: false,
        }
    }

    #[test]
    fn a_fixed_size_flow_becomes_one_stream_at_the_full_rate() {
        let streams = to_streams(&sample(), PgId(1), 10_000).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].wire_len, 64);
        assert!((streams[0].pps - 14_880_952.0).abs() < 1.0, "got {}", streams[0].pps);
    }

    #[test]
    fn a_mixture_becomes_one_stream_per_component_summing_to_the_flow_rate() {
        let mut flow = sample();
        flow.size = FrameSize::Imix { preset: ImixPreset::Simple };

        let streams = to_streams(&flow, PgId(10), 10_000).unwrap();
        assert_eq!(streams.len(), 3);
        assert_eq!(streams.iter().map(|s| s.wire_len).collect::<Vec<_>>(), vec![64, 570, 1518]);

        // Consecutive packet groups, so per-flow statistics can be summed back.
        assert_eq!(streams.iter().map(|s| s.pg_id.0).collect::<Vec<_>>(), vec![10, 11, 12]);

        let expected = rate::resolve_for_size(&flow.rate, &flow.size, 10_000).pps;
        let total: f64 = streams.iter().map(|s| s.pps).sum();
        assert!((total - expected).abs() < 1.0, "got {total}, expected {expected}");
    }

    #[test]
    fn mixture_components_are_weighted_seven_four_one() {
        let mut flow = sample();
        flow.size = FrameSize::Imix { preset: ImixPreset::Simple };
        let streams = to_streams(&flow, PgId(0), 10_000).unwrap();

        let ratio = streams[0].pps / streams[2].pps;
        assert!((ratio - 7.0).abs() < 0.001, "64B should be 7× the 1518B rate, got {ratio}");
    }

    #[test]
    fn an_ipv4_source_modifier_covers_the_whole_address_field() {
        let mut flow = sample();
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv4Src,
            mode: ModifierMode::Increment,
            count: 1000,
            step: 1,
        }];

        let streams = to_streams(&flow, PgId(1), 10_000).unwrap();
        let m = streams[0].modifiers[0];

        // Ethernet is 14 bytes and the IPv4 source starts 12 bytes in.
        assert_eq!(m.offset, 26);
        // Four bytes, because TRex flow variables come in 1, 2, 4, or 8 only.
        assert_eq!(m.width, 4);

        // The range still stays inside the host portion: 10.0.0.1 through
        // 10.0.3.232 never touches the first octet.
        assert_eq!(m.min, 0x0A00_0001);
        assert_eq!(m.max, 0x0A00_0001 + 999);
        assert_eq!(m.max >> 24, 10, "the network octet must not move");
    }

    #[test]
    fn a_modifier_starts_from_the_configured_value_not_from_zero() {
        let mut flow = sample();
        flow.headers[1] =
            HeaderLayer::Ipv4(Ipv4Fields { src: "10.1.2.100".into(), ..Default::default() });
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv4Src,
            mode: ModifierMode::Increment,
            count: 10,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        assert_eq!(m.min, 0x0A01_0264, "10.1.2.100 as a 32-bit value");
        assert_eq!(m.max, 0x0A01_0264 + 9);
    }

    #[test]
    fn a_mac_modifier_varies_the_low_four_bytes_leaving_the_oui_alone() {
        let mut flow = sample();
        flow.headers[0] = HeaderLayer::Ethernet(EthernetFields {
            src: "aa:bb:11:22:33:44".into(),
            ..Default::default()
        });
        flow.modifiers = vec![Modifier {
            field: ModifierField::EthSrc,
            mode: ModifierMode::Increment,
            count: 256,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // The source MAC starts at byte 6; its low four bytes at byte 8.
        assert_eq!(m.offset, 8);
        assert_eq!(m.width, 4);
        assert_eq!(m.min, 0x1122_3344);
    }

    #[test]
    fn the_step_widens_the_range_between_first_and_last_value() {
        let mut flow = sample();
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv4Dst,
            mode: ModifierMode::Increment,
            count: 5,
            step: 4,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // Five values four apart spans 16, not 20: the first value is included.
        assert_eq!(m.max - m.min, 16);
        assert_eq!(m.step, 4);
    }

    #[test]
    fn offsets_shift_when_a_vlan_tag_is_inserted() {
        let mut flow = sample();
        flow.headers.insert(1, HeaderLayer::Vlan(VlanFields::default()));
        flow.size = FrameSize::Fixed { bytes: 68 };
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv4Src,
            mode: ModifierMode::Increment,
            count: 10,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // Four bytes further along than the untagged case: 26 becomes 30.
        assert_eq!(m.offset, 30);
    }

    #[test]
    fn a_qinq_vlan_modifier_targets_the_inner_customer_tag() {
        let mut flow = sample();
        flow.headers.insert(
            1,
            HeaderLayer::Vlan(VlanFields { id: 10, tpid: 0x88a8, ..Default::default() }),
        );
        flow.headers.insert(
            2,
            HeaderLayer::Vlan(VlanFields { id: 20, tpid: 0x8100, ..Default::default() }),
        );
        flow.size = FrameSize::Fixed { bytes: 72 };
        flow.modifiers = vec![Modifier {
            field: ModifierField::VlanId,
            mode: ModifierMode::Increment,
            count: 100,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // Outer tag TCI is at 14, inner at 18.
        assert_eq!(m.offset, 18);
        assert_eq!(m.width, 2, "the TCI is two bytes and the write is masked");
        // The base value is the whole TCI; with priority zero that is the id.
        assert_eq!(m.min, 20);
    }

    #[test]
    fn an_l4_port_modifier_finds_the_transport_header() {
        let mut flow = sample();
        flow.modifiers = vec![Modifier {
            field: ModifierField::L4DstPort,
            mode: ModifierMode::Random,
            count: 1000,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // Ethernet 14 + IPv4 20 = 34, destination port at +2.
        assert_eq!(m.offset, 36);
        assert_eq!(m.width, 2);
        assert_eq!(m.mode, ModifierMode::Random);
        // UDP default destination port is 53.
        assert_eq!(m.min, 53);
    }

    #[test]
    fn an_ipv6_modifier_varies_the_interface_identifier() {
        let mut flow = sample();
        flow.headers[1] = HeaderLayer::Ipv6(Ipv6Fields::default());
        flow.size = FrameSize::Fixed { bytes: 128 };
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv6Dst,
            mode: ModifierMode::Increment,
            count: 16,
            step: 1,
        }];

        let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
        // Ethernet is 14; the IPv6 header is 8 bytes before its 16-byte source,
        // so the destination starts at 14 + 24 and its low word 12 bytes later.
        assert_eq!(m.offset, 50);
        assert_eq!(m.width, 4);
        // 2001:db8::2 has 2 in its lowest word.
        assert_eq!(m.min, 2);
        assert_eq!(m.max, 17);
    }

    #[test]
    fn a_modifier_for_an_absent_layer_is_reported_rather_than_misapplied() {
        let mut flow = sample();
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv6Src,
            mode: ModifierMode::Increment,
            count: 10,
            step: 1,
        }];

        assert_eq!(
            to_streams(&flow, PgId(1), 10_000),
            Err(TranslateError::NoSuchField { index: 0, field: "ipv6_src" })
        );
    }

    #[test]
    fn modifier_offsets_agree_with_where_the_builder_actually_wrote_the_field() {
        // The offset table and the frame builder walk the same stack. This pins
        // them together: read the bytes the modifier points at and check they
        // are the field it claims.
        let mut flow = sample();
        flow.headers[1] = HeaderLayer::Ipv4(Ipv4Fields {
            src: "203.0.113.45".into(),
            dst: "198.51.100.7".into(),
            ..Default::default()
        });
        flow.modifiers = vec![
            Modifier {
                field: ModifierField::Ipv4Src,
                mode: ModifierMode::Increment,
                count: 2,
                step: 1,
            },
            Modifier {
                field: ModifierField::Ipv4Dst,
                mode: ModifierMode::Increment,
                count: 2,
                step: 1,
            },
        ];

        let stream = &to_streams(&flow, PgId(1), 10_000).unwrap()[0];
        let packet = &stream.packet;

        let src = stream.modifiers[0];
        assert_eq!(
            &packet[src.offset as usize..src.offset as usize + 4],
            &[203, 0, 113, 45],
            "source modifier does not point at the source address"
        );

        let dst = stream.modifiers[1];
        assert_eq!(
            &packet[dst.offset as usize..dst.offset as usize + 4],
            &[198, 51, 100, 7],
            "destination modifier does not point at the destination address"
        );
    }

    #[test]
    fn every_modifier_width_is_one_trex_can_express() {
        // TRex flow variables are 1, 2, 4, or 8 bytes. A three-byte variable is
        // rejected at stream-programming time, which is a long way from here.
        let mut flow = sample();
        flow.headers.insert(1, HeaderLayer::Vlan(VlanFields::default()));
        flow.headers[2] = HeaderLayer::Ipv4(Ipv4Fields::default());
        flow.size = FrameSize::Fixed { bytes: 68 };

        for field in [
            ModifierField::EthSrc,
            ModifierField::EthDst,
            ModifierField::VlanId,
            ModifierField::Ipv4Src,
            ModifierField::Ipv4Dst,
            ModifierField::L4SrcPort,
            ModifierField::L4DstPort,
        ] {
            flow.modifiers =
                vec![Modifier { field, mode: ModifierMode::Increment, count: 4, step: 1 }];
            let m = to_streams(&flow, PgId(1), 10_000).unwrap()[0].modifiers[0];
            assert!(
                matches!(m.width, 1 | 2 | 4 | 8),
                "{} produced an unusable width of {}",
                field.as_str(),
                m.width
            );
        }
    }

    #[test]
    fn latency_tracking_carries_through_to_every_stream() {
        let mut flow = sample();
        flow.latency_track = true;
        flow.size = FrameSize::Imix { preset: ImixPreset::Simple };

        let streams = to_streams(&flow, PgId(1), 10_000).unwrap();
        assert!(streams.iter().all(|s| s.latency));
    }

    #[test]
    fn a_random_size_range_becomes_its_two_endpoints() {
        let mut flow = sample();
        flow.size = FrameSize::Random { min: 64, max: 1518 };

        let streams = to_streams(&flow, PgId(1), 10_000).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].wire_len, 64);
        assert_eq!(streams[1].wire_len, 1518);
    }

    #[test]
    fn a_degenerate_random_range_collapses_to_one_stream() {
        let mut flow = sample();
        flow.size = FrameSize::Random { min: 512, max: 512 };
        assert_eq!(to_streams(&flow, PgId(1), 10_000).unwrap().len(), 1);
    }

    #[test]
    fn a_frame_too_short_for_its_headers_fails_translation() {
        let mut flow = sample();
        flow.headers[1] = HeaderLayer::Ipv6(Ipv6Fields::default());
        flow.size = FrameSize::Fixed { bytes: 64 };
        assert!(matches!(to_streams(&flow, PgId(1), 10_000), Err(TranslateError::Frame(_))));
    }
}
