//! Encoding a stateful load as a TRex ASTF profile.
//!
//! TRex's stateful mode is programmed with a rather different document from its
//! stateless one: address generators, byte buffers, and a small program per side
//! describing who sends what and who waits for it. This module builds that
//! document from the engine-agnostic [`AstfProfile`].
//!
//! This is the third and last file carrying TRex-specific field names. As with
//! the stateless encoder, the shapes come from the ASTF documentation rather
//! than from a running instance, and each construct is marked.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use flux_core::engine::AstfProfile;
use serde_json::{json, Value};

/// Index of the client's request buffer in `buf_list`.
const BUF_REQUEST: usize = 0;
/// Index of the server's response buffer.
const BUF_RESPONSE: usize = 1;

/// Builds the ASTF profile document.
///
/// TODO(trex-verify): the top-level keys. `buf_list`, `ip_gen_dist_list`,
/// `program_list`, and `templates` are what the ASTF profile schema documents;
/// `c_glob_info`/`s_glob_info` carry TCP tuning and are omitted, which leaves
/// TRex's defaults in place.
pub fn encode(profile: &AstfProfile) -> Value {
    let (client_start, client_end) = address_range(&profile.client_cidr);
    let (server_start, server_end) = address_range(&profile.server_cidr);

    json!({
        // Payloads are carried once and referenced by index, so a large response
        // body is not repeated per template.
        "buf_list": [
            BASE64.encode(request_payload(profile)),
            BASE64.encode(response_payload(profile)),
        ],

        // TODO(trex-verify): `ip_gen_dist_list` entries are referenced by index
        // from each template's `ip_gen`. "seq" walks the range in order; "rand"
        // picks uniformly.
        "ip_gen_dist_list": [
            { "ip_start": client_start, "ip_end": client_end, "distribution": "seq" },
            { "ip_start": server_start, "ip_end": server_end, "distribution": "seq" },
        ],

        "program_list": [client_program(profile), server_program(profile)],

        "templates": [{
            "client_template": {
                "program_index": 0,
                "ip_gen": {
                    "dist_client": { "index": 0 },
                    "dist_server": { "index": 1 },
                },
                "cluster": {},
                "port": profile.server_listen_port,
                // TODO(trex-verify): `cps` on the template is the per-template
                // connection rate; the multiplier passed to `astf_start` scales
                // every template together.
                "cps": profile.target_cps,
                "limit": profile.max_concurrent,
            },
            "server_template": {
                "program_index": 1,
                "assoc": [{ "port": profile.server_listen_port }],
            },
            "tg_name": "flux",
        }],
    })
}

/// The client side: send the request, read the response, close.
///
/// TODO(trex-verify): command names. `tx`, `rx`, and `close_msg` are the
/// documented spellings; `rx` blocks until `min_bytes` have arrived.
fn client_program(profile: &AstfProfile) -> Value {
    json!({
        "commands": [
            { "name": "tx", "buf_index": BUF_REQUEST },
            { "name": "rx", "min_bytes": profile.response_bytes.max(1) },
            { "name": "close_msg" },
        ]
    })
}

/// The server side: wait for the request, answer it, close.
fn server_program(profile: &AstfProfile) -> Value {
    json!({
        "commands": [
            { "name": "rx", "min_bytes": profile.request_bytes.max(1) },
            { "name": "tx", "buf_index": BUF_RESPONSE },
            { "name": "close_msg" },
        ]
    })
}

/// The bytes the client sends.
///
/// A minimal HTTP request when the profile has no explicit payload, because a
/// device under test that inspects L7 should see something it recognises rather
/// than a run of filler.
fn request_payload(profile: &AstfProfile) -> Vec<u8> {
    let head = b"GET / HTTP/1.1\r\nHost: flux\r\nUser-Agent: flux\r\nConnection: close\r\n\r\n";
    pad_to(head, profile.request_bytes as usize)
}

/// The bytes the server sends.
fn response_payload(profile: &AstfProfile) -> Vec<u8> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nServer: flux\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
        profile.response_bytes
    );
    pad_to(head.as_bytes(), profile.response_bytes as usize)
}

/// Extends `head` with filler to reach `total` bytes.
///
/// The filler is a repeating printable pattern rather than zeroes: in a capture
/// it makes truncation and reordering visible, where a run of zeroes looks the
/// same however it went wrong.
fn pad_to(head: &[u8], total: usize) -> Vec<u8> {
    let mut out = head.to_vec();
    if out.len() >= total {
        return out;
    }

    const FILLER: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    while out.len() < total {
        let remaining = total - out.len();
        let take = remaining.min(FILLER.len());
        out.extend_from_slice(&FILLER[..take]);
    }
    out
}

/// The first and last usable address of a CIDR block.
///
/// Falls back to the block itself for a prefix with no room to exclude the
/// network and broadcast addresses. Validation rejects such blocks before a
/// profile is stored, so this is a safety net rather than a path.
fn address_range(cidr: &str) -> (String, String) {
    let Some((base, prefix)) = parse_cidr(cidr) else {
        return (cidr.to_string(), cidr.to_string());
    };

    let base = u32::from(base);
    let size = 1u64 << (32 - u32::from(prefix));
    let network = base & !((size - 1) as u32);

    let (first, last) = if size > 2 {
        (network + 1, network + (size as u32) - 2)
    } else {
        (network, network + (size as u32) - 1)
    };

    (std::net::Ipv4Addr::from(first).to_string(), std::net::Ipv4Addr::from(last).to_string())
}

/// Splits a CIDR into its base address and prefix length.
fn parse_cidr(cidr: &str) -> Option<(std::net::Ipv4Addr, u8)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address: std::net::Ipv4Addr = address.trim().parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some((address, prefix))
}

/// Reads a `get_astf_stats` reply.
///
/// TODO(trex-verify): counter names. These follow the ASTF statistics table:
/// `tcps_connattempt`, `tcps_connects`, `tcps_closed`, `tcps_drops`, and
/// `tcps_sndbyte`/`tcps_rcvbyte`. The reply nests them under `client` and
/// `server`; the client side is what a load figure means.
pub fn decode_stats(result: &Value) -> flux_core::engine::AstfStats {
    let client = result.get("client").unwrap_or(result);
    let get = |key: &str| client.get(key).and_then(Value::as_u64).unwrap_or(0);

    let attempted = get("tcps_connattempt");
    let established = get("tcps_connects");
    let closed = get("tcps_closed");

    flux_core::engine::AstfStats {
        attempted,
        established,
        closed,
        // TRex reports totals rather than a live gauge, so the open count is
        // what has been established but not yet closed.
        active: established.saturating_sub(closed),
        connect_errors: attempted.saturating_sub(established),
        resets: get("tcps_drops"),
        tx_bytes: get("tcps_sndbyte"),
        rx_bytes: get("tcps_rcvbyte"),
    }
}

#[cfg(test)]
mod tests {
    use flux_core::engine::EnginePortId;

    use super::*;

    /// A profile to encode.
    fn profile() -> AstfProfile {
        AstfProfile {
            client_port: EnginePortId(0),
            server_port: EnginePortId(1),
            client_cidr: "16.0.0.0/16".into(),
            server_cidr: "48.0.0.0/24".into(),
            client_port_min: 1024,
            client_port_max: 65535,
            server_listen_port: 80,
            request_bytes: 200,
            response_bytes: 32_768,
            target_cps: 10_000.0,
            max_concurrent: 100_000,
            warmup_secs: 10.0,
            pcap_ref: None,
        }
    }

    #[test]
    fn the_document_carries_the_four_sections_trex_expects() {
        let doc = encode(&profile());

        assert!(doc["buf_list"].is_array());
        assert!(doc["ip_gen_dist_list"].is_array());
        assert!(doc["program_list"].is_array());
        assert!(doc["templates"].is_array());
    }

    #[test]
    fn address_generators_span_the_usable_range_of_each_block() {
        // The network and broadcast addresses are excluded: a host emulated at
        // either is not one a device under test will answer.
        let doc = encode(&profile());

        assert_eq!(doc["ip_gen_dist_list"][0]["ip_start"], "16.0.0.1");
        assert_eq!(doc["ip_gen_dist_list"][0]["ip_end"], "16.0.255.254");
        assert_eq!(doc["ip_gen_dist_list"][1]["ip_start"], "48.0.0.1");
        assert_eq!(doc["ip_gen_dist_list"][1]["ip_end"], "48.0.0.254");
    }

    #[test]
    fn a_cidr_that_is_not_aligned_still_yields_its_network() {
        // 10.0.0.5/24 means the 10.0.0.0 block; an operator writing a host
        // address with a prefix should not get a range starting at the host.
        assert_eq!(address_range("10.0.0.5/24"), ("10.0.0.1".into(), "10.0.0.254".into()));
    }

    #[test]
    fn a_two_address_block_uses_both_of_them() {
        assert_eq!(address_range("10.0.0.0/31"), ("10.0.0.0".into(), "10.0.0.1".into()));
        assert_eq!(address_range("10.0.0.7/32"), ("10.0.0.7".into(), "10.0.0.7".into()));
    }

    #[test]
    fn the_client_sends_then_reads_then_closes() {
        let program = client_program(&profile());
        let commands = program["commands"].as_array().unwrap();

        assert_eq!(commands[0]["name"], "tx");
        assert_eq!(commands[1]["name"], "rx");
        assert_eq!(commands[2]["name"], "close_msg");
        assert_eq!(commands[1]["min_bytes"], 32_768);
    }

    #[test]
    fn the_server_mirrors_the_client() {
        // Reading before sending: a server that transmitted first would not be
        // answering a request.
        let program = server_program(&profile());
        let commands = program["commands"].as_array().unwrap();

        assert_eq!(commands[0]["name"], "rx");
        assert_eq!(commands[0]["min_bytes"], 200);
        assert_eq!(commands[1]["name"], "tx");
    }

    #[test]
    fn payloads_are_padded_to_the_configured_size() {
        let doc = encode(&profile());

        let request = BASE64.decode(doc["buf_list"][0].as_str().unwrap()).unwrap();
        let response = BASE64.decode(doc["buf_list"][1].as_str().unwrap()).unwrap();

        assert_eq!(request.len(), 200);
        assert_eq!(response.len(), 32_768);
    }

    #[test]
    fn payloads_look_like_http_so_an_inspecting_device_recognises_them() {
        let doc = encode(&profile());

        let request = BASE64.decode(doc["buf_list"][0].as_str().unwrap()).unwrap();
        let response = BASE64.decode(doc["buf_list"][1].as_str().unwrap()).unwrap();

        assert!(request.starts_with(b"GET / HTTP/1.1"));
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    }

    #[test]
    fn a_payload_smaller_than_its_header_is_not_truncated() {
        // Cutting an HTTP header in half would produce something no device
        // parses; the header wins and the size is a floor rather than a cap.
        let mut p = profile();
        p.request_bytes = 4;

        let doc = encode(&p);
        let request = BASE64.decode(doc["buf_list"][0].as_str().unwrap()).unwrap();
        assert!(request.starts_with(b"GET"), "the request line survives");
    }

    #[test]
    fn the_template_carries_the_rate_and_the_concurrency_ceiling() {
        let doc = encode(&profile());
        let template = &doc["templates"][0]["client_template"];

        assert_eq!(template["cps"], 10_000.0);
        assert_eq!(template["limit"], 100_000);
        assert_eq!(template["port"], 80);
    }

    #[test]
    fn connection_counters_decode_from_the_client_side() {
        let raw = json!({
            "client": {
                "tcps_connattempt": 1000,
                "tcps_connects": 990,
                "tcps_closed": 900,
                "tcps_drops": 5,
                "tcps_sndbyte": 200_000,
                "tcps_rcvbyte": 32_000_000,
            },
            "server": { "tcps_connects": 990 }
        });

        let stats = decode_stats(&raw);
        assert_eq!(stats.attempted, 1000);
        assert_eq!(stats.established, 990);
        assert_eq!(stats.closed, 900);
        assert_eq!(stats.active, 90, "established but not yet closed");
        assert_eq!(stats.connect_errors, 10);
        assert_eq!(stats.tx_bytes, 200_000);
        assert!((stats.failure_pct() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_reply_without_a_client_section_is_read_at_the_top_level() {
        // Some builds return the counters unnested.
        let stats = decode_stats(&json!({ "tcps_connattempt": 7, "tcps_connects": 7 }));
        assert_eq!(stats.attempted, 7);
        assert_eq!(stats.connect_errors, 0);
    }

    #[test]
    fn missing_counters_decode_to_zero_rather_than_failing_the_read() {
        let stats = decode_stats(&json!({}));
        assert_eq!(stats.attempted, 0);
        assert_eq!(stats.active, 0);
    }

    #[test]
    fn counters_that_went_backwards_saturate_instead_of_wrapping() {
        // A closed count above the established count would otherwise report
        // eighteen quintillion open connections.
        let stats = decode_stats(&json!({
            "tcps_connattempt": 10, "tcps_connects": 5, "tcps_closed": 900
        }));
        assert_eq!(stats.active, 0);
    }
}
