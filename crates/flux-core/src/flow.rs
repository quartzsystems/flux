//! Flow definitions: what traffic to generate.
//!
//! A flow is an ordered header stack, a frame size, a rate, and a set of field
//! modifiers. It is the unit an operator configures and the unit the engine
//! turns into a stream. The document defined here is what lands in
//! `flows.config` as JSONB, what the REST API accepts, and what a run's
//! `config_snapshot` preserves.
//!
//! Header layers are a list rather than a fixed struct because encapsulation
//! varies: a QinQ frame has two VLAN tags, a plain frame has none, and the order
//! is what determines the bytes on the wire.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::{is_ipv4, is_mac, Validate, Validation};
use crate::types::Id;

/// Smallest frame Ethernet permits, including FCS.
pub const MIN_FRAME_BYTES: u32 = 64;

/// Largest standard (non-jumbo) frame, including FCS.
pub const MAX_FRAME_BYTES: u32 = 1518;

/// Largest frame accepted at all. Beyond this is outside what the NICs we
/// support will transmit without reconfiguration.
pub const MAX_JUMBO_BYTES: u32 = 9216;

/// Bytes of Ethernet overhead that occupy the wire but are not part of the frame.
///
/// Seven bytes of preamble, one start-of-frame delimiter, and twelve bytes of
/// interframe gap. RFC 2544 rates are quoted against layer 1, so this is what
/// makes 14,880,952 pps the right answer for 64-byte frames on 10G rather than
/// 19,531,250.
pub const ETHERNET_OVERHEAD_BYTES: u32 = 20;

/// Length of the frame check sequence, which the NIC appends.
pub const FCS_BYTES: u32 = 4;

// ---------------------------------------------------------------------------
// The flow document
// ---------------------------------------------------------------------------

/// A complete traffic definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowConfig {
    /// Port that transmits.
    #[schema(value_type = String, format = Uuid)]
    pub tx_port: Id,
    /// Port expected to receive. May equal `tx_port` for a loopback test.
    #[schema(value_type = String, format = Uuid)]
    pub rx_port: Id,
    /// Header stack, outermost first.
    pub headers: Vec<HeaderLayer>,
    /// How large the generated frames are.
    pub size: FrameSize,
    /// How fast to send them.
    pub rate: Rate,
    /// Fields varied across generated frames.
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    /// Stop after this many seconds. `None` runs until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Whether to tag frames for one-way latency measurement.
    ///
    /// Latency streams carry a timestamp and cost transmit capacity, so this is
    /// off unless the test needs it.
    #[serde(default)]
    pub latency_track: bool,
}

impl Validate for FlowConfig {
    fn validate_into(&self, v: &mut Validation) {
        v.require(!self.headers.is_empty(), "headers", "a flow needs at least one header");

        let first_is_ethernet = matches!(self.headers.first(), Some(HeaderLayer::Ethernet(_)));
        v.require(
            self.headers.is_empty() || first_is_ethernet,
            "headers.0",
            "the outermost header must be Ethernet",
        );

        for (i, header) in self.headers.iter().enumerate() {
            v.scope("headers", |v| v.scope(i.to_string(), |v| header.validate_into(v)));
        }

        v.scope("size", |v| self.size.validate_into(v));
        v.scope("rate", |v| self.rate.validate_into(v));

        // A frame has to be long enough to hold the headers the operator asked
        // for; otherwise the builder would have to truncate them silently.
        let header_bytes = self.header_bytes();
        let smallest = self.size.min_bytes();
        v.require(
            smallest >= header_bytes + FCS_BYTES,
            "size",
            format!(
                "frames must be at least {} bytes to carry this header stack",
                header_bytes + FCS_BYTES
            ),
        );

        for (i, modifier) in self.modifiers.iter().enumerate() {
            v.scope("modifiers", |v| {
                v.scope(i.to_string(), |v| modifier.validate_into(v));
            });
            // A modifier naming a field no header provides is a typo that would
            // otherwise silently do nothing at run time.
            v.require(
                self.modifiers[i].field.targets_any(&self.headers),
                &format!("modifiers.{i}.field"),
                format!("no {} header in this flow", modifier.field.layer_name()),
            );
        }

        if let Some(seconds) = self.duration_secs {
            v.require(seconds > 0.0, "durationSecs", "must be greater than zero");
            v.require(seconds <= 86_400.0, "durationSecs", "must be at most 24 hours");
        }
    }
}

impl FlowConfig {
    /// Total length of the header stack in bytes.
    pub fn header_bytes(&self) -> u32 {
        self.headers.iter().map(HeaderLayer::byte_len).sum()
    }
}

// ---------------------------------------------------------------------------
// Header layers
// ---------------------------------------------------------------------------

/// One protocol header in the stack.
///
/// Serialised as `{"proto": "ipv4", "fields": {...}}`, which keeps the wire form
/// readable and lets the editor round-trip a layer it does not fully understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "proto", content = "fields", rename_all = "camelCase")]
pub enum HeaderLayer {
    /// Ethernet II.
    Ethernet(EthernetFields),
    /// An 802.1Q or 802.1ad tag. Two stacked tags make QinQ.
    Vlan(VlanFields),
    /// IPv4.
    Ipv4(Ipv4Fields),
    /// IPv6.
    Ipv6(Ipv6Fields),
    /// TCP.
    Tcp(TcpFields),
    /// UDP.
    Udp(UdpFields),
    /// Raw bytes, for a protocol Flux does not model.
    Custom(CustomFields),
}

impl HeaderLayer {
    /// Serialised length of this layer in bytes.
    pub fn byte_len(&self) -> u32 {
        match self {
            HeaderLayer::Ethernet(_) => 14,
            HeaderLayer::Vlan(_) => 4,
            HeaderLayer::Ipv4(_) => 20,
            HeaderLayer::Ipv6(_) => 40,
            HeaderLayer::Tcp(_) => 20,
            HeaderLayer::Udp(_) => 8,
            HeaderLayer::Custom(c) => c.bytes().map(|b| b.len() as u32).unwrap_or(0),
        }
    }

    /// Short name used in error paths and the UI.
    pub fn name(&self) -> &'static str {
        match self {
            HeaderLayer::Ethernet(_) => "ethernet",
            HeaderLayer::Vlan(_) => "vlan",
            HeaderLayer::Ipv4(_) => "ipv4",
            HeaderLayer::Ipv6(_) => "ipv6",
            HeaderLayer::Tcp(_) => "tcp",
            HeaderLayer::Udp(_) => "udp",
            HeaderLayer::Custom(_) => "custom",
        }
    }
}

impl Validate for HeaderLayer {
    fn validate_into(&self, v: &mut Validation) {
        match self {
            HeaderLayer::Ethernet(f) => f.validate_into(v),
            HeaderLayer::Vlan(f) => f.validate_into(v),
            HeaderLayer::Ipv4(f) => f.validate_into(v),
            HeaderLayer::Ipv6(f) => f.validate_into(v),
            HeaderLayer::Tcp(f) => f.validate_into(v),
            HeaderLayer::Udp(f) => f.validate_into(v),
            HeaderLayer::Custom(f) => f.validate_into(v),
        }
    }
}

/// Ethernet II header fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EthernetFields {
    /// Source hardware address.
    pub src: String,
    /// Destination hardware address.
    pub dst: String,
    /// EtherType. Derived from the next layer when omitted, which is what an
    /// operator wants unless they are deliberately building something malformed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ethertype: Option<u16>,
}

impl Default for EthernetFields {
    fn default() -> Self {
        Self { src: "00:00:00:00:00:01".into(), dst: "00:00:00:00:00:02".into(), ethertype: None }
    }
}

impl Validate for EthernetFields {
    fn validate_into(&self, v: &mut Validation) {
        v.require(is_mac(&self.src), "src", "must be a MAC address like 00:11:22:33:44:55");
        v.require(is_mac(&self.dst), "dst", "must be a MAC address like 00:11:22:33:44:55");
    }
}

/// An 802.1Q / 802.1ad tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VlanFields {
    /// VLAN identifier, 0-4095. 0 and 4095 are reserved but permitted here
    /// because testing a device's handling of them is a legitimate thing to do.
    pub id: u16,
    /// Priority code point, 0-7.
    #[serde(default)]
    pub pcp: u8,
    /// Drop eligible indicator.
    #[serde(default)]
    pub dei: bool,
    /// Tag protocol identifier. `0x8100` for 802.1Q, `0x88a8` for the outer tag
    /// of an 802.1ad stack.
    #[serde(default = "default_tpid")]
    pub tpid: u16,
}

/// 802.1Q.
fn default_tpid() -> u16 {
    0x8100
}

impl Default for VlanFields {
    fn default() -> Self {
        Self { id: 100, pcp: 0, dei: false, tpid: default_tpid() }
    }
}

impl Validate for VlanFields {
    fn validate_into(&self, v: &mut Validation) {
        v.require(self.id <= 4095, "id", "must be between 0 and 4095");
        v.require(self.pcp <= 7, "pcp", "must be between 0 and 7");
        v.require(
            matches!(self.tpid, 0x8100 | 0x88a8 | 0x9100),
            "tpid",
            "must be 0x8100, 0x88a8, or 0x9100",
        );
    }
}

/// IPv4 header fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ipv4Fields {
    /// Source address.
    pub src: String,
    /// Destination address.
    pub dst: String,
    /// Time to live.
    #[serde(default = "default_ttl")]
    pub ttl: u8,
    /// Differentiated services code point, 0-63.
    #[serde(default)]
    pub dscp: u8,
    /// Explicit congestion notification, 0-3.
    #[serde(default)]
    pub ecn: u8,
    /// Identification field.
    #[serde(default)]
    pub identification: u16,
    /// Don't-fragment flag.
    #[serde(default)]
    pub dont_fragment: bool,
    /// Protocol number. Derived from the next layer when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u8>,
}

/// The conventional default hop budget.
fn default_ttl() -> u8 {
    64
}

impl Default for Ipv4Fields {
    fn default() -> Self {
        Self {
            src: "10.0.0.1".into(),
            dst: "10.0.0.2".into(),
            ttl: default_ttl(),
            dscp: 0,
            ecn: 0,
            identification: 0,
            dont_fragment: false,
            protocol: None,
        }
    }
}

impl Validate for Ipv4Fields {
    fn validate_into(&self, v: &mut Validation) {
        v.require(is_ipv4(&self.src), "src", "must be a dotted-quad IPv4 address");
        v.require(is_ipv4(&self.dst), "dst", "must be a dotted-quad IPv4 address");
        v.require(self.dscp <= 63, "dscp", "must be between 0 and 63");
        v.require(self.ecn <= 3, "ecn", "must be between 0 and 3");
        // A zero TTL is dropped by the first router, which makes for a test that
        // silently measures nothing.
        v.require(self.ttl > 0, "ttl", "must be greater than zero");
    }
}

/// IPv6 header fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Fields {
    /// Source address.
    pub src: String,
    /// Destination address.
    pub dst: String,
    /// Hop limit, the IPv6 equivalent of TTL.
    #[serde(default = "default_ttl")]
    pub hop_limit: u8,
    /// Traffic class.
    #[serde(default)]
    pub traffic_class: u8,
    /// Flow label, 20 bits.
    #[serde(default)]
    pub flow_label: u32,
    /// Next header. Derived from the next layer when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_header: Option<u8>,
}

impl Default for Ipv6Fields {
    fn default() -> Self {
        Self {
            src: "2001:db8::1".into(),
            dst: "2001:db8::2".into(),
            hop_limit: default_ttl(),
            traffic_class: 0,
            flow_label: 0,
            next_header: None,
        }
    }
}

impl Validate for Ipv6Fields {
    fn validate_into(&self, v: &mut Validation) {
        v.require(is_ipv6(&self.src), "src", "must be an IPv6 address");
        v.require(is_ipv6(&self.dst), "dst", "must be an IPv6 address");
        v.require(self.flow_label <= 0xF_FFFF, "flowLabel", "must fit in 20 bits");
        v.require(self.hop_limit > 0, "hopLimit", "must be greater than zero");
    }
}

/// TCP header fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TcpFields {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Sequence number.
    #[serde(default)]
    pub seq: u32,
    /// Acknowledgement number.
    #[serde(default)]
    pub ack: u32,
    /// Flag bits, as the low six bits of the TCP flags octet.
    #[serde(default = "default_tcp_flags")]
    pub flags: u8,
    /// Advertised window.
    #[serde(default = "default_window")]
    pub window: u16,
}

/// SYN, the flag a stateless generator most often wants.
fn default_tcp_flags() -> u8 {
    0x02
}

/// A conventional non-zero window.
fn default_window() -> u16 {
    8192
}

impl Default for TcpFields {
    fn default() -> Self {
        Self {
            src_port: 1024,
            dst_port: 80,
            seq: 0,
            ack: 0,
            flags: default_tcp_flags(),
            window: default_window(),
        }
    }
}

impl Validate for TcpFields {
    fn validate_into(&self, v: &mut Validation) {
        v.require(self.flags <= 0x3F, "flags", "must fit in six bits");
    }
}

/// UDP header fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UdpFields {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
}

impl Default for UdpFields {
    fn default() -> Self {
        Self { src_port: 1024, dst_port: 53 }
    }
}

impl Validate for UdpFields {
    fn validate_into(&self, _v: &mut Validation) {
        // Every 16-bit value is a legal UDP port, including zero.
    }
}

/// Raw bytes appended verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomFields {
    /// Hex string, with or without separators.
    pub hex: String,
}

impl CustomFields {
    /// Decodes the hex string, ignoring whitespace, colons, and dashes.
    ///
    /// Returns `None` when the string is not valid hex or has an odd length.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let cleaned: String =
            self.hex.chars().filter(|c| !c.is_whitespace() && *c != ':' && *c != '-').collect();
        if cleaned.len() % 2 != 0 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        cleaned
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                let s = std::str::from_utf8(pair).ok()?;
                u8::from_str_radix(s, 16).ok()
            })
            .collect()
    }
}

impl Validate for CustomFields {
    fn validate_into(&self, v: &mut Validation) {
        match self.bytes() {
            None => v.error("hex", "must be an even number of hex digits"),
            Some(bytes) => v.require(
                !bytes.is_empty() && bytes.len() <= 512,
                "hex",
                "must be between 1 and 512 bytes",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame size
// ---------------------------------------------------------------------------

/// How large generated frames are.
///
/// All sizes are the on-wire layer 2 frame length **including** the four-byte
/// FCS, which is the RFC 2544 convention. The frame builder emits four bytes
/// fewer, because the NIC appends the FCS itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FrameSize {
    /// Every frame the same length.
    Fixed {
        /// Frame length including FCS.
        bytes: u32,
    },
    /// A weighted mixture approximating internet traffic.
    Imix {
        /// Which mixture.
        preset: ImixPreset,
    },
    /// Uniformly distributed between two bounds, inclusive.
    Random {
        /// Smallest frame.
        min: u32,
        /// Largest frame.
        max: u32,
    },
}

impl FrameSize {
    /// Mean frame length in bytes, which is what rate maths uses.
    pub fn average_bytes(&self) -> f64 {
        match self {
            FrameSize::Fixed { bytes } => f64::from(*bytes),
            FrameSize::Imix { preset } => preset.average_bytes(),
            FrameSize::Random { min, max } => (f64::from(*min) + f64::from(*max)) / 2.0,
        }
    }

    /// Shortest frame this size can produce.
    pub fn min_bytes(&self) -> u32 {
        match self {
            FrameSize::Fixed { bytes } => *bytes,
            FrameSize::Imix { preset } => {
                preset.entries().iter().map(|e| e.bytes).min().unwrap_or(0)
            }
            FrameSize::Random { min, .. } => *min,
        }
    }

    /// Longest frame this size can produce.
    pub fn max_bytes(&self) -> u32 {
        match self {
            FrameSize::Fixed { bytes } => *bytes,
            FrameSize::Imix { preset } => {
                preset.entries().iter().map(|e| e.bytes).max().unwrap_or(0)
            }
            FrameSize::Random { max, .. } => *max,
        }
    }
}

impl Validate for FrameSize {
    fn validate_into(&self, v: &mut Validation) {
        match self {
            FrameSize::Fixed { bytes } => {
                v.require(
                    (MIN_FRAME_BYTES..=MAX_JUMBO_BYTES).contains(bytes),
                    "bytes",
                    format!("must be between {MIN_FRAME_BYTES} and {MAX_JUMBO_BYTES}"),
                );
            }
            FrameSize::Imix { .. } => {}
            FrameSize::Random { min, max } => {
                v.require(
                    (MIN_FRAME_BYTES..=MAX_JUMBO_BYTES).contains(min),
                    "min",
                    format!("must be between {MIN_FRAME_BYTES} and {MAX_JUMBO_BYTES}"),
                );
                v.require(
                    (MIN_FRAME_BYTES..=MAX_JUMBO_BYTES).contains(max),
                    "max",
                    format!("must be between {MIN_FRAME_BYTES} and {MAX_JUMBO_BYTES}"),
                );
                v.require(min <= max, "max", "must be at least the minimum");
            }
        }
    }
}

/// A named frame-size mixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ImixPreset {
    /// The conventional 7:4:1 mixture of 64, 570, and 1518 byte frames.
    Simple,
    /// A three-part mixture weighted toward small frames, as used by several
    /// switch vendors for buffer testing.
    Tolly,
}

/// One component of a mixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImixEntry {
    /// Frame length including FCS.
    pub bytes: u32,
    /// Relative frequency.
    pub weight: u32,
}

impl ImixPreset {
    /// The components of this mixture.
    pub fn entries(self) -> &'static [ImixEntry] {
        match self {
            ImixPreset::Simple => &[
                ImixEntry { bytes: 64, weight: 7 },
                ImixEntry { bytes: 570, weight: 4 },
                ImixEntry { bytes: 1518, weight: 1 },
            ],
            ImixPreset::Tolly => &[
                ImixEntry { bytes: 64, weight: 55 },
                ImixEntry { bytes: 594, weight: 5 },
                ImixEntry { bytes: 1518, weight: 40 },
            ],
        }
    }

    /// Weighted mean frame length.
    pub fn average_bytes(self) -> f64 {
        let entries = self.entries();
        let total_weight: u32 = entries.iter().map(|e| e.weight).sum();
        if total_weight == 0 {
            return 0.0;
        }
        let weighted: u32 = entries.iter().map(|e| e.bytes * e.weight).sum();
        f64::from(weighted) / f64::from(total_weight)
    }
}

// ---------------------------------------------------------------------------
// Rate
// ---------------------------------------------------------------------------

/// How fast to transmit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Rate {
    /// Packets per second.
    Pps {
        /// Frames per second.
        value: f64,
    },
    /// Bits per second, measured at layer 1 — including preamble and interframe
    /// gap, so that "10 Gb/s" means a saturated 10G link rather than 97% of one.
    Bps {
        /// Bits per second.
        value: f64,
    },
    /// Percentage of the transmitting port's line rate.
    Percent {
        /// Percentage, 0 to 100.
        value: f64,
    },
}

impl Validate for Rate {
    fn validate_into(&self, v: &mut Validation) {
        match self {
            Rate::Pps { value } => {
                v.require(*value > 0.0, "value", "must be greater than zero");
                v.require(value.is_finite(), "value", "must be a finite number");
            }
            Rate::Bps { value } => {
                v.require(*value > 0.0, "value", "must be greater than zero");
                v.require(value.is_finite(), "value", "must be a finite number");
            }
            Rate::Percent { value } => {
                v.require(
                    *value > 0.0 && *value <= 100.0,
                    "value",
                    "must be greater than 0 and at most 100",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Modifiers
// ---------------------------------------------------------------------------

/// A field varied across generated frames.
///
/// This is how one flow definition emulates thousands of hosts: a modifier on
/// `ipv4.src` with a count of 10,000 produces frames cycling through 10,000
/// source addresses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Modifier {
    /// Which field to vary.
    pub field: ModifierField,
    /// How to vary it.
    pub mode: ModifierMode,
    /// How many distinct values to cycle through.
    pub count: u32,
    /// Distance between consecutive values, for `increment`.
    #[serde(default = "default_step")]
    pub step: u32,
}

/// One.
fn default_step() -> u32 {
    1
}

impl Validate for Modifier {
    fn validate_into(&self, v: &mut Validation) {
        v.require(self.count >= 1, "count", "must be at least 1");
        v.require(self.step >= 1, "step", "must be at least 1");

        // The generated values must fit the field, or they wrap into a different
        // subnet or port range than the operator intended.
        if let Some(width_bits) = self.field.width_bits() {
            let span = u64::from(self.count).saturating_mul(u64::from(self.step));
            let capacity = 1u64.checked_shl(width_bits).unwrap_or(u64::MAX);
            v.require(
                span <= capacity,
                "count",
                format!(
                    "{} × {} exceeds the {width_bits}-bit range of {}",
                    self.count,
                    self.step,
                    self.field.as_str()
                ),
            );
        }
    }
}

/// Which field a modifier varies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModifierField {
    /// Ethernet source address.
    EthSrc,
    /// Ethernet destination address.
    EthDst,
    /// VLAN identifier.
    VlanId,
    /// IPv4 source address.
    Ipv4Src,
    /// IPv4 destination address.
    Ipv4Dst,
    /// IPv6 source address.
    Ipv6Src,
    /// IPv6 destination address.
    Ipv6Dst,
    /// TCP or UDP source port.
    L4SrcPort,
    /// TCP or UDP destination port.
    L4DstPort,
}

impl ModifierField {
    /// Stable token used on the wire and in error paths.
    pub fn as_str(self) -> &'static str {
        match self {
            ModifierField::EthSrc => "eth_src",
            ModifierField::EthDst => "eth_dst",
            ModifierField::VlanId => "vlan_id",
            ModifierField::Ipv4Src => "ipv4_src",
            ModifierField::Ipv4Dst => "ipv4_dst",
            ModifierField::Ipv6Src => "ipv6_src",
            ModifierField::Ipv6Dst => "ipv6_dst",
            ModifierField::L4SrcPort => "l4_src_port",
            ModifierField::L4DstPort => "l4_dst_port",
        }
    }

    /// Which header layer must be present for this field to exist.
    pub fn layer_name(self) -> &'static str {
        match self {
            ModifierField::EthSrc | ModifierField::EthDst => "ethernet",
            ModifierField::VlanId => "vlan",
            ModifierField::Ipv4Src | ModifierField::Ipv4Dst => "ipv4",
            ModifierField::Ipv6Src | ModifierField::Ipv6Dst => "ipv6",
            ModifierField::L4SrcPort | ModifierField::L4DstPort => "tcp or udp",
        }
    }

    /// How many bits the modifier may range over.
    ///
    /// Addresses are capped well below their true width: varying the top bits of
    /// a MAC or an IPv4 address changes the OUI or the network, which is never
    /// what a host-emulation modifier means. `None` means unbounded.
    pub fn width_bits(self) -> Option<u32> {
        match self {
            // Low 24 bits — the device portion of a MAC, leaving the OUI intact.
            ModifierField::EthSrc | ModifierField::EthDst => Some(24),
            ModifierField::VlanId => Some(12),
            // Low 24 bits, so a modifier stays inside a /8 at worst.
            ModifierField::Ipv4Src | ModifierField::Ipv4Dst => Some(24),
            // Low 32 bits of the interface identifier.
            ModifierField::Ipv6Src | ModifierField::Ipv6Dst => Some(32),
            ModifierField::L4SrcPort | ModifierField::L4DstPort => Some(16),
        }
    }

    /// Whether `headers` contains the layer this field lives in.
    pub fn targets_any(self, headers: &[HeaderLayer]) -> bool {
        headers.iter().any(|h| match self {
            ModifierField::EthSrc | ModifierField::EthDst => {
                matches!(h, HeaderLayer::Ethernet(_))
            }
            ModifierField::VlanId => matches!(h, HeaderLayer::Vlan(_)),
            ModifierField::Ipv4Src | ModifierField::Ipv4Dst => matches!(h, HeaderLayer::Ipv4(_)),
            ModifierField::Ipv6Src | ModifierField::Ipv6Dst => matches!(h, HeaderLayer::Ipv6(_)),
            ModifierField::L4SrcPort | ModifierField::L4DstPort => {
                matches!(h, HeaderLayer::Tcp(_) | HeaderLayer::Udp(_))
            }
        })
    }
}

/// How a modifier walks its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModifierMode {
    /// Step through the range in order, wrapping at the end.
    Increment,
    /// Pick uniformly at random from the range.
    Random,
}

/// True for a parseable IPv6 literal.
pub fn is_ipv6(s: &str) -> bool {
    s.parse::<std::net::Ipv6Addr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid flow: Ethernet + IPv4 + UDP, 64-byte frames.
    fn sample_flow() -> FlowConfig {
        FlowConfig {
            tx_port: Id::new_v4(),
            rx_port: Id::new_v4(),
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
    fn a_minimal_flow_validates() {
        assert!(sample_flow().validate().is_ok(), "{:?}", sample_flow().validate());
    }

    #[test]
    fn the_outermost_header_must_be_ethernet() {
        let mut flow = sample_flow();
        flow.headers.remove(0);
        let errs = flow.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "headers.0"));
    }

    #[test]
    fn a_flow_with_no_headers_is_rejected() {
        let mut flow = sample_flow();
        flow.headers.clear();
        let errs = flow.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "headers"));
    }

    #[test]
    fn a_frame_too_short_for_its_headers_is_rejected() {
        let mut flow = sample_flow();
        // Ethernet + IPv6 + TCP is 14 + 40 + 20 = 74, plus FCS is 78.
        flow.headers = vec![
            HeaderLayer::Ethernet(EthernetFields::default()),
            HeaderLayer::Ipv6(Ipv6Fields::default()),
            HeaderLayer::Tcp(TcpFields::default()),
        ];
        flow.size = FrameSize::Fixed { bytes: 64 };

        let errs = flow.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "size" && e.msg.contains("78")),
            "expected a minimum-size error naming 78, got {errs:?}"
        );

        flow.size = FrameSize::Fixed { bytes: 78 };
        assert!(flow.validate().is_ok());
    }

    #[test]
    fn header_field_errors_carry_their_layer_index() {
        let mut flow = sample_flow();
        flow.headers[1] =
            HeaderLayer::Ipv4(Ipv4Fields { src: "not-an-address".into(), ..Default::default() });
        let errs = flow.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "headers.1.src"),
            "expected headers.1.src, got {errs:?}"
        );
    }

    #[test]
    fn a_modifier_targeting_an_absent_layer_is_rejected() {
        let mut flow = sample_flow();
        flow.modifiers = vec![Modifier {
            field: ModifierField::Ipv6Src,
            mode: ModifierMode::Increment,
            count: 10,
            step: 1,
        }];
        let errs = flow.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "modifiers.0.field"),
            "expected modifiers.0.field, got {errs:?}"
        );
    }

    #[test]
    fn a_modifier_range_may_not_overflow_its_field() {
        let mut flow = sample_flow();
        // A VLAN id is 12 bits, so 4096 values is exactly the capacity and 4097
        // is one too many.
        flow.headers.insert(1, HeaderLayer::Vlan(VlanFields::default()));
        flow.size = FrameSize::Fixed { bytes: 68 };

        flow.modifiers = vec![Modifier {
            field: ModifierField::VlanId,
            mode: ModifierMode::Increment,
            count: 4096,
            step: 1,
        }];
        assert!(flow.validate().is_ok(), "{:?}", flow.validate());

        flow.modifiers[0].count = 4097;
        let errs = flow.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "modifiers.0.count"));
    }

    #[test]
    fn a_modifier_step_multiplies_into_the_range_check() {
        let mut flow = sample_flow();
        // 2048 values two apart covers the full 12-bit VLAN range exactly.
        flow.headers.insert(1, HeaderLayer::Vlan(VlanFields::default()));
        flow.size = FrameSize::Fixed { bytes: 68 };
        flow.modifiers = vec![Modifier {
            field: ModifierField::VlanId,
            mode: ModifierMode::Increment,
            count: 2048,
            step: 2,
        }];
        assert!(flow.validate().is_ok());

        flow.modifiers[0].step = 3;
        assert!(flow.validate().is_err());
    }

    #[test]
    fn header_lengths_match_the_protocols() {
        assert_eq!(HeaderLayer::Ethernet(EthernetFields::default()).byte_len(), 14);
        assert_eq!(HeaderLayer::Vlan(VlanFields::default()).byte_len(), 4);
        assert_eq!(HeaderLayer::Ipv4(Ipv4Fields::default()).byte_len(), 20);
        assert_eq!(HeaderLayer::Ipv6(Ipv6Fields::default()).byte_len(), 40);
        assert_eq!(HeaderLayer::Tcp(TcpFields::default()).byte_len(), 20);
        assert_eq!(HeaderLayer::Udp(UdpFields::default()).byte_len(), 8);
    }

    #[test]
    fn the_simple_imix_average_matches_the_published_figure() {
        // (7×64 + 4×570 + 1×1518) / 12 = 4246 / 12
        let average = ImixPreset::Simple.average_bytes();
        assert!((average - 353.833).abs() < 0.01, "got {average}");
    }

    #[test]
    fn custom_hex_accepts_the_separators_people_paste() {
        let expected = vec![0xde, 0xad, 0xbe, 0xef];
        for form in ["deadbeef", "DE:AD:BE:EF", "de ad be ef", "de-ad-be-ef", "DEAD BEEF"] {
            let custom = CustomFields { hex: form.into() };
            assert_eq!(custom.bytes().as_deref(), Some(expected.as_slice()), "failed on {form:?}");
        }
    }

    #[test]
    fn custom_hex_rejects_odd_lengths_and_non_hex() {
        assert!(CustomFields { hex: "abc".into() }.bytes().is_none());
        assert!(CustomFields { hex: "zzzz".into() }.bytes().is_none());
        assert!(CustomFields { hex: String::new() }.bytes().is_some_and(|b| b.is_empty()));
    }

    #[test]
    fn frame_sizes_report_their_bounds() {
        let fixed = FrameSize::Fixed { bytes: 512 };
        assert_eq!(fixed.min_bytes(), 512);
        assert_eq!(fixed.max_bytes(), 512);
        assert_eq!(fixed.average_bytes(), 512.0);

        let random = FrameSize::Random { min: 64, max: 1518 };
        assert_eq!(random.min_bytes(), 64);
        assert_eq!(random.max_bytes(), 1518);
        assert_eq!(random.average_bytes(), 791.0);

        let imix = FrameSize::Imix { preset: ImixPreset::Simple };
        assert_eq!(imix.min_bytes(), 64);
        assert_eq!(imix.max_bytes(), 1518);
    }

    #[test]
    fn a_random_size_range_must_not_be_inverted() {
        let mut flow = sample_flow();
        flow.size = FrameSize::Random { min: 1000, max: 500 };
        let errs = flow.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "size.max"));
    }

    #[test]
    fn rates_must_be_positive_and_finite() {
        let mut flow = sample_flow();

        flow.rate = Rate::Pps { value: 0.0 };
        assert!(flow.validate().is_err());

        flow.rate = Rate::Pps { value: f64::INFINITY };
        assert!(flow.validate().is_err());

        flow.rate = Rate::Percent { value: 101.0 };
        assert!(flow.validate().is_err());

        flow.rate = Rate::Percent { value: 100.0 };
        assert!(flow.validate().is_ok());
    }

    #[test]
    fn the_header_wire_form_is_proto_plus_fields() {
        let layer = HeaderLayer::Udp(UdpFields { src_port: 1234, dst_port: 53 });
        let json = serde_json::to_string(&layer).unwrap();
        assert!(json.contains("\"proto\":\"udp\""), "got {json}");
        assert!(json.contains("\"fields\""), "got {json}");

        let back: HeaderLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layer);
    }

    #[test]
    fn a_flow_round_trips_through_json_unchanged() {
        // This document is stored as JSONB and restored from a run snapshot; a
        // field that does not survive the round trip is a silently altered test.
        let flow = sample_flow();
        let json = serde_json::to_string(&flow).unwrap();
        let back: FlowConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, flow);
    }
}
