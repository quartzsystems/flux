//! Encoding a [`StreamSpec`] as a TRex stream object.
//!
//! This is the second of the two files carrying TRex-specific field names. The
//! shapes come from the TRex RPC specification and its Python client rather than
//! from a running instance, so each construct is marked. Everything above this
//! layer speaks [`StreamSpec`], which is engine-agnostic.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use flux_core::engine::{StreamModifier, StreamSpec};
use flux_core::flow::ModifierMode;
use serde_json::{json, Value};

/// Sentinel meaning "this stream does not chain to another".
const NO_NEXT_STREAM: i64 = -1;

/// Encodes one stream for `add_stream`.
///
/// TODO(trex-verify): the object shape. `mode`, `packet.binary`, `vm`,
/// `flow_stats`, `self_start`, and `next_stream_id` are all as the Python client
/// builds them; the two most likely to differ on a given build are the
/// `flow_stats.rule_type` spelling and whether `packet.binary` wants base64 or a
/// byte array.
pub fn encode(spec: &StreamSpec, stream_id: u32) -> Value {
    json!({
        "mode": {
            "type": "continuous",
            "pps": spec.pps,
        },
        "packet": {
            // TODO(trex-verify): base64 is what the Python client sends; some
            // builds also accept a plain array of byte values.
            "binary": BASE64.encode(&spec.packet),
            "meta": "",
        },
        "vm": encode_vm(&spec.modifiers),
        // Per-group counters are what makes loss attributable to a flow rather
        // than only to a port, so every stream Flux programs carries them.
        "flow_stats": {
            "enabled": true,
            "stream_id": spec.pg_id.0,
            // TODO(trex-verify): "latency" enables the timestamped measurement
            // path; "stats" is counters only.
            "rule_type": if spec.latency { "latency" } else { "stats" },
        },
        "self_start": true,
        "enabled": true,
        "isg": 0.0,
        "next_stream_id": NO_NEXT_STREAM,
        "action_count": 0,
        "random_seed": 0,
        "core_id": -1,
        "stream_id": stream_id,
    })
}

/// Builds the field-engine program that applies `modifiers`.
///
/// TRex varies packet fields with a small instruction list: declare a variable,
/// then write it into the frame. Checksums have to be recomputed afterwards,
/// which is what the trailing fix-up instruction is for — without it, varying an
/// IP address produces frames every receiver discards.
fn encode_vm(modifiers: &[StreamModifier]) -> Value {
    if modifiers.is_empty() {
        return json!({ "instructions": [], "split_by_var": "" });
    }

    let mut instructions = Vec::with_capacity(modifiers.len() * 2 + 1);

    for (i, modifier) in modifiers.iter().enumerate() {
        let name = format!("var{i}");

        // TODO(trex-verify): `flow_var` op names. "inc" and "random" are the
        // documented spellings; some builds also accept "dec".
        instructions.push(json!({
            "type": "flow_var",
            "name": name,
            "size": modifier.width,
            "op": match modifier.mode {
                ModifierMode::Increment => "inc",
                ModifierMode::Random => "random",
            },
            "init_value": modifier.min,
            "min_value": modifier.min,
            "max_value": modifier.max,
            "step": modifier.step,
        }));

        // A two-byte field that is not the whole word — the VLAN id inside its
        // TCI — needs a masked write, or the priority and drop-eligible bits get
        // overwritten with zeroes.
        if modifier.width == 2 && is_vlan_tci(modifier) {
            // TODO(trex-verify): `write_mask_flow_var` field names.
            instructions.push(json!({
                "type": "write_mask_flow_var",
                "name": name,
                "pkt_offset": modifier.offset,
                "pkt_cast_size": 2,
                "mask": 0x0FFF,
                "shift": 0,
                "add_value": 0,
                "is_big_endian": true,
            }));
        } else {
            instructions.push(json!({
                "type": "write_flow_var",
                "name": name,
                "pkt_offset": modifier.offset,
                "add_value": 0,
                "is_big_endian": true,
            }));
        }
    }

    json!({
        "instructions": instructions,
        // Splitting a variable across cores is what keeps a multi-core instance
        // from generating the same address from every core. Empty means TRex
        // chooses, which is right when there is nothing sensible to split on.
        "split_by_var": "",
    })
}

/// Whether a modifier targets a VLAN tag control word.
///
/// Distinguished by width and by the range fitting in twelve bits: only the VLAN
/// id is a sub-field of its containing word, so only it needs a masked write.
fn is_vlan_tci(modifier: &StreamModifier) -> bool {
    modifier.width == 2 && modifier.max <= 0x0FFF
}

/// Builds the `start_traffic` multiplier object.
///
/// TODO(trex-verify): TRex accepts several multiplier types — `pps`, `bps`,
/// `percentage`, and `raw`. `raw` scales the stream rates as configured, which
/// is what RFC 2544's binary search wants: the streams stay put and only this
/// number moves.
pub fn multiplier(value: f64) -> Value {
    json!({ "type": "raw", "value": value, "op": "abs" })
}

#[cfg(test)]
mod tests {
    use flux_core::engine::PgId;

    use super::*;

    /// A stream with the given modifiers.
    fn spec(modifiers: Vec<StreamModifier>, latency: bool) -> StreamSpec {
        StreamSpec {
            pg_id: PgId(7),
            packet: vec![0xde, 0xad, 0xbe, 0xef],
            wire_len: 64,
            pps: 1000.0,
            modifiers,
            latency,
            total_packets: None,
        }
    }

    /// An increment modifier over a four-byte field.
    fn increment(offset: u16, width: u8, min: u64, max: u64) -> StreamModifier {
        StreamModifier { offset, width, mode: ModifierMode::Increment, min, max, step: 1 }
    }

    #[test]
    fn a_stream_carries_its_rate_and_packet() {
        let encoded = encode(&spec(Vec::new(), false), 1);

        assert_eq!(encoded["mode"]["type"], "continuous");
        assert_eq!(encoded["mode"]["pps"], 1000.0);
        assert_eq!(encoded["packet"]["binary"], BASE64.encode([0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(encoded["stream_id"], 1);
        assert_eq!(encoded["self_start"], true);
        assert_eq!(encoded["next_stream_id"], -1);
    }

    #[test]
    fn per_group_statistics_are_always_enabled_and_carry_the_packet_group() {
        // Loss that cannot be attributed to a flow is loss an operator cannot act
        // on, so this is not optional.
        let encoded = encode(&spec(Vec::new(), false), 1);
        assert_eq!(encoded["flow_stats"]["enabled"], true);
        assert_eq!(encoded["flow_stats"]["stream_id"], 7);
    }

    #[test]
    fn latency_tracking_selects_the_timestamped_rule_type() {
        assert_eq!(encode(&spec(Vec::new(), false), 1)["flow_stats"]["rule_type"], "stats");
        assert_eq!(encode(&spec(Vec::new(), true), 1)["flow_stats"]["rule_type"], "latency");
    }

    #[test]
    fn a_stream_with_no_modifiers_has_an_empty_program() {
        let encoded = encode(&spec(Vec::new(), false), 1);
        assert_eq!(encoded["vm"]["instructions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_modifier_becomes_a_declaration_followed_by_a_write() {
        let encoded = encode(&spec(vec![increment(26, 4, 0x0A00_0001, 0x0A00_03E8)], false), 1);
        let vm = encoded["vm"]["instructions"].as_array().unwrap();

        assert_eq!(vm.len(), 2, "declare the variable, then write it");

        assert_eq!(vm[0]["type"], "flow_var");
        assert_eq!(vm[0]["size"], 4);
        assert_eq!(vm[0]["op"], "inc");
        assert_eq!(vm[0]["init_value"], 0x0A00_0001u64);
        assert_eq!(vm[0]["max_value"], 0x0A00_03E8u64);

        assert_eq!(vm[1]["type"], "write_flow_var");
        assert_eq!(vm[1]["pkt_offset"], 26);
        assert_eq!(vm[1]["is_big_endian"], true);
        assert_eq!(vm[1]["name"], vm[0]["name"], "the write must name the variable declared");
    }

    #[test]
    fn a_random_modifier_selects_the_random_operator() {
        let m = StreamModifier {
            offset: 36,
            width: 2,
            mode: ModifierMode::Random,
            min: 1024,
            max: 65535,
            step: 1,
        };
        let encoded = encode(&spec(vec![m], false), 1);
        assert_eq!(encoded["vm"]["instructions"][0]["op"], "random");
    }

    #[test]
    fn several_modifiers_get_distinct_variable_names() {
        let encoded =
            encode(&spec(vec![increment(26, 4, 1, 100), increment(30, 4, 1, 100)], false), 1);
        let vm = encoded["vm"]["instructions"].as_array().unwrap();

        assert_eq!(vm.len(), 4);
        assert_ne!(
            vm[0]["name"], vm[2]["name"],
            "two variables sharing a name would collide in the field engine"
        );
        assert_eq!(vm[0]["name"], vm[1]["name"]);
        assert_eq!(vm[2]["name"], vm[3]["name"]);
    }

    #[test]
    fn a_vlan_id_modifier_uses_a_masked_write() {
        // An unmasked two-byte write would zero the priority and drop-eligible
        // bits that share the tag control word.
        let encoded = encode(&spec(vec![increment(18, 2, 100, 200)], false), 1);
        let vm = encoded["vm"]["instructions"].as_array().unwrap();

        assert_eq!(vm[1]["type"], "write_mask_flow_var");
        assert_eq!(vm[1]["mask"], 0x0FFF);
        assert_eq!(vm[1]["pkt_cast_size"], 2);
    }

    #[test]
    fn an_l4_port_modifier_uses_a_plain_write() {
        // Port numbers occupy their whole word, so masking would be wrong.
        let encoded = encode(&spec(vec![increment(36, 2, 1024, 65535)], false), 1);
        assert_eq!(encoded["vm"]["instructions"][1]["type"], "write_flow_var");
    }

    #[test]
    fn the_multiplier_scales_configured_rates_rather_than_replacing_them() {
        // RFC 2544's search moves this number and nothing else; a multiplier
        // that replaced the rate would discard the flow's own configuration.
        let m = multiplier(0.875);
        assert_eq!(m["type"], "raw");
        assert_eq!(m["value"], 0.875);
        assert_eq!(m["op"], "abs");
    }

    #[test]
    fn the_encoded_packet_round_trips_through_base64() {
        let packet: Vec<u8> = (0..=255).collect();
        let mut s = spec(Vec::new(), false);
        s.packet = packet.clone();

        let encoded = encode(&s, 1);
        let decoded = BASE64.decode(encoded["packet"]["binary"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, packet);
    }
}
