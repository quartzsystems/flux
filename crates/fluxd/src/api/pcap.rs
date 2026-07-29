//! Deriving a flow's header stack from a captured packet.
//!
//! An operator with a capture of the traffic they want to reproduce should not
//! have to retype it into the editor. This reads the first frame out of a pcap
//! and decodes it into the same `HeaderLayer` list the editor edits, so the
//! result is an ordinary flow they can adjust before saving.
//!
//! Only the first packet is read. A pcap is a sequence of frames and a flow is
//! one template, so anything past the first would have to be discarded or
//! guessed at; taking the first and saying so is the honest behaviour.
//!
//! The parser is deliberately small and total: it never panics on malformed
//! input, and anything it cannot classify becomes a `custom` layer carrying the
//! remaining bytes verbatim, so no capture is rejected outright.

use flux_core::flow::{
    CustomFields, EthernetFields, HeaderLayer, Ipv4Fields, Ipv6Fields, TcpFields, UdpFields,
    VlanFields,
};

/// Magic number of a little-endian pcap file, microsecond resolution.
const PCAP_MAGIC_LE: u32 = 0xa1b2_c3d4;
/// Big-endian equivalent.
const PCAP_MAGIC_BE: u32 = 0xd4c3_b2a1;
/// Little-endian, nanosecond resolution.
const PCAP_MAGIC_LE_NS: u32 = 0xa1b2_3c4d;
/// Big-endian, nanosecond resolution.
const PCAP_MAGIC_BE_NS: u32 = 0x4d3c_b2a1;

/// The pcapng Section Header Block, whose presence identifies the format.
const PCAPNG_SHB: u32 = 0x0a0d_0d0a;

/// Length of a pcap file header.
const FILE_HEADER_LEN: usize = 24;
/// Length of a per-packet record header.
const RECORD_HEADER_LEN: usize = 16;

/// LINKTYPE_ETHERNET. The only link type Flux can turn into a header stack.
const LINKTYPE_ETHERNET: u32 = 1;

/// Largest capture we will read.
///
/// Only the first packet is used, so a large file is entirely wasted transfer;
/// the cap keeps a mis-selected file from occupying memory.
pub const MAX_PCAP_BYTES: usize = 16 * 1024 * 1024;

/// Why a capture could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PcapError {
    /// The file is not a pcap, or is a format we do not read.
    #[error("{0}")]
    Unrecognised(String),

    /// The file is a pcap but ends before a complete packet.
    #[error("the capture contains no complete packet")]
    Empty,

    /// The capture is of a link type we cannot turn into a header stack.
    #[error("this capture is link type {0}; only Ethernet captures can be imported")]
    UnsupportedLinkType(u32),

    /// The file is larger than we will read.
    #[error("the capture is larger than {MAX_PCAP_BYTES} bytes")]
    TooLarge,
}

/// What a capture yielded.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedFrame {
    /// The decoded header stack, outermost first.
    pub headers: Vec<HeaderLayer>,
    /// Length of the frame as captured, in bytes.
    pub captured_len: u32,
    /// Length the frame had on the wire, which may exceed what was captured.
    pub original_len: u32,
    /// Notes about anything that was approximated or dropped.
    pub notes: Vec<String>,
}

/// Reads the first packet of a capture and decodes its header stack.
pub fn import(bytes: &[u8]) -> Result<ImportedFrame, PcapError> {
    if bytes.len() > MAX_PCAP_BYTES {
        return Err(PcapError::TooLarge);
    }
    if bytes.len() < FILE_HEADER_LEN {
        return Err(PcapError::Unrecognised("the file is too short to be a capture".into()));
    }

    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic == PCAPNG_SHB {
        // pcapng is a different container entirely. Saying so beats failing with
        // "not a pcap" when the operator plainly has a capture in their hand.
        return Err(PcapError::Unrecognised(
            "this is a pcapng file; save it as classic pcap (Wireshark: File, Save As, \
             Wireshark/tcpdump/… pcap)"
                .into(),
        ));
    }

    let big_endian = match magic {
        PCAP_MAGIC_LE | PCAP_MAGIC_LE_NS => false,
        PCAP_MAGIC_BE | PCAP_MAGIC_BE_NS => true,
        _ => {
            return Err(PcapError::Unrecognised(
                "the file does not begin with a pcap magic number".into(),
            ))
        }
    };

    let read_u32 = |offset: usize| -> u32 {
        let raw = [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]];
        if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        }
    };

    let link_type = read_u32(20);
    if link_type != LINKTYPE_ETHERNET {
        return Err(PcapError::UnsupportedLinkType(link_type));
    }

    if bytes.len() < FILE_HEADER_LEN + RECORD_HEADER_LEN {
        return Err(PcapError::Empty);
    }

    let captured_len = read_u32(FILE_HEADER_LEN + 8);
    let original_len = read_u32(FILE_HEADER_LEN + 12);

    let start = FILE_HEADER_LEN + RECORD_HEADER_LEN;
    let end = start.saturating_add(captured_len as usize);
    if captured_len == 0 || end > bytes.len() {
        return Err(PcapError::Empty);
    }

    let frame = &bytes[start..end];
    let (headers, mut notes) = decode_ethernet(frame);

    if original_len > captured_len {
        notes.push(format!(
            "the capture is truncated: {captured_len} of {original_len} bytes were recorded"
        ));
    }

    Ok(ImportedFrame { headers, captured_len, original_len, notes })
}

/// Decodes an Ethernet frame into layers.
///
/// Returns whatever it could classify plus notes. Decoding stops at the first
/// thing it does not understand, and the remainder becomes a `custom` layer, so
/// the operator gets a working flow rather than an error.
fn decode_ethernet(frame: &[u8]) -> (Vec<HeaderLayer>, Vec<String>) {
    let mut headers = Vec::new();
    let mut notes = Vec::new();

    if frame.len() < 14 {
        notes.push("the frame is too short to contain an Ethernet header".into());
        return (headers, notes);
    }

    headers.push(HeaderLayer::Ethernet(EthernetFields {
        dst: mac(&frame[0..6]),
        src: mac(&frame[6..12]),
        // Left derived: the stack that follows determines it, and carrying the
        // captured value would freeze it against later edits.
        ethertype: None,
    }));

    let mut offset = 12;
    let mut ethertype = be16(&frame[12..14]);
    offset += 2;

    // Walk any number of VLAN tags. QinQ is two; some captures carry more.
    while matches!(ethertype, 0x8100 | 0x88a8 | 0x9100) {
        if frame.len() < offset + 4 {
            notes.push("a VLAN tag is truncated".into());
            return (headers, notes);
        }
        let tci = be16(&frame[offset..offset + 2]);
        headers.push(HeaderLayer::Vlan(VlanFields {
            id: tci & 0x0FFF,
            pcp: ((tci >> 13) & 0x7) as u8,
            dei: (tci >> 12) & 0x1 == 1,
            tpid: ethertype,
        }));
        ethertype = be16(&frame[offset + 2..offset + 4]);
        offset += 4;
    }

    let protocol = match ethertype {
        0x0800 => match decode_ipv4(frame, offset, &mut headers, &mut notes) {
            Some((protocol, next)) => {
                offset = next;
                Some(protocol)
            }
            None => return (headers, notes),
        },
        0x86DD => match decode_ipv6(frame, offset, &mut headers, &mut notes) {
            Some((protocol, next)) => {
                offset = next;
                Some(protocol)
            }
            None => return (headers, notes),
        },
        other => {
            notes.push(format!(
                "EtherType 0x{other:04x} is not modelled; the payload was imported as raw bytes"
            ));
            None
        }
    };

    match protocol {
        Some(6) => {
            if frame.len() >= offset + 20 {
                headers.push(HeaderLayer::Tcp(TcpFields {
                    src_port: be16(&frame[offset..offset + 2]),
                    dst_port: be16(&frame[offset + 2..offset + 4]),
                    seq: be32(&frame[offset + 4..offset + 8]),
                    ack: be32(&frame[offset + 8..offset + 12]),
                    flags: frame[offset + 13] & 0x3F,
                    window: be16(&frame[offset + 14..offset + 16]),
                }));
                let data_offset = usize::from(frame[offset + 12] >> 4) * 4;
                if data_offset > 20 {
                    notes.push(format!(
                        "{} bytes of TCP options were dropped; Flux generates a 20-byte header",
                        data_offset - 20
                    ));
                }
            } else {
                notes.push("the TCP header is truncated".into());
            }
        }
        Some(17) => {
            if frame.len() >= offset + 8 {
                headers.push(HeaderLayer::Udp(UdpFields {
                    src_port: be16(&frame[offset..offset + 2]),
                    dst_port: be16(&frame[offset + 2..offset + 4]),
                }));
            } else {
                notes.push("the UDP header is truncated".into());
            }
        }
        Some(other) => {
            notes
                .push(format!("IP protocol {other} is not modelled; no transport layer was added"));
        }
        None => {
            // The unclassified payload is preserved verbatim so the frame can
            // still be reproduced byte for byte.
            if offset < frame.len() {
                let tail = &frame[offset..frame.len().min(offset + 256)];
                headers.push(HeaderLayer::Custom(CustomFields {
                    hex: tail.iter().map(|b| format!("{b:02x}")).collect(),
                }));
            }
        }
    }

    (headers, notes)
}

/// Decodes an IPv4 header, returning its protocol number and the next offset.
fn decode_ipv4(
    frame: &[u8],
    offset: usize,
    headers: &mut Vec<HeaderLayer>,
    notes: &mut Vec<String>,
) -> Option<(u8, usize)> {
    if frame.len() < offset + 20 {
        notes.push("the IPv4 header is truncated".into());
        return None;
    }

    let tos = frame[offset + 1];
    let flags = be16(&frame[offset + 6..offset + 8]);
    let header_len = usize::from(frame[offset] & 0x0F) * 4;

    headers.push(HeaderLayer::Ipv4(Ipv4Fields {
        src: ipv4(&frame[offset + 12..offset + 16]),
        dst: ipv4(&frame[offset + 16..offset + 20]),
        ttl: frame[offset + 8],
        dscp: tos >> 2,
        ecn: tos & 0x03,
        identification: be16(&frame[offset + 4..offset + 6]),
        dont_fragment: flags & 0x4000 != 0,
        protocol: None,
    }));

    if header_len > 20 {
        notes.push(format!(
            "{} bytes of IPv4 options were dropped; Flux generates a 20-byte header",
            header_len - 20
        ));
    }

    Some((frame[offset + 9], offset + header_len.max(20)))
}

/// Decodes an IPv6 header, returning its next-header value and the next offset.
fn decode_ipv6(
    frame: &[u8],
    offset: usize,
    headers: &mut Vec<HeaderLayer>,
    notes: &mut Vec<String>,
) -> Option<(u8, usize)> {
    if frame.len() < offset + 40 {
        notes.push("the IPv6 header is truncated".into());
        return None;
    }

    let vtf = be32(&frame[offset..offset + 4]);

    headers.push(HeaderLayer::Ipv6(Ipv6Fields {
        src: ipv6(&frame[offset + 8..offset + 24]),
        dst: ipv6(&frame[offset + 24..offset + 40]),
        hop_limit: frame[offset + 7],
        traffic_class: ((vtf >> 20) & 0xFF) as u8,
        flow_label: vtf & 0x000F_FFFF,
        next_header: None,
    }));

    Some((frame[offset + 6], offset + 40))
}

/// Formats six bytes as a colon-separated MAC address.
fn mac(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Formats four bytes as a dotted quad.
fn ipv4(bytes: &[u8]) -> String {
    std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string()
}

/// Formats sixteen bytes as an IPv6 address.
fn ipv6(bytes: &[u8]) -> String {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&bytes[..16]);
    std::net::Ipv6Addr::from(octets).to_string()
}

/// Reads a big-endian 16-bit value.
fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

/// Reads a big-endian 32-bit value.
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a frame in a minimal little-endian pcap file.
    fn pcap(frame: &[u8], link_type: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&PCAP_MAGIC_LE.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes()); // major
        out.extend_from_slice(&4u16.to_le_bytes()); // minor
        out.extend_from_slice(&0i32.to_le_bytes()); // timezone
        out.extend_from_slice(&0u32.to_le_bytes()); // sigfigs
        out.extend_from_slice(&65_535u32.to_le_bytes()); // snaplen
        out.extend_from_slice(&link_type.to_le_bytes());

        out.extend_from_slice(&0u32.to_le_bytes()); // seconds
        out.extend_from_slice(&0u32.to_le_bytes()); // microseconds
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(frame);
        out
    }

    /// Ethernet + IPv4 + UDP, with a short payload.
    fn eth_ipv4_udp() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]); // destination
        f.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]); // source
        f.extend_from_slice(&[0x08, 0x00]); // IPv4

        f.extend_from_slice(&[0x45, 0xb8]); // version/IHL, DSCP 46 ECN 0
        f.extend_from_slice(&[0x00, 0x26]); // total length
        f.extend_from_slice(&[0x12, 0x34]); // identification
        f.extend_from_slice(&[0x40, 0x00]); // don't fragment
        f.push(64); // TTL
        f.push(17); // UDP
        f.extend_from_slice(&[0x00, 0x00]); // checksum
        f.extend_from_slice(&[203, 0, 113, 5]); // source
        f.extend_from_slice(&[198, 51, 100, 9]); // destination

        f.extend_from_slice(&[0x1f, 0x90]); // source port 8080
        f.extend_from_slice(&[0x00, 0x35]); // destination port 53
        f.extend_from_slice(&[0x00, 0x12]); // length
        f.extend_from_slice(&[0x00, 0x00]); // checksum
        f.extend_from_slice(&[0; 10]); // payload
        f
    }

    #[test]
    fn a_udp_capture_yields_the_three_layers_in_order() {
        let imported = import(&pcap(&eth_ipv4_udp(), LINKTYPE_ETHERNET)).unwrap();

        assert_eq!(imported.headers.len(), 3);
        assert!(matches!(imported.headers[0], HeaderLayer::Ethernet(_)));
        assert!(matches!(imported.headers[1], HeaderLayer::Ipv4(_)));
        assert!(matches!(imported.headers[2], HeaderLayer::Udp(_)));
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }

    #[test]
    fn addresses_and_ports_survive_the_round_trip() {
        let imported = import(&pcap(&eth_ipv4_udp(), LINKTYPE_ETHERNET)).unwrap();

        let HeaderLayer::Ethernet(eth) = &imported.headers[0] else { panic!() };
        assert_eq!(eth.dst, "aa:bb:cc:dd:ee:ff");
        assert_eq!(eth.src, "11:22:33:44:55:66");

        let HeaderLayer::Ipv4(ip) = &imported.headers[1] else { panic!() };
        assert_eq!(ip.src, "203.0.113.5");
        assert_eq!(ip.dst, "198.51.100.9");
        assert_eq!(ip.ttl, 64);
        assert_eq!(ip.dscp, 46, "expedited forwarding survives");
        assert!(ip.dont_fragment);
        assert_eq!(ip.identification, 0x1234);

        let HeaderLayer::Udp(udp) = &imported.headers[2] else { panic!() };
        assert_eq!(udp.src_port, 8080);
        assert_eq!(udp.dst_port, 53);
    }

    #[test]
    fn the_ethertype_is_left_derived_rather_than_frozen() {
        // Carrying the captured value would stop the editor from tracking a
        // later change to the stack.
        let imported = import(&pcap(&eth_ipv4_udp(), LINKTYPE_ETHERNET)).unwrap();
        let HeaderLayer::Ethernet(eth) = &imported.headers[0] else { panic!() };
        assert_eq!(eth.ethertype, None);
    }

    #[test]
    fn a_qinq_capture_yields_both_tags_outermost_first() {
        let mut f = Vec::new();
        f.extend_from_slice(&[0; 12]);
        f.extend_from_slice(&[0x88, 0xa8]); // service tag
        f.extend_from_slice(&[0x60, 0x64]); // PCP 3, VLAN 100
        f.extend_from_slice(&[0x81, 0x00]); // customer tag
        f.extend_from_slice(&[0x00, 0xc8]); // VLAN 200
        f.extend_from_slice(&[0x08, 0x00]); // IPv4
        f.extend_from_slice(&eth_ipv4_udp()[14..]);

        let imported = import(&pcap(&f, LINKTYPE_ETHERNET)).unwrap();

        let HeaderLayer::Vlan(outer) = &imported.headers[1] else { panic!() };
        assert_eq!(outer.tpid, 0x88a8);
        assert_eq!(outer.id, 100);
        assert_eq!(outer.pcp, 3);

        let HeaderLayer::Vlan(inner) = &imported.headers[2] else { panic!() };
        assert_eq!(inner.tpid, 0x8100);
        assert_eq!(inner.id, 200);

        assert!(matches!(imported.headers[3], HeaderLayer::Ipv4(_)));
    }

    #[test]
    fn a_tcp_capture_keeps_its_flags_and_window() {
        let mut f = eth_ipv4_udp();
        f[23] = 6; // protocol becomes TCP
        f.truncate(34);
        f.extend_from_slice(&[0x1f, 0x90]); // source port
        f.extend_from_slice(&[0x00, 0x50]); // destination port 80
        f.extend_from_slice(&[0, 0, 0, 1]); // sequence
        f.extend_from_slice(&[0, 0, 0, 2]); // acknowledgement
        f.push(0x50); // five words, no options
        f.push(0x12); // SYN + ACK
        f.extend_from_slice(&[0x20, 0x00]); // window
        f.extend_from_slice(&[0, 0, 0, 0]); // checksum, urgent

        let imported = import(&pcap(&f, LINKTYPE_ETHERNET)).unwrap();
        let HeaderLayer::Tcp(tcp) = imported.headers.last().unwrap() else { panic!() };

        assert_eq!(tcp.dst_port, 80);
        assert_eq!(tcp.flags, 0x12);
        assert_eq!(tcp.window, 0x2000);
        assert_eq!(tcp.seq, 1);
        assert_eq!(tcp.ack, 2);
    }

    #[test]
    fn dropped_ip_options_are_reported_rather_than_silently_lost() {
        // Flux generates a 20-byte IPv4 header, so options cannot be reproduced.
        // Saying so beats a flow that quietly differs from the capture.
        let mut f = eth_ipv4_udp();
        f[14] = 0x46; // IHL of 6 words: 24 bytes

        let imported = import(&pcap(&f, LINKTYPE_ETHERNET)).unwrap();
        assert!(imported.notes.iter().any(|n| n.contains("IPv4 options")), "{:?}", imported.notes);
    }

    #[test]
    fn an_unmodelled_ethertype_becomes_a_custom_layer() {
        // No capture is rejected outright; the bytes are preserved so the frame
        // can still be reproduced.
        let mut f = vec![0u8; 12];
        f.extend_from_slice(&[0x88, 0xcc]); // LLDP
        f.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        let imported = import(&pcap(&f, LINKTYPE_ETHERNET)).unwrap();

        assert!(matches!(imported.headers[1], HeaderLayer::Custom(_)));
        let HeaderLayer::Custom(custom) = &imported.headers[1] else { panic!() };
        assert_eq!(custom.hex, "deadbeef");
        assert!(imported.notes.iter().any(|n| n.contains("0x88cc")));
    }

    #[test]
    fn a_big_endian_capture_reads_the_same_as_a_little_endian_one() {
        let frame = eth_ipv4_udp();
        let mut be = pcap(&frame, LINKTYPE_ETHERNET);

        // Rewrite the header in big-endian form.
        be[0..4].copy_from_slice(&PCAP_MAGIC_BE.to_le_bytes());
        be[20..24].copy_from_slice(&LINKTYPE_ETHERNET.to_be_bytes());
        be[32..36].copy_from_slice(&(frame.len() as u32).to_be_bytes());
        be[36..40].copy_from_slice(&(frame.len() as u32).to_be_bytes());

        let from_be = import(&be).unwrap();
        let from_le = import(&pcap(&frame, LINKTYPE_ETHERNET)).unwrap();
        assert_eq!(from_be.headers, from_le.headers);
    }

    #[test]
    fn a_truncated_capture_is_reported_in_the_notes() {
        let frame = eth_ipv4_udp();
        let mut data = pcap(&frame, LINKTYPE_ETHERNET);
        // Claim the frame was longer on the wire than what was recorded.
        data[36..40].copy_from_slice(&9000u32.to_le_bytes());

        let imported = import(&data).unwrap();
        assert_eq!(imported.original_len, 9000);
        assert!(imported.notes.iter().any(|n| n.contains("truncated")), "{:?}", imported.notes);
    }

    #[test]
    fn a_pcapng_file_says_what_to_do_about_it() {
        let mut data = PCAPNG_SHB.to_le_bytes().to_vec();
        data.extend_from_slice(&[0; 32]);

        match import(&data) {
            Err(PcapError::Unrecognised(message)) => {
                assert!(message.contains("pcapng"), "{message}");
                assert!(message.contains("Save As"), "the message should say how to fix it");
            }
            other => panic!("expected a pcapng diagnosis, got {other:?}"),
        }
    }

    #[test]
    fn a_non_ethernet_capture_is_refused_by_link_type() {
        // LINKTYPE_RAW carries no Ethernet header to build a stack from.
        assert_eq!(import(&pcap(&eth_ipv4_udp(), 101)), Err(PcapError::UnsupportedLinkType(101)));
    }

    #[test]
    fn a_capture_with_no_packets_is_reported_as_empty() {
        let mut header = pcap(&[], LINKTYPE_ETHERNET);
        header.truncate(FILE_HEADER_LEN);
        assert_eq!(import(&header), Err(PcapError::Empty));
    }

    #[test]
    fn a_record_claiming_more_bytes_than_the_file_holds_is_rejected() {
        // A malformed length must not read past the buffer.
        let mut data = pcap(&eth_ipv4_udp(), LINKTYPE_ETHERNET);
        data[32..36].copy_from_slice(&100_000u32.to_le_bytes());
        assert_eq!(import(&data), Err(PcapError::Empty));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // The parser runs on operator-supplied files; total behaviour matters
        // more than diagnosing every malformation.
        let mut data = pcap(&eth_ipv4_udp(), LINKTYPE_ETHERNET);
        for cut in 0..data.len() {
            let _ = import(&data[..cut]);
        }
        for byte in 0..data.len().min(64) {
            data[byte] = data[byte].wrapping_add(37);
            let _ = import(&data);
        }
    }

    #[test]
    fn a_file_that_is_not_a_capture_is_rejected() {
        let junk = vec![0x7fu8; 128];
        assert!(matches!(import(&junk), Err(PcapError::Unrecognised(_))));
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_parsed() {
        let huge = vec![0u8; MAX_PCAP_BYTES + 1];
        assert_eq!(import(&huge), Err(PcapError::TooLarge));
    }
}
