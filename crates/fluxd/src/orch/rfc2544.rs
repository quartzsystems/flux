//! The RFC 2544 search logic.
//!
//! Every function here is pure: given the configuration and the trials run so
//! far, decide what to do next. Nothing in this module touches an engine, a
//! clock, or a database, which is what lets the search be tested exhaustively —
//! convergence, all-pass, all-fail, boundary tolerance, iteration cutoff — in
//! milliseconds rather than by running hour-long tests against hardware.
//!
//! The async execution loop lives in `orch::run`. It calls these functions,
//! performs whatever they ask for, appends the result, and calls again.
//!
//! ## Why a fold over the whole history
//!
//! The search takes the complete trial list rather than carrying mutable state.
//! That makes it replayable: the orchestrator can reconstruct exactly where a
//! search was from the `run_results` rows alone, which is what a resumable run
//! needs and what makes a stored result auditable after the fact.

use flux_core::rfc2544::Rfc2544Config;

/// Tolerance applied when comparing measured loss against the configured limit.
///
/// Loss is a division, so a trial that lost exactly the tolerated fraction can
/// land a few ulps above it. RFC 2544 defines the comparison as "at or below",
/// and a throughput result that flipped on floating-point noise would be
/// irreproducible.
const LOSS_EPSILON: f64 = 1e-9;

/// One completed rate trial.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateTrial {
    /// The rate this trial ran at, as a percentage of line rate.
    pub rate_pct: f64,
    /// Frames transmitted during the trial.
    pub tx_packets: u64,
    /// Frames received during the trial.
    pub rx_packets: u64,
}

impl RateTrial {
    /// Loss as a percentage of frames transmitted.
    ///
    /// A trial that transmitted nothing reports total loss, not zero: a trial
    /// where the engine never started is a failure, and calling it a pass would
    /// let a broken run report line-rate throughput.
    pub fn loss_pct(&self) -> f64 {
        if self.tx_packets == 0 {
            return 100.0;
        }
        (self.tx_packets.saturating_sub(self.rx_packets) as f64 / self.tx_packets as f64) * 100.0
    }

    /// Whether this trial meets the loss tolerance.
    pub fn passed(&self, tolerance_pct: f64) -> bool {
        self.loss_pct() <= tolerance_pct + LOSS_EPSILON
    }
}

/// One completed burst trial, for the back-to-back test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstTrial {
    /// Burst length this trial used, in frames.
    pub burst_frames: u64,
    /// Frames transmitted.
    pub tx_packets: u64,
    /// Frames received.
    pub rx_packets: u64,
}

impl BurstTrial {
    /// Whether the whole burst arrived.
    ///
    /// Back-to-back is defined at zero loss — the question is how long a burst
    /// the device can absorb without dropping any of it — so this takes no
    /// tolerance.
    pub fn passed(&self) -> bool {
        self.tx_packets > 0 && self.rx_packets >= self.tx_packets
    }
}

/// What the rate search wants next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchAction {
    /// Run a trial at this rate.
    Trial {
        /// Percentage of line rate.
        rate_pct: f64,
    },
    /// The search is finished.
    Converged {
        /// The highest rate that passed, or `None` if nothing did.
        rate_pct: Option<f64>,
        /// Why the search stopped.
        reason: StopReason,
    },
}

/// What the burst search wants next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BurstAction {
    /// Run a trial with this burst length.
    Trial {
        /// Burst length in frames.
        burst_frames: u64,
    },
    /// The search is finished.
    Converged {
        /// The longest burst that arrived intact, or `None` if none did.
        burst_frames: Option<u64>,
        /// Why the search stopped.
        reason: StopReason,
    },
}

/// Why a search stopped.
///
/// Recorded with the result, because "converged at 87.5%" and "gave up at
/// 87.5% after twenty trials" are different claims and only one of them is a
/// measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The window closed to within the configured resolution. The normal outcome.
    Resolution,
    /// The highest rate passed, so there was nothing higher to try.
    CeilingPassed,
    /// Nothing passed, down to the finest rate the search would try.
    NoPassingRate,
    /// The iteration budget ran out before the window closed.
    IterationLimit,
    /// Every rung of the ladder was walked.
    LadderComplete,
    /// The ladder stopped early after consecutive lossless trials.
    LadderSettled,
}

impl StopReason {
    /// Whether the result is a converged measurement rather than a give-up.
    pub fn is_conclusive(self) -> bool {
        matches!(
            self,
            StopReason::Resolution
                | StopReason::CeilingPassed
                | StopReason::NoPassingRate
                | StopReason::LadderComplete
                | StopReason::LadderSettled
        )
    }

    /// A short description for the report and the run view.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Resolution => "converged to the configured resolution",
            StopReason::CeilingPassed => "passed at the maximum rate",
            StopReason::NoPassingRate => "no rate passed",
            StopReason::IterationLimit => "stopped at the iteration limit",
            StopReason::LadderComplete => "completed the rate ladder",
            StopReason::LadderSettled => "stopped after consecutive lossless trials",
        }
    }
}

// ---------------------------------------------------------------------------
// Throughput (section 26.1) and latency (26.2)
// ---------------------------------------------------------------------------

/// Decides the next step of a throughput binary search.
///
/// The search brackets the throughput between the highest passing rate and the
/// lowest failing one, halving the gap each trial. It starts at
/// `initial_rate_pct`, which is also the ceiling: there is no point testing
/// above the rate the operator asked about.
pub fn throughput_next(config: &Rfc2544Config, trials: &[RateTrial]) -> SearchAction {
    if trials.is_empty() {
        return SearchAction::Trial { rate_pct: config.initial_rate_pct };
    }

    let tolerance = config.loss_tolerance_pct;
    let best_pass = trials
        .iter()
        .filter(|t| t.passed(tolerance))
        .map(|t| t.rate_pct)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lowest_fail = trials
        .iter()
        .filter(|t| !t.passed(tolerance))
        .map(|t| t.rate_pct)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Passing at the ceiling ends the search: nothing higher will be tried, so
    // the answer is the ceiling itself.
    if best_pass.is_some_and(|rate| rate >= config.initial_rate_pct) {
        return SearchAction::Converged {
            rate_pct: best_pass,
            reason: StopReason::CeilingPassed,
        };
    }

    let lower = best_pass.unwrap_or(0.0);
    let upper = lowest_fail.unwrap_or(config.initial_rate_pct);

    if upper - lower <= config.resolution_pct {
        return SearchAction::Converged {
            rate_pct: best_pass,
            reason: if best_pass.is_some() {
                StopReason::Resolution
            } else {
                StopReason::NoPassingRate
            },
        };
    }

    // The budget is checked after the window, so a search that converged on its
    // last permitted trial reports convergence rather than exhaustion.
    if trials.len() as u32 >= config.max_iterations {
        return SearchAction::Converged {
            rate_pct: best_pass,
            reason: StopReason::IterationLimit,
        };
    }

    let next = (lower + upper) / 2.0;

    // Repeating a rate would loop forever without narrowing anything. It can
    // happen when the resolution is finer than the rates are distinguishable in
    // floating point.
    if trials.iter().any(|t| (t.rate_pct - next).abs() < f64::EPSILON * 16.0) {
        return SearchAction::Converged {
            rate_pct: best_pass,
            reason: if best_pass.is_some() {
                StopReason::Resolution
            } else {
                StopReason::NoPassingRate
            },
        };
    }

    SearchAction::Trial { rate_pct: next }
}

/// The bracket a search currently holds, for the progress display.
///
/// An operator watching a run wants to see the window closing; this is what the
/// live view renders as "between 87.5% and 93.75%".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchWindow {
    /// Highest rate known to pass, or zero.
    pub lower_pct: f64,
    /// Lowest rate known to fail, or the ceiling.
    pub upper_pct: f64,
}

/// The current bracket, given the trials so far.
pub fn search_window(config: &Rfc2544Config, trials: &[RateTrial]) -> SearchWindow {
    let tolerance = config.loss_tolerance_pct;

    let lower = trials
        .iter()
        .filter(|t| t.passed(tolerance))
        .map(|t| t.rate_pct)
        .fold(0.0f64, f64::max);
    let upper = trials
        .iter()
        .filter(|t| !t.passed(tolerance))
        .map(|t| t.rate_pct)
        .fold(config.initial_rate_pct, f64::min);

    SearchWindow { lower_pct: lower, upper_pct: upper }
}

// ---------------------------------------------------------------------------
// Frame-loss rate (section 26.3)
// ---------------------------------------------------------------------------

/// The rungs of the frame-loss ladder, highest first.
///
/// Section 26.3 describes descending from the maximum rate in steps until the
/// device stops losing frames. The floor is inclusive when it lands exactly on a
/// step, so a 100/10/10 configuration ends at 10% rather than 20%.
pub fn ladder_rates(config: &Rfc2544Config) -> Vec<f64> {
    let mut rates = Vec::new();
    let mut rate = config.initial_rate_pct;

    // A tiny epsilon keeps the floor inclusive when accumulated subtraction
    // leaves it a fraction below the configured minimum.
    while rate >= config.min_rate_pct - 1e-9 {
        rates.push(rate);
        rate -= config.ladder_step_pct;
    }

    rates
}

/// Decides the next step of a frame-loss ladder.
///
/// The ladder is a fixed sequence rather than a search, with one shortcut from
/// the RFC: once two successive trials show no loss, lower rates cannot
/// reveal anything and the test may stop.
pub fn frameloss_next(config: &Rfc2544Config, trials: &[RateTrial]) -> SearchAction {
    let rates = ladder_rates(config);

    // Two consecutive lossless trials mean the device has stopped losing
    // frames; descending further only spends time.
    if trials.len() >= 2 {
        let tail = &trials[trials.len() - 2..];
        if tail.iter().all(|t| t.loss_pct() <= LOSS_EPSILON) {
            return SearchAction::Converged {
                rate_pct: Some(tail[0].rate_pct),
                reason: StopReason::LadderSettled,
            };
        }
    }

    match rates.get(trials.len()) {
        Some(rate) => SearchAction::Trial { rate_pct: *rate },
        None => SearchAction::Converged {
            // The reported figure is the highest rate that lost nothing.
            rate_pct: trials
                .iter()
                .filter(|t| t.loss_pct() <= LOSS_EPSILON)
                .map(|t| t.rate_pct)
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)),
            reason: StopReason::LadderComplete,
        },
    }
}

// ---------------------------------------------------------------------------
// Back-to-back frames (section 26.4)
// ---------------------------------------------------------------------------

/// Decides the next step of a back-to-back burst search.
///
/// Structurally the same binary search as throughput, over burst length in
/// frames rather than rate, and always at zero loss: the question is how long a
/// burst the device can absorb without dropping any of it.
pub fn b2b_next(config: &Rfc2544Config, trials: &[BurstTrial]) -> BurstAction {
    if trials.is_empty() {
        return BurstAction::Trial { burst_frames: config.max_burst_frames };
    }

    let best_pass = trials.iter().filter(|t| t.passed()).map(|t| t.burst_frames).max();
    let lowest_fail = trials.iter().filter(|t| !t.passed()).map(|t| t.burst_frames).min();

    if best_pass.is_some_and(|frames| frames >= config.max_burst_frames) {
        return BurstAction::Converged {
            burst_frames: best_pass,
            reason: StopReason::CeilingPassed,
        };
    }

    let lower = best_pass.unwrap_or(0);
    let upper = lowest_fail.unwrap_or(config.max_burst_frames);

    if upper.saturating_sub(lower) <= config.burst_resolution_frames {
        return BurstAction::Converged {
            burst_frames: best_pass,
            reason: if best_pass.is_some() {
                StopReason::Resolution
            } else {
                StopReason::NoPassingRate
            },
        };
    }

    if trials.len() as u32 >= config.max_iterations {
        return BurstAction::Converged {
            burst_frames: best_pass,
            reason: StopReason::IterationLimit,
        };
    }

    // Integer midpoint written to avoid overflow on large bounds.
    let next = lower + (upper - lower) / 2;

    if trials.iter().any(|t| t.burst_frames == next) {
        return BurstAction::Converged {
            burst_frames: best_pass,
            reason: if best_pass.is_some() {
                StopReason::Resolution
            } else {
                StopReason::NoPassingRate
            },
        };
    }

    BurstAction::Trial { burst_frames: next }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration with a coarse resolution, so searches finish in few
    /// enough trials to reason about by hand.
    fn config() -> Rfc2544Config {
        Rfc2544Config {
            initial_rate_pct: 100.0,
            resolution_pct: 1.0,
            loss_tolerance_pct: 0.0,
            max_iterations: 20,
            ..Default::default()
        }
    }

    /// A trial that lost exactly `loss_pct` of a million frames.
    fn trial(rate_pct: f64, loss_pct: f64) -> RateTrial {
        let tx = 1_000_000u64;
        let lost = (tx as f64 * loss_pct / 100.0).round() as u64;
        RateTrial { rate_pct, tx_packets: tx, rx_packets: tx - lost }
    }

    /// A lossless trial.
    fn pass(rate_pct: f64) -> RateTrial {
        trial(rate_pct, 0.0)
    }

    /// A trial that lost five percent.
    fn fail(rate_pct: f64) -> RateTrial {
        trial(rate_pct, 5.0)
    }

    /// Runs a search to completion against a device whose true throughput is
    /// `true_throughput_pct`, returning every rate tried and the outcome.
    fn simulate(config: &Rfc2544Config, true_throughput_pct: f64) -> (Vec<f64>, SearchAction) {
        let mut trials: Vec<RateTrial> = Vec::new();
        let mut attempted = Vec::new();

        loop {
            match throughput_next(config, &trials) {
                SearchAction::Trial { rate_pct } => {
                    attempted.push(rate_pct);
                    trials.push(if rate_pct <= true_throughput_pct {
                        pass(rate_pct)
                    } else {
                        fail(rate_pct)
                    });

                    assert!(
                        attempted.len() < 200,
                        "the search did not terminate: {attempted:?}"
                    );
                }
                done => return (attempted, done),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Loss accounting
    // -----------------------------------------------------------------------

    #[test]
    fn loss_is_measured_against_frames_transmitted() {
        assert_eq!(RateTrial { rate_pct: 50.0, tx_packets: 1000, rx_packets: 1000 }.loss_pct(), 0.0);
        assert_eq!(RateTrial { rate_pct: 50.0, tx_packets: 1000, rx_packets: 990 }.loss_pct(), 1.0);
        assert_eq!(RateTrial { rate_pct: 50.0, tx_packets: 1000, rx_packets: 0 }.loss_pct(), 100.0);
    }

    #[test]
    fn a_trial_that_transmitted_nothing_is_total_loss_not_zero() {
        // Otherwise a run where the engine never started reports line rate.
        let broken = RateTrial { rate_pct: 100.0, tx_packets: 0, rx_packets: 0 };
        assert_eq!(broken.loss_pct(), 100.0);
        assert!(!broken.passed(0.0));
    }

    #[test]
    fn receiving_more_than_transmitted_is_not_negative_loss() {
        let odd = RateTrial { rate_pct: 50.0, tx_packets: 1000, rx_packets: 1005 };
        assert_eq!(odd.loss_pct(), 0.0);
        assert!(odd.passed(0.0));
    }

    #[test]
    fn loss_exactly_at_the_tolerance_passes() {
        // RFC 2544 defines the comparison as "at or below". A result that
        // flipped on the boundary would not be reproducible.
        let exact = trial(50.0, 0.1);
        assert!((exact.loss_pct() - 0.1).abs() < 1e-12);
        assert!(exact.passed(0.1), "loss equal to the tolerance must pass");
        assert!(!exact.passed(0.09), "loss above the tolerance must fail");
    }

    #[test]
    fn a_tolerance_of_zero_admits_only_a_lossless_trial() {
        assert!(pass(50.0).passed(0.0));
        assert!(!RateTrial { rate_pct: 50.0, tx_packets: 1_000_000, rx_packets: 999_999 }
            .passed(0.0));
    }

    // -----------------------------------------------------------------------
    // Throughput search
    // -----------------------------------------------------------------------

    #[test]
    fn the_search_opens_at_the_configured_starting_rate() {
        assert_eq!(
            throughput_next(&config(), &[]),
            SearchAction::Trial { rate_pct: 100.0 }
        );

        let low_start = Rfc2544Config { initial_rate_pct: 50.0, ..config() };
        assert_eq!(
            throughput_next(&low_start, &[]),
            SearchAction::Trial { rate_pct: 50.0 }
        );
    }

    #[test]
    fn passing_at_the_ceiling_ends_the_search_immediately() {
        // There is nothing above the ceiling to try, so one trial is the whole
        // test — which is the right answer for a device that forwards line rate.
        assert_eq!(
            throughput_next(&config(), &[pass(100.0)]),
            SearchAction::Converged {
                rate_pct: Some(100.0),
                reason: StopReason::CeilingPassed
            }
        );
    }

    #[test]
    fn a_failure_halves_the_window() {
        assert_eq!(
            throughput_next(&config(), &[fail(100.0)]),
            SearchAction::Trial { rate_pct: 50.0 }
        );
    }

    #[test]
    fn a_pass_searches_the_upper_half() {
        assert_eq!(
            throughput_next(&config(), &[fail(100.0), pass(50.0)]),
            SearchAction::Trial { rate_pct: 75.0 }
        );
    }

    #[test]
    fn the_search_brackets_the_true_throughput_within_the_resolution() {
        // The property that matters: whatever the device does, the reported
        // figure is at or below the truth and within one resolution of it.
        for truth in [0.5, 1.0, 12.3, 33.3, 50.0, 66.7, 87.5, 95.0, 99.9] {
            let (_, outcome) = simulate(&config(), truth);

            match outcome {
                SearchAction::Converged { rate_pct: Some(found), .. } => {
                    assert!(
                        found <= truth + 1e-9,
                        "reported {found} above the true throughput {truth}"
                    );
                    assert!(
                        truth - found <= config().resolution_pct,
                        "reported {found} is more than one resolution below {truth}"
                    );
                }
                SearchAction::Converged { rate_pct: None, .. } => {
                    assert!(truth < config().resolution_pct, "gave up although {truth} passes");
                }
                other => panic!("the search did not terminate: {other:?}"),
            }
        }
    }

    #[test]
    fn a_finer_resolution_costs_more_trials_and_gives_a_closer_answer() {
        let coarse = Rfc2544Config { resolution_pct: 5.0, ..config() };
        let fine = Rfc2544Config { resolution_pct: 0.1, ..config() };

        let (coarse_trials, coarse_result) = simulate(&coarse, 63.7);
        let (fine_trials, fine_result) = simulate(&fine, 63.7);

        assert!(
            fine_trials.len() > coarse_trials.len(),
            "finer resolution should take more trials"
        );

        let error = |action: SearchAction| match action {
            SearchAction::Converged { rate_pct: Some(r), .. } => 63.7 - r,
            other => panic!("expected convergence, got {other:?}"),
        };
        assert!(error(fine_result) < error(coarse_result));
    }

    #[test]
    fn a_device_that_forwards_nothing_converges_on_no_passing_rate() {
        let (_, outcome) = simulate(&config(), 0.0);
        assert_eq!(
            outcome,
            SearchAction::Converged { rate_pct: None, reason: StopReason::NoPassingRate }
        );
    }

    #[test]
    fn a_device_that_forwards_everything_converges_on_the_ceiling_in_one_trial() {
        let (attempted, outcome) = simulate(&config(), 100.0);

        assert_eq!(attempted, vec![100.0]);
        assert_eq!(
            outcome,
            SearchAction::Converged {
                rate_pct: Some(100.0),
                reason: StopReason::CeilingPassed
            }
        );
    }

    #[test]
    fn the_iteration_budget_stops_a_search_that_has_not_converged() {
        // A resolution far finer than the budget allows is the realistic way to
        // hit this: the operator asked for more precision than they paid for.
        let stingy = Rfc2544Config { max_iterations: 3, resolution_pct: 0.001, ..config() };
        let (attempted, outcome) = simulate(&stingy, 63.7);

        assert_eq!(attempted.len(), 3, "must not run more trials than permitted");
        match outcome {
            SearchAction::Converged { rate_pct, reason } => {
                assert_eq!(reason, StopReason::IterationLimit);
                assert!(rate_pct.is_some(), "the best passing rate so far is still reported");
                assert!(!reason.is_conclusive(), "a give-up is not a measurement");
            }
            other => panic!("expected convergence, got {other:?}"),
        }
    }

    #[test]
    fn a_search_that_converges_on_its_last_permitted_trial_reports_convergence() {
        // The window is checked before the budget, so a search that just made it
        // is a measurement rather than an exhaustion.
        let trials = vec![fail(100.0), pass(50.0), pass(75.0)];
        let tight = Rfc2544Config { max_iterations: 3, resolution_pct: 25.0, ..config() };

        assert_eq!(
            throughput_next(&tight, &trials),
            SearchAction::Converged {
                rate_pct: Some(75.0),
                reason: StopReason::Resolution
            }
        );
    }

    #[test]
    fn a_relaxed_tolerance_finds_a_higher_throughput() {
        // The same device, measured at zero loss and at one percent, should not
        // give the same answer — otherwise the tolerance is not being applied.
        let strict = config();
        let relaxed = Rfc2544Config { loss_tolerance_pct: 6.0, ..config() };

        // Under the relaxed tolerance the 5%-loss trials count as passes.
        let trials = vec![fail(100.0)];
        assert_eq!(
            throughput_next(&relaxed, &trials),
            SearchAction::Converged {
                rate_pct: Some(100.0),
                reason: StopReason::CeilingPassed
            }
        );
        assert_eq!(
            throughput_next(&strict, &trials),
            SearchAction::Trial { rate_pct: 50.0 }
        );
    }

    #[test]
    fn the_search_never_repeats_a_rate() {
        for truth in [0.0, 7.7, 42.0, 63.7, 99.99, 100.0] {
            let (attempted, _) = simulate(&config(), truth);

            let mut seen = std::collections::HashSet::new();
            for rate in &attempted {
                assert!(
                    seen.insert(rate.to_bits()),
                    "rate {rate} was tried twice for truth {truth}: {attempted:?}"
                );
            }
        }
    }

    #[test]
    fn the_search_terminates_at_an_absurdly_fine_resolution() {
        // Floating point eventually stops producing distinct midpoints; the
        // search has to notice rather than loop.
        let absurd = Rfc2544Config {
            resolution_pct: 1e-15,
            max_iterations: 100,
            ..config()
        };
        let (_, outcome) = simulate(&absurd, 63.7);
        assert!(matches!(outcome, SearchAction::Converged { .. }));
    }

    #[test]
    fn a_non_standard_ceiling_is_respected_throughout() {
        let capped = Rfc2544Config { initial_rate_pct: 40.0, ..config() };
        let (attempted, outcome) = simulate(&capped, 90.0);

        assert!(
            attempted.iter().all(|r| *r <= 40.0),
            "the search must never exceed its ceiling: {attempted:?}"
        );
        assert_eq!(
            outcome,
            SearchAction::Converged {
                rate_pct: Some(40.0),
                reason: StopReason::CeilingPassed
            }
        );
    }

    // -----------------------------------------------------------------------
    // Search window
    // -----------------------------------------------------------------------

    #[test]
    fn the_window_opens_at_zero_and_the_ceiling() {
        let window = search_window(&config(), &[]);
        assert_eq!(window.lower_pct, 0.0);
        assert_eq!(window.upper_pct, 100.0);
    }

    #[test]
    fn the_window_closes_around_the_trials_run_so_far() {
        let window = search_window(&config(), &[fail(100.0), pass(50.0), fail(75.0)]);
        assert_eq!(window.lower_pct, 50.0);
        assert_eq!(window.upper_pct, 75.0);
    }

    // -----------------------------------------------------------------------
    // Frame-loss ladder
    // -----------------------------------------------------------------------

    #[test]
    fn the_ladder_descends_from_the_ceiling_to_the_floor_inclusive() {
        let config = Rfc2544Config {
            initial_rate_pct: 100.0,
            ladder_step_pct: 10.0,
            min_rate_pct: 10.0,
            ..config()
        };
        assert_eq!(
            ladder_rates(&config),
            vec![100.0, 90.0, 80.0, 70.0, 60.0, 50.0, 40.0, 30.0, 20.0, 10.0]
        );
    }

    #[test]
    fn the_ladder_floor_survives_accumulated_subtraction() {
        // Repeated subtraction of 12.5 lands a hair off 25.0; the floor must
        // still be included.
        let config = Rfc2544Config {
            initial_rate_pct: 100.0,
            ladder_step_pct: 12.5,
            min_rate_pct: 25.0,
            ..config()
        };
        let rates = ladder_rates(&config);
        assert_eq!(rates.last().copied(), Some(25.0), "got {rates:?}");
    }

    #[test]
    fn the_ladder_walks_every_rung_when_loss_never_stops() {
        let config = Rfc2544Config { min_rate_pct: 80.0, ..config() };
        let rungs = ladder_rates(&config);

        let mut trials = Vec::new();
        for expected in &rungs {
            match frameloss_next(&config, &trials) {
                SearchAction::Trial { rate_pct } => {
                    assert_eq!(rate_pct, *expected);
                    trials.push(fail(rate_pct));
                }
                other => panic!("expected a trial at {expected}, got {other:?}"),
            }
        }

        assert_eq!(
            frameloss_next(&config, &trials),
            SearchAction::Converged { rate_pct: None, reason: StopReason::LadderComplete }
        );
    }

    #[test]
    fn the_ladder_stops_early_after_two_lossless_trials() {
        // Section 26.3's shortcut: once the device stops losing frames, lower
        // rates cannot reveal anything.
        let trials = vec![fail(100.0), fail(90.0), pass(80.0), pass(70.0)];

        assert_eq!(
            frameloss_next(&config(), &trials),
            SearchAction::Converged {
                rate_pct: Some(80.0),
                reason: StopReason::LadderSettled
            }
        );
    }

    #[test]
    fn one_lossless_trial_is_not_enough_to_stop_the_ladder() {
        let trials = vec![fail(100.0), pass(90.0)];
        assert!(matches!(
            frameloss_next(&config(), &trials),
            SearchAction::Trial { .. }
        ));
    }

    #[test]
    fn the_ladder_reports_the_highest_lossless_rate() {
        let config = Rfc2544Config { min_rate_pct: 90.0, ..config() };
        let trials = vec![fail(100.0), pass(90.0)];

        assert_eq!(
            frameloss_next(&config, &trials),
            SearchAction::Converged {
                rate_pct: Some(90.0),
                reason: StopReason::LadderComplete
            }
        );
    }

    // -----------------------------------------------------------------------
    // Back-to-back burst search
    // -----------------------------------------------------------------------

    /// A burst that arrived intact.
    fn burst_pass(frames: u64) -> BurstTrial {
        BurstTrial { burst_frames: frames, tx_packets: frames, rx_packets: frames }
    }

    /// A burst that lost frames.
    fn burst_fail(frames: u64) -> BurstTrial {
        BurstTrial { burst_frames: frames, tx_packets: frames, rx_packets: frames / 2 }
    }

    /// Runs a burst search against a device that can absorb `capacity` frames.
    fn simulate_burst(config: &Rfc2544Config, capacity: u64) -> (Vec<u64>, BurstAction) {
        let mut trials = Vec::new();
        let mut attempted = Vec::new();

        loop {
            match b2b_next(config, &trials) {
                BurstAction::Trial { burst_frames } => {
                    attempted.push(burst_frames);
                    trials.push(if burst_frames <= capacity {
                        burst_pass(burst_frames)
                    } else {
                        burst_fail(burst_frames)
                    });
                    assert!(attempted.len() < 200, "the burst search did not terminate");
                }
                done => return (attempted, done),
            }
        }
    }

    #[test]
    fn the_burst_search_opens_at_the_longest_burst() {
        let config = Rfc2544Config { max_burst_frames: 100_000, ..config() };
        assert_eq!(
            b2b_next(&config, &[]),
            BurstAction::Trial { burst_frames: 100_000 }
        );
    }

    #[test]
    fn a_device_that_absorbs_the_longest_burst_ends_the_search_at_once() {
        let config = Rfc2544Config { max_burst_frames: 100_000, ..config() };
        let (attempted, outcome) = simulate_burst(&config, 200_000);

        assert_eq!(attempted, vec![100_000]);
        assert_eq!(
            outcome,
            BurstAction::Converged {
                burst_frames: Some(100_000),
                reason: StopReason::CeilingPassed
            }
        );
    }

    #[test]
    fn the_burst_search_brackets_the_true_capacity() {
        let config = Rfc2544Config {
            max_burst_frames: 100_000,
            burst_resolution_frames: 100,
            max_iterations: 40,
            ..config()
        };

        for capacity in [0, 1, 999, 5_000, 37_412, 99_999] {
            let (_, outcome) = simulate_burst(&config, capacity);

            match outcome {
                BurstAction::Converged { burst_frames: Some(found), .. } => {
                    assert!(found <= capacity, "reported {found} above capacity {capacity}");
                    assert!(
                        capacity - found <= config.burst_resolution_frames,
                        "reported {found} is more than one resolution below {capacity}"
                    );
                }
                BurstAction::Converged { burst_frames: None, .. } => {
                    assert!(
                        capacity < config.burst_resolution_frames,
                        "gave up although {capacity} frames fit"
                    );
                }
                other => panic!("the burst search did not terminate: {other:?}"),
            }
        }
    }

    #[test]
    fn a_device_that_absorbs_nothing_reports_no_passing_burst() {
        let config = Rfc2544Config { max_burst_frames: 100_000, ..config() };
        let (_, outcome) = simulate_burst(&config, 0);

        assert_eq!(
            outcome,
            BurstAction::Converged { burst_frames: None, reason: StopReason::NoPassingRate }
        );
    }

    #[test]
    fn a_partially_received_burst_is_a_failure() {
        // Back-to-back is defined at zero loss; anything less than the whole
        // burst arriving means the device could not absorb it.
        assert!(!BurstTrial { burst_frames: 1000, tx_packets: 1000, rx_packets: 999 }.passed());
        assert!(BurstTrial { burst_frames: 1000, tx_packets: 1000, rx_packets: 1000 }.passed());
        assert!(!BurstTrial { burst_frames: 1000, tx_packets: 0, rx_packets: 0 }.passed());
    }

    #[test]
    fn the_burst_search_never_repeats_a_length() {
        let config = Rfc2544Config {
            max_burst_frames: 65_536,
            burst_resolution_frames: 1,
            max_iterations: 40,
            ..config()
        };

        for capacity in [0, 1, 4_096, 40_000, 65_535] {
            let (attempted, _) = simulate_burst(&config, capacity);
            let mut seen = std::collections::HashSet::new();
            for frames in &attempted {
                assert!(seen.insert(*frames), "burst {frames} tried twice: {attempted:?}");
            }
        }
    }

    #[test]
    fn the_burst_midpoint_does_not_overflow_on_large_bounds() {
        // Written as lower + (upper - lower) / 2 rather than (lower + upper) / 2
        // so that bounds near u64::MAX cannot wrap.
        let config = Rfc2544Config {
            max_burst_frames: u64::MAX,
            burst_resolution_frames: 1,
            ..config()
        };
        match b2b_next(&config, &[burst_fail(u64::MAX)]) {
            BurstAction::Trial { burst_frames } => {
                assert!(burst_frames > 0 && burst_frames < u64::MAX);
            }
            other => panic!("expected a trial, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Stop reasons
    // -----------------------------------------------------------------------

    #[test]
    fn only_the_iteration_limit_is_an_inconclusive_outcome() {
        assert!(StopReason::Resolution.is_conclusive());
        assert!(StopReason::CeilingPassed.is_conclusive());
        assert!(StopReason::NoPassingRate.is_conclusive());
        assert!(StopReason::LadderComplete.is_conclusive());
        assert!(StopReason::LadderSettled.is_conclusive());
        assert!(!StopReason::IterationLimit.is_conclusive());
    }
}
