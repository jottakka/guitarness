//! Clock mapping between the input stream, the output stream and the chart.
//!
//! Three clocks exist: the input frame counter `g`, the output frame counter `f` and the
//! host clock of the stamps (ns). The output frame counter is the master: chart time is
//! defined on it ([`ChartClock`]). [`ClockMap`] learns, for each stream, the line
//!
//! ```text
//! T(n) = t0 + p·(n − n0)        p ≈ 1/fs  (seconds per frame)
//! ```
//!
//! from the `(frame index, device time)` pair of every callback, and [`ClockSnapshot`]
//! composes the two lines: `in_of_out(f) = n0_in + (T_out(f) − t0_in) / p_in`.
//!
//! # The filter
//!
//! One second-order delay-locked loop per stream (after F. Adriaensen, "Using a DLL to
//! filter time", 2005), written for blocks of any size. For a stamp `(n_k, τ_k)`:
//!
//! ```text
//! Δ    = n_k − n0                 frames since the last accepted stamp
//! pred = t0 + p·Δ
//! e    = τ_k − pred
//! ω    = 2π·B·Δ/fs                B = natural frequency of the loop, Hz
//! b    = 1 − exp(−√2·ω)
//! c    = 1 + exp(−√2·ω) − 2·exp(−ω/√2)·cos(ω/√2)
//! t0   = pred + b·e
//! p   += c·e/Δ
//! n0   = n_k
//! ```
//!
//! With these gains the two poles of the loop (frozen Δ) are exactly
//! `exp(ω·(−1 ± i)/√2)`: the poles of the continuous loop `s² + √2·ωn·s + ωn²`
//! (`ωn = 2π·B`, damping ζ = 1/√2) sampled over the real time of the block. The loop is
//! therefore stable for every Δ and its time constants do not depend on the block size.
//! It is also stable for every *sequence* of intervals: at a fixed `B`, each update
//! decreases the same quadratic form of the error, the Lyapunov function of the
//! continuous loop (unit test `any_sequence_of_intervals_contracts_one_quadratic_form`).
//! This holds only because ω is the real interval. Gains computed for another interval
//! (a cap on ω, say) break it: see `gains_of_another_interval_can_be_unstable`.
//! For small ω the gains tend to Adriaensen's `b = √2·ω`, `c = ω²`; those are a
//! forward-Euler step, stable only for ω < √6 − √2 ≈ 1.04 in this update order (see the
//! unit tests), which a long gap between callbacks can exceed.
//!
//! A stamp that follows a silence longer than [`ClockConfig::gain_cap_s`] (0.5 s) is not
//! filtered against the old line: the loop would give that single stamp most of the
//! weight and take the period from a two-point secant. Instead the phase is re-seated
//! on the stamp and re-averaged over the next ones, the period is kept, and the stream
//! is not `Locked` again until a window confirms the line.
//!
//! ζ = 1/√2 is what the `√2` of the original gives. It is not critical damping (ζ = 1);
//! it is kept because it is near the minimum noise bandwidth, `B_L = 0.53·ωn ≈ 3.33·B`.
//! With stamp jitter σ and `r` stamps per second, the filtered time has a standard
//! deviation of `σ·√(2·B_L/r)`: 0.082·σ at B = 0.1 Hz and 100 stamps/s.
//!
//! `B` starts at [`ClockConfig::bw_start_hz`], holds for [`ClockConfig::bw_hold_s`] and
//! then falls as `1/t` down to [`ClockConfig::bw_final_hz`] (1 Hz → 0.1 Hz in 10 s by
//! default). After a start or a step the phase gain is `max(b, 1/k)` for the `k`-th
//! stamp: a running mean of the offset, so one noisy stamp does not set the phase.
//!
//! # Discontinuities
//!
//! - **Jump** (lost frames): `|e|` above [`ClockConfig::jump_threshold_s`] on
//!   [`ClockConfig::jump_count`] consecutive stamps that agree (same sign, each within
//!   `max(jump_threshold_s, 6·RMS of e)` of the mean of the ones before), or on the
//!   first stamp after [`ClockMap::note_xrun`]. Action: `t0 += mean(e)` over the stamps
//!   that agree, `p` kept, counted, a [`ClockStep`] is returned. Stamps above the
//!   threshold never enter the loop.
//! - **Outlier**: a large `e` that does not repeat is ignored and counted. A stamp that
//!   does not agree with the pending ones makes them outliers and starts a new run, so
//!   an outlier next to a real jump does not enter the size of the step.
//! - **Bad stamps**: every [`ClockConfig::lock_window_s`] a least-squares line through
//!   the accepted stamps of the window gives the apparent rate of the stream against the
//!   host clock. More than [`ClockConfig::max_drift_ppm`] away from nominal (beyond the
//!   statistical error of the fit), a window where most stamps were rejected, or
//!   [`ClockConfig::storm_steps`] jumps without a clean window in between (the silent
//!   realignments of the first stamps count too): the stream
//!   first falls back from device stamps to callback stamps (a [`ClockStep`] with
//!   [`StepCause::Fallback`]); if that fails too the map is [`ClockQuality::Degraded`]
//!   and uses the nominal rate ratio. A later clean window clears `Degraded`.
//!
//! # A notice that arrives late
//!
//! A backend may report an xrun after the first stamp that follows it has already been
//! pushed (the cpal backend delivers errors from another thread, a few milliseconds
//! later). For that case [`ClockMap::note_xrun_at`] takes the frame counter of the stream
//! at the xrun. The map keeps the state of each stream before each of its last
//! [`UNDO_STAMPS`] stamps; it goes back to the state before the first stamp at or after
//! that frame, applies the notice there and feeds the stamps again. The result is the
//! one an in-time notice would have given: the step is reported at the first stamp after
//! the xrun and that stamp never enters the loop. Without a late notice the history
//! changes nothing.
//!
//! - Steps found again while feeding (another discontinuity later in the same stamps,
//!   already reported) are not reported twice: only steps at a frame that had none are
//!   new. A step that already sits on the stamp at the boundary is this xrun, found
//!   without the notice: the notice is only counted.
//! - The history is finite. When the stamp at the boundary has left it, the notice is
//!   **not** moved to a later stamp: [`ClockMap::note_xrun_at`] says so
//!   ([`LateNotice::Unavailable`]), and the caller must treat everything from the
//!   boundary to the last stamp pushed as not judged. The notice is armed for the next
//!   stamp, so what is left of the loss re-anchors the line there.
//!
//! # Host time
//!
//! A [`ClockSnapshot`] also carries the host time of the first stamp of the epoch, so it
//! converts between frames and readings of the host clock
//! ([`ClockSnapshot::out_frame_at`], [`ClockSnapshot::host_ns_of_out`]).
//!
//! Nothing here allocates after construction.

use std::collections::VecDeque;
use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

use crate::types::{OutFrame, Side, Stamp};

/// Stamps accepted after a (re)start during which a jump realigns silently.
const STARTUP_STAMPS: u64 = 8;
/// Accepted stamps a window needs before it is evaluated.
const MIN_WINDOW_STAMPS: u32 = 8;
/// The frame period is kept within this factor of nominal.
const P_LIMIT: f64 = 1.25;
/// Stamps per stream that [`ClockMap::note_xrun_at`] can go back over (85 ms of 64-frame
/// blocks at 48 kHz, 0.64 s of 10 ms blocks).
pub const UNDO_STAMPS: usize = 64;

/// What [`ClockMap::note_xrun_at`] did with a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LateNotice {
    /// No stamp at or after the boundary was pushed yet (or the map is broken): armed
    /// for the next stamp, like [`ClockMap::note_xrun`].
    Armed,
    /// The stream went back to the stamp at the boundary and fed the stamps again. Every
    /// new step went to `on_step`.
    Replayed,
    /// A step already sits on the stamp at the boundary: the discontinuity was found
    /// without the notice. Only counted.
    Found,
    /// The stamp at the boundary is older than the history ([`UNDO_STAMPS`]): the xrun
    /// cannot be taken out of the loop any more and its exact place is not known to the
    /// map. Nothing was moved; the notice is armed for the next stamp (re-anchoring the
    /// line there; a step it makes goes to the normal pushes). The caller must treat the
    /// span from the boundary to the end of the last stamp pushed as not judged.
    Unavailable,
}

/// Trust level of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockQuality {
    /// Not enough clean stamps yet (also right after a reset, a fallback or a silence).
    Acquiring,
    /// Both streams track: mean error of the last window below the lock limit.
    Locked,
    /// Stamps are not usable; the map uses the nominal rate ratio. Rhythm must not be judged.
    Degraded,
    /// A frame counter went backwards or ran out of range. Only [`ClockMap::reset`]
    /// recovers.
    Broken,
}

/// Which time of the stamp feeds the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampSource {
    /// `Stamp::device_ns` (capture or playback time).
    Device,
    /// `Stamp::callback_ns` (fallback when the device stamps are bad).
    Callback,
}

/// Why a [`ClockStep`] happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepCause {
    /// Found by the jump rule (consecutive large errors of the same sign).
    Detected,
    /// Announced by [`ClockMap::note_xrun`] and confirmed by the next stamps.
    Notice,
    /// The stream switched to callback stamps and re-acquires; `step_s` is 0.
    Fallback,
}

/// A discontinuity of one stream's line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockStep {
    /// Stream that jumped.
    pub side: Side,
    /// Epoch of the map when it happened.
    pub epoch: u32,
    /// Frame index of that stream where the jump was first seen.
    pub frame: u64,
    /// Size in seconds. Positive: device time ran ahead of the frame count (frames lost).
    pub step_s: f64,
    /// Size in nominal frames of that stream (`step_s · fs`).
    pub frames: f64,
    /// Why.
    pub cause: StepCause,
}

/// Tuning of [`ClockMap`]. `Default` is the M2 plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockConfig {
    /// Loop natural frequency while acquiring, Hz.
    pub bw_start_hz: f64,
    /// Loop natural frequency at rest, Hz.
    pub bw_final_hz: f64,
    /// Seconds at `bw_start_hz` before the `1/t` descent.
    pub bw_hold_s: f64,
    /// Longest interval between two accepted stamps that the loop filters, seconds. A
    /// stamp after a longer silence re-seats the phase (re-averaged over the next
    /// stamps), keeps the period and drops `Locked` until the next clean window. The
    /// loop gains themselves are never capped: that is not stable. Not positive: no limit.
    pub gain_cap_s: f64,
    /// `false` disables the jump and outlier rule: every stamp enters the loop.
    /// For negative controls only.
    pub jump_rule: bool,
    /// `|e|` above this is not fed to the loop, seconds.
    pub jump_threshold_s: f64,
    /// Consecutive same-sign large errors that make a jump.
    pub jump_count: u32,
    /// After [`ClockMap::note_xrun`], the smallest `|e|` treated as a jump, seconds
    /// (raised to 4× the RMS error of the last window, up to `jump_threshold_s`).
    pub notice_threshold_s: f64,
    /// Length of the evaluation window, seconds.
    pub lock_window_s: f64,
    /// Locked when the mean of `e` over a window is below this, seconds.
    pub lock_mean_s: f64,
    /// Largest believable rate error of a stream against the host clock, ppm.
    pub max_drift_ppm: f64,
    /// Jumps without a clean window in between that count as bad stamps.
    pub storm_steps: u32,
    /// Try callback stamps before giving up (`Degraded`).
    pub callback_fallback: bool,
    /// Added to the input stamps, ns. Leave 0 for the backends of this crate: they add
    /// `StreamInfo::origin_offset_ns` to the input stamps themselves. Set it only for a
    /// source that delivers input stamps on a clock with another origin.
    pub origin_offset_ns: i64,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            bw_start_hz: 1.0,
            bw_final_hz: 0.1,
            bw_hold_s: 1.0,
            gain_cap_s: 0.5,
            jump_rule: true,
            jump_threshold_s: 3.0e-3,
            jump_count: 3,
            notice_threshold_s: 0.25e-3,
            lock_window_s: 2.0,
            lock_mean_s: 0.2e-3,
            max_drift_ppm: 1000.0,
            storm_steps: 3,
            callback_fallback: true,
            origin_offset_ns: 0,
        }
    }
}

impl ClockConfig {
    /// Loop natural frequency `t` seconds after a stream (re)started.
    pub fn bandwidth_at(&self, t: f64) -> f64 {
        let lo = self.bw_final_hz.min(self.bw_start_hz);
        let hi = self.bw_final_hz.max(self.bw_start_hz);
        if t <= self.bw_hold_s || t <= 0.0 {
            return self.bw_start_hz;
        }
        (self.bw_start_hz * self.bw_hold_s.max(0.0) / t).clamp(lo, hi)
    }
}

/// Loop gains `(b, c)` for one update over `omega = 2π·B·Δ/fs` radians: the pole-matched
/// form of Adriaensen's `(√2·ω, ω²)`. See the module documentation.
pub fn dll_gains(omega: f64) -> (f64, f64) {
    let b = -(-SQRT_2 * omega).exp_m1();
    let c = if omega < 1.0e-2 {
        // Series of the closed form: avoids the cancellation of the O(1) terms.
        // c = ω² − ω³/√2 + ω⁴/4 − ω⁵/(12·√2) + O(ω⁶)
        let w = omega;
        w * w * (1.0 - w * (FRAC_1_SQRT_2 - w * (0.25 - w * FRAC_1_SQRT_2 / 12.0)))
    } else {
        let u = omega * FRAC_1_SQRT_2;
        1.0 + (-2.0 * u).exp() - 2.0 * (-u).exp() * u.cos()
    };
    (b, c)
}

/// Per-stream numbers for reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockStreamStats {
    /// Stamps received.
    pub stamps: u64,
    /// Large errors that did not repeat (ignored).
    pub outliers: u64,
    /// Jumps applied (not counting the silent start-up realignment).
    pub steps: u64,
    /// Calls to [`ClockMap::note_xrun`] for this side.
    pub xrun_notices: u64,
    /// Windows evaluated.
    pub windows: u64,
    /// Switches from device to callback stamps (0 or 1 per epoch).
    pub fallbacks: u64,
    /// Stamps that followed a silence longer than [`ClockConfig::gain_cap_s`].
    pub gaps: u64,
    /// Mean of `e` over the last evaluated window, seconds.
    pub mean_e_s: f64,
    /// RMS of `e` over the last evaluated window, seconds.
    pub rms_e_s: f64,
    /// Apparent rate error against the host clock over the last window, ppm.
    pub slope_ppm: f64,
    /// Current loop natural frequency, Hz.
    pub bandwidth_hz: f64,
    /// Stamp time in use.
    pub source: StampSource,
    /// Last window met the lock criterion.
    pub locked: bool,
    /// Stamps unusable (drives [`ClockQuality::Degraded`]).
    pub degraded: bool,
}

#[derive(Clone, Copy, Default)]
struct Window {
    n0: u64,
    t0: f64,
    seen: u32,
    cnt: u32,
    se: f64,
    see: f64,
    sn: f64,
    st: f64,
    snn: f64,
    snt: f64,
}

impl Window {
    fn at(n0: u64, t0: f64) -> Self {
        Self {
            n0,
            t0,
            ..Self::default()
        }
    }
}

enum Verdict {
    /// Too few stamps to say anything.
    Unknown,
    Good,
    Bad,
}

type RawStep = (u64, f64, StepCause);

#[derive(Clone, Copy)]
struct StreamClock {
    fs: f64,
    source: StampSource,
    started: bool,
    n0: u64,
    t0: f64,
    p: f64,
    n_start: u64,
    accepted: u64,
    reacq: u32,
    bw: f64,
    pend: u32,
    pend_pos: bool,
    pend_sum: f64,
    pend_frame: u64,
    notice_ttl: u32,
    storm: u32,
    win: Window,
    locked: bool,
    degraded: bool,
    mean_e: f64,
    rms_e: f64,
    slope_ppm: f64,
    stamps: u64,
    outliers: u64,
    steps: u64,
    notices: u64,
    windows: u64,
    fallbacks: u64,
    gaps: u64,
    seen_any: bool,
    next_first: u64,
}

impl StreamClock {
    fn new(fs: f64, bw: f64) -> Self {
        Self {
            fs,
            source: StampSource::Device,
            started: false,
            n0: 0,
            t0: 0.0,
            p: 1.0 / fs,
            n_start: 0,
            accepted: 0,
            reacq: 0,
            bw,
            pend: 0,
            pend_pos: false,
            pend_sum: 0.0,
            pend_frame: 0,
            notice_ttl: 0,
            storm: 0,
            win: Window::default(),
            locked: false,
            degraded: false,
            mean_e: 0.0,
            rms_e: 0.0,
            slope_ppm: 0.0,
            stamps: 0,
            outliers: 0,
            steps: 0,
            notices: 0,
            windows: 0,
            fallbacks: 0,
            gaps: 0,
            seen_any: false,
            next_first: 0,
        }
    }

    fn start(&mut self, n: u64, tau: f64, cfg: &ClockConfig) {
        self.started = true;
        self.n0 = n;
        self.t0 = tau;
        self.p = 1.0 / self.fs;
        self.n_start = n;
        self.accepted = 0;
        self.reacq = 1;
        self.bw = cfg.bw_start_hz;
        self.pend = 0;
        self.win = Window::at(n, tau);
    }

    /// Forgets the line; the next stamp starts a new acquisition.
    fn restart(&mut self) {
        self.started = false;
        self.locked = false;
        self.pend = 0;
        self.notice_ttl = 0;
        self.storm = 0;
    }

    fn push(&mut self, n: u64, tau: f64, cfg: &ClockConfig) -> Option<RawStep> {
        self.stamps += 1;
        if !self.started {
            self.start(n, tau, cfg);
            return None;
        }
        if n <= self.n0 {
            return None;
        }
        let d = (n - self.n0) as f64;
        let pred = self.t0 + self.p * d;
        let e = tau - pred;
        self.win.seen += 1;
        let notice = self.notice_ttl > 0;
        self.notice_ttl = self.notice_ttl.saturating_sub(1);

        let mut step = None;
        if cfg.jump_rule && e.abs() > cfg.jump_threshold_s {
            // Held: a stamp this far off never enters the loop. It joins the pending run
            // only if it agrees with it; otherwise the run was outliers and a new one
            // starts here, so an outlier next to a real jump stays out of the step.
            let pos = e > 0.0;
            let tol = cfg.jump_threshold_s.max(6.0 * self.rms_e);
            let agrees = self.pend > 0
                && pos == self.pend_pos
                && (e - self.pend_sum / f64::from(self.pend)).abs() <= tol;
            if agrees {
                self.pend += 1;
                self.pend_sum += e;
            } else {
                self.outliers += u64::from(self.pend);
                self.pend = 1;
                self.pend_pos = pos;
                self.pend_sum = e;
                self.pend_frame = n;
            }
            if self.pend >= cfg.jump_count || notice {
                let mean = self.pend_sum / f64::from(self.pend);
                let cause = if notice {
                    StepCause::Notice
                } else {
                    StepCause::Detected
                };
                step = self.jump(n, pred, mean, self.pend, self.pend_frame, cause);
            }
        } else {
            self.outliers += u64::from(self.pend);
            self.pend = 0;
            let floor = (4.0 * self.rms_e)
                .min(cfg.jump_threshold_s)
                .max(cfg.notice_threshold_s);
            if cfg.jump_rule && notice && e.abs() > floor {
                step = self.jump(n, pred, e, 1, n, StepCause::Notice);
            } else if cfg.gain_cap_s > 0.0 && d > cfg.gain_cap_s * self.fs {
                self.resume(n, tau);
            } else {
                self.update(n, d, pred, e, tau, cfg);
            }
        }

        let mut bad = self.storm >= cfg.storm_steps.max(1);
        if (n - self.win.n0) as f64 >= cfg.lock_window_s * self.fs {
            match self.close_window(cfg) {
                Verdict::Good => self.degraded = false,
                Verdict::Bad => bad = true,
                Verdict::Unknown => {}
            }
            self.win = Window::at(self.n0, self.t0);
        }
        if bad {
            self.restart();
            if self.source == StampSource::Device && cfg.callback_fallback {
                self.source = StampSource::Callback;
                self.fallbacks += 1;
                return Some((n, 0.0, StepCause::Fallback));
            }
            self.degraded = true;
        }
        step
    }

    /// Puts the line through `(n, t)`; the period is kept and the phase is re-averaged
    /// over the next stamps (`averaged` stamps are already in `t`).
    fn reseat(&mut self, n: u64, t: f64, averaged: u32) {
        self.t0 = t;
        self.n0 = n;
        self.pend = 0;
        self.reacq = averaged.max(1);
        self.win = Window::at(n, t);
    }

    fn jump(
        &mut self,
        n: u64,
        pred: f64,
        step: f64,
        averaged: u32,
        frame: u64,
        cause: StepCause,
    ) -> Option<RawStep> {
        self.reseat(n, pred + step, averaged);
        self.notice_ttl = 0;
        // Counted before the start-up exemption: stamps that only ever realign are bad
        // stamps, even if no step is reported for them.
        self.storm += 1;
        if self.accepted < STARTUP_STAMPS {
            return None;
        }
        self.steps += 1;
        Some((frame, step, cause))
    }

    /// A stamp within the jump threshold after a silence longer than the loop filters.
    fn resume(&mut self, n: u64, tau: f64) {
        self.reseat(n, tau, 1);
        self.locked = false;
        self.accepted += 1;
        self.gaps += 1;
    }

    fn update(&mut self, n: u64, d: f64, pred: f64, e: f64, tau: f64, cfg: &ClockConfig) {
        self.bw = cfg.bandwidth_at((n - self.n_start) as f64 / self.fs);
        let omega = 2.0 * PI * self.bw * d / self.fs;
        let (mut b, c) = dll_gains(omega);
        if self.reacq > 0 {
            let mean_gain = 1.0 / (f64::from(self.reacq) + 1.0);
            if mean_gain > b {
                b = mean_gain;
                self.reacq += 1;
            } else {
                self.reacq = 0;
            }
        }
        self.t0 = pred + b * e;
        self.p = (self.p + c * e / d).clamp(1.0 / (P_LIMIT * self.fs), P_LIMIT / self.fs);
        self.n0 = n;
        self.accepted += 1;

        let w = &mut self.win;
        let dn = (n - w.n0) as f64;
        let dt = tau - w.t0;
        w.cnt += 1;
        w.se += e;
        w.see += e * e;
        w.sn += dn;
        w.st += dt;
        w.snn += dn * dn;
        w.snt += dn * dt;
    }

    fn close_window(&mut self, cfg: &ClockConfig) -> Verdict {
        let w = self.win;
        self.windows += 1;
        if w.seen < MIN_WINDOW_STAMPS {
            self.locked = false;
            return Verdict::Unknown;
        }
        if w.cnt < MIN_WINDOW_STAMPS || w.cnt * 2 < w.seen {
            self.locked = false;
            return Verdict::Bad;
        }
        let cnt = f64::from(w.cnt);
        self.mean_e = w.se / cnt;
        self.rms_e = (w.see / cnt).sqrt();
        let den = cnt * w.snn - w.sn * w.sn;
        let slope = if den > 0.0 {
            (cnt * w.snt - w.sn * w.st) / den
        } else {
            1.0 / self.fs
        };
        self.slope_ppm = (slope * self.fs - 1.0) * 1.0e6;
        // Standard error of the slope for white residuals of size rms_e.
        let se_ppm = if den > 0.0 {
            self.rms_e / (den / cnt).sqrt() * self.fs * 1.0e6
        } else {
            0.0
        };
        if self.slope_ppm.abs() > cfg.max_drift_ppm + 4.0 * se_ppm {
            self.locked = false;
            return Verdict::Bad;
        }
        self.locked = self.mean_e.abs() < cfg.lock_mean_s;
        self.storm = 0;
        Verdict::Good
    }

    fn stats(&self) -> ClockStreamStats {
        ClockStreamStats {
            stamps: self.stamps,
            outliers: self.outliers,
            steps: self.steps,
            xrun_notices: self.notices,
            windows: self.windows,
            fallbacks: self.fallbacks,
            gaps: self.gaps,
            mean_e_s: self.mean_e,
            rms_e_s: self.rms_e,
            slope_ppm: self.slope_ppm,
            bandwidth_hz: self.bw,
            source: self.source,
            locked: self.started && self.locked,
            degraded: self.degraded,
        }
    }
}

/// One pushed stamp and the stream as it was before it, for a late xrun notice.
#[derive(Clone, Copy)]
struct Undo {
    stamp: Stamp,
    before: StreamClock,
    /// A notice was armed between the stamp before this one and this one (`before` has
    /// it).
    noticed: bool,
    /// The frame of the step the push returned.
    step: Option<u64>,
}

fn side_idx(side: Side) -> usize {
    usize::from(side == Side::Out)
}

/// Learns the two stream lines from the callback stamps. Owned by one thread.
pub struct ClockMap {
    cfg: ClockConfig,
    fs_in: f64,
    fs_out: f64,
    epoch: u32,
    base: Option<i128>,
    broken: bool,
    cin: StreamClock,
    cout: StreamClock,
    /// The last [`UNDO_STAMPS`] stamps of each stream (input, output), oldest first.
    undo: [VecDeque<Undo>; 2],
    /// `first` of the newest stamp that left the history of each stream.
    evicted: [Option<u64>; 2],
    /// A notice was armed on the stream (input, output) since its last stamp.
    noticed: [bool; 2],
}

impl ClockMap {
    /// `fs_in`, `fs_out`: nominal sample rates in Hz.
    pub fn new(fs_in: f64, fs_out: f64, cfg: ClockConfig) -> Self {
        Self {
            cfg,
            fs_in,
            fs_out,
            epoch: 0,
            base: None,
            broken: false,
            cin: StreamClock::new(fs_in, cfg.bw_start_hz),
            cout: StreamClock::new(fs_out, cfg.bw_start_hz),
            undo: [
                VecDeque::with_capacity(UNDO_STAMPS),
                VecDeque::with_capacity(UNDO_STAMPS),
            ],
            evicted: [None; 2],
            noticed: [false; 2],
        }
    }

    fn stream(&self, side: Side) -> &StreamClock {
        match side {
            Side::In => &self.cin,
            Side::Out => &self.cout,
        }
    }

    fn stream_mut(&mut self, side: Side) -> &mut StreamClock {
        match side {
            Side::In => &mut self.cin,
            Side::Out => &mut self.cout,
        }
    }

    /// Feeds the stamp of one input block. Returns a step if the input line jumped.
    pub fn push_in(&mut self, s: Stamp) -> Option<ClockStep> {
        self.push(Side::In, s)
    }

    /// Feeds the stamp of one output block. Returns a step if the output line jumped.
    pub fn push_out(&mut self, s: Stamp) -> Option<ClockStep> {
        self.push(Side::Out, s)
    }

    fn push(&mut self, side: Side, s: Stamp) -> Option<ClockStep> {
        if self.broken || s.frames == 0 {
            return None;
        }
        let before = *self.stream(side);
        let step = self.feed(side, s);
        if self.broken {
            return None;
        }
        let k = side_idx(side);
        let noticed = std::mem::take(&mut self.noticed[k]);
        let undo = &mut self.undo[k];
        if undo.len() >= UNDO_STAMPS
            && let Some(old) = undo.pop_front()
        {
            self.evicted[k] = Some(old.stamp.first);
        }
        undo.push_back(Undo {
            stamp: s,
            before,
            noticed,
            step: step.map(|st| st.frame),
        });
        step
    }

    fn feed(&mut self, side: Side, s: Stamp) -> Option<ClockStep> {
        let (sc, fs, offset) = match side {
            Side::In => (&mut self.cin, self.fs_in, self.cfg.origin_offset_ns),
            Side::Out => (&mut self.cout, self.fs_out, 0),
        };
        let end = s.first.checked_add(u64::from(s.frames));
        let (false, Some(end)) = (sc.seen_any && s.first < sc.next_first, end) else {
            self.broken = true;
            return None;
        };
        sc.seen_any = true;
        sc.next_first = end;
        // Callback time follows the end of an input block and precedes an output block.
        let (n, ns) = match (sc.source, side) {
            (StampSource::Device, _) => (s.first, s.device_ns),
            (StampSource::Callback, Side::In) => (sc.next_first, s.callback_ns),
            (StampSource::Callback, Side::Out) => (s.first, s.callback_ns),
        };
        let abs = i128::from(ns) + i128::from(offset);
        let base = *self.base.get_or_insert(abs);
        let tau = (abs - base) as f64 * 1.0e-9;
        let (frame, step_s, cause) = sc.push(n, tau, &self.cfg)?;
        Some(ClockStep {
            side,
            epoch: self.epoch,
            frame,
            step_s,
            frames: step_s * fs,
            cause,
        })
    }

    /// The backend reported an xrun on `side`: the next stamps confirm a jump at once.
    pub fn note_xrun(&mut self, side: Side) {
        let ttl = self.cfg.jump_count.max(1);
        let sc = self.stream_mut(side);
        sc.notices += 1;
        sc.notice_ttl = ttl;
        self.noticed[side_idx(side)] = true;
    }

    /// [`ClockMap::note_xrun`] for a notice that knows where the xrun was and may come
    /// late. `at` is the frame counter of the stream of `side` when the backend saw the
    /// error: the `first` of the first stamp after the lost frames.
    ///
    /// If that stamp has been pushed already, the stream goes back to its state before
    /// it, the notice is applied there and the stamps pushed since are fed again (see the
    /// module documentation). Every new step this finds goes to `on_step`, in order. A
    /// step found again at the frame where one of those stamps had already reported one
    /// (a later, unrelated discontinuity) is not new and is not passed on.
    ///
    /// - No stamp at or after `at` was pushed yet: the notice is armed for the next stamp,
    ///   as `note_xrun` does ([`LateNotice::Armed`]). A caller that can still see earlier
    ///   stamps coming must hold the notice until the stamp with `first >= at` is next,
    ///   and then call `note_xrun`.
    /// - A step already sits on the stamp at `at`: the discontinuity was found without
    ///   the notice (or the stream started over there). The notice is only counted
    ///   ([`LateNotice::Found`]).
    /// - The stamp at `at` has left the history ([`UNDO_STAMPS`] stamps): nothing is
    ///   moved to a later stamp ([`LateNotice::Unavailable`], which see).
    pub fn note_xrun_at(
        &mut self,
        side: Side,
        at: u64,
        mut on_step: impl FnMut(ClockStep),
    ) -> LateNotice {
        let k = side_idx(side);
        let found = self.undo[k].iter().position(|u| u.stamp.first >= at);
        let Some(i) = found.filter(|_| !self.broken) else {
            self.note_xrun(side);
            return LateNotice::Armed;
        };
        // The stamp before the oldest one kept may be the one at the boundary.
        if i == 0 && self.evicted[k].is_some_and(|first| first >= at) {
            self.note_xrun(side);
            return LateNotice::Unavailable;
        }
        let target = self.undo[k][i].stamp.first;
        if self.undo[k].iter().skip(i).any(|u| u.step == Some(target)) {
            self.stream_mut(side).notices += 1;
            return LateNotice::Found;
        }
        // The stamps to feed again. A notice that was armed after one of them is armed
        // again in the same place (one armed before the first is in the restored state).
        // `old`: frames of the steps they reported, which must not be reported again.
        let mut redo = [None::<(Stamp, bool)>; UNDO_STAMPS];
        let mut old = [None::<u64>; UNDO_STAMPS];
        for (n, ((slot, seen), u)) in redo
            .iter_mut()
            .zip(old.iter_mut())
            .zip(self.undo[k].iter().skip(i))
            .enumerate()
        {
            *slot = Some((u.stamp, n > 0 && u.noticed));
            *seen = u.step;
        }
        let armed_after = self.noticed[k];
        let notices = self.stream(side).notices + 1;
        *self.stream_mut(side) = self.undo[k][i].before;
        self.undo[k].truncate(i);
        self.note_xrun(side);
        for (stamp, armed) in redo.into_iter().flatten() {
            if armed {
                self.note_xrun(side);
            }
            let Some(step) = self.push(side, stamp) else {
                continue;
            };
            match old.iter_mut().find(|f| **f == Some(step.frame)) {
                Some(f) => *f = None,
                None => on_step(step),
            }
        }
        if armed_after {
            self.note_xrun(side);
        }
        self.stream_mut(side).notices = notices;
        LateNotice::Replayed
    }

    /// Current trust level.
    pub fn quality(&self) -> ClockQuality {
        if self.broken {
            ClockQuality::Broken
        } else if self.cin.degraded || self.cout.degraded {
            ClockQuality::Degraded
        } else if self.cin.started && self.cin.locked && self.cout.started && self.cout.locked {
            ClockQuality::Locked
        } else {
            ClockQuality::Acquiring
        }
    }

    /// The map as plain numbers, to send to other threads.
    pub fn snapshot(&self) -> ClockSnapshot {
        let quality = self.quality();
        let nominal = quality == ClockQuality::Degraded;
        ClockSnapshot {
            epoch: self.epoch,
            quality,
            n_in: self.cin.n0 as f64,
            t_in: self.cin.t0,
            p_in: if nominal {
                1.0 / self.fs_in
            } else {
                self.cin.p
            },
            n_out: self.cout.n0 as f64,
            t_out: self.cout.t0,
            p_out: if nominal {
                1.0 / self.fs_out
            } else {
                self.cout.p
            },
            fs_in: self.fs_in,
            fs_out: self.fs_out,
            src_in: self.cin.source,
            src_out: self.cout.source,
            base_ns: self.base,
        }
    }

    /// New epoch (streams reopened): forgets both lines and every counter.
    pub fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.base = None;
        self.broken = false;
        self.cin = StreamClock::new(self.fs_in, self.cfg.bw_start_hz);
        self.cout = StreamClock::new(self.fs_out, self.cfg.bw_start_hz);
        self.undo.iter_mut().for_each(VecDeque::clear);
        self.evicted = [None; 2];
        self.noticed = [false; 2];
    }

    /// Sets the offset added to the input stamps (call before the first stamp of an epoch).
    /// See [`ClockConfig::origin_offset_ns`]: 0 for stamps that are already on one clock.
    pub fn set_origin_offset_ns(&mut self, ns: i64) {
        self.cfg.origin_offset_ns = ns;
    }

    /// Epoch counter: 0 at construction, +1 per [`ClockMap::reset`].
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Configuration in use.
    pub fn config(&self) -> &ClockConfig {
        &self.cfg
    }

    /// Numbers of one stream for reports.
    pub fn stats(&self, side: Side) -> ClockStreamStats {
        match side {
            Side::In => self.cin.stats(),
            Side::Out => self.cout.stats(),
        }
    }
}

/// A copy of the map at one instant. Times are seconds on the host clock, counted from
/// the first stamp of the epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockSnapshot {
    epoch: u32,
    quality: ClockQuality,
    n_in: f64,
    t_in: f64,
    p_in: f64,
    n_out: f64,
    t_out: f64,
    p_out: f64,
    fs_in: f64,
    fs_out: f64,
    src_in: StampSource,
    src_out: StampSource,
    base_ns: Option<i128>,
}

/// Every number of a [`ClockSnapshot`], to store it and build it again (the tape).
///
/// Each stream line is `T(n) = t + p·(n − n0)`: `T` in seconds since `base_ns`, `n` in
/// frames of that stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockParts {
    /// Epoch of the map.
    pub epoch: u32,
    /// Trust level.
    pub quality: ClockQuality,
    /// Input line: frame of its last accepted stamp (`n0`).
    pub n_in: f64,
    /// Input line: filtered time of `n_in`, seconds since `base_ns`.
    pub t_in: f64,
    /// Input line: seconds per frame.
    pub p_in: f64,
    /// Output line: frame of its last accepted stamp (`n0`).
    pub n_out: f64,
    /// Output line: filtered time of `n_out`, seconds since `base_ns`.
    pub t_out: f64,
    /// Output line: seconds per frame.
    pub p_out: f64,
    /// Nominal input rate, Hz.
    pub fs_in: f64,
    /// Nominal output rate, Hz.
    pub fs_out: f64,
    /// Stamps that feed the input line.
    pub src_in: StampSource,
    /// Stamps that feed the output line.
    pub src_out: StampSource,
    /// Host clock reading of time zero of the lines, ns: the first stamp of the epoch.
    /// `None` before any stamp.
    pub base_ns: Option<i128>,
}

impl ClockSnapshot {
    /// Every number of the snapshot.
    pub fn parts(&self) -> ClockParts {
        ClockParts {
            epoch: self.epoch,
            quality: self.quality,
            n_in: self.n_in,
            t_in: self.t_in,
            p_in: self.p_in,
            n_out: self.n_out,
            t_out: self.t_out,
            p_out: self.p_out,
            fs_in: self.fs_in,
            fs_out: self.fs_out,
            src_in: self.src_in,
            src_out: self.src_out,
            base_ns: self.base_ns,
        }
    }

    /// The snapshot with these numbers. `from_parts(s.parts()) == s`. Nothing is checked:
    /// a zero period gives infinite frames, as it would in the arithmetic of any line.
    pub fn from_parts(p: ClockParts) -> Self {
        Self {
            epoch: p.epoch,
            quality: p.quality,
            n_in: p.n_in,
            t_in: p.t_in,
            p_in: p.p_in,
            n_out: p.n_out,
            t_out: p.t_out,
            p_out: p.p_out,
            fs_in: p.fs_in,
            fs_out: p.fs_out,
            src_in: p.src_in,
            src_out: p.src_out,
            base_ns: p.base_ns,
        }
    }

    /// Seconds from the base of the lines to host time `host_ns`.
    fn host_s(&self, host_ns: u64) -> Option<f64> {
        self.base_ns
            .map(|base| (i128::from(host_ns) - base) as f64 * 1.0e-9)
    }

    /// Output frame at host time `host_ns` (a reading of `HostClock::host_now_ns`), by the
    /// output line. `None` before the first stamp of the epoch.
    ///
    /// What the frame is depends on the stamps of the output line
    /// ([`ClockSnapshot::source`]): with [`StampSource::Device`] it is the frame the
    /// system says is playing at that instant; with [`StampSource::Callback`] it is the
    /// frame being requested then, ahead of the sound by the output latency, which is not
    /// known. Like every reading of the map it is only as good as
    /// [`ClockSnapshot::quality`]. For drawing; judging compares frames, not host times.
    pub fn out_frame_at(&self, host_ns: u64) -> Option<f64> {
        let t = self.host_s(host_ns)?;
        Some(self.n_out + (t - self.t_out) / self.p_out)
    }

    /// Input frame at host time `host_ns`, by the input line. `None` before the first
    /// stamp of the epoch. With [`StampSource::Device`] it is the frame being captured at
    /// that instant; with [`StampSource::Callback`], the frames delivered by then.
    pub fn in_frame_at(&self, host_ns: u64) -> Option<f64> {
        let t = self.host_s(host_ns)?;
        Some(self.n_in + (t - self.t_in) / self.p_in)
    }

    /// Host time of output frame `f`, in ns: the inverse of
    /// [`ClockSnapshot::out_frame_at`]. `None` before the first stamp of the epoch.
    pub fn host_ns_of_out(&self, f: f64) -> Option<f64> {
        self.base_ns
            .map(|base| base as f64 + self.out_time_s(f) * 1.0e9)
    }

    /// Output frame of the last stamp the output line accepted: how far the output had
    /// come when the snapshot was taken.
    pub fn last_out_frame(&self) -> f64 {
        self.n_out
    }

    /// Input frame of the last stamp the input line accepted: how far the input had come
    /// when the snapshot was taken.
    pub fn last_in_frame(&self) -> f64 {
        self.n_in
    }

    /// Seconds from the last accepted output stamp to host time `host_ns`.
    pub(crate) fn out_age_s(&self, host_ns: u64) -> Option<f64> {
        Some(self.host_s(host_ns)? - self.t_out)
    }

    /// Device time of output frame `f`, seconds.
    pub fn out_time_s(&self, f: f64) -> f64 {
        self.t_out + self.p_out * (f - self.n_out)
    }

    /// Device time of input frame `g`, seconds.
    pub fn in_time_s(&self, g: f64) -> f64 {
        self.t_in + self.p_in * (g - self.n_in)
    }

    /// Input frame captured at the instant output frame `f` plays.
    pub fn in_of_out(&self, f: f64) -> f64 {
        self.n_in + (self.out_time_s(f) - self.t_in) / self.p_in
    }

    /// Output frame playing at the instant input frame `g` is captured.
    pub fn out_of_in(&self, g: f64) -> f64 {
        self.n_out + (self.in_time_s(g) - self.t_out) / self.p_out
    }

    /// Rate of the input device relative to the output device, minus one, in ppm:
    /// `(p_out·fs_out)/(p_in·fs_in) − 1`. Positive: the input clock runs fast.
    pub fn drift_ppm(&self) -> f64 {
        ((self.p_out * self.fs_out) / (self.p_in * self.fs_in) - 1.0) * 1.0e6
    }

    /// Trust level when the snapshot was taken. Only `Locked` is fit for judging.
    pub fn quality(&self) -> ClockQuality {
        self.quality
    }

    /// Epoch of the map when the snapshot was taken.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Which stamps fed the line of `side` when the snapshot was taken. A latency measured
    /// on one source does not hold on the other: `in_of_out` differs by a constant (the
    /// time between a callback and its sound).
    pub fn source(&self, side: Side) -> StampSource {
        match side {
            Side::In => self.src_in,
            Side::Out => self.src_out,
        }
    }
}

/// Chart seconds ↔ output frames. The output frame counter is the master clock, so this
/// conversion has no estimation error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartClock {
    /// Output frame of chart time zero.
    pub anchor: OutFrame,
    /// Nominal output sample rate, Hz.
    pub fs_out: f64,
}

impl ChartClock {
    /// Chart time zero at output frame `anchor`.
    pub fn new(anchor: OutFrame, fs_out: f64) -> Self {
        Self { anchor, fs_out }
    }

    /// Chart time of output frame `f`, seconds (negative before the anchor).
    pub fn seconds_of_frame(&self, f: f64) -> f64 {
        (f - self.anchor as f64) / self.fs_out
    }

    /// Output frame of chart time `t` seconds.
    pub fn frame_of_seconds(&self, t: f64) -> f64 {
        self.anchor as f64 + t * self.fs_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poles of the frozen loop `z² + (b + c − 2)·z + (1 − b)`: largest modulus.
    fn pole_radius(b: f64, c: f64) -> f64 {
        let (a1, a0) = (b + c - 2.0, 1.0 - b);
        let disc = a1 * a1 - 4.0 * a0;
        if disc >= 0.0 {
            let r = disc.sqrt();
            ((-a1 + r) / 2.0).abs().max(((-a1 - r) / 2.0).abs())
        } else {
            a0.sqrt()
        }
    }

    #[test]
    fn gains_tend_to_adriaensen_for_small_omega() {
        for omega in [1.0e-6, 1.0e-4, 1.0e-3, 5.0e-3] {
            let (b, c) = dll_gains(omega);
            assert!((b / (SQRT_2 * omega) - 1.0).abs() < 1.0e-2, "b at {omega}");
            assert!((c / (omega * omega) - 1.0).abs() < 1.0e-2, "c at {omega}");
        }
    }

    #[test]
    fn series_and_closed_form_agree_at_the_switch() {
        let below = dll_gains(1.0e-2 * (1.0 - 1.0e-12)).1;
        let above = dll_gains(1.0e-2).1;
        assert!((below / above - 1.0).abs() < 1.0e-8, "{below} vs {above}");
    }

    #[test]
    fn poles_match_the_continuous_loop_for_any_block() {
        // Continuous poles ωn·(−1 ± i)/√2 sampled over the block: modulus exp(−ω/√2),
        // angle ω/√2, for every ω. Hence stable for every Δ.
        for omega in [1.0e-3, 0.0628, 0.5, 1.0, 1.2, 2.0, 5.0, 20.0, 200.0] {
            let (b, c) = dll_gains(omega);
            let u = omega * FRAC_1_SQRT_2;
            let sum = 2.0 - b - c; // z1 + z2
            let prod = 1.0 - b; // z1·z2
            assert!(
                (prod - (-2.0 * u).exp()).abs() < 1.0e-12,
                "product at {omega}"
            );
            assert!(
                (sum - 2.0 * (-u).exp() * u.cos()).abs() < 1.0e-9,
                "sum at {omega}"
            );
            assert!(pole_radius(b, c) < 1.0, "unstable at {omega}");
        }
    }

    #[test]
    fn the_plan_formula_is_unstable_for_long_blocks() {
        // b = √2·ω, c = ω² with the new period used in the next prediction:
        // stable only below ω = √6 − √2.
        let limit = 6.0f64.sqrt() - SQRT_2;
        let plan = |w: f64| pole_radius(SQRT_2 * w, w * w);
        assert!(plan(limit - 0.01) < 1.0);
        assert!(plan(limit + 0.01) > 1.0);
        assert!(plan(1.2) > 1.0);
        let (b, c) = dll_gains(1.2);
        assert!(pole_radius(b, c) < 1.0);
    }

    /// Largest modulus of the eigenvalues of a 2×2 matrix.
    fn radius(m: [[f64; 2]; 2]) -> f64 {
        let (tr, det) = (m[0][0] + m[1][1], m[0][0] * m[1][1] - m[0][1] * m[1][0]);
        let disc = tr * tr - 4.0 * det;
        if disc >= 0.0 {
            let r = disc.sqrt();
            ((tr + r) / 2.0).abs().max(((tr - r) / 2.0).abs())
        } else {
            det.sqrt()
        }
    }

    fn mul(a: [[f64; 2]; 2], b: [[f64; 2]; 2]) -> [[f64; 2]; 2] {
        let f = |i: usize, j: usize| a[i][0] * b[0][j] + a[i][1] * b[1][j];
        [[f(0, 0), f(0, 1)], [f(1, 0), f(1, 1)]]
    }

    /// One update on the error state (phase error, period error): the line is
    /// extrapolated over `dt`, then corrected with gains `(b, c)`.
    fn update_matrix((b, c): (f64, f64), dt: f64) -> [[f64; 2]; 2] {
        [[1.0 - b, (1.0 - b) * dt], [-c / dt, 1.0 - c]]
    }

    #[test]
    fn any_sequence_of_intervals_contracts_one_quadratic_form() {
        // In units of 1/ωn the update depends only on ω = ωn·Δ. V(x) = xᵀ·P·x with
        // P = [[1/√2, −1/2], [−1/2, √2]] is the Lyapunov function of the continuous loop
        // (AᵀP + PA = −I for A = [[−√2, 1], [−1, 0]]). Every update decreases it, by at
        // least 0.1·min(ω, 1) of the unit form: no sequence of intervals makes the
        // error grow at a fixed bandwidth.
        let p = [[FRAC_1_SQRT_2, -0.5], [-0.5, SQRT_2]];
        let mut weakest = f64::MAX;
        for k in 0..=7000 {
            let omega = 10f64.powf(-4.0 + f64::from(k) * 1.0e-3); // 1e-4 … 1e3
            let m = update_matrix(dll_gains(omega), omega);
            let mt = [[m[0][0], m[1][0]], [m[0][1], m[1][1]]];
            let d = mul(mul(mt, p), m);
            let d = [
                [d[0][0] - p[0][0], d[0][1] - p[0][1]],
                [d[1][0] - p[1][0], d[1][1] - p[1][1]],
            ];
            assert!((d[0][1] - d[1][0]).abs() < 1.0e-12);
            // Largest eigenvalue of the symmetric difference.
            let (tr, det) = (d[0][0] + d[1][1], d[0][0] * d[1][1] - d[0][1] * d[1][0]);
            let top = tr / 2.0 + (tr * tr / 4.0 - det).max(0.0).sqrt();
            let margin = -top / omega.min(1.0);
            assert!(margin > 0.1, "ω = {omega}: V changes by up to {top}");
            weakest = weakest.min(margin);
        }
        println!("weakest contraction: {weakest:.4}·min(ω, 1)");
    }

    #[test]
    fn gains_of_another_interval_can_be_unstable() {
        // Why ω is never capped. At 0.1 Hz, stamps alternately 0.1 s and 120 s apart:
        // with the gains of a 0.1 s interval used for both, each update is stable on its
        // own but the pair multiplies the error by 1.87. With the gains of the real
        // intervals the pair is stable.
        let wn = 2.0 * PI * 0.1;
        let (short, long) = (0.1, 120.0);
        let capped = dll_gains(wn * short);
        assert!(pole_radius(capped.0, capped.1) < 0.96);
        let pair = mul(update_matrix(capped, long), update_matrix(capped, short));
        assert!((radius(pair) - 1.8686).abs() < 1.0e-3, "{}", radius(pair));
        let pair = mul(
            update_matrix(dll_gains(wn * long), long),
            update_matrix(dll_gains(wn * short), short),
        );
        assert!(radius(pair) < 0.96, "{}", radius(pair));
    }

    #[test]
    fn damping_is_one_over_root_two() {
        // For small ω the discrete poles are 1 + s·Δt with s = ωn·(−ζ ± i·√(1−ζ²)).
        let omega = 1.0e-3;
        let (b, c) = dll_gains(omega);
        let (a1, a0) = (b + c - 2.0, 1.0 - b);
        let re = -a1 / 2.0 - 1.0; // real part of z − 1
        let im = (a0 - (a1 / 2.0) * (a1 / 2.0)).sqrt();
        let zeta = -re / (re * re + im * im).sqrt();
        assert!((zeta - FRAC_1_SQRT_2).abs() < 1.0e-3, "zeta {zeta}");
    }

    #[test]
    fn bandwidth_schedule() {
        let cfg = ClockConfig::default();
        assert_eq!(cfg.bandwidth_at(0.0), 1.0);
        assert_eq!(cfg.bandwidth_at(1.0), 1.0);
        assert!((cfg.bandwidth_at(2.0) - 0.5).abs() < 1.0e-12);
        assert!((cfg.bandwidth_at(5.0) - 0.2).abs() < 1.0e-12);
        assert_eq!(cfg.bandwidth_at(10.0), 0.1);
        assert_eq!(cfg.bandwidth_at(3600.0), 0.1);
        // Monotonic, never outside [final, start].
        let mut last = f64::INFINITY;
        for i in 0..2000 {
            let b = cfg.bandwidth_at(f64::from(i) * 0.01);
            assert!(b <= last && (0.1..=1.0).contains(&b));
            last = b;
        }
    }

    #[test]
    fn chart_clock_round_trip() {
        let c = ChartClock::new(48_000, 48_000.0);
        assert_eq!(c.seconds_of_frame(48_000.0), 0.0);
        assert_eq!(c.seconds_of_frame(0.0), -1.0);
        assert_eq!(c.frame_of_seconds(2.5), 168_000.0);
        assert_eq!(c.seconds_of_frame(c.frame_of_seconds(12.345)), 12.345);
    }

    #[test]
    fn empty_map_is_the_nominal_ratio() {
        let m = ClockMap::new(44_100.0, 48_000.0, ClockConfig::default());
        let s = m.snapshot();
        assert_eq!(s.quality(), ClockQuality::Acquiring);
        assert!((s.in_of_out(48_000.0) - 44_100.0).abs() < 1.0e-6);
        assert!(s.drift_ppm().abs() < 1.0e-9);
    }
}
