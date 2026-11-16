//! The pulse judge: expectations and heard onsets to verdicts (owned by C2).
//!
//! A pure function of its inputs (plan I2): it reads no clock. Input time is the `in_now`
//! of [`JudgeInput::Progress`] and nothing else, so a tape replays to the same verdicts.
//!
//! # Cells
//!
//! Every expected event `k` owns a **cell** on the input time axis, centred where its
//! click is captured (`g_k`) plus the timing offset `off` ([`TimingRef::offset_ms`]). With
//! `gap_k` the distance to the nearest neighbour, in ms:
//!
//! ```text
//! P_k  = min(0.5 · gap_k, pair_max_ms)             pairing radius
//! W_k  = min(level window, 0.25 · gap_k)
//! W_ef = min(W_k + u, P_k)                          u = TimingRef::center_uncertainty_ms
//! e    = (t − g_k) · 1000 / fs_in − off             negative = early
//! ```
//!
//! An attack belongs to the cell with `|e| ≤ P_k` (the nearest one if cells touch), and to
//! no other: one attack never serves two events. `|e| ≤ W_ef` is in time (`Center` within
//! `center_ms` when the loop was measured well enough), the rest of the cell is `Outside`.
//! `W_k` stops at a quarter of the gap, so a player who is consistently far off is
//! `Outside` in the cell they fell into and never "in time" on the neighbour. The window
//! grows by the uncertainty of the center: accusing takes more evidence than confirming.
//!
//! The edges are inclusive, and `e` comes out of frame arithmetic in `f64`. Every edge
//! therefore has a slack of a nanosecond plus the rounding of the frame numbers
//! ([`EDGE_MS`]), always on the side of the player: an attack exactly on an edge is in the
//! cell, in the window, in the center.
//!
//! The first strum in a cell gives a provisional verdict at once (`fin == false`). A later
//! strum replaces it only if it is nearer the center, so a verdict only gets better; the
//! other strum is an [`JudgeOut::Extra`], shown small and never an error. A ghost event has
//! a cell of its own and swallows what falls in it. The cell closes, and the verdict
//! becomes final, when input time passes `g_k + off + P_k + close_margin_ms`; only then can
//! an event be `NotHeard`. The margin covers the longest delay between an attack and the
//! arrival of its `Strum`: the detector waits for the strum to end
//! ([`JudgeConfig::STRUM_DELAY_MS`]), and the `Strum` then waits for the end of its block
//! of input.
//!
//! # What is not judged
//!
//! The judge would rather say nothing than accuse without evidence (plan I3):
//!
//! | reason | when |
//! |---|---|
//! | `NoCalibration` | no usable timing basis: attacks cannot be placed against the click |
//! | `GateLocked` | the `Attack` or `Timing` gate is locked; an event closer than [`MIN_GAP_MS`] to a neighbour |
//! | `ClockNotLocked` | the clock was not locked when the click sounded, when the strum was paired, or (for an event with no attack) while the cell was listening |
//! | `Glitch` | `judged == false`; the click or the strum lies inside an `Unjudged` window; `Reset`; a strum that came after its cell had closed; input time that ran backward |
//! | `Disputed` | the player contested it |
//! | `Ghost` | a ghost stroke: nothing was expected to sound |
//! | `NoAttack` | (timing only) the event was `NotHeard`: there is nothing to time |
//!
//! An accusation (`NotHeard`, `Outside`) needs a cell that was heard whole. So, beyond "the
//! click or the strum inside the window", and always on the side of the player:
//!
//! - An event with **no** attack is `NotJudged(Glitch)` if a window overlaps any part of
//!   its cell: the attack may be in the audio that was lost.
//! - An `Outside` keeps its attack but loses its timing if a window overlaps the cell, or
//!   if an attack that cannot be trusted (`judged == false`, inside a window, clock not
//!   locked) fell in the cell: the attack that was nearer may be the one that was lost.
//! - A confirmed hit stays a hit in both cases.
//!
//! A reason that arrives after the final verdict withdraws it: [`JudgeOut::Withdrawn`].
//! While the cell is still open the provisional verdict is sent again with the reason. A
//! strum that arrives after its cell closed takes an accusation back the same way: a
//! `NotHeard` always, an `Outside` if the late strum is nearer than the one that was judged
//! or cannot be trusted.
//!
//! # Dispute
//!
//! [`JudgeInput::Dispute`] contests **the last final verdict shown** (plan C2): the event
//! whose final verdict was sent last, whatever provisional verdicts of later events came
//! after it, and also when that final verdict only confirmed its provisional. It is
//! withdrawn as `Withdrawn { why: Disputed }`: out of the statistics, and never a hit, not
//! even if a strum for it arrives later. A provisional verdict is never contested. A press
//! does nothing when that verdict is already out of the statistics (not judged, withdrawn,
//! or contested by the press before), or when no final verdict was sent since `Reset`.
//! Ghost events show nothing and are never the target.
//!
//! # Contract with the caller
//!
//! - Ids of new expectations increase (a counter) until `Reset`. A second `Expect` with
//!   the id of an open cell refines it (a better `g_in`) as long as no strum was paired
//!   with it. Any other `Expect` whose id is not above every id before it is ignored and
//!   counted: its verdict is given and is not given twice.
//! - `Expect` must come before input time reaches the cell; `Strum` must come before the
//!   `Progress` that passes the end of the block in which the detector emitted it.
//! - `Progress::in_now` is a finite frame number and does not decrease until `Reset`. A
//!   report that is not a number closes nothing, and the clock counts as not locked until
//!   the next one. A report below the one before is a clock step nobody announced: the
//!   stretch that runs twice is treated as an `Unjudged` window. Both are counted. To end
//!   a round, send `Reset` (open events end `NotJudged`) or a finite time past the last
//!   cell.
//! - Nothing is allocated after [`PulseJudge::new`]. `out` is the caller's: it grows only
//!   if the caller gives it no room.

use std::collections::VecDeque;

use crate::gates::GateTable;
use crate::timing::{Level, TimingRef};
use crate::verdict::{
    Attack, Direction, ExpectKind, Judge, JudgeInput, JudgeOut, PulseVerdict, Timing, VerdictType,
    Why,
};

/// Expectations that may be open at once. An `Expect` beyond it is answered
/// `NotJudged(Glitch)` at once.
pub const OPEN_CELLS: usize = 256;
/// Closed cells remembered for a late `Unjudged` window or a late strum.
pub const CLOSED_CELLS: usize = 1024;
/// `Unjudged` windows remembered for the cells and strums that arrive after them.
const WINDOWS: usize = 64;
/// Events closer than this are not judged: the onset detector joins attacks less than
/// 45 ms apart into one strum, so the second one could never be heard.
pub const MIN_GAP_MS: f64 = 60.0;
/// Slack of every inclusive edge, ms: one nanosecond, far below anything a detector can
/// resolve and far above the rounding of `e` in any stream shorter than a month.
pub const EDGE_MS: f64 = 1e-6;
/// The slack grows with the rounding of the frame numbers themselves (this many units in
/// the last place of the larger one), so no stream is too long for it.
const EDGE_ULPS: f64 = 8.0;
/// Rate used to keep input time moving when `fs_in` is not a rate (nothing is judged then).
const FALLBACK_FS: f64 = 48_000.0;
/// The pulse judge has no evidence about direction and no way to accuse it.
const DIRECTION: Direction = Direction::NotJudged(Why::GateLocked);

/// Settings of the judge. Frozen for the round.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JudgeConfig {
    /// Input sample rate, Hz: `g_in`, `t_in` and `in_now` are frames at this rate.
    pub fs_in: f64,
    pub level: Level,
    /// `Center` is `|e|` up to this, ms.
    pub center_ms: f64,
    /// Largest pairing radius, ms.
    pub pair_max_ms: f64,
    /// How long after the end of its cell an event waits for a strum still on its way, ms.
    /// At least [`Self::STRUM_DELAY_MS`] plus the longest block of input, plus whatever the
    /// delivery adds.
    pub close_margin_ms: f64,
}

impl JudgeConfig {
    pub const CENTER_MS: f64 = 25.0;
    pub const PAIR_MAX_MS: f64 = 250.0;
    /// Longest time from an attack to the moment the onset detector emits its `Strum`, ms,
    /// as the detector is built: it looks back 60 ms from the first flux peak of a strum,
    /// a strum lasts up to 180 ms, and it ends 45 ms after its last peak. One hop, the
    /// energy window and the filter delays add less than 15 ms.
    pub const STRUM_DELAY_MS: f64 = 300.0;
    /// [`Self::STRUM_DELAY_MS`] and 50 ms for the block in which the `Strum` leaves the
    /// detector (2048 frames at 44.1 kHz are 46.4 ms). A host with longer blocks, or one
    /// that reports the clock ahead of the detector, must add the excess.
    pub const CLOSE_MARGIN_MS: f64 = 350.0;

    /// The plan's values for a stream at `fs_in`.
    pub fn new(fs_in: f64, level: Level) -> Self {
        Self {
            fs_in,
            level,
            center_ms: Self::CENTER_MS,
            pair_max_ms: Self::PAIR_MAX_MS,
            close_margin_ms: Self::CLOSE_MARGIN_MS,
        }
    }
}

/// Radii of one cell, ms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellShape {
    /// Pairing radius `P`: an attack farther than this from the click is not this event's.
    pub pair_ms: f64,
    /// Effective window `W_ef`: an attack inside it is in time.
    pub window_ms: f64,
    /// False for an event too close to a neighbour to be told apart.
    pub judged: bool,
}

/// Things that should not happen, counted instead of hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JudgeCounters {
    /// Strums that arrived after their own cell had closed.
    pub late_strums: u64,
    /// `Expect`s that were not a new event: the id of a cell already paired, or an id not
    /// above every id before it.
    pub ignored_expects: u64,
    /// `Expect`s refused because [`OPEN_CELLS`] were open.
    pub overflowed_expects: u64,
    /// `Progress` reports that were not a position, or that ran backward.
    pub clock_faults: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Open,
    /// Closed with an attack.
    Hit,
    /// Closed as `NotHeard`.
    Missed,
    /// Closed without a judgement, or withdrawn: nothing left to take back.
    Void,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    id: u32,
    pos_in_bar: u8,
    ghost: bool,
    /// Too close to a neighbour.
    dense: bool,
    /// Input frame at which the click is captured.
    g_in: f64,
    pair_ms: f64,
    win_ms: f64,
    /// Slack of the edges of this cell, ms.
    edge_ms: f64,
    /// The cell on the input axis, and when it closes (the slack included).
    lo_in: f64,
    hi_in: f64,
    close_in: f64,
    /// The paired strum (NaN: none) and its error.
    t_in: f64,
    e_ms: f64,
    /// Why the paired strum, the click, or the listening cannot be trusted.
    onset_why: Option<Why>,
    beat_why: Option<Why>,
    listen_why: Option<Why>,
    /// Input time has reached the click.
    beat_seen: bool,
    /// The last verdict sent for this cell.
    shown: Option<PulseVerdict>,
    outcome: Outcome,
}

impl Cell {
    fn new(id: u32, ghost: bool, pos_in_bar: u8) -> Self {
        Self {
            id,
            pos_in_bar,
            ghost,
            dense: false,
            g_in: f64::NAN,
            pair_ms: 0.0,
            win_ms: 0.0,
            edge_ms: EDGE_MS,
            lo_in: f64::NAN,
            hi_in: f64::NAN,
            close_in: f64::NAN,
            t_in: f64::NAN,
            e_ms: f64::NAN,
            onset_why: None,
            beat_why: None,
            listen_why: None,
            beat_seen: false,
            shown: None,
            outcome: Outcome::Open,
        }
    }

    fn paired(&self) -> bool {
        !self.t_in.is_nan()
    }

    /// Record what an `Unjudged` window `[lo, hi]` touches.
    fn mark(&mut self, lo: f64, hi: f64) {
        if lo <= self.g_in && self.g_in <= hi {
            self.beat_why = stronger(self.beat_why, Why::Glitch);
        }
        if lo <= self.t_in && self.t_in <= hi {
            self.onset_why = stronger(self.onset_why, Why::Glitch);
        }
        if lo <= self.hi_in && self.lo_in <= hi {
            self.listen_why = stronger(self.listen_why, Why::Glitch);
        }
    }
}

/// `Glitch` outranks `ClockNotLocked`.
fn stronger(a: Option<Why>, b: Why) -> Option<Why> {
    match a {
        Some(Why::Glitch) => a,
        _ => Some(b),
    }
}

fn worst(a: Option<Why>, b: Option<Why>) -> Option<Why> {
    match b {
        Some(w) => stronger(a, w),
        None => a,
    }
}

/// The judge of the Pulso mode. See the module documentation.
pub struct PulseJudge {
    cfg: JudgeConfig,
    timing: TimingRef,
    ms_per_frame: f64,
    off_ms: f64,
    u_ms: f64,
    show_center: bool,
    /// Attacks are not placed at all: no calibration, or the gate is locked.
    attack_block: Option<Why>,
    /// Attacks are placed but their timing is not reported.
    timing_block: Option<Why>,
    open: Vec<Cell>,
    closed: VecDeque<Cell>,
    windows: VecDeque<(f64, f64)>,
    in_now: f64,
    locked: bool,
    /// The largest id expected so far.
    top_id: Option<u32>,
    /// The event of the last final verdict sent (ghosts have none to contest).
    last_final: Option<u32>,
    counters: JudgeCounters,
}

impl PulseJudge {
    pub fn new(cfg: JudgeConfig, timing: TimingRef, gates: &GateTable) -> Self {
        let or = |x: f64, d: f64| if x.is_finite() && x >= 0.0 { x } else { d };
        let fs_ok = cfg.fs_in.is_finite() && cfg.fs_in > 0.0;
        let cfg = JudgeConfig {
            center_ms: or(cfg.center_ms, JudgeConfig::CENTER_MS),
            pair_max_ms: or(cfg.pair_max_ms, JudgeConfig::PAIR_MAX_MS),
            close_margin_ms: or(cfg.close_margin_ms, JudgeConfig::CLOSE_MARGIN_MS),
            ..cfg
        };
        let off = if fs_ok { timing.offset_ms() } else { None };
        let attack_block = if !gates.judges(VerdictType::Attack) {
            Some(Why::GateLocked)
        } else if off.is_none() {
            Some(Why::NoCalibration)
        } else {
            None
        };
        let timing_block = if !gates.judges(VerdictType::Timing) {
            Some(Why::GateLocked)
        } else {
            attack_block
        };
        Self {
            cfg,
            timing,
            ms_per_frame: 1000.0 / if fs_ok { cfg.fs_in } else { FALLBACK_FS },
            off_ms: off.unwrap_or(0.0),
            u_ms: timing.center_uncertainty_ms(),
            show_center: timing.may_show_center(),
            attack_block,
            timing_block,
            open: Vec::with_capacity(OPEN_CELLS),
            closed: VecDeque::with_capacity(CLOSED_CELLS),
            windows: VecDeque::with_capacity(WINDOWS),
            in_now: f64::NEG_INFINITY,
            locked: false,
            top_id: None,
            last_final: None,
            counters: JudgeCounters::default(),
        }
    }

    /// The settings in use (out-of-range values replaced by the plan's).
    pub fn config(&self) -> &JudgeConfig {
        &self.cfg
    }

    pub fn timing(&self) -> &TimingRef {
        &self.timing
    }

    pub fn counters(&self) -> JudgeCounters {
        self.counters
    }

    /// The cell of an event with these gaps to its neighbours, seconds.
    pub fn shape(&self, prev_gap_s: f64, next_gap_s: f64) -> CellShape {
        // `f64::min` skips a NaN side; two NaNs give no cell at all.
        let gap_ms = prev_gap_s.min(next_gap_s) * 1000.0;
        let judged = gap_ms >= MIN_GAP_MS;
        let gap = if gap_ms.is_nan() {
            0.0
        } else {
            gap_ms.max(0.0)
        };
        let pair_ms = (0.5 * gap).min(self.cfg.pair_max_ms);
        let window_ms = (self.cfg.level.window_ms().min(0.25 * gap) + self.u_ms).min(pair_ms);
        CellShape {
            pair_ms,
            window_ms,
            judged,
        }
    }

    fn place(&self, c: &mut Cell, g_in: f64, prev_gap_s: f64, next_gap_s: f64) {
        let s = self.shape(prev_gap_s, next_gap_s);
        let frames = 1.0 / self.ms_per_frame;
        // Rounding must not put an attack that is exactly on an edge beyond it.
        let edge = EDGE_MS + EDGE_ULPS * f64::EPSILON * g_in.abs() * self.ms_per_frame;
        c.g_in = g_in;
        c.pair_ms = s.pair_ms;
        c.win_ms = s.window_ms;
        c.edge_ms = edge;
        c.dense = !s.judged;
        c.lo_in = g_in + (self.off_ms - s.pair_ms - edge) * frames;
        c.hi_in = g_in + (self.off_ms + s.pair_ms + edge) * frames;
        c.close_in =
            g_in + ((self.off_ms + s.pair_ms).max(0.0) + self.cfg.close_margin_ms + edge) * frames;
        for &(lo, hi) in &self.windows {
            c.mark(lo, hi);
        }
    }

    fn error_ms(&self, c: &Cell, t_in: f64) -> f64 {
        (t_in - c.g_in) * self.ms_per_frame - self.off_ms
    }

    /// The cell that `t_in` falls in: the nearest center, the earlier cell on a tie.
    fn locate<'a>(&self, cells: impl Iterator<Item = &'a Cell>, t_in: f64) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in cells.enumerate() {
            let e = self.error_ms(c, t_in);
            if e.abs() <= c.pair_ms + c.edge_ms && best.is_none_or(|(_, b)| e.abs() < b.abs()) {
                best = Some((i, e));
            }
        }
        best
    }

    fn in_window(&self, t_in: f64) -> bool {
        self.windows
            .iter()
            .any(|&(lo, hi)| lo <= t_in && t_in <= hi)
    }

    fn classify(&self, c: &Cell) -> Timing {
        let a = c.e_ms.abs();
        if a > c.win_ms + c.edge_ms {
            Timing::Outside
        } else if self.show_center && a <= self.cfg.center_ms + c.edge_ms {
            Timing::Center
        } else {
            Timing::InWindow
        }
    }

    fn verdict(&self, c: &Cell, fin: bool) -> PulseVerdict {
        let paired = c.paired();
        let none = |a: Why, t: Why| (Attack::NotJudged(a), Timing::NotJudged(t), None);
        let (attack, timing, dt_ms) = if c.ghost {
            none(Why::Ghost, Why::Ghost)
        } else if let Some(w) = self.attack_block {
            none(w, self.timing_block.unwrap_or(w))
        } else if c.dense {
            none(Why::GateLocked, Why::GateLocked)
        } else if let Some(w) = worst(c.beat_why, if paired { c.onset_why } else { c.listen_why }) {
            none(w, w)
        } else if !paired {
            (Attack::NotHeard, Timing::NotJudged(Why::NoAttack), None)
        } else if let Some(w) = self.timing_block {
            (Attack::Heard, Timing::NotJudged(w), None)
        } else {
            match (self.classify(c), c.listen_why) {
                // an accusation needs a cell that was heard whole
                (Timing::Outside, Some(w)) => (Attack::Heard, Timing::NotJudged(w), None),
                (class, _) => (Attack::Heard, class, Some(c.e_ms as f32)),
            }
        };
        PulseVerdict {
            id: c.id,
            attack,
            timing,
            dt_ms,
            dir: DIRECTION,
            onset_in: paired.then_some(c.t_in),
            pos_in_bar: c.pos_in_bar,
            fin,
        }
    }

    /// Send the provisional verdict of open cell `i` if it differs from the last one sent.
    fn show(&mut self, i: usize, out: &mut Vec<JudgeOut>) {
        let v = self.verdict(&self.open[i], false);
        if self.open[i].shown != Some(v) {
            self.open[i].shown = Some(v);
            out.push(JudgeOut::Verdict(v));
        }
    }

    /// As [`Self::show`], for a cell that already has a verdict on screen.
    fn refresh(&mut self, i: usize, out: &mut Vec<JudgeOut>) {
        if self.open[i].shown.is_some() {
            self.show(i, out);
        }
    }

    /// Final verdict of `c`, which then joins the closed cells.
    fn finish(&mut self, mut c: Cell, out: &mut Vec<JudgeOut>) {
        let v = self.verdict(&c, true);
        out.push(JudgeOut::Verdict(v));
        if !c.ghost {
            self.last_final = Some(c.id);
        }
        c.shown = Some(v);
        c.outcome = match v.attack {
            Attack::Heard => Outcome::Hit,
            Attack::NotHeard => Outcome::Missed,
            Attack::NotJudged(_) => Outcome::Void,
        };
        if self.closed.len() == CLOSED_CELLS {
            self.closed.pop_front();
        }
        self.closed.push_back(c);
    }

    #[allow(clippy::too_many_arguments)]
    fn expect(
        &mut self,
        id: u32,
        g_in: f64,
        kind: ExpectKind,
        prev_gap_s: f64,
        next_gap_s: f64,
        pos_in_bar: u8,
        out: &mut Vec<JudgeOut>,
    ) {
        if let Some(i) = self.open.iter().position(|c| c.id == id) {
            let mut c = self.open[i];
            if !c.paired() && g_in.is_finite() {
                self.place(&mut c, g_in, prev_gap_s, next_gap_s);
                self.open[i] = c;
            } else {
                self.counters.ignored_expects += 1;
            }
            return;
        }
        // Not open and not above every id so far: this event has its verdict (however long
        // ago), or the ids are out of order. Either way it is not judged twice.
        if self.top_id.is_some_and(|top| id <= top) {
            self.counters.ignored_expects += 1;
            return;
        }
        self.top_id = Some(id);
        let mut c = Cell::new(id, kind == ExpectKind::Ghost, pos_in_bar);
        if !g_in.is_finite() {
            // the clock could not place the click
            c.beat_why = Some(Why::ClockNotLocked);
            self.finish(c, out);
            return;
        }
        self.place(&mut c, g_in, prev_gap_s, next_gap_s);
        if self.open.len() == OPEN_CELLS {
            self.counters.overflowed_expects += 1;
            c.beat_why = Some(Why::Glitch);
            self.finish(c, out);
            return;
        }
        self.open.push(c);
    }

    fn heard(&mut self, t_in: f64, judged: bool, out: &mut Vec<JudgeOut>) {
        let mut flash = (None, None);
        if self.attack_block.is_none()
            && let Some((i, e)) = self.locate(self.open.iter(), t_in)
        {
            let c = &self.open[i];
            let clean = judged
                && self.locked
                && self.timing_block.is_none()
                && !c.ghost
                && !c.dense
                && c.beat_why.is_none()
                && !self.in_window(t_in);
            flash = (Some(c.id), clean.then_some(e as f32));
        }
        out.push(JudgeOut::Flash {
            id: flash.0,
            dt_ms: flash.1,
        });
    }

    fn strum(&mut self, t_in: f64, judged: bool, out: &mut Vec<JudgeOut>) {
        if self.attack_block.is_some() {
            return;
        }
        if !t_in.is_finite() {
            // An attack at an unknown time: any cell that is listening may be its own.
            let reach = self.in_now + self.cfg.close_margin_ms / self.ms_per_frame;
            for i in 0..self.open.len() {
                if self.open[i].lo_in <= reach {
                    self.open[i].listen_why = stronger(self.open[i].listen_why, Why::Glitch);
                    self.refresh(i, out);
                }
            }
            return;
        }
        let why = if !judged || self.in_window(t_in) {
            Some(Why::Glitch)
        } else if !self.locked {
            Some(Why::ClockNotLocked)
        } else {
            None
        };
        if let Some((i, e)) = self.locate(self.open.iter(), t_in) {
            let c = &mut self.open[i];
            if c.ghost || c.dense {
                return; // swallowed
            }
            if let Some(w) = why {
                // Where this attack really fell is not known, so it may be the nearest one:
                // the cell was not heard whole, and nothing in it is "far off" any more.
                c.listen_why = stronger(c.listen_why, w);
            }
            if c.paired() {
                // Nearer wins; a strum that cannot be trusted never replaces one that can.
                let better = e.abs() < c.e_ms.abs() && !(c.onset_why.is_none() && why.is_some());
                if !better {
                    out.push(JudgeOut::Extra { t_in });
                    self.refresh(i, out);
                    return;
                }
                out.push(JudgeOut::Extra { t_in: c.t_in });
            }
            c.t_in = t_in;
            c.e_ms = e;
            c.onset_why = why;
            self.show(i, out);
        } else if let Some((j, e)) = self.locate(self.closed.iter(), t_in) {
            self.counters.late_strums += 1;
            let c = &mut self.closed[j];
            if c.ghost || c.dense {
                return; // swallowed
            }
            // It was played after all, or played nearer than the attack that was called far
            // off (or perhaps nearer: this one cannot be placed): take the accusation back.
            let undermined = match c.outcome {
                Outcome::Missed => true,
                Outcome::Hit => {
                    c.shown.is_some_and(|v| v.timing == Timing::Outside)
                        && (why.is_some() || e.abs() < c.e_ms.abs())
                }
                Outcome::Open | Outcome::Void => false,
            };
            if undermined {
                c.outcome = Outcome::Void;
                out.push(JudgeOut::Withdrawn {
                    id: c.id,
                    why: Why::Glitch,
                });
            } else {
                out.push(JudgeOut::Extra { t_in });
            }
        } else {
            out.push(JudgeOut::Extra { t_in });
        }
    }

    fn unjudged(&mut self, from_in: f64, to_in: f64, out: &mut Vec<JudgeOut>) {
        if from_in.is_nan() || to_in.is_nan() {
            return;
        }
        let (lo, hi) = (from_in.min(to_in), from_in.max(to_in));
        if self.windows.len() == WINDOWS {
            self.windows.pop_front();
        }
        self.windows.push_back((lo, hi));
        for i in 0..self.open.len() {
            self.open[i].mark(lo, hi);
            self.refresh(i, out);
        }
        for c in self.closed.iter_mut() {
            let inside = |x: f64| lo <= x && x <= hi;
            let cell_hit = lo <= c.hi_in && c.lo_in <= hi;
            let hit = match c.outcome {
                Outcome::Hit => {
                    inside(c.g_in)
                        || inside(c.t_in)
                        || (cell_hit && c.shown.is_some_and(|v| v.timing == Timing::Outside))
                }
                Outcome::Missed => inside(c.g_in) || cell_hit,
                Outcome::Open | Outcome::Void => false,
            };
            if hit {
                c.outcome = Outcome::Void;
                out.push(JudgeOut::Withdrawn {
                    id: c.id,
                    why: Why::Glitch,
                });
            }
        }
    }

    fn progress(&mut self, in_now: f64, locked: bool, out: &mut Vec<JudgeOut>) {
        if !in_now.is_finite() {
            // Not a position: nothing closes on it (an infinite one would end every open
            // event as `NotHeard`), and the clock is not trusted until it reports one.
            self.counters.clock_faults += 1;
            self.locked = false;
            return;
        }
        if in_now < self.in_now {
            // A step back that nobody announced. Which of the two passes over this stretch
            // has the right time is not known: nothing in it is judged.
            self.counters.clock_faults += 1;
            let from = self.in_now;
            self.unjudged(in_now, from, out);
        }
        // The clock held from the last report to this one only if both say so.
        let clean = self.locked && locked;
        self.locked = locked;
        self.in_now = in_now;
        let mut i = 0;
        while i < self.open.len() {
            let c = &mut self.open[i];
            if !c.beat_seen && in_now >= c.g_in {
                c.beat_seen = true;
                if !clean {
                    c.beat_why = stronger(c.beat_why, Why::ClockNotLocked);
                }
            }
            if !clean && in_now >= c.lo_in {
                c.listen_why = stronger(c.listen_why, Why::ClockNotLocked);
            }
            if in_now > c.close_in {
                let c = self.open.remove(i);
                self.finish(c, out);
            } else {
                self.refresh(i, out);
                i += 1;
            }
        }
    }

    fn dispute(&mut self, out: &mut Vec<JudgeOut>) {
        // The last final verdict sent is the newest closed cell that is not a ghost.
        let Some(j) = self
            .last_final
            .and_then(|id| self.closed.iter().rposition(|c| c.id == id))
        else {
            return;
        };
        let c = &mut self.closed[j];
        if c.outcome == Outcome::Void {
            return; // already out of the statistics
        }
        c.outcome = Outcome::Void;
        out.push(JudgeOut::Withdrawn {
            id: c.id,
            why: Why::Disputed,
        });
    }

    fn reset(&mut self, out: &mut Vec<JudgeOut>) {
        for c in &self.open {
            let mut c = *c;
            c.beat_why = Some(Why::Glitch);
            out.push(JudgeOut::Verdict(self.verdict(&c, true)));
        }
        self.open.clear();
        self.closed.clear();
        self.windows.clear();
        self.in_now = f64::NEG_INFINITY;
        self.locked = false;
        self.top_id = None;
        self.last_final = None;
    }
}

impl Judge for PulseJudge {
    fn push(&mut self, input: &JudgeInput, out: &mut Vec<JudgeOut>) {
        match *input {
            JudgeInput::Expect {
                id,
                g_in,
                kind,
                prev_gap_s,
                next_gap_s,
                pos_in_bar,
            } => self.expect(id, g_in, kind, prev_gap_s, next_gap_s, pos_in_bar, out),
            JudgeInput::Heard { t_in, judged } => self.heard(t_in, judged, out),
            JudgeInput::Strum { t_in, judged } => self.strum(t_in, judged, out),
            JudgeInput::Unjudged { from_in, to_in } => self.unjudged(from_in, to_in, out),
            JudgeInput::Progress { in_now, locked } => self.progress(in_now, locked, out),
            JudgeInput::Dispute => self.dispute(out),
            JudgeInput::Reset => self.reset(out),
        }
    }
}
