//! Turning a load profile into a programmed stateful load.
//!
//! The counterpart of `orch::translate` for L4-7: it resolves the operator's
//! document into the engine-agnostic [`AstfProfile`], mapping database port ids
//! onto engine port numbers and flattening the application spec into the request
//! and response sizes both engines understand.

use std::collections::HashMap;

use flux_core::engine::{AstfProfile, EnginePortId};
use flux_core::profile::{AppSpec, LoadProfileConfig};
use flux_core::types::Id;

/// The port an emulated HTTP server listens on.
///
/// Fixed rather than configurable: the profile already chooses the application,
/// and a port number is one more thing to get wrong for no measurement benefit.
/// It is exposed as a constant so the report and the UI can state it.
pub const SERVER_LISTEN_PORT: u16 = 80;

/// A profile could not be turned into a load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// A named port is not a member of the engine's port group.
    #[error("{side} port is not a member of this port group")]
    PortOutsideGroup {
        /// Which side named it.
        side: &'static str,
    },

    /// The application spec cannot be realised by this build.
    #[error("{0}")]
    Unsupported(String),
}

/// Builds the engine-agnostic load a profile describes.
///
/// `engine_index` maps database port ids onto the engine's port numbering,
/// which is the port group's member ordering.
pub fn to_astf(
    profile: &LoadProfileConfig,
    engine_index: &HashMap<Id, EnginePortId>,
) -> Result<AstfProfile, ProfileError> {
    let client_port = *engine_index
        .get(&profile.client_port)
        .ok_or(ProfileError::PortOutsideGroup { side: "client" })?;
    let server_port = *engine_index
        .get(&profile.server_port)
        .ok_or(ProfileError::PortOutsideGroup { side: "server" })?;

    let (request_bytes, response_bytes, pcap_ref) = match &profile.app {
        AppSpec::HttpGet { path, response_bytes } => {
            // The request size is the header block the encoder will actually
            // build, so the figure the operator sees matches the bytes sent.
            let request = 80 + path.len() as u32;
            (request, *response_bytes, None)
        }
        AppSpec::Raw { request_bytes, response_bytes } => {
            (*request_bytes, *response_bytes, None)
        }
        AppSpec::Pcap { pcap_ref } => {
            // Replay is a different program shape entirely; the reference is
            // carried through so the engine can refuse it by name rather than
            // silently substituting a synthetic exchange.
            return Err(ProfileError::Unsupported(format!(
                "replaying capture {pcap_ref} is not implemented; use an HTTP or raw application"
            )));
        }
    };

    Ok(AstfProfile {
        client_port,
        server_port,
        client_cidr: profile.client_pool.cidr.clone(),
        server_cidr: profile.server_pool.cidr.clone(),
        client_port_min: profile.client_pool.port_min,
        client_port_max: profile.client_pool.port_max,
        server_listen_port: SERVER_LISTEN_PORT,
        request_bytes,
        response_bytes,
        target_cps: profile.target_cps,
        max_concurrent: profile.max_concurrent,
        warmup_secs: profile.ramp.warmup_secs,
        pcap_ref,
    })
}

/// The throughput a profile implies once it reaches its target rate.
///
/// Shown in the editor, because a connection rate and a response size that each
/// look reasonable can multiply into something well past the link — and that is
/// far easier to see here than from a run that quietly tops out.
pub fn implied_bits_per_second(profile: &LoadProfileConfig) -> f64 {
    profile.target_cps * profile.app.bytes_per_connection() as f64 * 8.0
}

#[cfg(test)]
mod tests {
    use flux_core::profile::{IpPool, Ramp};

    use super::*;

    /// Database ids for the two sides.
    fn ports() -> (Id, Id, HashMap<Id, EnginePortId>) {
        let client = Id::from_u128(1);
        let server = Id::from_u128(2);

        let mut index = HashMap::new();
        index.insert(client, EnginePortId(0));
        index.insert(server, EnginePortId(1));

        (client, server, index)
    }

    /// A profile using the given application.
    fn profile(app: AppSpec) -> LoadProfileConfig {
        let (client, server, _) = ports();
        LoadProfileConfig {
            client_port: client,
            server_port: server,
            client_pool: IpPool { cidr: "16.0.0.0/16".into(), port_min: 1024, port_max: 65535 },
            server_pool: IpPool { cidr: "48.0.0.0/24".into(), port_min: 80, port_max: 80 },
            app,
            target_cps: 10_000.0,
            max_concurrent: 100_000,
            ramp: Ramp { warmup_secs: 10.0, settle_secs: 5.0 },
            duration_secs: None,
        }
    }

    #[test]
    fn database_ports_resolve_to_engine_port_numbers() {
        let (_, _, index) = ports();
        let load = to_astf(&profile(AppSpec::default()), &index).unwrap();

        assert_eq!(load.client_port, EnginePortId(0));
        assert_eq!(load.server_port, EnginePortId(1));
    }

    #[test]
    fn a_port_outside_the_group_is_reported_by_side() {
        // Naming which side is wrong is the difference between a fixable error
        // and one an operator has to bisect.
        let (client, _, mut index) = ports();
        index.remove(&client);

        assert_eq!(
            to_astf(&profile(AppSpec::default()), &index),
            Err(ProfileError::PortOutsideGroup { side: "client" })
        );
    }

    #[test]
    fn the_address_pools_and_port_range_carry_through() {
        let (_, _, index) = ports();
        let load = to_astf(&profile(AppSpec::default()), &index).unwrap();

        assert_eq!(load.client_cidr, "16.0.0.0/16");
        assert_eq!(load.server_cidr, "48.0.0.0/24");
        assert_eq!(load.client_port_min, 1024);
        assert_eq!(load.client_port_max, 65535);
        assert_eq!(load.server_listen_port, SERVER_LISTEN_PORT);
    }

    #[test]
    fn an_http_profile_derives_its_request_size_from_the_path() {
        // The figure has to match what the encoder actually builds, or the
        // operator's estimate disagrees with the bytes on the wire.
        let (_, _, index) = ports();
        let short = to_astf(
            &profile(AppSpec::HttpGet { path: "/".into(), response_bytes: 1000 }),
            &index,
        )
        .unwrap();
        let long = to_astf(
            &profile(AppSpec::HttpGet { path: "/a/rather/longer/path".into(), response_bytes: 1000 }),
            &index,
        )
        .unwrap();

        assert!(long.request_bytes > short.request_bytes);
        assert_eq!(short.response_bytes, 1000);
    }

    #[test]
    fn a_raw_profile_uses_its_sizes_verbatim() {
        let (_, _, index) = ports();
        let load = to_astf(
            &profile(AppSpec::Raw { request_bytes: 128, response_bytes: 4096 }),
            &index,
        )
        .unwrap();

        assert_eq!(load.request_bytes, 128);
        assert_eq!(load.response_bytes, 4096);
    }

    #[test]
    fn a_capture_replay_is_refused_by_name_rather_than_substituted() {
        // Quietly sending a synthetic exchange instead of the capture would
        // make the run measure something the operator did not configure.
        let (_, _, index) = ports();
        let result = to_astf(&profile(AppSpec::Pcap { pcap_ref: "web.pcap".into() }), &index);

        match result {
            Err(ProfileError::Unsupported(message)) => {
                assert!(message.contains("web.pcap"), "{message}");
            }
            other => panic!("expected a refusal naming the capture, got {other:?}"),
        }
    }

    #[test]
    fn the_ramp_carries_through_to_the_engine() {
        let (_, _, index) = ports();
        let load = to_astf(&profile(AppSpec::default()), &index).unwrap();
        assert_eq!(load.warmup_secs, 10.0);
    }

    #[test]
    fn implied_throughput_multiplies_rate_by_conversation_size() {
        // 10,000 connections a second each moving about 33 kB is a few gigabits;
        // an operator seeing that number will notice if it exceeds their link.
        let p = profile(AppSpec::Raw { request_bytes: 100, response_bytes: 900 });
        assert_eq!(implied_bits_per_second(&p), 10_000.0 * 1000.0 * 8.0);
    }

    #[test]
    fn a_capture_profile_implies_no_throughput_until_it_is_loaded() {
        let p = profile(AppSpec::Pcap { pcap_ref: "web.pcap".into() });
        assert_eq!(implied_bits_per_second(&p), 0.0);
    }
}
