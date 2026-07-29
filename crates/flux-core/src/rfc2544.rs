//! RFC 2544 test configuration.
//!
//! The document an operator fills in for a benchmarking test, stored in
//! `tests.config`. The four test types in RFC 2544 that Flux implements share
//! most of it — frame sizes, trial length, loss tolerance — and each uses a
//! subset of the rest.
//!
//! The search that consumes this lives in `fluxd::orch::rfc2544` and is a pure
//! function, so it can be exhaustively tested without an engine.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::{Validate, Validation};
use crate::flow::{MAX_JUMBO_BYTES, MIN_FRAME_BYTES};

/// The frame sizes RFC 2544 section 9 names for Ethernet.
///
/// A result set that omits one of these is not comparable with anybody else's,
/// so this is what every wizard starts from.
pub const STANDARD_FRAME_SIZES: [u32; 7] = [64, 128, 256, 512, 1024, 1280, 1518];

/// Trial duration RFC 2544 section 24 requires for a reportable result.
///
/// Shorter trials are allowed while an operator is iterating, and the report
/// says so, because a 10-second throughput figure is not an RFC 2544 result.
pub const REPORTABLE_TRIAL_SECONDS: f64 = 60.0;

/// Configuration for any of the four RFC 2544 tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Rfc2544Config {
    /// Frame sizes to test, in bytes including FCS.
    #[serde(default = "default_frame_sizes")]
    pub frame_sizes: Vec<u32>,

    /// How long each trial transmits for.
    #[serde(default = "default_trial_seconds")]
    pub trial_seconds: f64,

    /// Loss at or below this percentage counts as a passing trial.
    ///
    /// RFC 2544 throughput is defined at zero loss. Anything above zero is a
    /// deliberate relaxation, and the report states it.
    #[serde(default)]
    pub loss_tolerance_pct: f64,

    /// Trials per frame size before the search gives up.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    /// Where the search starts, and the highest rate it will try.
    #[serde(default = "default_initial_rate_pct")]
    pub initial_rate_pct: f64,

    /// The search stops once its window is this narrow, in percent of line rate.
    #[serde(default = "default_resolution_pct")]
    pub resolution_pct: f64,

    // --- Frame-loss rate (section 26.3) ------------------------------------
    /// How far the frame-loss ladder steps down between trials.
    #[serde(default = "default_ladder_step_pct")]
    pub ladder_step_pct: f64,

    /// The lowest rate the frame-loss ladder descends to.
    #[serde(default = "default_min_rate_pct")]
    pub min_rate_pct: f64,

    // --- Back-to-back frames (section 26.4) --------------------------------
    /// Longest burst the back-to-back search will try, in frames.
    #[serde(default = "default_max_burst_frames")]
    pub max_burst_frames: u64,

    /// The burst search stops once its window is this narrow, in frames.
    #[serde(default = "default_burst_resolution_frames")]
    pub burst_resolution_frames: u64,
}

/// The seven standard sizes.
fn default_frame_sizes() -> Vec<u32> {
    STANDARD_FRAME_SIZES.to_vec()
}

/// Sixty seconds, per section 24.
fn default_trial_seconds() -> f64 {
    REPORTABLE_TRIAL_SECONDS
}

/// Twenty trials is far more than a binary search over 0-100% at 0.1%
/// resolution needs (which is about ten), so hitting this means something is
/// oscillating rather than converging.
fn default_max_iterations() -> u32 {
    20
}

/// Full line rate.
fn default_initial_rate_pct() -> f64 {
    100.0
}

/// A tenth of a percent. At 10G with 64-byte frames that is about 15 kpps,
/// which is finer than run-to-run variation on real hardware.
fn default_resolution_pct() -> f64 {
    0.1
}

/// Ten percent steps give the ten-point ladder the RFC's example uses.
fn default_ladder_step_pct() -> f64 {
    10.0
}

/// Below ten percent a frame-loss ladder stops telling you anything new.
fn default_min_rate_pct() -> f64 {
    10.0
}

/// A million frames is about 84 ms of 64-byte frames at 10G line rate, which is
/// deeper than any switch buffer this class of appliance tests.
fn default_max_burst_frames() -> u64 {
    1_000_000
}

/// A hundred frames is finer than the burst granularity hardware can produce.
fn default_burst_resolution_frames() -> u64 {
    100
}

impl Default for Rfc2544Config {
    fn default() -> Self {
        Self {
            frame_sizes: default_frame_sizes(),
            trial_seconds: default_trial_seconds(),
            loss_tolerance_pct: 0.0,
            max_iterations: default_max_iterations(),
            initial_rate_pct: default_initial_rate_pct(),
            resolution_pct: default_resolution_pct(),
            ladder_step_pct: default_ladder_step_pct(),
            min_rate_pct: default_min_rate_pct(),
            max_burst_frames: default_max_burst_frames(),
            burst_resolution_frames: default_burst_resolution_frames(),
        }
    }
}

impl Rfc2544Config {
    /// True when this configuration would produce a reportable RFC 2544 result.
    ///
    /// Anything else is still useful while iterating, but the report has to say
    /// that it is not a conformant measurement.
    pub fn is_reportable(&self) -> bool {
        self.trial_seconds >= REPORTABLE_TRIAL_SECONDS
            && self.loss_tolerance_pct == 0.0
            && STANDARD_FRAME_SIZES.iter().all(|s| self.frame_sizes.contains(s))
    }

    /// Why this configuration is not reportable, for the report header.
    pub fn reportability_notes(&self) -> Vec<String> {
        let mut notes = Vec::new();

        if self.trial_seconds < REPORTABLE_TRIAL_SECONDS {
            notes.push(format!(
                "trial duration is {}s; RFC 2544 section 24 requires at least {REPORTABLE_TRIAL_SECONDS}s",
                self.trial_seconds
            ));
        }
        if self.loss_tolerance_pct > 0.0 {
            notes.push(format!(
                "loss tolerance is {}%; RFC 2544 throughput is defined at zero loss",
                self.loss_tolerance_pct
            ));
        }

        let missing: Vec<String> = STANDARD_FRAME_SIZES
            .iter()
            .filter(|s| !self.frame_sizes.contains(s))
            .map(|s| s.to_string())
            .collect();
        if !missing.is_empty() {
            notes.push(format!(
                "frame sizes {} from RFC 2544 section 9 were not tested",
                missing.join(", ")
            ));
        }

        notes
    }
}

impl Validate for Rfc2544Config {
    fn validate_into(&self, v: &mut Validation) {
        v.require(
            !self.frame_sizes.is_empty(),
            "frameSizes",
            "at least one frame size is required",
        );
        v.require(
            self.frame_sizes.len() <= 32,
            "frameSizes",
            "at most 32 frame sizes; each one is a full search",
        );

        for (i, size) in self.frame_sizes.iter().enumerate() {
            v.require(
                (MIN_FRAME_BYTES..=MAX_JUMBO_BYTES).contains(size),
                &format!("frameSizes.{i}"),
                format!("must be between {MIN_FRAME_BYTES} and {MAX_JUMBO_BYTES}"),
            );
        }

        let mut seen = std::collections::HashSet::new();
        v.require(
            self.frame_sizes.iter().all(|s| seen.insert(*s)),
            "frameSizes",
            "each frame size may appear only once",
        );

        v.require(self.trial_seconds > 0.0, "trialSeconds", "must be greater than zero");
        v.require(
            self.trial_seconds <= 3600.0,
            "trialSeconds",
            "must be at most one hour per trial",
        );

        v.require(
            (0.0..=100.0).contains(&self.loss_tolerance_pct),
            "lossTolerancePct",
            "must be between 0 and 100",
        );

        v.require(self.max_iterations >= 1, "maxIterations", "must be at least 1");
        v.require(self.max_iterations <= 100, "maxIterations", "must be at most 100");

        v.require(
            self.initial_rate_pct > 0.0 && self.initial_rate_pct <= 100.0,
            "initialRatePct",
            "must be greater than 0 and at most 100",
        );

        v.require(self.resolution_pct > 0.0, "resolutionPct", "must be greater than zero");
        // A resolution as wide as the search window means the first trial
        // "converges" immediately and the result is meaningless.
        v.require(
            self.resolution_pct < self.initial_rate_pct,
            "resolutionPct",
            "must be narrower than the starting rate, or the search cannot run",
        );

        v.require(
            self.ladder_step_pct > 0.0 && self.ladder_step_pct <= 100.0,
            "ladderStepPct",
            "must be greater than 0 and at most 100",
        );
        v.require(
            self.min_rate_pct > 0.0 && self.min_rate_pct <= self.initial_rate_pct,
            "minRatePct",
            "must be greater than 0 and no higher than the starting rate",
        );

        v.require(self.max_burst_frames >= 1, "maxBurstFrames", "must be at least 1");
        v.require(self.burst_resolution_frames >= 1, "burstResolutionFrames", "must be at least 1");
        v.require(
            self.burst_resolution_frames < self.max_burst_frames,
            "burstResolutionFrames",
            "must be narrower than the longest burst, or the search cannot run",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_configuration_is_valid_and_reportable() {
        // The defaults are what a wizard opens with; if they were not
        // conformant, every result would carry a caveat nobody asked for.
        let config = Rfc2544Config::default();
        assert!(config.validate().is_ok(), "{:?}", config.validate());
        assert!(config.is_reportable());
        assert!(config.reportability_notes().is_empty());
    }

    #[test]
    fn the_defaults_are_the_standard_frame_sizes() {
        assert_eq!(Rfc2544Config::default().frame_sizes, vec![64, 128, 256, 512, 1024, 1280, 1518]);
    }

    #[test]
    fn a_short_trial_is_allowed_but_not_reportable() {
        let config = Rfc2544Config { trial_seconds: 10.0, ..Default::default() };

        assert!(config.validate().is_ok(), "short trials are useful while iterating");
        assert!(!config.is_reportable());
        assert!(config.reportability_notes()[0].contains("section 24"));
    }

    #[test]
    fn a_relaxed_loss_tolerance_is_allowed_but_not_reportable() {
        let config = Rfc2544Config { loss_tolerance_pct: 0.01, ..Default::default() };

        assert!(config.validate().is_ok());
        assert!(!config.is_reportable());
        assert!(config.reportability_notes()[0].contains("zero loss"));
    }

    #[test]
    fn omitting_a_standard_frame_size_is_noted_in_the_report() {
        let config = Rfc2544Config { frame_sizes: vec![64, 1518], ..Default::default() };

        assert!(config.validate().is_ok());
        assert!(!config.is_reportable());

        let note = &config.reportability_notes()[0];
        assert!(note.contains("128"), "{note}");
        assert!(note.contains("1280"), "{note}");
    }

    #[test]
    fn every_reason_a_configuration_is_unreportable_is_listed_at_once() {
        let config = Rfc2544Config {
            trial_seconds: 5.0,
            loss_tolerance_pct: 0.1,
            frame_sizes: vec![64],
            ..Default::default()
        };
        assert_eq!(config.reportability_notes().len(), 3);
    }

    #[test]
    fn an_empty_frame_size_list_is_rejected() {
        let config = Rfc2544Config { frame_sizes: Vec::new(), ..Default::default() };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "frameSizes"));
    }

    #[test]
    fn a_duplicated_frame_size_is_rejected() {
        // Running 64 twice doubles the test duration and produces two rows that
        // disagree, with nothing to say which is the result.
        let config = Rfc2544Config { frame_sizes: vec![64, 128, 64], ..Default::default() };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "frameSizes"));
    }

    #[test]
    fn frame_size_errors_carry_their_index() {
        let config = Rfc2544Config { frame_sizes: vec![64, 12], ..Default::default() };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "frameSizes.1"), "{errors:?}");
    }

    #[test]
    fn a_resolution_wider_than_the_search_window_is_rejected() {
        // Otherwise the first trial "converges" immediately at whatever it
        // happened to measure.
        let config =
            Rfc2544Config { resolution_pct: 100.0, initial_rate_pct: 100.0, ..Default::default() };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "resolutionPct"));
    }

    #[test]
    fn a_burst_resolution_wider_than_the_longest_burst_is_rejected() {
        let config = Rfc2544Config {
            max_burst_frames: 100,
            burst_resolution_frames: 100,
            ..Default::default()
        };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "burstResolutionFrames"));
    }

    #[test]
    fn the_ladder_floor_may_not_exceed_its_ceiling() {
        let config =
            Rfc2544Config { initial_rate_pct: 50.0, min_rate_pct: 80.0, ..Default::default() };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.path == "minRatePct"));
    }

    #[test]
    fn a_config_round_trips_through_json_unchanged() {
        // Stored as JSONB and restored from a run snapshot; a field that does
        // not survive is a silently altered test.
        let config = Rfc2544Config::default();
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Rfc2544Config>(&json).unwrap(), config);
    }

    #[test]
    fn an_empty_document_deserialises_to_the_defaults() {
        // A wizard that omits an advanced field must not produce a config with
        // zeroes in it.
        let config: Rfc2544Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config, Rfc2544Config::default());
    }
}
