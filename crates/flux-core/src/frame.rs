//! Turning a header stack into bytes.
//!
//! This is the one place a flow becomes a packet. It exists in Rust rather than
//! being duplicated in the browser because the derived fields — EtherTypes,
//! lengths, and three different checksums — are exactly the kind of thing that
//! goes subtly wrong in a second implementation, and a flow whose preview
//! disagrees with what the engine transmits is worse than no preview at all.
//!
//! ## Length convention
//!
//! [`FrameSize`](crate::flow::FrameSize) is the on-wire layer 2 length
//! **including** the four-byte FCS, which is the RFC 2544 convention. The bytes
//! produced here are four shorter, because the NIC computes and appends the FCS
//! itself.
//!
//! ## Derived fields
//!
//! Anything that can be worked out from the stack is, unless the operator set it
//! explicitly. Building a deliberately malformed frame is a legitimate thing to
//! want — testing how a device under test handles a bad length is a real test —
//! so an explicit value always wins over a derived one.

use crate::flow::{
    CustomFields, EthernetFields, FlowConfig, FrameSize, HeaderLayer, Ipv4Fields, Ipv6Fields,
    TcpFields, UdpFields, FCS_BYTES,
};

/// EtherType for IPv4.
const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType for IPv6.
const ETHERTYPE_IPV6: u16 = 0x86DD;

/// IEEE 802 local experimental EtherType.
///
/// Used when the payload is raw bytes we cannot classify. Claiming such a frame
/// is IPv4 would make a capture actively misleading.
const ETHERTYPE_EXPERIMENTAL: u16 = 0x88B5;

/// IP protocol number for TCP.
const IP_PROTO_TCP: u8 = 6;
/// IP protocol number for UDP.
const IP_PROTO_UDP: u8 = 17;

/// RFC 3692 experimental protocol number, for an unclassifiable payload.
const IP_PROTO_EXPERIMENTAL: u8 = 0xFD;

/// A frame could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The requested length cannot hold the header stack.
    #[error("a {requested}-byte frame cannot hold {header_bytes} bytes of headers plus FCS")]
    TooShort {
        /// Frame length that was asked for, including FCS.
        requested: u32,
        /// Length of the header stack.
        header_bytes: u32,
    },

    /// A custom layer's hex string could not be decoded.
    #[error("layer {index} contains invalid hex")]
    BadHex {
        /// Position of the offending layer in the stack.
        index: usize,
    },
}

/// A built frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The bytes the NIC will transmit, excluding the FCS it appends.
    pub bytes: Vec<u8>,
    /// The on-wire length including FCS, which is what the operator asked for.
    pub wire_len: u32,
}

impl Frame {
    /// Renders the frame as a classic hex dump.
    ///
    /// Sixteen bytes per line, offset on the left, ASCII on the right — the
    /// layout `tcpdump -x` and every other tool uses, so it can be compared
    /// against a capture without re-reading it.
    pub fn hex_dump(&self) -> String {
        let mut out = String::with_capacity(self.bytes.len() * 4);
        for (offset, chunk) in self.bytes.chunks(16).enumerate() {
            out.push_str(&format!("{:04x}  ", offset * 16));

            for i in 0..16 {
                match chunk.get(i) {
                    Some(b) => out.push_str(&format!("{b:02x} ")),
                    None => out.push_str("   "),
                }
                if i == 7 {
                    out.push(' ');
                }
            }

            out.push_str(" |");
            for b in chunk {
                out.push(if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' });
            }
            out.push_str("|\n");
        }
        out
    }
}

/// Builds the first frame a flow would generate.
///
/// "First" matters: modifiers vary fields across frames, and this renders the
/// stack with its base values, which is what an operator is checking when they
/// look at a preview.
pub fn build(flow: &FlowConfig) -> Result<Frame, FrameError> {
    build_with_size(flow, flow.size.min_bytes())
}

/// Builds one frame at an explicit on-wire length.
///
/// Used by the preview to show what a mixture's shortest and longest frames look
/// like, and by the translator when a stream needs a concrete size.
pub fn build_with_size(flow: &FlowConfig, wire_len: u32) -> Result<Frame, FrameError> {
    let header_bytes = flow.header_bytes();
    if wire_len < header_bytes + FCS_BYTES {
        return Err(FrameError::TooShort { requested: wire_len, header_bytes });
    }

    // What the NIC transmits: everything except the FCS it appends itself.
    let emit_len = (wire_len - FCS_BYTES) as usize;
    let mut bytes = Vec::with_capacity(emit_len);

    // Offsets are recorded on the way down so the checksum and length fields —
    // which depend on how much follows them — can be patched on the way back up.
    let mut ipv4_starts: Vec<usize> = Vec::new();
    let mut ipv6_starts: Vec<usize> = Vec::new();
    let mut l4_start: Option<(usize, L4Kind)> = None;

    for (index, layer) in flow.headers.iter().enumerate() {
        let next = flow.headers.get(index + 1);
        match layer {
            HeaderLayer::Ethernet(f) => write_ethernet(&mut bytes, f, next),
            HeaderLayer::Vlan(f) => write_vlan(&mut bytes, f, next),
            HeaderLayer::Ipv4(f) => {
                ipv4_starts.push(bytes.len());
                write_ipv4(&mut bytes, f, next);
            }
            HeaderLayer::Ipv6(f) => {
                ipv6_starts.push(bytes.len());
                write_ipv6(&mut bytes, f, next);
            }
            HeaderLayer::Tcp(f) => {
                l4_start = Some((bytes.len(), L4Kind::Tcp));
                write_tcp(&mut bytes, f);
            }
            HeaderLayer::Udp(f) => {
                l4_start = Some((bytes.len(), L4Kind::Udp));
                write_udp(&mut bytes, f);
            }
            HeaderLayer::Custom(f) => {
                let raw = f.bytes().ok_or(FrameError::BadHex { index })?;
                bytes.extend_from_slice(&raw);
            }
        }
    }

    write_payload(&mut bytes, emit_len);

    // Lengths and checksums, innermost first: an outer IPv4 total length counts
    // the inner headers, so the inner ones must already be correct.
    if let Some((start, kind)) = l4_start {
        patch_l4(&mut bytes, start, kind, ipv4_starts.last().copied(), ipv6_starts.last().copied());
    }
    for start in ipv6_starts.iter().rev() {
        patch_ipv6(&mut bytes, *start);
    }
    for start in ipv4_starts.iter().rev() {
        patch_ipv4(&mut bytes, *start);
    }

    Ok(Frame { bytes, wire_len })
}

/// Which transport header was written, for checksum purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum L4Kind {
    Tcp,
    Udp,
}

// ---------------------------------------------------------------------------
// Layer serialisation
// ---------------------------------------------------------------------------

/// Writes an Ethernet II header.
///
/// The EtherType field carries the *next* layer's identity, and for a VLAN that
/// is the tag protocol identifier rather than a payload type — which is why the
/// tag itself contributes only four bytes further on.
fn write_ethernet(out: &mut Vec<u8>, f: &EthernetFields, next: Option<&HeaderLayer>) {
    out.extend_from_slice(&parse_mac(&f.dst));
    out.extend_from_slice(&parse_mac(&f.src));
    out.extend_from_slice(&f.ethertype.unwrap_or_else(|| ethertype_for(next)).to_be_bytes());
}

/// Writes the tag control information and the following EtherType.
fn write_vlan(out: &mut Vec<u8>, f: &crate::flow::VlanFields, next: Option<&HeaderLayer>) {
    let tci = (u16::from(f.pcp & 0x7) << 13) | (u16::from(f.dei) << 12) | (f.id & 0x0FFF);
    out.extend_from_slice(&tci.to_be_bytes());
    out.extend_from_slice(&ethertype_for(next).to_be_bytes());
}

/// Writes an IPv4 header with placeholder length and checksum.
fn write_ipv4(out: &mut Vec<u8>, f: &Ipv4Fields, next: Option<&HeaderLayer>) {
    out.push(0x45); // version 4, 5 words of header
    out.push(((f.dscp & 0x3F) << 2) | (f.ecn & 0x03));
    out.extend_from_slice(&[0, 0]); // total length, patched later
    out.extend_from_slice(&f.identification.to_be_bytes());

    let flags = if f.dont_fragment { 0x4000u16 } else { 0 };
    out.extend_from_slice(&flags.to_be_bytes());

    out.push(f.ttl);
    out.push(f.protocol.unwrap_or_else(|| ip_protocol_for(next)));
    out.extend_from_slice(&[0, 0]); // header checksum, patched later
    out.extend_from_slice(&parse_ipv4(&f.src));
    out.extend_from_slice(&parse_ipv4(&f.dst));
}

/// Writes an IPv6 header with a placeholder payload length.
fn write_ipv6(out: &mut Vec<u8>, f: &Ipv6Fields, next: Option<&HeaderLayer>) {
    let vtf = (6u32 << 28) | (u32::from(f.traffic_class) << 20) | (f.flow_label & 0x000F_FFFF);
    out.extend_from_slice(&vtf.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // payload length, patched later
    out.push(f.next_header.unwrap_or_else(|| ip_protocol_for(next)));
    out.push(f.hop_limit);
    out.extend_from_slice(&parse_ipv6(&f.src));
    out.extend_from_slice(&parse_ipv6(&f.dst));
}

/// Writes a TCP header with a placeholder checksum.
fn write_tcp(out: &mut Vec<u8>, f: &TcpFields) {
    out.extend_from_slice(&f.src_port.to_be_bytes());
    out.extend_from_slice(&f.dst_port.to_be_bytes());
    out.extend_from_slice(&f.seq.to_be_bytes());
    out.extend_from_slice(&f.ack.to_be_bytes());
    out.push(0x50); // five 32-bit words of header, no options
    out.push(f.flags & 0x3F);
    out.extend_from_slice(&f.window.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // checksum, patched later
    out.extend_from_slice(&[0, 0]); // urgent pointer
}

/// Writes a UDP header with placeholder length and checksum.
fn write_udp(out: &mut Vec<u8>, f: &UdpFields) {
    out.extend_from_slice(&f.src_port.to_be_bytes());
    out.extend_from_slice(&f.dst_port.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // length, patched later
    out.extend_from_slice(&[0, 0]); // checksum, patched later
}

/// Pads the frame out to its target length.
///
/// The pattern is an incrementing byte sequence rather than zeroes: in a capture
/// it makes truncation, reordering, and off-by-one padding immediately visible,
/// where a run of zeroes looks the same however it went wrong.
fn write_payload(out: &mut Vec<u8>, emit_len: usize) {
    let start = out.len();
    for i in start..emit_len {
        out.push((i - start) as u8);
    }
}

// ---------------------------------------------------------------------------
// Derived field patching
// ---------------------------------------------------------------------------

/// Fills in an IPv4 total length and header checksum.
fn patch_ipv4(bytes: &mut [u8], start: usize) {
    let total_len = (bytes.len() - start) as u16;
    bytes[start + 2..start + 4].copy_from_slice(&total_len.to_be_bytes());

    // The checksum covers the header only, and is computed with its own field
    // zeroed — which it already is, since nothing has written it yet.
    bytes[start + 10..start + 12].copy_from_slice(&[0, 0]);
    let sum = checksum(&bytes[start..start + 20]);
    bytes[start + 10..start + 12].copy_from_slice(&sum.to_be_bytes());
}

/// Fills in an IPv6 payload length.
///
/// Unlike IPv4's total length, this counts only what follows the 40-byte header.
fn patch_ipv6(bytes: &mut [u8], start: usize) {
    let payload_len = (bytes.len() - start - 40) as u16;
    bytes[start + 4..start + 6].copy_from_slice(&payload_len.to_be_bytes());
}

/// Fills in a transport length and checksum.
///
/// Both TCP and UDP checksum a pseudo-header drawn from the enclosing IP layer,
/// so this needs to know where that layer starts. With no IP layer present there
/// is nothing to build a pseudo-header from, and the checksum is left zero —
/// which for UDP is explicitly "not computed" and for TCP is a frame the
/// operator built deliberately.
fn patch_l4(
    bytes: &mut [u8],
    start: usize,
    kind: L4Kind,
    ipv4_start: Option<usize>,
    ipv6_start: Option<usize>,
) {
    let l4_len = bytes.len() - start;

    if kind == L4Kind::Udp {
        bytes[start + 4..start + 6].copy_from_slice(&(l4_len as u16).to_be_bytes());
    }

    let checksum_offset = match kind {
        L4Kind::Tcp => start + 16,
        L4Kind::Udp => start + 6,
    };
    bytes[checksum_offset..checksum_offset + 2].copy_from_slice(&[0, 0]);

    let protocol = match kind {
        L4Kind::Tcp => IP_PROTO_TCP,
        L4Kind::Udp => IP_PROTO_UDP,
    };

    let pseudo: Vec<u8> = if let Some(ip) = ipv6_start {
        let mut p = Vec::with_capacity(40);
        p.extend_from_slice(&bytes[ip + 8..ip + 40]); // source and destination
        p.extend_from_slice(&(l4_len as u32).to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, protocol]);
        p
    } else if let Some(ip) = ipv4_start {
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&bytes[ip + 12..ip + 20]); // source and destination
        p.push(0);
        p.push(protocol);
        p.extend_from_slice(&(l4_len as u16).to_be_bytes());
        p
    } else {
        return;
    };

    let sum = checksum_over(&[&pseudo, &bytes[start..]]);
    bytes[checksum_offset..checksum_offset + 2].copy_from_slice(&sum.to_be_bytes());
}

/// The internet checksum (RFC 1071) of one buffer.
fn checksum(data: &[u8]) -> u16 {
    checksum_over(&[data])
}

/// The internet checksum of several buffers treated as one.
///
/// Each buffer is padded independently to an even length. That is correct here
/// because every pseudo-header this is called with already has an even length,
/// so only the final buffer can need padding — which is exactly the rule.
fn checksum_over(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;

    for part in parts {
        let mut chunks = part.chunks_exact(2);
        for pair in &mut chunks {
            sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
        }
        if let Some(&last) = chunks.remainder().first() {
            sum += u32::from(u16::from_be_bytes([last, 0]));
        }
    }

    // Fold the carries back in, twice: the first fold can itself carry.
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !(sum as u16)
}

// ---------------------------------------------------------------------------
// Field derivation
// ---------------------------------------------------------------------------

/// The EtherType identifying `next`.
fn ethertype_for(next: Option<&HeaderLayer>) -> u16 {
    match next {
        Some(HeaderLayer::Ipv4(_)) => ETHERTYPE_IPV4,
        Some(HeaderLayer::Ipv6(_)) => ETHERTYPE_IPV6,
        Some(HeaderLayer::Vlan(v)) => v.tpid,
        _ => ETHERTYPE_EXPERIMENTAL,
    }
}

/// The IP protocol number identifying `next`.
fn ip_protocol_for(next: Option<&HeaderLayer>) -> u8 {
    match next {
        Some(HeaderLayer::Tcp(_)) => IP_PROTO_TCP,
        Some(HeaderLayer::Udp(_)) => IP_PROTO_UDP,
        _ => IP_PROTO_EXPERIMENTAL,
    }
}

// ---------------------------------------------------------------------------
// Address parsing
// ---------------------------------------------------------------------------

/// Parses a MAC address, yielding zeroes for anything malformed.
///
/// Validation runs before a flow is ever stored, so a malformed address cannot
/// reach here through the API. Returning zeroes rather than panicking keeps a
/// hand-edited database row from taking the daemon down.
fn parse_mac(s: &str) -> [u8; 6] {
    let mut out = [0u8; 6];
    for (i, part) in s.split(':').take(6).enumerate() {
        out[i] = u8::from_str_radix(part, 16).unwrap_or(0);
    }
    out
}

/// Parses a dotted-quad address, yielding zeroes for anything malformed.
fn parse_ipv4(s: &str) -> [u8; 4] {
    s.parse::<std::net::Ipv4Addr>().map(|a| a.octets()).unwrap_or([0; 4])
}

/// Parses an IPv6 address, yielding zeroes for anything malformed.
fn parse_ipv6(s: &str) -> [u8; 16] {
    s.parse::<std::net::Ipv6Addr>().map(|a| a.octets()).unwrap_or([0; 16])
}

/// Convenience for building a custom layer from raw bytes.
pub fn custom_from_bytes(bytes: &[u8]) -> CustomFields {
    CustomFields { hex: bytes.iter().map(|b| format!("{b:02x}")).collect() }
}

/// The frame sizes a mixture will actually emit, for preview purposes.
pub fn sizes_in(size: &FrameSize) -> Vec<u32> {
    match size {
        FrameSize::Fixed { bytes } => vec![*bytes],
        FrameSize::Imix { preset } => preset.entries().iter().map(|e| e.bytes).collect(),
        FrameSize::Random { min, max } => vec![*min, *max],
    }
}

#[cfg(test)]
mod tests {
    use crate::flow::{
        EthernetFields, FlowConfig, FrameSize, Ipv4Fields, Ipv6Fields, Rate, TcpFields, UdpFields,
        VlanFields,
    };
    use crate::types::Id;

    use super::*;

    /// Builds a flow from a header stack at a given frame size.
    fn flow(headers: Vec<HeaderLayer>, bytes: u32) -> FlowConfig {
        FlowConfig {
            tx_port: Id::nil(),
            rx_port: Id::nil(),
            headers,
            size: FrameSize::Fixed { bytes },
            rate: Rate::Percent { value: 100.0 },
            modifiers: Vec::new(),
            duration_secs: None,
            latency_track: false,
        }
    }

    #[test]
    fn a_frame_is_four_bytes_shorter_than_its_wire_length() {
        // The NIC appends the FCS; emitting it here would make every frame four
        // bytes too long on the wire.
        let f = build(&flow(vec![HeaderLayer::Ethernet(EthernetFields::default())], 64)).unwrap();
        assert_eq!(f.bytes.len(), 60);
        assert_eq!(f.wire_len, 64);
    }

    #[test]
    fn ethernet_puts_destination_before_source() {
        let eth = EthernetFields {
            src: "11:22:33:44:55:66".into(),
            dst: "aa:bb:cc:dd:ee:ff".into(),
            ethertype: None,
        };
        let f = build(&flow(vec![HeaderLayer::Ethernet(eth)], 64)).unwrap();

        assert_eq!(&f.bytes[0..6], &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(&f.bytes[6..12], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    }

    #[test]
    fn the_ethertype_is_derived_from_the_next_layer() {
        let ipv4 = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
            ],
            64,
        ))
        .unwrap();
        assert_eq!(&ipv4.bytes[12..14], &[0x08, 0x00]);

        let ipv6 = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv6(Ipv6Fields::default()),
            ],
            80,
        ))
        .unwrap();
        assert_eq!(&ipv6.bytes[12..14], &[0x86, 0xDD]);
    }

    #[test]
    fn an_explicit_ethertype_overrides_the_derived_one() {
        // Building a deliberately mislabelled frame is a legitimate test.
        let eth = EthernetFields { ethertype: Some(0x1234), ..Default::default() };
        let f = build(&flow(
            vec![HeaderLayer::Ethernet(eth), HeaderLayer::Ipv4(Ipv4Fields::default())],
            64,
        ))
        .unwrap();
        assert_eq!(&f.bytes[12..14], &[0x12, 0x34]);
    }

    #[test]
    fn an_unclassifiable_payload_is_not_labelled_ipv4() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Custom(CustomFields { hex: "deadbeef".into() }),
            ],
            64,
        ))
        .unwrap();
        assert_eq!(
            &f.bytes[12..14],
            &ETHERTYPE_EXPERIMENTAL.to_be_bytes(),
            "raw bytes must not claim to be IPv4"
        );
    }

    #[test]
    fn a_vlan_tag_sits_between_the_ethertype_and_the_payload() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Vlan(VlanFields { id: 100, pcp: 3, dei: false, tpid: 0x8100 }),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
            ],
            68,
        ))
        .unwrap();

        assert_eq!(&f.bytes[12..14], &[0x81, 0x00], "Ethernet carries the TPID");
        // PCP 3 in the top three bits, DEI clear, VLAN 100 in the low twelve.
        assert_eq!(&f.bytes[14..16], &[0x60, 0x64]);
        assert_eq!(&f.bytes[16..18], &[0x08, 0x00], "then the payload EtherType");
    }

    #[test]
    fn qinq_stacks_two_tags_with_the_outer_tpid_first() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Vlan(VlanFields { id: 10, tpid: 0x88a8, ..Default::default() }),
                HeaderLayer::Vlan(VlanFields { id: 20, tpid: 0x8100, ..Default::default() }),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
            ],
            72,
        ))
        .unwrap();

        assert_eq!(&f.bytes[12..14], &[0x88, 0xa8], "service tag TPID");
        assert_eq!(&f.bytes[14..16], &[0x00, 0x0a], "service VLAN 10");
        assert_eq!(&f.bytes[16..18], &[0x81, 0x00], "customer tag TPID");
        assert_eq!(&f.bytes[18..20], &[0x00, 0x14], "customer VLAN 20");
        assert_eq!(&f.bytes[20..22], &[0x08, 0x00], "payload EtherType");
    }

    #[test]
    fn the_ipv4_total_length_counts_everything_from_its_own_header_on() {
        // A 128-byte frame emits 124 bytes; 14 are Ethernet, so IPv4 sees 110.
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
            ],
            128,
        ))
        .unwrap();
        assert_eq!(u16::from_be_bytes([f.bytes[16], f.bytes[17]]), 110);
    }

    #[test]
    fn the_ipv6_payload_length_excludes_its_own_header() {
        // A 128-byte frame emits 124; minus 14 Ethernet and 40 IPv6 leaves 70.
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv6(Ipv6Fields::default()),
            ],
            128,
        ))
        .unwrap();
        assert_eq!(u16::from_be_bytes([f.bytes[18], f.bytes[19]]), 70);
    }

    #[test]
    fn the_ipv4_header_checksum_verifies_to_zero() {
        // A correct checksum makes the whole header sum to zero. This is the
        // property a receiver actually checks, so it is the one worth testing.
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            128,
        ))
        .unwrap();
        assert_eq!(checksum(&f.bytes[14..34]), 0, "IPv4 header checksum does not verify");
    }

    #[test]
    fn the_ipv4_checksum_matches_a_hand_computed_reference() {
        // Reference header from the worked example in RFC 1071 §3, whose
        // checksum is widely published as 0xb861.
        let header: [u8; 20] = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(checksum(&header), 0xb861);
    }

    #[test]
    fn the_udp_length_covers_the_header_and_its_payload() {
        // 256-byte frame emits 252; minus 14 Ethernet and 20 IPv4 leaves 218.
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            256,
        ))
        .unwrap();
        assert_eq!(u16::from_be_bytes([f.bytes[38], f.bytes[39]]), 218);
    }

    #[test]
    fn the_udp_checksum_verifies_against_its_pseudo_header() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            128,
        ))
        .unwrap();

        let ip = 14;
        let l4 = 34;
        let l4_len = f.bytes.len() - l4;

        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&f.bytes[ip + 12..ip + 20]);
        pseudo.push(0);
        pseudo.push(IP_PROTO_UDP);
        pseudo.extend_from_slice(&(l4_len as u16).to_be_bytes());

        assert_eq!(
            checksum_over(&[&pseudo, &f.bytes[l4..]]),
            0,
            "UDP checksum does not verify against its pseudo-header"
        );
    }

    #[test]
    fn the_tcp_checksum_verifies_over_ipv6() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv6(Ipv6Fields::default()),
                HeaderLayer::Tcp(TcpFields::default()),
            ],
            256,
        ))
        .unwrap();

        let ip = 14;
        let l4 = 54;
        let l4_len = f.bytes.len() - l4;

        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&f.bytes[ip + 8..ip + 40]);
        pseudo.extend_from_slice(&(l4_len as u32).to_be_bytes());
        pseudo.extend_from_slice(&[0, 0, 0, IP_PROTO_TCP]);

        assert_eq!(checksum_over(&[&pseudo, &f.bytes[l4..]]), 0);
    }

    #[test]
    fn the_ip_protocol_number_is_derived_from_the_transport_layer() {
        let udp = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            128,
        ))
        .unwrap();
        assert_eq!(udp.bytes[23], IP_PROTO_UDP);

        let tcp = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Tcp(TcpFields::default()),
            ],
            128,
        ))
        .unwrap();
        assert_eq!(tcp.bytes[23], IP_PROTO_TCP);
    }

    #[test]
    fn a_frame_too_short_for_its_headers_is_an_error_not_a_truncation() {
        let result = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv6(Ipv6Fields::default()),
                HeaderLayer::Tcp(TcpFields::default()),
            ],
            64,
        ));
        assert!(matches!(result, Err(FrameError::TooShort { requested: 64, header_bytes: 74 })));
    }

    #[test]
    fn invalid_custom_hex_is_reported_with_its_layer_index() {
        let result = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Custom(CustomFields { hex: "not hex".into() }),
            ],
            64,
        ));
        assert_eq!(result, Err(FrameError::BadHex { index: 1 }));
    }

    #[test]
    fn padding_is_a_recognisable_pattern_not_a_run_of_zeroes() {
        let f = build(&flow(vec![HeaderLayer::Ethernet(EthernetFields::default())], 64)).unwrap();
        // Payload starts right after the 14-byte header and counts up from zero.
        assert_eq!(&f.bytes[14..20], &[0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_jumbo_frame_builds_at_full_length() {
        let f = build(&flow(
            vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            9216,
        ))
        .unwrap();
        assert_eq!(f.bytes.len(), 9212);
        // The IPv4 total length field is 16 bits, so 9198 must not have wrapped.
        assert_eq!(u16::from_be_bytes([f.bytes[16], f.bytes[17]]), 9198);
    }

    #[test]
    fn the_hex_dump_is_sixteen_bytes_to_a_line_with_ascii() {
        let f = build(&flow(vec![HeaderLayer::Ethernet(EthernetFields::default())], 64)).unwrap();
        let dump = f.hex_dump();
        let lines: Vec<&str> = dump.lines().collect();

        assert_eq!(lines.len(), 4, "60 bytes is four lines of sixteen");
        assert!(lines[0].starts_with("0000  "));
        assert!(lines[1].starts_with("0010  "));
        assert!(lines[0].contains('|'), "each line carries an ASCII column");
    }

    #[test]
    fn checksum_folds_carries_correctly() {
        // 0xFFFF + 0xFFFF is 0x1FFFE, which folds to 0xFFFF and complements to
        // zero. Without the fold it would truncate to 0xFFFE and complement to 1.
        assert_eq!(checksum(&[0xff, 0xff, 0xff, 0xff]), 0x0000);

        // The all-zero buffer is the other end of the same identity.
        assert_eq!(checksum(&[0x00, 0x00]), 0xffff);

        // A sum that carries twice: three words each just under the limit.
        assert_eq!(checksum(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]), 0x0000);
    }

    #[test]
    fn an_odd_length_buffer_is_padded_on_the_right() {
        // The trailing byte is treated as the high half of a word.
        assert_eq!(checksum(&[0x12]), checksum(&[0x12, 0x00]));
    }
}
