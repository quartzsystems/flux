//! Rate arithmetic.
//!
//! Three ways of expressing "how fast" — packets per second, bits per second,
//! and percent of line rate — all have to reduce to the same number before the
//! engine sees them, and RFC 2544 drives its binary search through percentages
//! of a rate this module computes. Getting it wrong is not a rounding error; it
//! is a throughput figure that disagrees with every other tester in the lab.
//!
//! ## Layer 1 versus layer 2
//!
//! Every bits-per-second figure here is **layer 1**: it counts the 7-byte
//! preamble, the start-of-frame delimiter, and the 12-byte interframe gap
//! alongside the frame itself. That is the convention RFC 2544 and commercial
//! testers use, and it is what makes 14,880,952 pps the correct answer for
//! 64-byte frames on a 10G link. Quoting layer 2 instead would report a
//! saturated link as 76% utilised for small frames.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::flow::{FrameSize, Rate, ETHERNET_OVERHEAD_BYTES};

/// A rate expressed every way at once.
///
/// The flow editor shows all of these together, because an operator who enters
/// "100%" wants to see the packet rate it implies, and one who enters a packet
/// rate wants to know whether it fits on the link.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRate {
    /// Frames per second.
    pub pps: f64,
    /// Bits per second on the wire, including preamble and interframe gap.
    pub bps_l1: f64,
    /// Bits per second of frame content, excluding the gap.
    pub bps_l2: f64,
    /// Percentage of the transmitting port's line rate.
    ///
    /// May exceed 100, which is how the UI tells an operator their requested
    /// rate does not fit before the engine refuses it.
    pub line_pct: f64,
}

impl ResolvedRate {
    /// True when the rate exceeds what the port can transmit.
    ///
    /// A hair over 100% is arithmetic noise from a percentage round-trip, not an
    /// operator asking for the impossible, so the check has a small tolerance.
    pub fn exceeds_line_rate(&self) -> bool {
        self.line_pct > 100.000_001
    }
}

/// Bits occupied on the wire by one frame of `frame_bytes`, including overhead.
pub fn wire_bits_per_frame(frame_bytes: f64) -> f64 {
    (frame_bytes + f64::from(ETHERNET_OVERHEAD_BYTES)) * 8.0
}

/// The port's capacity in bits per second.
pub fn port_bits_per_second(speed_mbps: u32) -> f64 {
    // Network speeds are decimal: a 10G link is 10,000,000,000 bits per second.
    f64::from(speed_mbps) * 1_000_000.0
}

/// The maximum frame rate a port can sustain at a given frame size.
///
/// This is the denominator of every RFC 2544 percentage.
pub fn line_rate_pps(speed_mbps: u32, frame_bytes: f64) -> f64 {
    let per_frame = wire_bits_per_frame(frame_bytes);
    if per_frame <= 0.0 {
        return 0.0;
    }
    port_bits_per_second(speed_mbps) / per_frame
}

/// Reduces a configured rate to concrete numbers.
///
/// `frame_bytes` is the *average* frame length for the flow's size setting,
/// which is what makes a mixture's rate meaningful. `speed_mbps` is the
/// transmitting port's line speed.
///
/// A port with no known speed yields zeroes rather than infinities: an unbound
/// or absent port has no line rate, and propagating a NaN into the UI would
/// render as "NaN Gb/s" rather than as the missing information it is.
pub fn resolve(rate: &Rate, frame_bytes: f64, speed_mbps: u32) -> ResolvedRate {
    let per_frame_l1 = wire_bits_per_frame(frame_bytes);
    let port_bps = port_bits_per_second(speed_mbps);

    if per_frame_l1 <= 0.0 || !frame_bytes.is_finite() {
        return ResolvedRate { pps: 0.0, bps_l1: 0.0, bps_l2: 0.0, line_pct: 0.0 };
    }

    let pps = match rate {
        Rate::Pps { value } => *value,
        Rate::Bps { value } => value / per_frame_l1,
        Rate::Percent { value } => {
            if port_bps <= 0.0 {
                0.0
            } else {
                (port_bps / per_frame_l1) * (value / 100.0)
            }
        }
    };

    let bps_l1 = pps * per_frame_l1;
    let bps_l2 = pps * frame_bytes * 8.0;
    let line_pct = if port_bps > 0.0 { (bps_l1 / port_bps) * 100.0 } else { 0.0 };

    ResolvedRate { pps, bps_l1, bps_l2, line_pct }
}

/// Reduces a rate against a frame-size setting.
pub fn resolve_for_size(rate: &Rate, size: &FrameSize, speed_mbps: u32) -> ResolvedRate {
    resolve(rate, size.average_bytes(), speed_mbps)
}

/// Scales a rate to a percentage of itself.
///
/// This is the operation RFC 2544's binary search performs on every trial: the
/// streams stay as configured and only the engine's multiplier moves, so the
/// search never has to reprogram the flow.
pub fn at_percent(full: &ResolvedRate, percent: f64) -> ResolvedRate {
    let factor = percent / 100.0;
    ResolvedRate {
        pps: full.pps * factor,
        bps_l1: full.bps_l1 * factor,
        bps_l2: full.bps_l2 * factor,
        line_pct: full.line_pct * factor,
    }
}

#[cfg(test)]
mod tests {
    use crate::flow::ImixPreset;

    use super::*;

    /// Compares against a reference with a relative tolerance.
    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() <= expected.abs() * tolerance
    }

    #[test]
    fn sixty_four_byte_frames_at_ten_gigabit_give_the_canonical_rate() {
        // 14,880,952 pps is the figure on every datasheet; if this drifts, every
        // throughput result Flux produces disagrees with the rest of the industry.
        let pps = line_rate_pps(10_000, 64.0);
        assert!(close(pps, 14_880_952.0, 1e-6), "got {pps}");
    }

    #[test]
    fn the_canonical_rate_holds_at_other_speeds_and_sizes() {
        assert!(close(line_rate_pps(1_000, 64.0), 1_488_095.0, 1e-6));
        assert!(close(line_rate_pps(100_000, 64.0), 148_809_523.0, 1e-6));
        assert!(close(line_rate_pps(10_000, 1518.0), 812_743.0, 1e-5));
        assert!(close(line_rate_pps(10_000, 512.0), 2_349_624.0, 1e-5));
    }

    #[test]
    fn a_hundred_percent_saturates_the_link_exactly() {
        for frame_bytes in [64.0, 128.0, 512.0, 1518.0, 9216.0] {
            let r = resolve(&Rate::Percent { value: 100.0 }, frame_bytes, 10_000);
            assert!(close(r.bps_l1, 10e9, 1e-9), "{frame_bytes}B gave {} bps", r.bps_l1);
            assert!(close(r.line_pct, 100.0, 1e-9));
            assert!(!r.exceeds_line_rate());
        }
    }

    #[test]
    fn layer_two_throughput_is_lower_than_layer_one_by_the_overhead() {
        // At 64 bytes the gap is 20 of every 84 bytes — a quarter of the link.
        let r = resolve(&Rate::Percent { value: 100.0 }, 64.0, 10_000);
        assert!(close(r.bps_l2, 10e9 * (64.0 / 84.0), 1e-9), "got {}", r.bps_l2);

        // At 1518 bytes the same overhead is nearly invisible.
        let big = resolve(&Rate::Percent { value: 100.0 }, 1518.0, 10_000);
        assert!(big.bps_l2 / big.bps_l1 > 0.98);
    }

    #[test]
    fn the_three_rate_forms_agree_when_they_describe_the_same_traffic() {
        let by_percent = resolve(&Rate::Percent { value: 50.0 }, 512.0, 10_000);
        let by_pps = resolve(&Rate::Pps { value: by_percent.pps }, 512.0, 10_000);
        let by_bps = resolve(&Rate::Bps { value: by_percent.bps_l1 }, 512.0, 10_000);

        assert!(close(by_pps.pps, by_percent.pps, 1e-9));
        assert!(close(by_bps.pps, by_percent.pps, 1e-9));
        assert!(close(by_pps.line_pct, 50.0, 1e-9));
        assert!(close(by_bps.line_pct, 50.0, 1e-9));
    }

    #[test]
    fn a_rate_beyond_the_port_is_reported_rather_than_clamped() {
        // The UI needs to say "this does not fit", which it cannot do if the
        // number has already been silently capped at 100%.
        let r = resolve(&Rate::Pps { value: 20_000_000.0 }, 64.0, 10_000);
        assert!(r.line_pct > 100.0, "got {}", r.line_pct);
        assert!(r.exceeds_line_rate());
    }

    #[test]
    fn a_percentage_round_trip_does_not_trip_the_line_rate_check() {
        // Floating point makes 100% come back as 100.00000000000001; treating
        // that as over-subscription would reject a perfectly ordinary flow.
        let r = resolve(&Rate::Percent { value: 100.0 }, 64.0, 10_000);
        let round_tripped = resolve(&Rate::Bps { value: r.bps_l1 }, 64.0, 10_000);
        assert!(!round_tripped.exceeds_line_rate(), "got {}", round_tripped.line_pct);
    }

    #[test]
    fn a_port_with_no_known_speed_yields_zero_not_infinity() {
        // An absent or DPDK-bound port reports no speed. "0 Gb/s" is missing
        // information; "NaN Gb/s" is a rendering bug.
        let r = resolve(&Rate::Percent { value: 50.0 }, 64.0, 0);
        assert_eq!(r.pps, 0.0);
        assert_eq!(r.line_pct, 0.0);
        assert!(r.bps_l1.is_finite());
    }

    #[test]
    fn an_explicit_packet_rate_does_not_depend_on_the_port_speed() {
        // Only the percentage figure needs a line rate to compare against.
        let r = resolve(&Rate::Pps { value: 1_000_000.0 }, 64.0, 0);
        assert_eq!(r.pps, 1_000_000.0);
        assert!(r.bps_l1 > 0.0);
        assert_eq!(r.line_pct, 0.0);
    }

    #[test]
    fn a_mixture_resolves_against_its_weighted_average() {
        let size = FrameSize::Imix { preset: ImixPreset::Simple };
        let r = resolve_for_size(&Rate::Percent { value: 100.0 }, &size, 10_000);
        let expected = line_rate_pps(10_000, ImixPreset::Simple.average_bytes());
        assert!(close(r.pps, expected, 1e-9), "got {} expected {expected}", r.pps);
    }

    #[test]
    fn scaling_a_rate_moves_every_figure_together() {
        let full = resolve(&Rate::Percent { value: 100.0 }, 64.0, 10_000);
        let half = at_percent(&full, 50.0);

        assert!(close(half.pps, full.pps / 2.0, 1e-9));
        assert!(close(half.bps_l1, full.bps_l1 / 2.0, 1e-9));
        assert!(close(half.bps_l2, full.bps_l2 / 2.0, 1e-9));
        assert!(close(half.line_pct, 50.0, 1e-9));
    }

    #[test]
    fn scaling_to_zero_and_to_full_are_both_exact() {
        let full = resolve(&Rate::Percent { value: 100.0 }, 512.0, 10_000);

        let none = at_percent(&full, 0.0);
        assert_eq!(none.pps, 0.0);
        assert_eq!(none.line_pct, 0.0);

        let same = at_percent(&full, 100.0);
        assert!(close(same.pps, full.pps, 1e-12));
    }
}
