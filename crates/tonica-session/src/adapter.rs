//! Engine events to judge inputs (owned by S2).
//!
//! The adapter is a pure function of the chart, the anchor and the events it is given: it
//! reads no clock and does no timing arithmetic of its own. The one number it computes is
//! the input frame of each click, `g_in = in_of_out(f_k)` with `f_k` the output frame of
//! chart event `k`, always from the newest clock snapshot. The judge subtracts the
//! calibration offset; the adapter never does.
//!
//! What reaches the judge, and when:
//!
//! - `Clock`: the snapshot is kept. Events that are due within [`LOOKAHEAD_S`] seconds of
//!   output time are announced (`Expect`), in order, before input time can reach them. After
//!   a `Step`, or when the map's quality changes (`Acquiring` to `Locked`), the open events
//!   are announced again on the next snapshot, so the judge can refine their `g_in` on the
//!   new map (it moves only cells that are not paired yet). Then `Progress` follows, always
//!   after the expectations it may close.
//! - `Onset`: `Heard` (early notice) or `Strum`, with the engine's `judged` flag. An onset
//!   at an unknown time (`t_in` NaN) is passed on too: the judge makes the cells it may
//!   belong to `Glitch`, which a dropped one would turn into a false `NotHeard`.
//! - `Unjudged`: passed on as is, unless a bound is NaN (an infinite bound is the judge's).
//! - Epochs: every event of an epoch below the newest seen (or below the floor the session
//!   sets after a `Rewire`) is stale and changes nothing, a `Clock` included.
//! - `Dropped` on a queue that carries onsets or stamps: the onsets in that stretch may have
//!   been lost, so a conservative window around "now" is `Unjudged`.
//! - `Stopped`, `Reopened`: `Reset`. Every open event ends `NotJudged(Glitch)`.

use tonica_audio::{ChartClock, ClockQuality, ClockSnapshot, EngineEvent, Ring};
use tonica_core::chart::{Chart, EventKind};
use tonica_core::pulse::{self, Expectation};
use tonica_core::verdict::{ExpectKind, JudgeInput};

/// Events are announced this long (output seconds) before they sound.
pub const LOOKAHEAD_S: f64 = 2.0;
/// Events older than this (output seconds) are not announced again after a clock step.
pub const RETIRE_S: f64 = 2.0;
/// A queue overflow drops an unknown stretch of onsets: the window it makes `Unjudged`
/// starts this far before the newest input frame, seconds.
pub const DROP_BEFORE_S: f64 = 1.0;
/// ... and ends this far after it, seconds.
pub const DROP_AFTER_S: f64 = 0.5;

/// The index in `chart.events()` of the event the expectation `id` stands for. Expectation
/// ids count the events that are not rests, so for a chart with rests the two differ.
pub fn event_index_of(chart: &Chart, id: u32) -> Option<usize> {
    chart
        .events()
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.kind, EventKind::Rest))
        .nth(id as usize)
        .map(|(i, _)| i)
}

/// One announced chart.
struct Plan {
    exps: Vec<Expectation>,
    /// Output frame of each expectation (rounded, as the mixer rounds its beats).
    frames: Vec<f64>,
    /// First expectation that may still be announced again.
    live: usize,
    /// First expectation not announced yet.
    next: usize,
}

/// Engine events to judge inputs. See the module documentation.
pub struct Adapter {
    fs_in: f64,
    fs_out: f64,
    epoch: Option<u32>,
    snap: Option<ClockSnapshot>,
    plan: Option<Plan>,
    resend: bool,
    /// Expectations sent (counting each announcement), for the tests.
    sent: u64,
}

impl Adapter {
    /// An adapter with no chart. `fs_in` and `fs_out` are the rates of the streams (the
    /// snapshots carry their own).
    pub fn new(fs_in: f64, fs_out: f64) -> Self {
        Self {
            fs_in,
            fs_out,
            epoch: None,
            snap: None,
            plan: None,
            resend: false,
            sent: 0,
        }
    }

    /// Starts announcing `chart`, whose chart time zero is output frame `anchor`.
    pub fn begin(&mut self, chart: &Chart, anchor: u64) {
        let exps = pulse::expectations(chart);
        let clock = ChartClock::new(anchor, self.fs_out);
        let frames = exps
            .iter()
            .map(|e| clock.frame_of_seconds(e.t_s).round())
            .collect();
        self.plan = Some(Plan {
            exps,
            frames,
            live: 0,
            next: 0,
        });
        self.resend = false;
    }

    /// Stops announcing. Onsets and windows still go to the judge.
    pub fn end(&mut self) {
        self.plan = None;
        self.resend = false;
    }

    /// Expectations in the active chart.
    pub fn expected(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| p.exps.len())
    }

    /// The newest clock snapshot.
    pub fn snapshot(&self) -> Option<&ClockSnapshot> {
        self.snap.as_ref()
    }

    /// Announcements sent so far.
    pub fn sent(&self) -> u64 {
        self.sent
    }

    /// The newest clock epoch seen (or set as a floor).
    pub fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    /// Events of an epoch below `floor` are stale from now on: the session holds the
    /// handles of streams at least that new.
    pub fn set_epoch_floor(&mut self, floor: u32) {
        self.epoch = Some(self.epoch.map_or(floor, |e| e.max(floor)));
    }

    /// True for an event of an epoch before the newest one seen.
    pub fn is_stale(&self, epoch: u32) -> bool {
        self.stale(epoch)
    }

    /// Forgets the clock map: the streams it described are gone.
    pub fn reset_clock(&mut self) {
        self.snap = None;
        self.resend = false;
    }

    /// The judge inputs one engine event makes, appended to `out`.
    pub fn on_engine(&mut self, e: &EngineEvent, out: &mut Vec<JudgeInput>) {
        match e {
            EngineEvent::Clock(s) => {
                if self.stale(s.epoch()) {
                    return;
                }
                self.see(s.epoch());
                if self.snap.is_some_and(|p| p.quality() != s.quality()) {
                    self.resend = true;
                }
                self.snap = Some(*s);
                self.on_clock(s, out);
            }
            EngineEvent::Step(st) => {
                if !self.stale(st.epoch) {
                    self.resend = true;
                }
            }
            EngineEvent::Onset(n) => {
                if self.stale(n.epoch) {
                    return;
                }
                self.see(n.epoch);
                let judged = n.judged;
                out.push(if n.at_in.is_some() {
                    JudgeInput::Heard {
                        t_in: n.t_in,
                        judged,
                    }
                } else {
                    JudgeInput::Strum {
                        t_in: n.t_in,
                        judged,
                    }
                });
            }
            EngineEvent::Unjudged {
                epoch,
                from_in,
                to_in,
            } => {
                if self.stale(*epoch) || from_in.is_nan() || to_in.is_nan() {
                    return;
                }
                self.see(*epoch);
                out.push(JudgeInput::Unjudged {
                    from_in: *from_in,
                    to_in: *to_in,
                });
            }
            EngineEvent::Dropped { ring, n } => {
                if *n == 0 {
                    return;
                }
                if matches!(
                    ring,
                    Ring::Events | Ring::InEvents | Ring::OutStamps | Ring::Backend
                ) && let Some(s) = &self.snap
                {
                    let now = s.last_in_frame();
                    let fs = self.fs_in;
                    if now.is_finite() {
                        out.push(JudgeInput::Unjudged {
                            from_in: now - DROP_BEFORE_S * fs,
                            to_in: now + DROP_AFTER_S * fs,
                        });
                    }
                }
            }
            EngineEvent::Stopped(_) => {
                self.snap = None;
                self.resend = false;
                if let Some(p) = &mut self.plan {
                    // Nothing more of this chart is announced: the round is over.
                    p.next = p.exps.len();
                }
                out.push(JudgeInput::Reset);
            }
            EngineEvent::Reopened { epoch } => {
                self.see(*epoch);
                self.snap = None;
                self.resend = false;
                self.plan = None;
                out.push(JudgeInput::Reset);
            }
            EngineEvent::Pitch(_) | EngineEvent::Mix(_) | EngineEvent::Backend(_) => {}
        }
    }

    /// True for an event of an epoch before the current one.
    fn stale(&self, epoch: u32) -> bool {
        self.epoch.is_some_and(|cur| epoch < cur)
    }

    /// The newest epoch is at least `epoch`. A newer one than known (its `Reopened` was
    /// lost) ends the old map.
    fn see(&mut self, epoch: u32) {
        if self.epoch.is_some_and(|cur| epoch > cur) {
            self.snap = None;
            self.resend = false;
        }
        self.epoch = Some(self.epoch.map_or(epoch, |cur| cur.max(epoch)));
    }

    fn on_clock(&mut self, s: &ClockSnapshot, out: &mut Vec<JudgeInput>) {
        let out_now = s.last_out_frame();
        let locked = s.quality() == ClockQuality::Locked;
        if let Some(p) = &mut self.plan
            && out_now.is_finite()
        {
            let fs_out = self.fs_out;
            let horizon = out_now + LOOKAHEAD_S * fs_out;
            let retire = out_now - RETIRE_S * fs_out;
            while p.live < p.next && p.frames[p.live] < retire {
                p.live += 1;
            }
            if self.resend {
                self.resend = false;
                for k in p.live..p.next {
                    out.push(expect(&p.exps[k], s.in_of_out(p.frames[k])));
                    self.sent += 1;
                }
            }
            while p.next < p.exps.len() && p.frames[p.next] <= horizon {
                let k = p.next;
                out.push(expect(&p.exps[k], s.in_of_out(p.frames[k])));
                self.sent += 1;
                p.next += 1;
            }
        }
        let in_now = s.last_in_frame();
        if in_now.is_finite() {
            out.push(JudgeInput::Progress { in_now, locked });
        }
    }
}

fn expect(e: &Expectation, g_in: f64) -> JudgeInput {
    JudgeInput::Expect {
        id: e.id,
        g_in,
        kind: if e.ghost {
            ExpectKind::Ghost
        } else {
            ExpectKind::Attack
        },
        prev_gap_s: e.prev_gap_s,
        next_gap_s: e.next_gap_s,
        pos_in_bar: e.pos_in_bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonica_audio::clock::{ClockParts, ClockQuality};
    use tonica_audio::{BackendEvent, StampSource};
    use tonica_core::pulse::{Exercise, exercise};

    #[test]
    fn dropped_onsets_make_an_unjudged_window() {
        let mut a = Adapter::new(48_000.0, 48_000.0);
        let mut out = Vec::new();
        // Nothing is known about "now" yet: no window can be placed.
        a.on_engine(
            &EngineEvent::Dropped {
                ring: Ring::InEvents,
                n: 3,
            },
            &mut out,
        );
        assert!(out.is_empty());
        // Queues that carry no onsets, or a zero count, never make a window.
        a.on_engine(
            &EngineEvent::Dropped {
                ring: Ring::Pitch,
                n: 9,
            },
            &mut out,
        );
        a.on_engine(
            &EngineEvent::Dropped {
                ring: Ring::Events,
                n: 0,
            },
            &mut out,
        );
        assert!(out.is_empty());
        // With a clock map, a dropped onset queue makes a window around "now".
        let snap = ClockSnapshot::from_parts(ClockParts {
            epoch: 0,
            quality: ClockQuality::Locked,
            n_in: 96_000.0,
            t_in: 2.0,
            p_in: 1.0 / 48_000.0,
            n_out: 96_000.0,
            t_out: 2.0,
            p_out: 1.0 / 48_000.0,
            fs_in: 48_000.0,
            fs_out: 48_000.0,
            src_in: StampSource::Device,
            src_out: StampSource::Device,
            base_ns: Some(0),
        });
        a.on_engine(&EngineEvent::Clock(snap), &mut out);
        out.clear();
        a.on_engine(
            &EngineEvent::Dropped {
                ring: Ring::InEvents,
                n: 3,
            },
            &mut out,
        );
        let now = snap.last_in_frame();
        assert_eq!(
            out,
            vec![JudgeInput::Unjudged {
                from_in: now - DROP_BEFORE_S * 48_000.0,
                to_in: now + DROP_AFTER_S * 48_000.0,
            }]
        );
    }

    #[test]
    fn stop_and_reopen_reset_the_judge() {
        let mut a = Adapter::new(48_000.0, 48_000.0);
        let mut out = Vec::new();
        a.begin(&exercise(Exercise::OnClickQuarters, 80.0, 1), 100_000);
        assert_eq!(a.expected(), 4);
        a.on_engine(&EngineEvent::Stopped(BackendEvent::DeviceLost), &mut out);
        assert_eq!(out, vec![JudgeInput::Reset]);
        out.clear();
        a.on_engine(&EngineEvent::Reopened { epoch: 1 }, &mut out);
        assert_eq!(out, vec![JudgeInput::Reset]);
        assert_eq!(a.expected(), 0);
    }

    fn snap_at(epoch: u32, quality: ClockQuality, now_s: f64, lat_s: f64) -> ClockSnapshot {
        // Output frame f sounds at f / fs; input frame g is captured at g / fs + lat_s.
        ClockSnapshot::from_parts(ClockParts {
            epoch,
            quality,
            n_in: now_s * 48_000.0,
            t_in: now_s + lat_s,
            p_in: 1.0 / 48_000.0,
            n_out: now_s * 48_000.0,
            t_out: now_s,
            p_out: 1.0 / 48_000.0,
            fs_in: 48_000.0,
            fs_out: 48_000.0,
            src_in: StampSource::Device,
            src_out: StampSource::Device,
            base_ns: Some(0),
        })
    }

    fn onset(epoch: u32, t_in: f64) -> EngineEvent {
        EngineEvent::Onset(tonica_audio::OnsetNote {
            event: tonica_dsp::OnsetEvent::Heard { t: 0.0, at: 0.0 },
            t_in,
            at_in: None,
            judged: true,
            epoch,
        })
    }

    /// P1-1: an attack at an unknown time is passed on (the judge makes it a `Glitch`), so
    /// it never becomes a false `NotHeard`. Windows are dropped only for a NaN bound.
    #[test]
    fn a_strum_at_an_unknown_time_reaches_the_judge() {
        use tonica_core::gates::GateTable;
        use tonica_core::judge::{JudgeConfig, PulseJudge};
        use tonica_core::timing::{Level, TimingRef};
        use tonica_core::verdict::{Attack, Judge, JudgeOut, Why};
        let mut a = Adapter::new(48_000.0, 48_000.0);
        let mut ins = Vec::new();
        // The first click sounds 0.1 s after "now".
        a.begin(
            &exercise(Exercise::OnClickQuarters, 80.0, 1),
            48_000 + 4_800,
        );
        a.on_engine(
            &EngineEvent::Clock(snap_at(0, ClockQuality::Locked, 1.0, 0.0)),
            &mut ins,
        );
        a.on_engine(&onset(0, f64::NAN), &mut ins);
        assert!(
            ins.iter()
                .any(|i| matches!(i, JudgeInput::Strum { t_in, .. } if t_in.is_nan())),
            "{ins:?}"
        );
        a.on_engine(
            &EngineEvent::Unjudged {
                epoch: 0,
                from_in: f64::NEG_INFINITY,
                to_in: 10.0,
            },
            &mut ins,
        );
        assert!(
            ins.iter()
                .any(|i| matches!(i, JudgeInput::Unjudged { from_in, .. } if *from_in == f64::NEG_INFINITY)),
            "an infinite bound is the judge's to read"
        );
        let before = ins.len();
        a.on_engine(
            &EngineEvent::Unjudged {
                epoch: 0,
                from_in: f64::NAN,
                to_in: 10.0,
            },
            &mut ins,
        );
        assert_eq!(ins.len(), before, "a NaN bound places nothing");
        // Through the judge: the first event is not called missed.
        let mut j = PulseJudge::new(
            JudgeConfig::new(48_000.0, Level::Apprentice),
            TimingRef::loopback(0.0, 0.2),
            &GateTable::builtin(),
        );
        let mut outs = Vec::new();
        for i in &ins {
            j.push(i, &mut outs);
        }
        j.push(
            &JudgeInput::Progress {
                in_now: 48_000.0 * 10.0,
                locked: true,
            },
            &mut outs,
        );
        let first = outs
            .iter()
            .find_map(|o| match o {
                JudgeOut::Verdict(v) if v.id == 0 && v.fin => Some(*v),
                _ => None,
            })
            .expect("event 0 has a final verdict");
        assert_eq!(first.attack, Attack::NotJudged(Why::Glitch), "{first:?}");
    }

    /// P2-3: a snapshot or onset of an epoch before the newest seen changes nothing.
    #[test]
    fn a_stale_epoch_never_rolls_the_map_back() {
        let mut a = Adapter::new(48_000.0, 48_000.0);
        let mut out = Vec::new();
        a.on_engine(&EngineEvent::Reopened { epoch: 1 }, &mut out);
        out.clear();
        a.on_engine(
            &EngineEvent::Clock(snap_at(0, ClockQuality::Locked, 5.0, 0.0)),
            &mut out,
        );
        assert!(a.snapshot().is_none(), "the old map is not kept");
        assert!(out.is_empty(), "{out:?}");
        a.on_engine(&onset(0, 1000.0), &mut out);
        assert!(out.is_empty(), "an onset of the old streams: {out:?}");
        assert_eq!(a.epoch(), Some(1));
        // A floor set by the session (handles of a newer epoch) works the same way.
        let mut b = Adapter::new(48_000.0, 48_000.0);
        b.set_epoch_floor(2);
        b.on_engine(
            &EngineEvent::Clock(snap_at(1, ClockQuality::Locked, 5.0, 0.0)),
            &mut out,
        );
        assert!(b.snapshot().is_none() && out.is_empty());
        b.on_engine(
            &EngineEvent::Clock(snap_at(2, ClockQuality::Locked, 5.0, 0.0)),
            &mut out,
        );
        assert!(b.snapshot().is_some());
    }

    /// P2-5: when the map's quality changes (Acquiring to Locked) the open events are
    /// announced again, so the judge places them on the better map.
    #[test]
    fn a_quality_change_announces_the_open_events_again() {
        let mut a = Adapter::new(48_000.0, 48_000.0);
        let mut out = Vec::new();
        a.begin(
            &exercise(Exercise::OnClickQuarters, 80.0, 1),
            48_000 + 4_800,
        );
        a.on_engine(
            &EngineEvent::Clock(snap_at(0, ClockQuality::Acquiring, 1.0, 0.010)),
            &mut out,
        );
        let first = a.sent();
        assert!(first >= 2, "{first}");
        a.on_engine(
            &EngineEvent::Clock(snap_at(0, ClockQuality::Acquiring, 1.005, 0.010)),
            &mut out,
        );
        assert_eq!(a.sent(), first, "same quality: nothing again");
        out.clear();
        a.on_engine(
            &EngineEvent::Clock(snap_at(0, ClockQuality::Locked, 1.010, 0.020)),
            &mut out,
        );
        let again: Vec<f64> = out
            .iter()
            .filter_map(|i| match i {
                JudgeInput::Expect { id: 0, g_in, .. } => Some(*g_in),
                _ => None,
            })
            .collect();
        assert_eq!(again.len(), 1, "{out:?}");
        // On the locked map the click of event 0 is captured 20 ms later than on the old.
        let want = snap_at(0, ClockQuality::Locked, 1.010, 0.020).in_of_out(52_800.0);
        assert!((again[0] - want).abs() < 1e-6, "{} vs {want}", again[0]);
    }

    #[test]
    fn event_index_skips_rests_in_the_count() {
        let c = exercise(Exercise::OnClickQuarters, 80.0, 2);
        for id in 0..8 {
            assert_eq!(event_index_of(&c, id), Some(id as usize));
        }
        assert_eq!(event_index_of(&c, 8), None);
    }
}
