//! Note tracker: faithful port of `makeTracker` (legacy/web-demo/src/tracker.js).
//! Turns detector frames into game events through two paths:
//! - `Target`: the expected note appeared right after an attack (fast, tolerant).
//! - `Mpm`: a single clean note is sounding (free play, tuner, "what was played").
//!
//! Thresholds count frames of the demo's ~10.7 ms hop. They are kept as frame counts here
//! for parity; [`TrackerParams::for_hop`] converts them when the hop changes.

use crate::frontend::{Frame, MAX_TARGETS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Target,
    Mpm,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Attack {
        t: f64,
        level: f64,
    },
    Note {
        t: f64,
        midi: i32,
        cents: f64,
        attack: bool,
        via: Via,
    },
}

impl Event {
    pub fn t(&self) -> f64 {
        match *self {
            Event::Attack { t, .. } | Event::Note { t, .. } => t,
        }
    }
}

/// Frame-count thresholds (demo values are for a 10.7 ms hop).
#[derive(Clone, Copy, Debug)]
pub struct TrackerParams {
    /// Silent frames before the current note is released (demo: 8).
    pub silent_release: u32,
    /// Frames of a new pitch without attack to accept a legato change (demo: 4).
    pub legato_run: u32,
    /// Frames of agreement to accept a note (demo: 2).
    pub confirm_run: u32,
}

impl TrackerParams {
    pub const DEMO_HOP_S: f64 = 257.0 / 24000.0;

    pub fn demo() -> Self {
        Self {
            silent_release: 8,
            legato_run: 4,
            confirm_run: 2,
        }
    }

    /// Keep the same durations in seconds for a different hop.
    pub fn for_hop(hop_s: f64) -> Self {
        let conv = |n: u32| ((n as f64 * Self::DEMO_HOP_S / hop_s).round() as u32).max(1);
        let d = Self::demo();
        Self {
            silent_release: conv(d.silent_release),
            legato_run: conv(d.legato_run),
            confirm_run: conv(d.confirm_run),
        }
    }
}

const C_ON: f64 = 0.86;
const P_ON: f64 = 0.55;
const P_FAST: f64 = 0.85;

pub struct Tracker {
    prm: TrackerParams,
    nf: f64,
    cur: Option<i32>,
    cand: Option<i32>,
    run: u32,
    silent: u32,
    e0: f64,
    e1: f64,
    e2: f64,
    onset_t: f64,
    fresh: bool,
    had_note: bool,
    rms_pre: f64,
    last_rms: f64,
    targets: [i32; MAX_TARGETS],
    n_targets: usize,
    tid: u32,
    shown_t: f64,
    t_run: [u32; MAX_TARGETS],
    t_pre: [f64; MAX_TARGETS],
    h0: [f64; MAX_TARGETS],
    h1: [f64; MAX_TARGETS],
    h2: [f64; MAX_TARGETS],
    cents: f64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(TrackerParams::demo())
    }
}

impl Tracker {
    pub fn new(prm: TrackerParams) -> Self {
        Self {
            prm,
            nf: 0.003,
            cur: None,
            cand: None,
            run: 0,
            silent: 0,
            e0: 0.0,
            e1: 0.0,
            e2: 0.0,
            onset_t: -9.0,
            fresh: false,
            had_note: false,
            rms_pre: 0.0,
            last_rms: 0.0,
            targets: [0; MAX_TARGETS],
            n_targets: 0,
            tid: 0,
            shown_t: 0.0,
            t_run: [0; MAX_TARGETS],
            t_pre: [1.0; MAX_TARGETS],
            h0: [1.0; MAX_TARGETS],
            h1: [1.0; MAX_TARGETS],
            h2: [1.0; MAX_TARGETS],
            cents: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.prm);
    }

    /// Noise gate derived from the adaptive noise floor.
    pub fn gate(&self) -> f64 {
        (self.nf * 4.0).max(0.002)
    }
    pub fn current(&self) -> Option<i32> {
        self.cur
    }
    pub fn cents(&self) -> f64 {
        self.cents
    }
    pub fn noise_floor(&self) -> f64 {
        self.nf
    }

    pub fn set_targets(&mut self, midis: &[i32], tid: u32, shown_t: f64) {
        let n = midis.len().min(MAX_TARGETS);
        self.targets[..n].copy_from_slice(&midis[..n]);
        self.n_targets = n;
        self.tid = tid;
        self.shown_t = shown_t;
        self.t_run = [0; MAX_TARGETS];
        self.t_pre = [1.0; MAX_TARGETS];
        self.h0 = [1.0; MAX_TARGETS];
        self.h1 = [1.0; MAX_TARGETS];
        self.h2 = [1.0; MAX_TARGETS];
    }

    pub fn push(&mut self, fr: &Frame, mut emit: impl FnMut(Event)) {
        let (t, f, c, rms, rms_s) = (fr.t, fr.f, fr.c, fr.rms, fr.rms_s);

        // noise floor: falls fast in silence, rises slowly with non-tonal noise
        if rms < self.nf {
            self.nf += (rms - self.nf) * 0.2;
        } else if c < 0.6 && rms < self.nf * 3.0 {
            self.nf += (rms - self.nf) * 0.02;
        }
        self.nf = self.nf.clamp(0.0004, 0.02);
        let g = self.gate();
        let n_t = if fr.tid == self.tid {
            self.n_targets.min(fr.n_p)
        } else {
            0
        };

        // attack: energy of the last hop against the minimum of the three before
        let mn = self.e0.min(self.e1).min(self.e2);
        if rms_s > g * 1.3 && rms_s > mn * 1.6 + 0.0015 && t - self.onset_t > 0.05 {
            self.onset_t = t;
            self.fresh = true;
            self.had_note = self.cur.is_some();
            self.rms_pre = self.last_rms;
            self.cand = None;
            self.run = 0;
            for k in 0..self.n_targets {
                self.t_pre[k] = self.h0[k].max(self.h1[k]).max(self.h2[k]);
                self.t_run[k] = 0;
            }
            emit(Event::Attack { t, level: rms_s });
        }
        self.e0 = self.e1;
        self.e1 = self.e2;
        self.e2 = rms_s;
        if self.fresh && t - self.onset_t > 0.45 {
            self.fresh = false;
        }

        // "target" path
        let mut best_k: Option<usize> = None;
        let mut best_p = 0.0f64;
        for k in 0..n_t {
            let p = if rms > g { fr.p[k] } else { 0.0 };
            if self.fresh
                && self.onset_t >= self.shown_t - 0.02
                && p >= P_ON
                && (self.t_pre[k] < 0.45 || p > self.t_pre[k] + 0.12)
            {
                self.t_run[k] += 1;
                if (self.t_run[k] >= self.prm.confirm_run || p >= P_FAST) && p > best_p {
                    best_p = p;
                    best_k = Some(k);
                }
            } else {
                self.t_run[k] = 0;
            }
            self.h0[k] = self.h1[k];
            self.h1[k] = self.h2[k];
            self.h2[k] = p;
        }
        if let Some(k) = best_k {
            self.fresh = false;
            let m = self.targets[k];
            self.cur = Some(m);
            self.cand = Some(m);
            self.run = 2;
            self.silent = 0;
            self.cents = 0.0;
            emit(Event::Note {
                t,
                midi: m,
                cents: 0.0,
                attack: true,
                via: Via::Target,
            });
            self.last_rms = rms;
            return;
        }

        // "mpm" path
        let mut n: Option<i32> = None;
        let mut cents = 0.0;
        if rms > g && f > 0.0 && c >= C_ON {
            let mf = 69.0 + 12.0 * (f / 440.0).ln() / std::f64::consts::LN_2;
            let r = (mf + 0.5).floor();
            cents = (mf - r) * 100.0;
            let ri = r as i32;
            if (38..=91).contains(&ri) {
                n = Some(ri);
            }
        }
        // right after an attack over another note the window still mixes both
        if n.is_some() && self.fresh && self.had_note && t - self.onset_t < 0.024 {
            n = None;
        }
        if n == self.cand {
            self.run += 1;
        } else {
            self.cand = n;
            self.run = 1;
        }
        match n {
            None => {
                self.silent += 1;
                if self.silent >= self.prm.silent_release {
                    self.cur = None;
                }
            }
            Some(n) => {
                self.silent = 0;
                if Some(n) == self.cur {
                    self.cents = cents;
                }
                if self.run >= self.prm.confirm_run {
                    if self.fresh {
                        if Some(n) != self.cur {
                            self.cur = Some(n);
                            self.cents = cents;
                            self.fresh = false;
                            emit(Event::Note {
                                t,
                                midi: n,
                                cents,
                                attack: true,
                                via: Via::Mpm,
                            });
                        } else if t - self.onset_t >= 0.06 && rms >= self.rms_pre * 1.12 {
                            self.fresh = false; // same note, played again
                            emit(Event::Note {
                                t,
                                midi: n,
                                cents,
                                attack: true,
                                via: Via::Mpm,
                            });
                        }
                    } else if Some(n) != self.cur && self.run >= self.prm.legato_run {
                        self.cur = Some(n); // changed without attack (legato, slide)
                        self.cents = cents;
                        emit(Event::Note {
                            t,
                            midi: n,
                            cents,
                            attack: false,
                            via: Via::Mpm,
                        });
                    }
                }
            }
        }
        self.last_rms = rms;
    }
}
