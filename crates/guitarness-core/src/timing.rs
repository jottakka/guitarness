//! Timing reference, calibration basis and judging level (owned by C2).
//!
//! The judge measures the timing error of an attack against the click, in ms, negative =
//! early:
//!
//! ```text
//! e = (t − g) · 1000 / fs_in − off
//! ```
//!
//! `t` is the input frame of the attack, `g` the input frame captured when the click
//! sounds, and `off` the only thing a calibration may take away. What `off` is decides what
//! the game is allowed to say (plan §2.1):
//!
//! | basis      | `off`                 | timing judged | tendency claimed | "center" shown |
//! |------------|-----------------------|---------------|------------------|----------------|
//! | `Loopback` | the measured loop     | yes           | by the round rule | loop σ ≤ 8 ms |
//! | `Estimate` | latency **+** player  | yes           | never            | never          |
//! | `None`     | —                     | no            | never            | never          |
//!
//! With a measured loop the player's habitual early or late tendency stays in `e`: it is
//! shown, never subtracted. Without a loop the median of the player's own strums is the
//! only reference there is, latency and tendency cannot be told apart, and the game says
//! nothing about tendency.
//!
//! A basis with numbers that cannot be a measurement (not finite, negative spread, a
//! negative loop, an estimate from no strums or one scattered beyond 15 ms) counts as
//! `None`: it is never judged with.

/// σ of a normal distribution from its median absolute deviation.
pub const MAD_TO_SIGMA: f64 = 1.4826;
/// Standard error of a median, in units of `σ / √n`.
pub const MEDIAN_SE: f64 = 1.2533;
/// What the loop cannot see: speaker → microphone against speaker → ear plus guitar →
/// microphone (about 1 ms per 34 cm), ms.
pub const LOOP_ACOUSTIC_MS: f64 = 3.0;
/// An estimate scattered beyond this MAD is not judged with, ms.
pub const ESTIMATE_MAX_MAD_MS: f64 = 15.0;
/// "Center" is shown only when the loop was measured with at most this σ, ms.
pub const CENTER_MAX_SIGMA_MS: f64 = 8.0;

/// Where the judge's offset comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimingBasis {
    /// The acoustic loop was measured: `l_rt_ms` is system latency only.
    Loopback { l_rt_ms: f64, mad_ms: f64 },
    /// No loop: the median of `n` muted strums, latency and player tendency together.
    Estimate { offset_ms: f64, mad_ms: f64, n: u32 },
    /// No calibration (or one that does not hold on this stream): timing is not judged.
    None,
}

/// The frozen timing reference of a round.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingRef {
    pub basis: TimingBasis,
}

impl TimingRef {
    pub fn loopback(l_rt_ms: f64, mad_ms: f64) -> Self {
        Self {
            basis: TimingBasis::Loopback { l_rt_ms, mad_ms },
        }
    }

    pub fn estimate(offset_ms: f64, mad_ms: f64, n: u32) -> Self {
        Self {
            basis: TimingBasis::Estimate {
                offset_ms,
                mad_ms,
                n,
            },
        }
    }

    pub fn none() -> Self {
        Self {
            basis: TimingBasis::None,
        }
    }

    /// The basis, or `None` when its numbers cannot be a measurement.
    fn valid(&self) -> TimingBasis {
        let ok = match self.basis {
            TimingBasis::Loopback { l_rt_ms, mad_ms } => {
                l_rt_ms.is_finite() && l_rt_ms >= 0.0 && mad_ms.is_finite() && mad_ms >= 0.0
            }
            TimingBasis::Estimate {
                offset_ms,
                mad_ms,
                n,
            } => {
                offset_ms.is_finite()
                    && mad_ms.is_finite()
                    && (0.0..=ESTIMATE_MAX_MAD_MS).contains(&mad_ms)
                    && n >= 1
            }
            TimingBasis::None => true,
        };
        if ok { self.basis } else { TimingBasis::None }
    }

    /// What the judge subtracts, ms. `None`: timing is not judged.
    pub fn offset_ms(&self) -> Option<f64> {
        match self.valid() {
            TimingBasis::Loopback { l_rt_ms, .. } => Some(l_rt_ms),
            TimingBasis::Estimate { offset_ms, .. } => Some(offset_ms),
            TimingBasis::None => None,
        }
    }

    /// True when attacks can be placed against the click at all.
    pub fn judges_timing(&self) -> bool {
        self.offset_ms().is_some()
    }

    /// Uncertainty `u` of the center, ms: how far the true click may lie from `off`.
    /// Infinite when nothing was measured.
    pub fn center_uncertainty_ms(&self) -> f64 {
        match self.valid() {
            TimingBasis::Loopback { mad_ms, .. } => MAD_TO_SIGMA * mad_ms + LOOP_ACOUSTIC_MS,
            TimingBasis::Estimate { mad_ms, n, .. } => {
                2.0 * MEDIAN_SE * MAD_TO_SIGMA * mad_ms / f64::from(n).sqrt()
            }
            TimingBasis::None => f64::INFINITY,
        }
    }

    /// Only a measured loop separates the player from the system.
    pub fn may_claim_tendency(&self) -> bool {
        matches!(self.valid(), TimingBasis::Loopback { .. })
    }

    /// "Center" needs a measured loop with σ ≤ 8 ms.
    pub fn may_show_center(&self) -> bool {
        match self.valid() {
            TimingBasis::Loopback { mad_ms, .. } => MAD_TO_SIGMA * mad_ms <= CENTER_MAX_SIGMA_MS,
            _ => false,
        }
    }
}

/// Tolerance level: the widest window an attack may fall in and still be inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// ±120 ms. Where the game starts.
    Apprentice,
    /// ±80 ms.
    Band,
    /// ±50 ms.
    Stage,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Apprentice, Level::Band, Level::Stage];

    /// Half-width of the window, ms.
    pub fn window_ms(self) -> f64 {
        match self {
            Level::Apprentice => 120.0,
            Level::Band => 80.0,
            Level::Stage => 50.0,
        }
    }

    /// The narrowest level whose window holds `abs_ms`; `None` beyond the widest.
    pub fn step_for(abs_ms: f64) -> Option<Level> {
        if abs_ms <= Level::Stage.window_ms() {
            Some(Level::Stage)
        } else if abs_ms <= Level::Band.window_ms() {
            Some(Level::Band)
        } else if abs_ms <= Level::Apprentice.window_ms() {
            Some(Level::Apprentice)
        } else {
            None
        }
    }

    /// True when `self` has a narrower window than `other`.
    pub fn is_narrower_than(self, other: Level) -> bool {
        self.window_ms() < other.window_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_subtracts_the_loop_only() {
        let t = TimingRef::loopback(87.3, 0.4);
        assert_eq!(t.offset_ms(), Some(87.3));
        assert!((t.center_uncertainty_ms() - (1.4826 * 0.4 + 3.0)).abs() < 1e-12);
        assert!(t.may_claim_tendency());
        assert!(t.may_show_center());
        assert!(t.judges_timing());
    }

    #[test]
    fn center_needs_a_loop_with_sigma_up_to_8_ms() {
        let at = 8.0 / MAD_TO_SIGMA;
        assert!(TimingRef::loopback(50.0, at - 1e-9).may_show_center());
        assert!(!TimingRef::loopback(50.0, at + 1e-9).may_show_center());
        assert!(!TimingRef::estimate(50.0, 0.1, 16).may_show_center());
        assert!(!TimingRef::none().may_show_center());
    }

    #[test]
    fn estimate_judges_but_never_claims() {
        let t = TimingRef::estimate(112.0, 9.0, 16);
        assert_eq!(t.offset_ms(), Some(112.0));
        let u = 2.0 * 1.2533 * 1.4826 * 9.0 / 4.0;
        assert!((t.center_uncertainty_ms() - u).abs() < 1e-12);
        assert!(!t.may_claim_tendency());
        assert!(!t.may_show_center());
    }

    #[test]
    fn numbers_that_are_not_a_measurement_are_no_calibration() {
        let bad = [
            TimingRef::loopback(f64::NAN, 1.0),
            TimingRef::loopback(80.0, f64::NAN),
            TimingRef::loopback(80.0, -0.1),
            TimingRef::loopback(-1.0, 1.0),
            TimingRef::loopback(f64::INFINITY, 1.0),
            TimingRef::estimate(f64::NAN, 5.0, 16),
            TimingRef::estimate(100.0, 15.000_001, 16),
            TimingRef::estimate(100.0, -1.0, 16),
            TimingRef::estimate(100.0, 5.0, 0),
            TimingRef::none(),
        ];
        for t in bad {
            assert_eq!(t.offset_ms(), None, "{t:?}");
            assert!(!t.judges_timing());
            assert!(!t.may_claim_tendency());
            assert!(!t.may_show_center());
            assert_eq!(t.center_uncertainty_ms(), f64::INFINITY);
        }
        // the edge itself is still judged
        assert!(TimingRef::estimate(100.0, 15.0, 16).judges_timing());
        // an estimate may be negative: a short path and a player who rushes
        assert_eq!(TimingRef::estimate(-12.0, 5.0, 16).offset_ms(), Some(-12.0));
    }

    #[test]
    fn levels_and_steps() {
        assert_eq!(Level::Apprentice.window_ms(), 120.0);
        assert_eq!(Level::Band.window_ms(), 80.0);
        assert_eq!(Level::Stage.window_ms(), 50.0);
        assert_eq!(Level::step_for(50.0), Some(Level::Stage));
        assert_eq!(Level::step_for(50.001), Some(Level::Band));
        assert_eq!(Level::step_for(80.0), Some(Level::Band));
        assert_eq!(Level::step_for(120.0), Some(Level::Apprentice));
        assert_eq!(Level::step_for(120.001), None);
        assert_eq!(Level::step_for(f64::NAN), None);
        assert!(Level::Stage.is_narrower_than(Level::Band));
        assert!(!Level::Apprentice.is_narrower_than(Level::Apprentice));
    }
}
