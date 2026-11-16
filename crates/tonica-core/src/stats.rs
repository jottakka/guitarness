//! Round statistics, tendency and tolerance suggestion (owned by C2).
//!
//! Everything here is computed from the **final** verdicts of a round, after withdrawals
//! ([`VerdictLog`] keeps that list). A verdict that was not judged, withdrawn or disputed
//! is in no rate and in no median: it is neither a hit nor a miss.
//!
//! # Tendency (plan §2.1)
//!
//! Over the timing errors `e` of the timed verdicts: `m` = median, `σ̂ = 1.4826 · MAD`,
//! `SE = 1.2533 · σ̂ / √n`, `u_t = 2 · SE + u` with `u` the uncertainty of the center.
//!
//! 1. no measured loop → not claimed (`EstimateOnly`): without a loop the offset already
//!    holds the player's own median, so "early" or "late" would be made up;
//! 2. `n < 12` → not claimed (`TooFew`);
//! 3. `u_t > 15 ms` → not claimed (`Uncertain`);
//! 4. `|m| ≤ max(10, u_t)` → `Centered`; otherwise `Early(m)` or `Late(m)`.
//!
//! # Tolerance
//!
//! The suggested level is the narrowest whose window holds the 90th percentile of `|e|`.
//! An expected attack that was not heard counts as beyond every window: an attack outside
//! its cell is never timed, so leaving those out would make a scattered round look tight.
//! It is a suggestion; nothing here narrows a window.

use crate::gates::GateTable;
use crate::timing::{Level, MAD_TO_SIGMA, MEDIAN_SE, TimingRef};
use crate::verdict::{Attack, JudgeOut, PulseVerdict, Timing, VerdictType, Why};

/// Fewest timed attacks for a tendency or a tolerance suggestion.
pub const MIN_TIMED: u32 = 12;
/// Above this uncertainty of the round median, no tendency is claimed, ms.
pub const TENDENCY_MAX_UNCERTAINTY_MS: f64 = 15.0;
/// A median within this of the click is centered, ms.
pub const CENTERED_MS: f64 = 10.0;
/// Fewest timed attacks in each round to compare their spread or in-window share.
pub const MIN_COMPARED: u32 = 8;

/// Why no tendency is claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoClaim {
    /// No measured loop: latency and tendency cannot be told apart.
    EstimateOnly,
    /// Fewer than [`MIN_TIMED`] timed attacks.
    TooFew,
    /// The round median is not known well enough.
    Uncertain,
    /// The `Tendency` gate is locked.
    GateLocked,
}

/// The player's habitual place against the click, over one round.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tendency {
    NotClaimed(NoClaim),
    Centered,
    /// Median, ms (negative).
    Early(f64),
    /// Median, ms (positive).
    Late(f64),
}

/// Something that got better from one round to the next. There is no "got worse".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Improvement {
    /// A larger share of the expected attacks was heard (shares in 0..=1).
    MoreHeard { from: f64, to: f64 },
    /// A larger share of the timed attacks was in time.
    MoreInWindow { from: f64, to: f64 },
    /// The spread got smaller, ms.
    Steadier { from_ms: f64, to_ms: f64 },
    /// The round median moved toward the click, ms. Only between two rounds that both
    /// claim a tendency.
    CloserToCenter { from_ms: f64, to_ms: f64 },
}

/// Counts and timing statistics of one round.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundStats {
    /// Final verdicts of events that expected an attack (ghosts are not events here).
    pub events: u32,
    pub heard: u32,
    pub not_heard: u32,
    /// Attack not judged, for any reason but a dispute.
    pub not_judged: u32,
    pub disputed: u32,
    pub center: u32,
    pub in_window: u32,
    pub outside: u32,
    /// Verdicts with a timing error: the `n` of the tendency rule.
    pub timed: u32,
    /// Median timing error, ms (negative = early). `None` without a timed attack.
    pub median_ms: Option<f64>,
    /// Spread `σ̂ = 1.4826 · MAD`, ms.
    pub spread_ms: Option<f64>,
    /// `u_t`: how well the median is known, ms. `None` without a timed attack or a basis.
    pub uncertainty_ms: Option<f64>,
    /// 90th percentile of `|e|`, ms, an expected attack that was not heard counting as
    /// beyond every window. `None` below [`MIN_TIMED`] timed attacks, or when more than
    /// one expected attack in ten was not heard (the percentile is then beyond any cell).
    pub p90_abs_ms: Option<f64>,
    /// The 90th percentile is beyond the widest window: constancy comes before precision.
    pub constancy_focus: bool,
    pub tendency: Tendency,
}

fn median_of_sorted(x: &[f64]) -> f64 {
    let n = x.len();
    if n.is_multiple_of(2) {
        0.5 * (x[n / 2 - 1] + x[n / 2])
    } else {
        x[n / 2]
    }
}

/// 90th percentile of `|e|`, an event that was not heard counting as beyond everything.
enum P90 {
    TooFew,
    Beyond,
    Ms(f64),
}

fn p90_abs(sorted_abs: &[f64], not_heard: u32) -> P90 {
    let timed = sorted_abs.len();
    let n = timed + not_heard as usize;
    if (timed as u32) < MIN_TIMED {
        return P90::TooFew;
    }
    // nearest rank: the smallest value with at least 90% of the sample at or below it
    let rank = (9 * n).div_ceil(10);
    if rank > timed {
        P90::Beyond
    } else {
        P90::Ms(sorted_abs[rank - 1])
    }
}

fn sorted_abs(v: &[PulseVerdict]) -> (Vec<f64>, u32) {
    let mut abs = Vec::new();
    let mut not_heard = 0;
    for p in v.iter().filter(|p| p.fin) {
        match (p.attack, p.dt_ms) {
            (Attack::Heard, Some(dt)) if dt.is_finite() => abs.push(f64::from(dt).abs()),
            (Attack::NotHeard, _) => not_heard += 1,
            _ => {}
        }
    }
    abs.sort_by(f64::total_cmp);
    (abs, not_heard)
}

impl RoundStats {
    /// Statistics of the final verdicts in `v` (provisional ones are skipped). `v` holds
    /// one verdict per event, withdrawals applied: see [`VerdictLog`].
    pub fn from_verdicts(v: &[PulseVerdict], t: &TimingRef) -> Self {
        let mut s = RoundStats {
            events: 0,
            heard: 0,
            not_heard: 0,
            not_judged: 0,
            disputed: 0,
            center: 0,
            in_window: 0,
            outside: 0,
            timed: 0,
            median_ms: None,
            spread_ms: None,
            uncertainty_ms: None,
            p90_abs_ms: None,
            constancy_focus: false,
            tendency: Tendency::NotClaimed(NoClaim::TooFew),
        };
        let mut e: Vec<f64> = Vec::new();
        for p in v.iter().filter(|p| p.fin) {
            match p.attack {
                Attack::NotJudged(Why::Ghost) => continue,
                Attack::NotJudged(Why::Disputed) => s.disputed += 1,
                Attack::NotJudged(_) => s.not_judged += 1,
                Attack::NotHeard => s.not_heard += 1,
                Attack::Heard => s.heard += 1,
            }
            s.events += 1;
            if p.attack != Attack::Heard {
                continue;
            }
            match p.timing {
                Timing::Center => s.center += 1,
                Timing::InWindow => s.in_window += 1,
                Timing::Outside => s.outside += 1,
                Timing::NotJudged(_) => {}
            }
            if let Some(dt) = p.dt_ms
                && dt.is_finite()
            {
                e.push(f64::from(dt));
            }
        }
        s.timed = e.len() as u32;
        if !e.is_empty() {
            e.sort_by(f64::total_cmp);
            let m = median_of_sorted(&e);
            let mut dev: Vec<f64> = e.iter().map(|x| (x - m).abs()).collect();
            dev.sort_by(f64::total_cmp);
            let sigma = MAD_TO_SIGMA * median_of_sorted(&dev);
            let se = MEDIAN_SE * sigma / f64::from(s.timed).sqrt();
            let u = t.center_uncertainty_ms();
            s.median_ms = Some(m);
            s.spread_ms = Some(sigma);
            s.uncertainty_ms = u.is_finite().then_some(2.0 * se + u);
        }
        s.tendency = match (s.median_ms, s.uncertainty_ms) {
            _ if !t.may_claim_tendency() => Tendency::NotClaimed(NoClaim::EstimateOnly),
            (Some(m), Some(u_t)) if s.timed >= MIN_TIMED => {
                if u_t > TENDENCY_MAX_UNCERTAINTY_MS {
                    Tendency::NotClaimed(NoClaim::Uncertain)
                } else if m.abs() <= CENTERED_MS.max(u_t) {
                    Tendency::Centered
                } else if m < 0.0 {
                    Tendency::Early(m)
                } else {
                    Tendency::Late(m)
                }
            }
            _ => Tendency::NotClaimed(NoClaim::TooFew),
        };
        let (abs, not_heard) = sorted_abs(v);
        match p90_abs(&abs, not_heard) {
            P90::TooFew => {}
            P90::Beyond => s.constancy_focus = true,
            P90::Ms(p) => {
                s.p90_abs_ms = Some(p);
                s.constancy_focus = Level::step_for(p).is_none();
            }
        }
        s
    }

    /// The same statistics with the tendency hidden when its gate is locked.
    pub fn gated(mut self, gates: &GateTable) -> Self {
        if !gates.judges(VerdictType::Tendency) {
            self.tendency = Tendency::NotClaimed(NoClaim::GateLocked);
        }
        self
    }

    /// Share of the attacks judged for presence that were heard.
    pub fn heard_share(&self) -> Option<f64> {
        let n = self.heard + self.not_heard;
        (n > 0).then(|| f64::from(self.heard) / f64::from(n))
    }

    /// Share of the timed attacks that were in time.
    pub fn in_window_share(&self) -> Option<f64> {
        (self.timed > 0).then(|| f64::from(self.center + self.in_window) / f64::from(self.timed))
    }

    fn median_se_ms(&self) -> Option<f64> {
        Some(MEDIAN_SE * self.spread_ms? / f64::from(self.timed).sqrt())
    }

    /// What got better since `prev`, most basic first. Empty when nothing did: the caller
    /// then shows something that only grows. A difference too small to be more than the
    /// scatter of two rounds is not an improvement, and nothing about the center is said
    /// unless both rounds may claim a tendency.
    pub fn improvements(&self, prev: &RoundStats) -> Vec<Improvement> {
        let mut out = Vec::new();
        if let (Some(from), Some(to)) = (prev.heard_share(), self.heard_share())
            && prev.heard + prev.not_heard >= 4
            && self.heard + self.not_heard >= 4
            && to - from >= 0.05
        {
            out.push(Improvement::MoreHeard { from, to });
        }
        let both_timed = prev.timed >= MIN_COMPARED && self.timed >= MIN_COMPARED;
        if let (Some(from), Some(to)) = (prev.in_window_share(), self.in_window_share())
            && both_timed
            && to - from >= 0.05
        {
            out.push(Improvement::MoreInWindow { from, to });
        }
        if let (Some(from_ms), Some(to_ms)) = (prev.spread_ms, self.spread_ms)
            && both_timed
            && from_ms - to_ms >= (0.1 * from_ms).max(2.0)
        {
            out.push(Improvement::Steadier { from_ms, to_ms });
        }
        let off_center = |t: Tendency| match t {
            Tendency::Early(m) | Tendency::Late(m) => Some(m),
            _ => None,
        };
        if let Some(from_ms) = off_center(prev.tendency)
            && let (Some(to_ms), Some(se_a), Some(se_b)) =
                (self.median_ms, prev.median_se_ms(), self.median_se_ms())
        {
            let closer = match self.tendency {
                Tendency::Centered => true,
                Tendency::Early(_) | Tendency::Late(_) => {
                    from_ms.abs() - to_ms.abs() >= (se_a + se_b).max(3.0)
                }
                Tendency::NotClaimed(_) => false,
            };
            if closer {
                out.push(Improvement::CloserToCenter { from_ms, to_ms });
            }
        }
        out
    }
}

/// The level whose window would hold nine in ten of this round's attacks. `None` with too
/// few timed attacks, or when even the widest window would not. The caller offers it only
/// when it is narrower than the level in use; nothing narrows by itself.
pub fn tolerance_suggestion(v: &[PulseVerdict]) -> Option<Level> {
    let (abs, not_heard) = sorted_abs(v);
    match p90_abs(&abs, not_heard) {
        P90::Ms(p) => Level::step_for(p),
        P90::TooFew | P90::Beyond => None,
    }
}

/// The final verdicts of a round, kept current while the judge speaks: one per event,
/// with withdrawals applied. Feed it every [`JudgeOut`].
///
/// The log is one round's: `clear` it when the judge is `Reset`, because ids start again
/// there and a verdict with an id already in the log replaces the one before.
///
/// Allocation: [`VerdictLog::with_capacity`] takes the memory for the events of the round
/// at once, and `push` then allocates nothing until that many events have a final verdict
/// (`clear` keeps the memory). A log from [`VerdictLog::new`] grows as it fills, so `push`
/// may allocate: that one is for tools and tests. The statistics ([`VerdictLog::stats`],
/// [`RoundStats::improvements`], [`tolerance_suggestion`]) sort copies of the errors and do
/// allocate: they are for the end of the round, not for the path of every attack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerdictLog {
    verdicts: Vec<PulseVerdict>,
    extras: u32,
}

impl VerdictLog {
    /// An empty log that grows as it fills.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty log with room for the final verdicts of `events` events.
    pub fn with_capacity(events: usize) -> Self {
        Self {
            verdicts: Vec::with_capacity(events),
            extras: 0,
        }
    }

    /// Final verdicts the log holds without allocating again.
    pub fn capacity(&self) -> usize {
        self.verdicts.capacity()
    }

    pub fn push(&mut self, out: &JudgeOut) {
        match *out {
            JudgeOut::Verdict(v) if v.fin => match self.find(v.id) {
                Some(i) => self.verdicts[i] = v,
                None => self.verdicts.push(v),
            },
            JudgeOut::Withdrawn { id, why } => {
                if let Some(i) = self.find(id) {
                    let v = &mut self.verdicts[i];
                    v.attack = Attack::NotJudged(why);
                    v.timing = Timing::NotJudged(why);
                    v.dt_ms = None;
                }
            }
            JudgeOut::Extra { .. } => self.extras += 1,
            JudgeOut::Verdict(_) | JudgeOut::Flash { .. } => {}
        }
    }

    fn find(&self, id: u32) -> Option<usize> {
        self.verdicts.iter().rposition(|v| v.id == id)
    }

    /// Final verdicts in the order they were first given.
    pub fn verdicts(&self) -> &[PulseVerdict] {
        &self.verdicts
    }

    /// Attacks that matched no expectation. Not errors.
    pub fn extras(&self) -> u32 {
        self.extras
    }

    pub fn stats(&self, t: &TimingRef) -> RoundStats {
        RoundStats::from_verdicts(&self.verdicts, t)
    }

    pub fn clear(&mut self) {
        self.verdicts.clear();
        self.extras = 0;
    }
}
