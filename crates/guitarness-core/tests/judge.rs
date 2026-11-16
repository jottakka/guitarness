//! The pulse judge (plan §5, unit C2). Direct synthetic inputs, except the property on the
//! guitarness-sim grid, which runs the real onset detector. No device, no file, no clock.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use guitarness_core::gates::{GateEntry, GateEvidence, GateRule, GateState, GateTable};
use guitarness_core::judge::{CLOSED_CELLS, JudgeConfig, OPEN_CELLS, PulseJudge};
use guitarness_core::stats::VerdictLog;
use guitarness_core::timing::{Level, TimingRef};
use guitarness_core::verdict::{
    Attack, Direction, ExpectKind, Judge, JudgeInput, JudgeOut, PulseVerdict, Timing, VerdictType,
    Why,
};
use guitarness_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};
use guitarness_sim::strum::{Dir, StrumSpec, render_take, voicing};
use guitarness_sim::synth::Mulberry32;
use guitarness_sim::timing::{Take, make_take, standard_grid};

// ---------------------------------------------------------------------------------------
// Allocation counter (per thread, so tests may run in parallel).

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn bump() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(|c| c.get())
}

// ---------------------------------------------------------------------------------------
// Bench

const FS: f64 = 48_000.0;
const L_RT: f64 = 87.3;
const LOOP_MAD: f64 = 0.5;
/// Input frame of time zero: the stream has been running for a while.
const BASE: f64 = 1_000_000.0;
/// A strum reaches the judge this long after it sounded (the detector waits for it to end).
const STRUM_DELAY_MS: f64 = 60.0;
/// How long a cell waits after its end before it closes.
const MARGIN: f64 = JudgeConfig::CLOSE_MARGIN_MS;
// the plan's floor: never `NotHeard` before g + off + P + 300 ms
const _: () = assert!(MARGIN >= 300.0);

fn loopback() -> TimingRef {
    TimingRef::loopback(L_RT, LOOP_MAD)
}

fn u_loop() -> f64 {
    1.4826 * LOOP_MAD + 3.0
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
    /// Roughly normal, σ = 1.
    fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }
}

/// The cell of the plan, computed here on its own.
#[derive(Clone, Copy, Debug)]
struct Geo {
    p: f64,
    w: f64,
    w_ef: f64,
}

impl Geo {
    fn new(level: Level, gap_ms: f64, u: f64) -> Self {
        let p = (0.5 * gap_ms).min(250.0);
        let w = level.window_ms().min(0.25 * gap_ms);
        Self {
            p,
            w,
            w_ef: (w + u).min(p),
        }
    }

    fn class(&self, e: f64, center: bool) -> Timing {
        let a = e.abs();
        if center && a <= 25.0 && a <= self.w_ef {
            Timing::Center
        } else if a <= self.w_ef {
            Timing::InWindow
        } else {
            Timing::Outside
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Beat {
    id: u32,
    g_ms: f64,
    kind: ExpectKind,
    gap_ms: f64,
}

/// `n` attacks `gap_ms` apart, the first at time zero.
fn grid(n: u32, gap_ms: f64) -> Vec<Beat> {
    (0..n)
        .map(|k| Beat {
            id: k,
            g_ms: f64::from(k) * gap_ms,
            kind: ExpectKind::Attack,
            gap_ms,
        })
        .collect()
}

struct Bench {
    j: PulseJudge,
    fs: f64,
    off: f64,
    all: Vec<JudgeOut>,
}

impl Bench {
    fn new(level: Level, timing: TimingRef) -> Self {
        Self::with(JudgeConfig::new(FS, level), timing, &GateTable::builtin())
    }

    fn with(cfg: JudgeConfig, timing: TimingRef, gates: &GateTable) -> Self {
        Self {
            j: PulseJudge::new(cfg, timing, gates),
            fs: cfg.fs_in,
            off: timing.offset_ms().unwrap_or(0.0),
            all: Vec::new(),
        }
    }

    /// Input frame of time `ms`.
    fn frame(&self, ms: f64) -> f64 {
        BASE + ms * self.fs / 1000.0
    }

    fn push(&mut self, i: JudgeInput) -> Vec<JudgeOut> {
        let mut out = Vec::new();
        self.j.push(&i, &mut out);
        self.all.extend_from_slice(&out);
        out
    }

    fn input(&self, b: &Beat) -> JudgeInput {
        JudgeInput::Expect {
            id: b.id,
            g_in: self.frame(b.g_ms),
            kind: b.kind,
            prev_gap_s: b.gap_ms / 1000.0,
            next_gap_s: b.gap_ms / 1000.0,
            pos_in_bar: (b.id % 8) as u8,
        }
    }

    fn expect(&mut self, id: u32, g_ms: f64, gap_ms: f64) -> Vec<JudgeOut> {
        self.expect_kind(id, g_ms, gap_ms, ExpectKind::Attack)
    }

    fn expect_kind(&mut self, id: u32, g_ms: f64, gap_ms: f64, kind: ExpectKind) -> Vec<JudgeOut> {
        let i = self.input(&Beat {
            id,
            g_ms,
            kind,
            gap_ms,
        });
        self.push(i)
    }

    /// Input frame of an attack `e_ms` away from where the click at `g_ms` is heard.
    fn t_in(&self, g_ms: f64, e_ms: f64) -> f64 {
        self.frame(g_ms + self.off + e_ms)
    }

    fn strum(&mut self, g_ms: f64, e_ms: f64) -> Vec<JudgeOut> {
        self.push(JudgeInput::Strum {
            t_in: self.t_in(g_ms, e_ms),
            judged: true,
        })
    }

    fn heard(&mut self, g_ms: f64, e_ms: f64) -> Vec<JudgeOut> {
        self.push(JudgeInput::Heard {
            t_in: self.t_in(g_ms, e_ms),
            judged: true,
        })
    }

    fn progress(&mut self, now_ms: f64) -> Vec<JudgeOut> {
        self.push(JudgeInput::Progress {
            in_now: self.frame(now_ms),
            locked: true,
        })
    }

    fn unlocked(&mut self, now_ms: f64) -> Vec<JudgeOut> {
        self.push(JudgeInput::Progress {
            in_now: self.frame(now_ms),
            locked: false,
        })
    }

    fn unjudged(&mut self, from_ms: f64, to_ms: f64) -> Vec<JudgeOut> {
        self.push(JudgeInput::Unjudged {
            from_in: self.frame(from_ms),
            to_in: self.frame(to_ms),
        })
    }

    /// Feed a round the way the session does: each expectation 2 s before its click, each
    /// strum [`STRUM_DELAY_MS`] after it sounded (its early notice after 5 ms), a clock
    /// report every 10 ms. `strums_ms` are input times.
    fn play(&mut self, beats: &[Beat], strums_ms: &[f64], end_ms: f64) {
        self.play_with(beats, strums_ms, &[], end_ms);
    }

    /// As [`Self::play`], with other inputs at their own times (just before the clock
    /// report of that time).
    fn play_with(
        &mut self,
        beats: &[Beat],
        strums_ms: &[f64],
        more: &[(f64, JudgeInput)],
        end_ms: f64,
    ) {
        let mut items: Vec<(f64, u8, JudgeInput)> = Vec::new();
        items.extend(more.iter().map(|&(at, i)| (at, 3, i)));
        for b in beats {
            items.push((b.g_ms - 2000.0, 0, self.input(b)));
        }
        for &t in strums_ms {
            let t_in = self.frame(t);
            let judged = true;
            items.push((t + 5.0, 1, JudgeInput::Heard { t_in, judged }));
            items.push((t + STRUM_DELAY_MS, 2, JudgeInput::Strum { t_in, judged }));
        }
        let mut now = beats.first().map_or(0.0, |b| b.g_ms) - 2500.0;
        while now <= end_ms {
            let in_now = self.frame(now);
            items.push((
                now,
                4,
                JudgeInput::Progress {
                    in_now,
                    locked: true,
                },
            ));
            now += 10.0;
        }
        items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (_, _, i) in items {
            self.push(i);
        }
    }

    fn log(&self) -> VerdictLog {
        let mut l = VerdictLog::new();
        for o in &self.all {
            l.push(o);
        }
        l
    }

    /// The verdict of `id` as it stands: final, withdrawals applied.
    fn result(&self, id: u32) -> PulseVerdict {
        *self
            .log()
            .verdicts()
            .iter()
            .find(|v| v.id == id)
            .unwrap_or_else(|| panic!("no final verdict for {id}"))
    }
}

fn verdicts(out: &[JudgeOut]) -> Vec<PulseVerdict> {
    out.iter()
        .filter_map(|o| match o {
            JudgeOut::Verdict(v) => Some(*v),
            _ => None,
        })
        .collect()
}

fn finals(out: &[JudgeOut]) -> Vec<PulseVerdict> {
    verdicts(out).into_iter().filter(|v| v.fin).collect()
}

fn extras(out: &[JudgeOut]) -> Vec<f64> {
    out.iter()
        .filter_map(|o| match o {
            JudgeOut::Extra { t_in } => Some(*t_in),
            _ => None,
        })
        .collect()
}

fn withdrawn(out: &[JudgeOut]) -> Vec<(u32, Why)> {
    out.iter()
        .filter_map(|o| match o {
            JudgeOut::Withdrawn { id, why } => Some((*id, *why)),
            _ => None,
        })
        .collect()
}

fn flashes(out: &[JudgeOut]) -> Vec<(Option<u32>, Option<f32>)> {
    out.iter()
        .filter_map(|o| match o {
            JudgeOut::Flash { id, dt_ms } => Some((*id, *dt_ms)),
            _ => None,
        })
        .collect()
}

fn is_hit(v: &PulseVerdict) -> bool {
    matches!(v.timing, Timing::Center | Timing::InWindow)
}

fn not_judged(v: &PulseVerdict, why: Why) -> bool {
    v.attack == Attack::NotJudged(why) && v.timing == Timing::NotJudged(why) && v.dt_ms.is_none()
}

fn near(v: &PulseVerdict, e: f64) -> bool {
    v.dt_ms.is_some_and(|d| (f64::from(d) - e).abs() <= 0.01)
}

/// Invariants of every output stream.
fn check_stream(all: &[JudgeOut]) {
    for v in verdicts(all) {
        assert_eq!(v.dir, Direction::NotJudged(Why::GateLocked), "{v:?}");
        assert!(
            v.fin || v.attack != Attack::NotHeard,
            "provisional NotHeard: {v:?}"
        );
        let timed = matches!(
            v.timing,
            Timing::Center | Timing::InWindow | Timing::Outside
        );
        assert_eq!(v.dt_ms.is_some(), timed, "{v:?}");
        if timed {
            assert_eq!(v.attack, Attack::Heard, "{v:?}");
        }
        if v.attack == Attack::NotHeard {
            assert_eq!(v.timing, Timing::NotJudged(Why::NoAttack), "{v:?}");
            assert_eq!(v.onset_in, None, "{v:?}");
        }
    }
    let fin = finals(all);
    for (i, a) in fin.iter().enumerate() {
        assert!(
            fin[i + 1..].iter().all(|b| b.id != a.id),
            "two final verdicts for {}",
            a.id
        );
    }
}

/// One event at time zero with one strum `e_ms` from its click.
/// Returns what the strum gave at once, the final verdict and the extras.
fn one(
    level: Level,
    timing: TimingRef,
    fs: f64,
    gap_ms: f64,
    e_ms: f64,
) -> (Vec<JudgeOut>, PulseVerdict, usize) {
    let mut b = Bench::with(JudgeConfig::new(fs, level), timing, &GateTable::builtin());
    b.progress(-1000.0);
    b.expect(0, 0.0, gap_ms);
    b.progress(-900.0);
    let at_once = b.strum(0.0, e_ms);
    let closing = b.progress(b.off + 250.0 + MARGIN + 1.0);
    let fin = finals(&closing);
    assert_eq!(fin.len(), 1, "{closing:?}");
    check_stream(&b.all);
    (at_once, fin[0], extras(&b.all).len())
}

// ---------------------------------------------------------------------------------------
// (1) Known error

#[test]
fn t01_known_error_comes_back_with_its_class() {
    let cases = [
        (48_000.0, Level::Band, 1000.0, loopback()),
        (
            44_100.0,
            Level::Apprentice,
            250.0,
            TimingRef::loopback(142.6, 2.0),
        ),
        (
            48_000.0,
            Level::Stage,
            500.0,
            TimingRef::estimate(112.0, 9.0, 16),
        ),
        // loop measured too loosely for "center" (σ = 10.4 ms)
        (48_000.0, Level::Band, 750.0, TimingRef::loopback(35.0, 7.0)),
    ];
    for (n, &(fs, level, gap, timing)) in cases.iter().enumerate() {
        let geo = Geo::new(level, gap, timing.center_uncertainty_ms());
        let center = timing.may_show_center();
        let mut b = Bench::with(JudgeConfig::new(fs, level), timing, &GateTable::builtin());
        let mut rng = Rng(11 + n as u64);
        let mut seen = [0u32; 3];
        let mut announced = 0u32;
        b.progress(-3000.0);
        for k in 0..1000u32 {
            while announced < 1000 && announced <= k + 4 {
                b.expect(announced, f64::from(announced) * gap, gap);
                announced += 1;
            }
            let g = f64::from(k) * gap;
            let e = loop {
                let e = rng.range(-geo.p, geo.p);
                let a = e.abs();
                let clear = |edge: f64| (a - edge).abs() > 1e-3;
                if clear(25.0) && clear(geo.w_ef) && clear(geo.p) {
                    break e;
                }
            };
            let want = geo.class(e, center);
            let t = b.t_in(g, e);

            let at_once = b.strum(g, e);
            assert_eq!(at_once.len(), 1, "case {n} k {k}: {at_once:?}");
            let p = verdicts(&at_once)[0];
            assert!(!p.fin && p.id == k && p.attack == Attack::Heard, "{p:?}");
            assert_eq!(p.timing, want, "case {n} e {e}: {p:?}");
            assert!(near(&p, e), "case {n} e {e}: {p:?}");
            assert_eq!(p.onset_in, Some(t));
            assert_eq!(p.pos_in_bar, (k % 8) as u8);

            let closing = b.progress(g + b.off + geo.p + MARGIN + 1.0);
            let f = finals(&closing);
            assert_eq!(f.len(), 1, "case {n} k {k}: {closing:?}");
            assert_eq!(f[0], PulseVerdict { fin: true, ..p });

            seen[match want {
                Timing::Center => 0,
                Timing::InWindow => 1,
                _ => 2,
            }] += 1;
        }
        check_stream(&b.all);
        assert!(extras(&b.all).is_empty());
        assert_eq!(seen[0] > 0, center, "case {n}: {seen:?}");
        assert!(seen[1] > 0 && seen[2] > 0, "case {n}: {seen:?}");
        assert_eq!(b.j.counters().late_strums, 0);
    }
}

#[test]
fn t01_class_edges_are_w_plus_u_and_p() {
    const EPS: f64 = 1e-4;
    let u = u_loop();
    // (level, gap, timing): wide cell; cell cut by the gap; window that fills the cell
    let wide_u = TimingRef::estimate(100.0, 15.0, 1); // u = 55.7 ms
    let cases = [
        (Level::Band, 1000.0, loopback()),
        (Level::Apprentice, 250.0, loopback()),
        (Level::Stage, 100.0, loopback()),
        (Level::Apprentice, 100.0, wide_u),
    ];
    for (level, gap, timing) in cases {
        let geo = Geo::new(level, gap, timing.center_uncertainty_ms());
        let center = timing.may_show_center();
        let run = |e: f64| one(level, timing, FS, gap, e);
        for sign in [-1.0, 1.0] {
            // The nominal window is not the edge: just past W is still in time.
            if geo.w + EPS < geo.w_ef {
                let (_, v, _) = run(sign * (geo.w + EPS));
                assert!(is_hit(&v), "past W must stay in time: {geo:?} {v:?}");
            }
            // W + u is.
            let (_, v, _) = run(sign * (geo.w_ef - EPS));
            assert!(is_hit(&v), "{geo:?} {v:?}");
            if geo.w_ef + EPS < geo.p {
                let (_, v, _) = run(sign * (geo.w_ef + EPS));
                assert_eq!(v.timing, Timing::Outside, "{geo:?} {v:?}");
                assert_eq!(v.attack, Attack::Heard);
                assert!(near(&v, sign * (geo.w_ef + EPS)));
            }
            // P: inside it the attack is this event's, beyond it is nobody's.
            let (_, v, x) = run(sign * (geo.p - EPS));
            assert_eq!(v.attack, Attack::Heard, "{geo:?} {v:?}");
            assert_eq!(x, 0);
            let (at_once, v, x) = run(sign * (geo.p + EPS));
            assert_eq!(v.attack, Attack::NotHeard, "{geo:?} {v:?}");
            assert_eq!(extras(&at_once).len(), 1);
            assert_eq!(x, 1);
            // Center.
            if center && geo.w_ef > 25.0 {
                let (_, v, _) = run(sign * (25.0 - EPS));
                assert_eq!(v.timing, Timing::Center, "{v:?}");
                let (_, v, _) = run(sign * (25.0 + EPS));
                assert_eq!(v.timing, Timing::InWindow, "{v:?}");
            }
        }
        let (_, v, _) = run(0.0);
        assert_eq!(
            v.timing,
            if center {
                Timing::Center
            } else {
                Timing::InWindow
            }
        );
    }
    // the numbers of the plan, once by hand: Band, quarter notes at 60 BPM
    let g = Geo::new(Level::Band, 1000.0, u);
    assert_eq!((g.p, g.w), (250.0, 80.0));
    assert!((g.w_ef - 83.7413).abs() < 1e-9);
    // an estimate never shows "center", however tight
    let (_, v, _) = one(
        Level::Band,
        TimingRef::estimate(100.0, 0.5, 16),
        FS,
        1000.0,
        1.0,
    );
    assert_eq!(v.timing, Timing::InWindow);
}

#[test]
fn t01_only_the_offset_of_the_basis_is_subtracted() {
    // Same strums, 30 ms early against the true click, judged with the measured loop and
    // with an estimate that swallowed the player's habit: the loop shows −30, the estimate
    // cannot (and so must never be used to claim a tendency).
    let (_, v, _) = one(Level::Apprentice, loopback(), FS, 1000.0, -30.0);
    assert!(near(&v, -30.0));
    let mut b = Bench::new(Level::Apprentice, TimingRef::estimate(L_RT - 30.0, 6.0, 16));
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let t_in = b.frame(L_RT - 30.0);
    let out = b.push(JudgeInput::Strum { t_in, judged: true });
    assert!(near(&verdicts(&out)[0], 0.0));
}

#[test]
fn t01_an_attack_exactly_on_an_edge_is_inside_it() {
    // The edges are inclusive, and frame arithmetic rounds. 48 kHz, click at frame
    // 1 000 000, loop 87.3 ms, P = 125 ms: the attack on the edge of the cell is at frame
    // 1 010 190.4, which reads back as 125.00000000000047 ms.
    let (at_once, v, x) = one(Level::Apprentice, loopback(), FS, 250.0, 125.0);
    assert_eq!((v.attack, x), (Attack::Heard, 0), "{at_once:?}");

    let mut outward = 0u32;
    for fs in [44_100.0, 48_000.0, 96_000.0] {
        // the stream just began; an hour in; a day in; far beyond any session
        for base in [0.0, 1.0e6, 123_456_789.25, 4.2e9, 1.0e12] {
            for off in [87.3, 57.25, 0.1, 142.6, 33.333] {
                for gap in [1000.0, 250.0, 333.3, 100.0] {
                    for level in Level::ALL {
                        let timing = TimingRef::loopback(off, LOOP_MAD);
                        assert!(timing.may_show_center());
                        let geo = Geo::new(level, gap, timing.center_uncertainty_ms());
                        let fr = |ms: f64| base + ms * fs / 1000.0;
                        let run = |e: f64| {
                            let mut j = PulseJudge::new(
                                JudgeConfig::new(fs, level),
                                timing,
                                &GateTable::builtin(),
                            );
                            let mut out = Vec::new();
                            for i in [
                                JudgeInput::Progress {
                                    in_now: fr(-1000.0),
                                    locked: true,
                                },
                                JudgeInput::Expect {
                                    id: 0,
                                    g_in: fr(0.0),
                                    kind: ExpectKind::Attack,
                                    prev_gap_s: gap / 1000.0,
                                    next_gap_s: gap / 1000.0,
                                    pos_in_bar: 0,
                                },
                                JudgeInput::Strum {
                                    t_in: fr(off + e),
                                    judged: true,
                                },
                                JudgeInput::Progress {
                                    in_now: fr(off + geo.p + MARGIN + 1.0),
                                    locked: true,
                                },
                            ] {
                                j.push(&i, &mut out);
                            }
                            let f = finals(&out);
                            assert_eq!(f.len(), 1, "{out:?}");
                            f[0]
                        };
                        let what = format!("fs {fs} base {base} off {off} gap {gap} {level:?}");
                        for sign in [-1.0, 1.0] {
                            // on the edge of the cell: this event's attack
                            let v = run(sign * geo.p);
                            assert_eq!(v.attack, Attack::Heard, "{what}: {v:?}");
                            // on the edge of the window: in time
                            let v = run(sign * geo.w_ef);
                            assert!(is_hit(&v), "{what}: {v:?}");
                            // on the edge of the center
                            if geo.w_ef >= 25.0 {
                                let v = run(sign * 25.0);
                                assert_eq!(v.timing, Timing::Center, "{what}: {v:?}");
                            }
                            // The slack is no window of its own: 10 µs past an edge is past it.
                            let v = run(sign * (geo.p + 0.01));
                            assert_eq!(v.attack, Attack::NotHeard, "{what}: {v:?}");
                            if geo.w_ef + 0.01 < geo.p {
                                let v = run(sign * (geo.w_ef + 0.01));
                                assert_eq!(v.timing, Timing::Outside, "{what}: {v:?}");
                            }
                            // how the edge of the cell reads back from its frames
                            let e = (fr(off + sign * geo.p) - fr(0.0)) * 1000.0 / fs - off;
                            if e.abs() > geo.p {
                                outward += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    // the sweep is a real test: many of its edges are rounded outward
    assert!(outward > 100, "{outward}");
}

// ---------------------------------------------------------------------------------------
// (2) One to one

#[test]
fn t02_two_attacks_in_a_cell_are_one_hit_and_one_extra() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let first = b.strum(0.0, 30.0);
    assert_eq!(verdicts(&first).len(), 1);
    let second = b.strum(0.0, -60.0);
    assert_eq!(
        second,
        vec![JudgeOut::Extra {
            t_in: b.t_in(0.0, -60.0)
        }]
    );
    b.progress(1000.0);
    check_stream(&b.all);
    let f = finals(&b.all);
    assert_eq!(f.len(), 1);
    assert!(f[0].attack == Attack::Heard && near(&f[0], 30.0));
    assert_eq!(f[0].onset_in, Some(b.t_in(0.0, 30.0)));
    assert_eq!(extras(&b.all).len(), 1);
}

#[test]
fn t02_one_attack_never_serves_two_events() {
    // Regression of B7: beats at 0 and 100 ms, one strum half way used to fill both.
    for e in [49.0, 49.999, 50.0, 50.001, 51.0] {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 100.0);
        b.expect(1, 100.0, 100.0);
        b.strum(0.0, e);
        b.progress(2000.0);
        check_stream(&b.all);
        let f = finals(&b.all);
        assert_eq!(f.len(), 2);
        let heard: Vec<_> = f.iter().filter(|v| v.attack == Attack::Heard).collect();
        let missed = f.iter().filter(|v| v.attack == Attack::NotHeard).count();
        assert_eq!((heard.len(), missed), (1, 1), "e {e}: {f:?}");
        assert_eq!(heard[0].id, if e < 50.0 { 0 } else { 1 }, "e {e}");
        assert!(extras(&b.all).is_empty(), "e {e}");
    }
}

#[test]
fn t02_every_strum_is_counted_exactly_once() {
    // Random strums over a random-density grid: each one is the attack of exactly one
    // event or exactly one extra; no two events share one.
    let mut rng = Rng(202);
    for round in 0..40 {
        let gap = [125.0, 250.0, 500.0, 1000.0][round % 4];
        let beats = grid(24, gap);
        let end = 24.0 * gap;
        let mut strums: Vec<f64> = (0..40).map(|_| rng.range(-400.0, end + 400.0)).collect();
        strums.sort_by(f64::total_cmp);
        let mut b = Bench::new(Level::Band, loopback());
        b.play(&beats, &strums, end + 2000.0);
        check_stream(&b.all);

        let f = finals(&b.all);
        assert_eq!(f.len(), 24);
        let mut used: Vec<f64> = f.iter().filter_map(|v| v.onset_in).collect();
        used.extend(extras(&b.all));
        used.sort_by(f64::total_cmp);
        let mut sent: Vec<f64> = strums.iter().map(|&t| b.frame(t)).collect();
        sent.sort_by(f64::total_cmp);
        assert_eq!(used, sent, "round {round}");
        // and an attack is credited to the event whose cell it is in
        for v in f.iter().filter(|v| v.attack == Attack::Heard) {
            let center = b.frame(f64::from(v.id) * gap + L_RT);
            let e = (v.onset_in.unwrap() - center) * 1000.0 / FS;
            assert!(e.abs() <= 0.5 * gap.min(500.0) + 1e-6, "{v:?} e {e}");
        }
    }
}

// ---------------------------------------------------------------------------------------
// (3) A verdict only gets better

#[test]
fn t03_a_later_attack_replaces_only_if_nearer() {
    // provisional at −100, then −5: final −5
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let a = verdicts(&b.strum(0.0, -100.0))[0];
    assert!(!a.fin && a.timing == Timing::InWindow && near(&a, -100.0));
    let out = b.strum(0.0, -5.0);
    assert_eq!(extras(&out), vec![b.t_in(0.0, -100.0)]);
    let c = verdicts(&out)[0];
    assert!(!c.fin && c.timing == Timing::Center && near(&c, -5.0));
    b.progress(1000.0);
    let f = finals(&b.all);
    assert_eq!(f.len(), 1);
    assert!(f[0].timing == Timing::Center && near(&f[0], -5.0));

    // the other order does not swap
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.strum(0.0, -5.0);
    let out = b.strum(0.0, -100.0);
    assert_eq!(
        out,
        vec![JudgeOut::Extra {
            t_in: b.t_in(0.0, -100.0)
        }]
    );
    b.progress(1000.0);
    let f = finals(&b.all);
    assert_eq!(f.len(), 1);
    assert!(f[0].timing == Timing::Center && near(&f[0], -5.0));
    assert_eq!(verdicts(&b.all).len(), 2, "one provisional, one final");
}

#[test]
fn t03_the_class_never_gets_worse() {
    let rank = |t: Timing| match t {
        Timing::Center => 3,
        Timing::InWindow => 2,
        Timing::Outside => 1,
        Timing::NotJudged(_) => 0,
    };
    let mut rng = Rng(303);
    for _ in 0..500 {
        let mut b = Bench::new(Level::Band, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        let n = 1 + (rng.next() % 5) as usize;
        let mut best = f64::INFINITY;
        let mut last = 0;
        for _ in 0..n {
            let e = rng.range(-250.0, 250.0);
            for v in verdicts(&b.strum(0.0, e)) {
                assert!(rank(v.timing) >= last, "{v:?}");
                last = rank(v.timing);
            }
            if e.abs() < best.abs() {
                best = e;
            }
        }
        b.progress(1000.0);
        let f = finals(&b.all)[0];
        assert!(near(&f, best) && rank(f.timing) >= last);
        assert_eq!(extras(&b.all).len(), n - 1);
    }
}

// ---------------------------------------------------------------------------------------
// (4) Strong bias

#[test]
fn t04_strong_bias_is_outside_where_it_fell_never_a_hit_next_door() {
    // Eighths at 120 BPM (250 ms apart), every strum 140 ms early. A judge that took the
    // nearest event inside the level window (120 ms) would call each one "in time, +110"
    // on the event before. The cell stops the window at a quarter of the gap.
    let n = 16;
    let beats = grid(n, 250.0);
    let strums: Vec<f64> = beats.iter().map(|b| b.g_ms + L_RT - 140.0).collect();
    for level in Level::ALL {
        let mut b = Bench::new(level, loopback());
        b.play(&beats, &strums, 6000.0);
        check_stream(&b.all);
        let f = finals(&b.all);
        assert_eq!(f.len(), n as usize);
        assert_eq!(f.iter().filter(|v| is_hit(v)).count(), 0, "{level:?}");
        // the strum meant for event k is in the cell of event k − 1, 110 ms late
        for k in 1..n {
            let v = b.result(k - 1);
            assert_eq!(v.attack, Attack::Heard);
            assert_eq!(v.timing, Timing::Outside, "{level:?} {v:?}");
            assert!(near(&v, 110.0), "{v:?}");
            assert_eq!(v.onset_in, Some(b.frame(strums[k as usize])));
        }
        // the first strum fell before every cell; the last event got none
        assert_eq!(extras(&b.all), vec![b.frame(strums[0])]);
        assert_eq!(b.result(n - 1).attack, Attack::NotHeard);
        // every early notice of a paired strum names the cell it fell in, never a hit
        for (id, dt) in flashes(&b.all).into_iter().skip(1) {
            assert!(id.is_some());
            assert!(dt.is_some_and(|d| (f64::from(d) - 110.0).abs() < 0.01));
        }
    }
}

// ---------------------------------------------------------------------------------------
// (5) Ghost

#[test]
fn t05_a_ghost_swallows_the_attack() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 250.0);
    b.expect_kind(1, 250.0, 250.0, ExpectKind::Ghost);
    b.expect(2, 500.0, 250.0);
    // early notice and strum inside the ghost's cell
    let fl = b.heard(250.0, 10.0);
    assert_eq!(flashes(&fl), vec![(Some(1), None)]);
    assert!(
        b.strum(250.0, 10.0).is_empty(),
        "swallowed: no verdict, no extra"
    );
    assert!(b.strum(250.0, -40.0).is_empty());
    b.progress(3000.0);
    check_stream(&b.all);
    let f = finals(&b.all);
    assert_eq!(f.len(), 3);
    assert!(not_judged(&b.result(1), Why::Ghost));
    assert_eq!(b.result(1).onset_in, None);
    // not an error, not an extra, and not a hit for the neighbours
    assert!(extras(&b.all).is_empty());
    assert_eq!(b.result(0).attack, Attack::NotHeard);
    assert_eq!(b.result(2).attack, Attack::NotHeard);
    // the only event that needs no attack is the ghost, and it is not an event of the round
    let s = b.log().stats(&loopback());
    assert_eq!((s.events, s.heard, s.not_heard), (2, 0, 2));

    // without the ghost the same strum is nobody's: an extra
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 250.0);
    b.expect(2, 500.0, 250.0);
    assert_eq!(extras(&b.strum(250.0, 10.0)).len(), 1);
}

// ---------------------------------------------------------------------------------------
// (6) NotHeard

#[test]
fn t06_not_heard_only_after_the_cell_closed() {
    for (gap, level) in [
        (1000.0_f64, Level::Apprentice),
        (250.0, Level::Band),
        (125.0, Level::Stage),
    ] {
        let p = (0.5 * gap).min(250.0);
        let close = L_RT + p + MARGIN;
        let mut b = Bench::new(level, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, gap);
        let mut first = None;
        let mut now = -999.0;
        while now < 1500.0 {
            let out = b.progress(now);
            if !out.is_empty() && first.is_none() {
                first = Some((now, out));
            }
            now += 1.0;
        }
        let (at, out) = first.expect("a verdict");
        assert!(
            at > close && at <= close + 1.0,
            "gap {gap}: at {at}, close {close}"
        );
        let want = PulseVerdict {
            id: 0,
            attack: Attack::NotHeard,
            timing: Timing::NotJudged(Why::NoAttack),
            dt_ms: None,
            dir: Direction::NotJudged(Why::GateLocked),
            onset_in: None,
            pos_in_bar: 0,
            fin: true,
        };
        assert_eq!(out, vec![JudgeOut::Verdict(want)]);
        assert_eq!(b.all.len(), 1, "said once");

        // the edge itself, and a strum that arrives in the last instant
        let mut b = Bench::new(level, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, gap);
        assert!(b.progress(close - 0.01).is_empty());
        let late = verdicts(&b.strum(0.0, p - 0.01));
        assert_eq!(late[0].attack, Attack::Heard);
        let out = b.progress(close + 0.01);
        assert_eq!(finals(&out)[0].attack, Attack::Heard);
        assert_eq!(b.j.counters().late_strums, 0);
        check_stream(&b.all);
    }
}

#[test]
fn t06_a_strum_after_its_cell_closed_takes_the_accusation_back() {
    // The margin is wrong on purpose (50 ms): the strum arrives after `NotHeard`.
    let cfg = JudgeConfig {
        close_margin_ms: 50.0,
        ..JudgeConfig::new(FS, Level::Apprentice)
    };
    let mut b = Bench::with(cfg, loopback(), &GateTable::builtin());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let out = b.progress(L_RT + 250.0 + 51.0);
    assert_eq!(finals(&out)[0].attack, Attack::NotHeard);
    let out = b.strum(0.0, 240.0);
    assert_eq!(
        out,
        vec![JudgeOut::Withdrawn {
            id: 0,
            why: Why::Glitch
        }]
    );
    assert_eq!(b.j.counters().late_strums, 1);
    assert!(not_judged(&b.result(0), Why::Glitch));
    // a second late strum is an extra: there is nothing left to take back
    assert_eq!(extras(&b.strum(0.0, 200.0)).len(), 1);
}

#[test]
fn t06_a_late_strum_takes_outside_back_when_it_undermines_it() {
    // The margin is wrong on purpose (50 ms), so strums can arrive after the final verdict.
    let cfg = JudgeConfig {
        close_margin_ms: 50.0,
        ..JudgeConfig::new(FS, Level::Apprentice)
    };
    let glitch = vec![JudgeOut::Withdrawn {
        id: 0,
        why: Why::Glitch,
    }];
    let outside = || {
        let mut b = Bench::with(cfg, loopback(), &GateTable::builtin());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        b.strum(0.0, -200.0);
        let out = b.progress(L_RT + 250.0 + 51.0);
        assert_eq!(finals(&out)[0].timing, Timing::Outside);
        b
    };

    // A late strum farther than the one that was judged changes nothing; a nearer one
    // means the event was not played far off after all.
    let mut b = outside();
    assert_eq!(extras(&b.strum(0.0, 230.0)).len(), 1);
    assert!(withdrawn(&b.all).is_empty());
    assert_eq!(b.strum(0.0, 10.0), glitch);
    assert_eq!(b.j.counters().late_strums, 2);
    assert!(not_judged(&b.result(0), Why::Glitch));
    // nothing is left to take back
    assert_eq!(extras(&b.strum(0.0, 5.0)).len(), 1);
    check_stream(&b.all);

    // A late strum that cannot be placed may have been the nearer one, wherever it reads.
    let mut b = outside();
    let t_in = b.t_in(0.0, 240.0);
    let out = b.push(JudgeInput::Strum {
        t_in,
        judged: false,
    });
    assert_eq!(out, glitch);

    // A hit is final: a late strum never touches it, however near.
    let mut b = Bench::with(cfg, loopback(), &GateTable::builtin());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.strum(0.0, 60.0);
    b.progress(L_RT + 250.0 + 51.0);
    assert_eq!(extras(&b.strum(0.0, 1.0)).len(), 1);
    let t_in = b.t_in(0.0, 2.0);
    let out = b.push(JudgeInput::Strum {
        t_in,
        judged: false,
    });
    assert_eq!(extras(&out).len(), 1);
    assert!(withdrawn(&b.all).is_empty());
    assert!(b.result(0).timing == Timing::InWindow && near(&b.result(0), 60.0));
}

// ---------------------------------------------------------------------------------------
// (7) Not judged

#[test]
fn t07_unjudged_window_before_the_verdict() {
    // the click inside the window
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    assert!(b.unjudged(-10.0, 10.0).is_empty());
    let p = verdicts(&b.strum(0.0, 5.0))[0];
    assert!(!p.fin && not_judged(&p, Why::Glitch), "{p:?}");
    assert_eq!(p.onset_in, Some(b.t_in(0.0, 5.0)));
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::Glitch));

    // only the strum inside the window (the window came first)
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.unjudged(L_RT + 20.0, L_RT + 40.0);
    let fl = b.heard(0.0, 30.0);
    assert_eq!(flashes(&fl), vec![(Some(0), None)], "no error shown for it");
    assert!(not_judged(&verdicts(&b.strum(0.0, 30.0))[0], Why::Glitch));
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::Glitch));

    // the window comes between the provisional and the final verdict
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    assert_eq!(verdicts(&b.strum(0.0, 30.0))[0].timing, Timing::InWindow);
    let out = b.unjudged(L_RT + 20.0, L_RT + 40.0);
    let v = verdicts(&out);
    assert_eq!(v.len(), 1, "{out:?}");
    assert!(!v[0].fin && not_judged(&v[0], Why::Glitch));
    assert!(withdrawn(&out).is_empty(), "nothing final to withdraw yet");
    let out = b.progress(2000.0);
    assert!(finals(&out)[0].fin && not_judged(&finals(&out)[0], Why::Glitch));
    check_stream(&b.all);
}

#[test]
fn t07_unjudged_window_after_the_verdict_withdraws_it() {
    for (from, to) in [(-5.0, 5.0), (L_RT + 25.0, L_RT + 35.0)] {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        b.expect(1, 1000.0, 1000.0);
        b.strum(0.0, 30.0);
        b.progress(900.0);
        assert_eq!(b.result(0).timing, Timing::InWindow);
        let out = b.unjudged(from, to);
        assert_eq!(
            out,
            vec![JudgeOut::Withdrawn {
                id: 0,
                why: Why::Glitch
            }]
        );
        assert!(b.unjudged(from, to).is_empty(), "withdrawn once");
        assert!(not_judged(&b.result(0), Why::Glitch));
        // the next event is untouched
        b.strum(1000.0, -20.0);
        b.progress(3000.0);
        assert!(b.result(1).timing == Timing::Center && near(&b.result(1), -20.0));
        check_stream(&b.all);
    }
}

#[test]
fn t07_only_the_events_of_the_window() {
    // Eight events 500 ms apart, all played 10 ms late. An audio glitch at 2000 ms makes
    // the engine send the window [1900, 2250]. Event 4 has its click (2000) and its strum
    // (2097.3) in it; no other event has either.
    let beats = grid(8, 500.0);
    let strums: Vec<f64> = beats.iter().map(|b| b.g_ms + L_RT + 10.0).collect();
    for arrives in [2300.0, 3500.0] {
        let mut b = Bench::new(Level::Apprentice, loopback());
        let window = JudgeInput::Unjudged {
            from_in: b.frame(1900.0),
            to_in: b.frame(2250.0),
        };
        b.play_with(&beats, &strums, &[(arrives, window)], 6000.0);
        check_stream(&b.all);
        if arrives > 2700.0 {
            // after the final verdict: withdrawn
            assert_eq!(withdrawn(&b.all), vec![(4, Why::Glitch)]);
        } else {
            // before it: the provisional verdict is taken back, then the final one agrees
            assert!(withdrawn(&b.all).is_empty());
            let v4: Vec<_> = verdicts(&b.all).into_iter().filter(|v| v.id == 4).collect();
            assert_eq!(v4.len(), 3, "{v4:?}");
            assert!(!v4[0].fin && v4[0].timing == Timing::Center);
            assert!(!v4[1].fin && not_judged(&v4[1], Why::Glitch));
            assert!(v4[2].fin && not_judged(&v4[2], Why::Glitch));
        }
        for k in 0..8 {
            let v = b.result(k);
            if k == 4 {
                assert!(not_judged(&v, Why::Glitch), "{v:?}");
            } else {
                assert!(v.timing == Timing::Center && near(&v, 10.0), "{k}: {v:?}");
            }
        }
    }
}

#[test]
fn t07_an_accusation_needs_a_cell_that_was_heard_whole() {
    // No strum, and a window over part of the cell (not over the click): the attack may be
    // in the lost audio. Not judged, not "not heard".
    let cell = L_RT; // center of the cell of the click at 0; the cell is ±250 ms around it
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.unjudged(cell + 100.0, cell + 180.0);
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::Glitch));

    // the same window far from the cell changes nothing
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.unjudged(cell + 251.0, cell + 400.0);
    b.progress(2000.0);
    assert_eq!(b.result(0).attack, Attack::NotHeard);

    // a `NotHeard` already given is withdrawn by a late window over the cell
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.progress(2000.0);
    assert_eq!(b.result(0).attack, Attack::NotHeard);
    let out = b.unjudged(cell - 240.0, cell - 200.0);
    assert_eq!(
        out,
        vec![JudgeOut::Withdrawn {
            id: 0,
            why: Why::Glitch
        }]
    );

    // "outside" keeps the attack and loses the timing; "in time" stands
    for (e, want_timing) in [
        (200.0, Timing::NotJudged(Why::Glitch)),
        (30.0, Timing::InWindow),
    ] {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        b.strum(0.0, e);
        b.unjudged(cell - 240.0, cell - 200.0);
        b.progress(2000.0);
        let v = b.result(0);
        assert_eq!((v.attack, v.timing), (Attack::Heard, want_timing), "e {e}");
        assert_eq!(v.dt_ms.is_some(), e < 100.0);
        check_stream(&b.all);
    }
}

#[test]
fn t07_judged_false_is_a_glitch() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let t_in = b.t_in(0.0, 5.0);
    let fl = b.push(JudgeInput::Heard {
        t_in,
        judged: false,
    });
    assert_eq!(flashes(&fl), vec![(Some(0), None)]);
    let out = b.push(JudgeInput::Strum {
        t_in,
        judged: false,
    });
    assert!(not_judged(&verdicts(&out)[0], Why::Glitch));
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::Glitch));

    // A strum that cannot be trusted never replaces one that can, however near;
    // one that can replaces one that cannot only if it is nearer.
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.strum(0.0, 60.0);
    let t_in = b.t_in(0.0, 1.0);
    let out = b.push(JudgeInput::Strum {
        t_in,
        judged: false,
    });
    assert_eq!(out, vec![JudgeOut::Extra { t_in }]);
    b.progress(2000.0);
    assert!(b.result(0).timing == Timing::InWindow && near(&b.result(0), 60.0));

    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    let t_in = b.t_in(0.0, 60.0);
    b.push(JudgeInput::Strum {
        t_in,
        judged: false,
    });
    assert_eq!(
        extras(&b.strum(0.0, 200.0)).len(),
        1,
        "farther: stays not judged"
    );
    assert!(not_judged(verdicts(&b.all).last().unwrap(), Why::Glitch));
    let out = b.strum(0.0, 10.0);
    assert_eq!(verdicts(&out)[0].timing, Timing::Center, "nearer and clean");

    // a strum with no time at all: every cell that was listening loses its accusation
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.expect(1, 1000.0, 1000.0);
    b.progress(100.0);
    assert!(
        b.push(JudgeInput::Strum {
            t_in: f64::NAN,
            judged: true
        })
        .is_empty()
    );
    b.progress(3000.0);
    assert!(not_judged(&b.result(0), Why::Glitch));
    assert_eq!(
        b.result(1).attack,
        Attack::NotHeard,
        "not yet listening then"
    );
}

#[test]
fn t07_clock_not_locked() {
    // not locked when the click sounds
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.unlocked(20.0);
    b.progress(60.0);
    b.progress(70.0);
    let p = verdicts(&b.strum(0.0, 5.0))[0];
    assert!(not_judged(&p, Why::ClockNotLocked), "{p:?}");
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::ClockNotLocked));

    // not locked when the strum is paired
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.progress(50.0);
    b.unlocked(120.0);
    let fl = b.heard(0.0, 5.0);
    assert_eq!(flashes(&fl), vec![(Some(0), None)]);
    assert!(not_judged(
        &verdicts(&b.strum(0.0, 5.0))[0],
        Why::ClockNotLocked
    ));
    b.progress(2000.0);
    b.progress(2010.0);
    assert!(not_judged(&b.result(0), Why::ClockNotLocked));

    // the lock is lost after a clean provisional verdict and the click is not yet past:
    // the verdict is taken back at once
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    assert_eq!(verdicts(&b.strum(0.0, -150.0))[0].timing, Timing::Outside);
    let out = b.unlocked(10.0);
    assert!(not_judged(&verdicts(&out)[0], Why::ClockNotLocked));

    // no strum, and the lock is lost while the cell listens: not "not heard"
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.progress(50.0);
    b.unlocked(200.0);
    b.progress(210.0);
    b.progress(2000.0);
    assert!(not_judged(&b.result(0), Why::ClockNotLocked));

    // lost and found again long before the cell: judged as usual
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-3000.0);
    b.expect(0, 0.0, 1000.0);
    b.unlocked(-1500.0);
    b.progress(-1400.0);
    b.progress(-1300.0);
    b.progress(50.0);
    assert_eq!(verdicts(&b.strum(0.0, 5.0))[0].timing, Timing::Center);
    b.progress(2000.0);
    assert_eq!(b.result(0).timing, Timing::Center);

    // before any clock report nothing is known: not judged
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.expect(0, 0.0, 1000.0);
    assert!(not_judged(
        &verdicts(&b.strum(0.0, 5.0))[0],
        Why::ClockNotLocked
    ));

    // a glitch outranks the clock
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.unlocked(20.0);
    b.unjudged(-10.0, 10.0);
    b.progress(2000.0);
    b.progress(2010.0);
    assert!(not_judged(&b.result(0), Why::Glitch));
    // a click the clock could not place at all
    let mut b = Bench::new(Level::Apprentice, loopback());
    let out = b.push(JudgeInput::Expect {
        id: 9,
        g_in: f64::NAN,
        kind: ExpectKind::Attack,
        prev_gap_s: 1.0,
        next_gap_s: 1.0,
        pos_in_bar: 0,
    });
    assert!(finals(&out)[0].fin && not_judged(&finals(&out)[0], Why::ClockNotLocked));
}

#[test]
fn t07_reset_ends_every_open_event_as_a_glitch() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 500.0);
    b.expect(1, 500.0, 500.0);
    b.expect_kind(2, 1000.0, 500.0, ExpectKind::Ghost);
    b.expect(3, 1500.0, 500.0);
    b.strum(0.0, 5.0);
    b.progress(700.0); // event 0 is final and stays; 1 has no strum yet
    assert_eq!(b.result(0).timing, Timing::Center);
    b.strum(500.0, 20.0);
    let out = b.push(JudgeInput::Reset);
    let f = finals(&out);
    assert_eq!(f.len(), 3, "{out:?}");
    assert_eq!(out.len(), 3);
    assert!(not_judged(&f[0], Why::Glitch) && f[0].id == 1);
    assert!(not_judged(&f[1], Why::Ghost) && f[1].id == 2);
    assert!(not_judged(&f[2], Why::Glitch) && f[2].id == 3);
    // nothing more about them, whatever comes
    assert!(b.progress(9000.0).is_empty());
    assert!(b.push(JudgeInput::Dispute).is_empty());
    assert!(
        b.unjudged(-100.0, 100.0).is_empty(),
        "event 0 was forgotten"
    );
    check_stream(&b.all);
    assert_eq!(b.result(0).timing, Timing::Center);
    // a new epoch starts clean: the clock must be seen locked again
    assert!(b.push(JudgeInput::Reset).is_empty());
    b.expect(10, 10_000.0, 500.0);
    assert!(not_judged(
        &verdicts(&b.strum(10_000.0, 0.0))[0],
        Why::ClockNotLocked
    ));
}

#[test]
fn t07_an_attack_that_cannot_be_placed_leaves_no_accusation_standing() {
    let fresh = || {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        b
    };
    let untrusted = |b: &mut Bench, e: f64| {
        let t_in = b.t_in(0.0, e);
        b.push(JudgeInput::Strum {
            t_in,
            judged: false,
        })
    };
    // heard, and nothing said about its timing
    let abstains = |v: &PulseVerdict, why: Why| {
        v.attack == Attack::Heard && v.timing == Timing::NotJudged(why) && v.dt_ms.is_none()
    };

    // A clean attack far off, then one right on the click in a stretch the engine could
    // not judge. The second cannot replace the first, and the first can no longer be
    // called far off: the best-timed attack is the one whose time is not known.
    let mut b = fresh();
    assert_eq!(verdicts(&b.strum(0.0, -200.0))[0].timing, Timing::Outside);
    let out = untrusted(&mut b, 0.0);
    assert_eq!(extras(&out), vec![b.t_in(0.0, 0.0)]);
    let v = verdicts(&out);
    assert_eq!(v.len(), 1, "{out:?}");
    assert!(!v[0].fin && abstains(&v[0], Why::Glitch), "{v:?}");
    assert_eq!(v[0].onset_in, Some(b.t_in(0.0, -200.0)));
    b.progress(2000.0);
    check_stream(&b.all);
    assert!(abstains(&b.result(0), Why::Glitch), "{:?}", b.result(0));
    let s = b.log().stats(&loopback());
    assert_eq!((s.heard, s.outside, s.timed), (1, 0, 0));

    // It need not read nearer: where it really fell is exactly what is not known.
    let mut b = fresh();
    b.strum(0.0, -200.0);
    untrusted(&mut b, 240.0);
    b.progress(2000.0);
    assert!(abstains(&b.result(0), Why::Glitch));

    // The other order. The untrusted attack comes first: nothing is judged. A clean one
    // that is nearer replaces it, and if that one is far off it is still not accused.
    let mut b = fresh();
    assert!(not_judged(
        &verdicts(&untrusted(&mut b, -240.0))[0],
        Why::Glitch
    ));
    let out = b.strum(0.0, -200.0);
    assert!(abstains(&verdicts(&out)[0], Why::Glitch), "{out:?}");
    b.progress(2000.0);
    assert!(abstains(&b.result(0), Why::Glitch));

    // A confirmed hit stays a hit, before and after.
    let mut b = fresh();
    b.strum(0.0, 60.0);
    assert_eq!(
        untrusted(&mut b, 0.0).len(),
        1,
        "an extra, and no new verdict"
    );
    b.progress(2000.0);
    assert!(b.result(0).timing == Timing::InWindow && near(&b.result(0), 60.0));
    let mut b = fresh();
    untrusted(&mut b, -240.0);
    assert_eq!(verdicts(&b.strum(0.0, 10.0))[0].timing, Timing::Center);
    b.progress(2000.0);
    assert!(b.result(0).timing == Timing::Center && near(&b.result(0), 10.0));

    // The same when the clock had lost its lock as the nearer attack arrived.
    let mut b = fresh();
    b.strum(0.0, -200.0);
    b.unlocked(-300.0); // before the cell starts to listen
    let out = b.strum(0.0, 0.0);
    assert_eq!(extras(&out).len(), 1);
    assert!(abstains(&verdicts(&out)[0], Why::ClockNotLocked), "{out:?}");
}

// ---------------------------------------------------------------------------------------
// (8) No calibration

#[test]
fn t08_without_a_basis_there_is_only_the_flash() {
    for timing in [
        TimingRef::none(),
        TimingRef::estimate(100.0, 20.0, 16), // too scattered to judge with
        TimingRef::loopback(f64::NAN, 1.0),
    ] {
        let beats = grid(8, 500.0);
        let strums: Vec<f64> = beats.iter().map(|b| b.g_ms + 90.0).collect();
        let mut b = Bench::new(Level::Apprentice, timing);
        b.play(&beats, &strums, 6000.0);
        check_stream(&b.all);
        let v = verdicts(&b.all);
        assert_eq!(
            v.len(),
            8,
            "one final verdict per event, nothing provisional"
        );
        for v in &v {
            assert!(v.fin && not_judged(v, Why::NoCalibration), "{v:?}");
        }
        assert_eq!(flashes(&b.all), vec![(None, None); 8], "heard, and no more");
        assert!(extras(&b.all).is_empty());
        assert!(withdrawn(&b.all).is_empty());
        let s = b.log().stats(&timing);
        assert_eq!((s.events, s.not_judged, s.heard, s.timed), (8, 8, 0, 0));
    }
    // a rate that is not a rate: nothing is judged, and the events still end
    let cfg = JudgeConfig::new(0.0, Level::Apprentice);
    let mut j = PulseJudge::new(cfg, loopback(), &GateTable::builtin());
    let mut out = Vec::new();
    j.push(
        &JudgeInput::Expect {
            id: 0,
            g_in: 0.0,
            kind: ExpectKind::Attack,
            prev_gap_s: 1.0,
            next_gap_s: 1.0,
            pos_in_bar: 0,
        },
        &mut out,
    );
    j.push(
        &JudgeInput::Progress {
            in_now: 1.0e9,
            locked: true,
        },
        &mut out,
    );
    assert!(not_judged(&finals(&out)[0], Why::NoCalibration));
}

// ---------------------------------------------------------------------------------------
// (9) Gates

fn gates_with(locked: &[VerdictType]) -> GateTable {
    let mut g = GateTable::builtin();
    for &t in locked {
        g.set(
            t,
            GateEntry {
                state: GateState::Locked,
                rule: GateRule::default(),
                evidence: GateEvidence::default(),
                reason: "test".into(),
            },
        );
    }
    g
}

#[test]
fn t09_timing_gate_locked_still_reports_the_attack() {
    let gates = gates_with(&[VerdictType::Timing]);
    let cfg = JudgeConfig::new(FS, Level::Apprentice);
    let mut b = Bench::with(cfg, loopback(), &gates);
    let beats = grid(4, 500.0);
    // one strum before every cell; events 0 and 2 are played (one far off), 1 and 3 not
    let strums = [L_RT - 300.0, L_RT + 5.0, 1000.0 + L_RT + 200.0];
    b.play(&beats, &strums, 4000.0);
    check_stream(&b.all);
    for (k, heard) in [(0, true), (1, false), (2, true), (3, false)] {
        let v = b.result(k);
        if heard {
            assert_eq!(v.attack, Attack::Heard);
            assert_eq!(v.timing, Timing::NotJudged(Why::GateLocked));
            assert!(v.onset_in.is_some());
        } else {
            assert_eq!(v.attack, Attack::NotHeard);
            assert_eq!(v.timing, Timing::NotJudged(Why::NoAttack));
        }
        assert_eq!(v.dt_ms, None);
    }
    assert_eq!(extras(&b.all), vec![b.frame(L_RT - 300.0)]);
    assert_eq!(
        flashes(&b.all),
        vec![(None, None), (Some(0), None), (Some(2), None)],
        "heard, and no error on screen"
    );
    let s = b.log().stats(&loopback());
    assert_eq!((s.heard, s.not_heard, s.timed), (2, 2, 0));
    assert_eq!(s.center + s.in_window + s.outside, 0);

    // the attack gate locked: nothing at all is claimed
    for locked in [
        vec![VerdictType::Attack],
        vec![VerdictType::Attack, VerdictType::Timing],
    ] {
        let mut b = Bench::with(cfg, loopback(), &gates_with(&locked));
        b.play(&beats, &strums, 4000.0);
        for v in finals(&b.all) {
            assert!(not_judged(&v, Why::GateLocked), "{v:?}");
        }
        assert_eq!(finals(&b.all).len(), 4);
        assert!(extras(&b.all).is_empty());
    }
    // no calibration under a locked timing gate: each says its own reason
    let mut b = Bench::with(cfg, TimingRef::none(), &gates);
    b.play(&beats, &strums, 4000.0);
    let v = b.result(0);
    assert_eq!(v.attack, Attack::NotJudged(Why::NoCalibration));
    assert_eq!(v.timing, Timing::NotJudged(Why::GateLocked));
    // the builtin table judges attack and timing; direction never
    assert!(GateTable::builtin().judges(VerdictType::Attack));
    assert!(GateTable::builtin().judges(VerdictType::Timing));
    assert!(!GateTable::builtin().judges(VerdictType::Direction));
}

#[test]
fn t09_events_too_close_to_tell_apart_are_not_judged() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 40.0);
    b.expect(1, 40.0, 40.0);
    b.expect(2, 1000.0, 960.0);
    assert!(b.strum(0.0, 3.0).is_empty(), "swallowed");
    b.strum(1000.0, 3.0);
    b.progress(3000.0);
    assert!(not_judged(&b.result(0), Why::GateLocked));
    assert!(not_judged(&b.result(1), Why::GateLocked));
    assert_eq!(b.result(2).timing, Timing::Center);
    assert!(extras(&b.all).is_empty());
    assert!(!b.j.shape(0.04, 0.04).judged);
    assert!(b.j.shape(0.0625, 2.0).judged);
    assert!(!b.j.shape(f64::NAN, f64::NAN).judged);
}

// ---------------------------------------------------------------------------------------
// (11) Dispute

/// Four quarter notes at 80 BPM: in the center, far off, not played, in time.
const ROUND: [Option<f64>; 4] = [Some(5.0), Some(200.0), None, Some(60.0)];
/// The event a press right after each final verdict contests: that one, hit or not.
const ROUND_TARGET: [u32; 4] = [0, 1, 2, 3];

/// Play [`ROUND`]; press the key right after the final verdict of `dispute_after`.
/// Returns the bench and what the judge answered to the press.
fn disputed_round(dispute_after: Option<u32>) -> (Bench, Vec<JudgeOut>) {
    let gap = 750.0;
    let mut b = Bench::new(Level::Apprentice, loopback());
    let mut answer = Vec::new();
    b.progress(-1000.0);
    for k in 0..4 {
        b.expect(k, f64::from(k) * gap, gap);
    }
    for (k, e) in ROUND.iter().enumerate() {
        let g = k as f64 * gap;
        if let Some(e) = e {
            b.strum(g, *e);
        }
        b.progress(g + L_RT + 250.0 + MARGIN + 1.0);
        if dispute_after == Some(k as u32) {
            answer = b.push(JudgeInput::Dispute);
        }
    }
    b.progress(9000.0);
    check_stream(&b.all);
    (b, answer)
}

fn disputed(id: u32) -> Vec<JudgeOut> {
    vec![JudgeOut::Withdrawn {
        id,
        why: Why::Disputed,
    }]
}

#[test]
fn t11_dispute_never_adds_a_hit() {
    let t = loopback();
    let plain = disputed_round(None).0;
    let base = plain.log().stats(&t);
    assert_eq!((base.events, base.heard, base.not_heard), (4, 3, 1));
    assert_eq!((base.center, base.in_window, base.outside), (1, 1, 1));
    let hits = |s: &guitarness_core::stats::RoundStats| s.center + s.in_window;
    for k in 0..4u32 {
        let (b, answer) = disputed_round(Some(k));
        let target = ROUND_TARGET[k as usize];
        assert_eq!(answer, disputed(target), "press after event {k}");
        let v = b.result(target);
        assert!(not_judged(&v, Why::Disputed), "{k}: {v:?}");
        assert!(!is_hit(&v) && v.attack != Attack::Heard);
        let s = b.log().stats(&t);
        assert_eq!(s.disputed, 1);
        assert_eq!(s.events, 4);
        assert!(hits(&s) <= hits(&base), "{k}: hits went up");
        assert!(s.heard <= base.heard, "{k}: heard went up");
        assert!(s.center <= base.center && s.in_window <= base.in_window);
        assert_eq!(s.not_judged, 0);
        // out of the statistics: one judged event less, on the side it was on
        let judged = |s: &guitarness_core::stats::RoundStats| s.heard + s.not_heard;
        assert_eq!(judged(&s), judged(&base) - 1);
        match ROUND[target as usize] {
            None => assert_eq!((s.heard, s.not_heard), (base.heard, 0)),
            Some(_) => assert_eq!((s.heard, s.not_heard), (base.heard - 1, 1)),
        }
        // the other events are as they were
        for other in (0..4).filter(|&o| o != target) {
            assert_eq!(b.result(other), plain.result(other));
        }
    }
}

#[test]
fn t11_dispute_goes_to_the_last_final_verdict() {
    let gap = 750.0;
    let closed = |g: f64| g + L_RT + 250.0 + MARGIN + 1.0;
    let fresh = || {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        for k in 0..4 {
            b.expect(k, f64::from(k) * gap, gap);
        }
        b
    };
    let press = |b: &mut Bench| b.push(JudgeInput::Dispute);

    // No final verdict yet: nothing happens, also with a provisional one on screen.
    let mut b = fresh();
    assert!(press(&mut b).is_empty());
    assert_eq!(verdicts(&b.strum(0.0, 200.0))[0].timing, Timing::Outside);
    assert!(
        press(&mut b).is_empty(),
        "a provisional verdict is not contested"
    );

    // Event 0 was not played and its `NotHeard` is final. Then event 1 shows a provisional
    // hit. The press contests event 0: a provisional verdict after it changes nothing.
    let mut b = fresh();
    b.progress(closed(0.0));
    assert_eq!(b.result(0).attack, Attack::NotHeard);
    assert!(is_hit(&verdicts(&b.strum(gap, 5.0))[0]));
    assert_eq!(press(&mut b), disputed(0));
    assert!(
        press(&mut b).is_empty(),
        "pressing again contests nothing more"
    );
    b.progress(9000.0);
    check_stream(&b.all);
    assert!(not_judged(&b.result(0), Why::Disputed));
    assert_eq!(b.result(1).timing, Timing::Center);
    // a glitch window later does not speak over the player
    assert!(b.unjudged(-50.0, 50.0).is_empty());
    assert!(not_judged(&b.result(0), Why::Disputed));

    // A final verdict that only confirms its provisional is the target all the same, and a
    // nearer strum after the press does not turn the contested event into a hit.
    let mut b = fresh();
    assert_eq!(verdicts(&b.strum(0.0, 200.0))[0].timing, Timing::Outside);
    let f = finals(&b.progress(closed(0.0)));
    assert!(f.len() == 1 && f[0].timing == Timing::Outside, "{f:?}");
    assert_eq!(press(&mut b), disputed(0));
    assert_eq!(extras(&b.strum(0.0, 3.0)).len(), 1);
    b.progress(9000.0);
    check_stream(&b.all);
    assert!(not_judged(&b.result(0), Why::Disputed));

    // Eighths at 120 BPM: event 1 shows "outside" before event 0 becomes final, and the
    // final verdict of event 0 only confirms its hit. The press contests that hit; the
    // provisional accusation of event 1 is not the last final verdict.
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 250.0);
    b.expect(1, 250.0, 250.0);
    b.strum(0.0, 5.0);
    assert_eq!(verdicts(&b.strum(250.0, 100.0))[0].timing, Timing::Outside);
    let out = b.progress(L_RT + 125.0 + MARGIN + 1.0);
    assert_eq!(finals(&out).len(), 1, "event 0 is final, event 1 is not");
    assert_eq!(press(&mut b), disputed(0));
    b.progress(5000.0);
    check_stream(&b.all);
    assert!(not_judged(&b.result(0), Why::Disputed));
    assert_eq!(b.result(1).timing, Timing::Outside);
    // the next press goes to the new last final verdict
    assert_eq!(press(&mut b), disputed(1));
    let s = b.log().stats(&loopback());
    assert_eq!((s.center, s.in_window, s.outside, s.disputed), (0, 0, 0, 2));

    // A last final verdict that is out of the statistics already has nothing to contest,
    // and the verdict before it is not reached for.
    let mut b = fresh();
    b.strum(0.0, 5.0);
    b.progress(closed(0.0));
    b.progress(closed(gap));
    assert_eq!(
        withdrawn(&b.unjudged(gap - 10.0, gap + 10.0)),
        vec![(1, Why::Glitch)]
    );
    assert!(press(&mut b).is_empty());
    assert_eq!(b.result(0).timing, Timing::Center);
    // a final verdict since: a press of its own
    b.strum(2.0 * gap, 5.0);
    b.progress(closed(2.0 * gap));
    assert_eq!(press(&mut b), disputed(2));

    // A ghost shows nothing: its final verdict is never the target.
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, gap);
    b.expect_kind(1, gap, gap, ExpectKind::Ghost);
    b.progress(closed(gap));
    assert_eq!(b.result(1).attack, Attack::NotJudged(Why::Ghost));
    assert_eq!(press(&mut b), disputed(0));

    // `Reset` starts a new round: nothing to contest until a final verdict in it.
    b.push(JudgeInput::Reset);
    assert!(press(&mut b).is_empty());
}

/// What the player has on screen, rebuilt from the judge's output and nothing else.
#[derive(Default)]
struct Screen {
    /// The event of the last final verdict (a ghost shows nothing), and whether that
    /// verdict still counts in the statistics.
    last_final: Option<(u32, bool)>,
}

impl Screen {
    fn take(&mut self, o: &JudgeOut) {
        match *o {
            JudgeOut::Verdict(v) if v.fin && v.attack != Attack::NotJudged(Why::Ghost) => {
                let counts = matches!(v.attack, Attack::Heard | Attack::NotHeard);
                self.last_final = Some((v.id, counts));
            }
            JudgeOut::Withdrawn { id, .. } => {
                if let Some((last, counts)) = &mut self.last_final
                    && *last == id
                {
                    *counts = false;
                }
            }
            _ => {}
        }
    }

    /// What a press must contest.
    fn target(&self) -> Option<u32> {
        self.last_final
            .and_then(|(id, counts)| counts.then_some(id))
    }
}

#[test]
fn t11_every_press_contests_the_last_final_verdict() {
    for (seed, who) in [
        (5u64, Player::ROUGH),
        (6, Player::ROUGH),
        (7, Player::TIDY),
        (8, Player::TIDY),
    ] {
        // the busy round, with every third press doubled or tripled
        let mut rng = Rng(seed);
        let mut inputs = Vec::new();
        for i in busy_stream_of(seed, 2000, who) {
            inputs.push(i);
            if i == JudgeInput::Dispute && rng.chance(0.35) {
                inputs.push(i);
                if rng.chance(0.3) {
                    inputs.push(i);
                }
            }
        }
        let mut j = PulseJudge::new(
            JudgeConfig::new(FS, Level::Band),
            loopback(),
            &GateTable::builtin(),
        );
        let mut screen = Screen::default();
        let mut out = Vec::new();
        let (mut presses, mut contested, mut idle) = (0, 0, 0);
        for i in &inputs {
            out.clear();
            j.push(i, &mut out);
            if *i == JudgeInput::Dispute {
                presses += 1;
                let want: Vec<JudgeOut> = screen.target().map_or(vec![], disputed);
                assert_eq!(out, want, "seed {seed}: press {presses}");
                if want.is_empty() {
                    idle += 1;
                } else {
                    contested += 1;
                }
            }
            for o in &out {
                screen.take(o);
            }
            if *i == JudgeInput::Reset {
                screen = Screen::default();
            }
        }
        // both branches walked: a verdict contested, a press that found nothing
        println!("seed {seed}: {presses} presses: {contested} contested, {idle} idle");
        assert!(contested > 50 && idle > 20, "seed {seed}");

        // and no press ever gave anything: against the same round without the key, every
        // event is as it was or is out of the statistics
        let plain: Vec<JudgeInput> = inputs
            .iter()
            .copied()
            .filter(|i| *i != JudgeInput::Dispute)
            .collect();
        let log = |inputs: &[JudgeInput]| {
            let mut l = VerdictLog::new();
            for o in run(inputs, loopback()) {
                l.push(&o);
            }
            l
        };
        let (with, without) = (log(&inputs), log(&plain));
        assert_eq!(with.verdicts().len(), without.verdicts().len());
        let mut changed = 0;
        for (a, b) in with.verdicts().iter().zip(without.verdicts()) {
            assert_eq!(a.id, b.id);
            if a != b {
                assert!(not_judged(a, Why::Disputed), "seed {seed}: {a:?} was {b:?}");
                changed += 1;
            }
        }
        assert!(changed > 50, "seed {seed}: {changed}");
    }
}

// ---------------------------------------------------------------------------------------
// Contract with the caller

#[test]
fn a_second_expect_refines_an_untouched_cell() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 1000.0);
    b.expect(0, 20.0, 1000.0); // the clock moved: the click is captured 20 ms later
    assert_eq!(b.j.counters().ignored_expects, 0);
    let v = verdicts(&b.strum(0.0, 0.0))[0];
    assert!(near(&v, -20.0), "{v:?}");
    // once a strum is paired the cell is frozen
    b.expect(0, 0.0, 1000.0);
    assert_eq!(b.j.counters().ignored_expects, 1);
    b.progress(2000.0);
    assert!(near(&b.result(0), -20.0));
    // and a closed id is not opened again
    assert!(b.expect(0, 5000.0, 1000.0).is_empty());
    assert_eq!(b.j.counters().ignored_expects, 2);
    assert!(b.progress(9000.0).is_empty());
    check_stream(&b.all);
}

#[test]
fn too_many_open_cells_are_refused_in_the_open() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    for k in 0..OPEN_CELLS as u32 {
        assert!(b.expect(k, f64::from(k) * 500.0, 500.0).is_empty());
    }
    let k = OPEN_CELLS as u32;
    let out = b.expect(k, f64::from(k) * 500.0, 500.0);
    assert!(finals(&out)[0].id == k && not_judged(&finals(&out)[0], Why::Glitch));
    assert_eq!(b.j.counters().overflowed_expects, 1);
}

#[test]
fn early_notice_shows_the_error_only_when_it_may() {
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    b.expect(0, 0.0, 500.0);
    let fl = flashes(&b.heard(0.0, -42.0));
    assert_eq!(fl[0].0, Some(0));
    assert!((f64::from(fl[0].1.unwrap()) + 42.0).abs() < 0.01);
    // between cells
    assert_eq!(flashes(&b.heard(0.0, 300.0)), vec![(None, None)]);
    // a notice changes nothing: the event is still unanswered
    b.progress(2000.0);
    assert_eq!(b.result(0).attack, Attack::NotHeard);
    assert_eq!(verdicts(&b.all).len(), 1);
}

#[test]
fn an_event_is_not_judged_twice_however_long_ago_it_was() {
    // more events than the judge keeps
    let n = CLOSED_CELLS as u32 + 76;
    let gap = 500.0;
    let mut b = Bench::new(Level::Apprentice, loopback());
    b.progress(-1000.0);
    for k in 0..n {
        let g = f64::from(k) * gap;
        b.expect(k, g, gap);
        b.strum(g, 5.0);
        b.progress(g + L_RT + 250.0 + MARGIN + 1.0);
    }
    assert_eq!(finals(&b.all).len(), n as usize);
    assert_eq!(b.result(0).timing, Timing::Center);
    // The caller repeats ids (a fault on its side): one that the judge forgot long ago,
    // one that it still holds, one that was never used but is out of order.
    let again = f64::from(n + 4) * gap;
    b.expect(n + 3, again, gap);
    for id in [0, n - 1, n + 1] {
        assert!(b.expect(id, again, gap).is_empty());
    }
    assert_eq!(b.j.counters().ignored_expects, 3);
    let out = b.progress(again + 5000.0);
    assert_eq!(finals(&out).len(), 1, "{out:?}");
    assert_eq!(finals(&out)[0].id, n + 3);
    check_stream(&b.all);
    assert_eq!(b.result(0).timing, Timing::Center, "the hit is still a hit");
    assert_eq!(b.result(n - 1).timing, Timing::Center);
    // `Reset` starts the ids again
    b.push(JudgeInput::Reset);
    b.progress(again + 6000.0);
    assert!(b.expect(0, again + 8000.0, gap).is_empty());
    assert_eq!(b.j.counters().ignored_expects, 3);
    assert_eq!(finals(&b.progress(again + 9000.0))[0].id, 0);
}

#[test]
fn a_clock_report_that_is_not_a_position_closes_nothing() {
    for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        b.expect(0, 0.0, 1000.0);
        b.expect(1, 1000.0, 1000.0);
        b.strum(0.0, 5.0);
        b.progress(100.0);
        let out = b.push(JudgeInput::Progress {
            in_now: bad,
            locked: true,
        });
        assert!(out.is_empty(), "{bad}: {out:?}");
        assert_eq!(b.j.counters().clock_faults, 1);
        // The clock is not trusted again until it reports a position. Event 1 is
        // listening when it does, and nobody plays it: not judged, instead of "not heard".
        b.progress(1000.0 + L_RT - 100.0);
        b.progress(5000.0);
        check_stream(&b.all);
        assert_eq!(b.result(0).timing, Timing::Center, "{bad}");
        assert!(not_judged(&b.result(1), Why::ClockNotLocked), "{bad}");
        assert_eq!(b.j.counters().clock_faults, 1);
    }
}

#[test]
fn input_time_that_runs_backward_is_not_judged() {
    let fresh = || {
        let mut b = Bench::new(Level::Apprentice, loopback());
        b.progress(-1000.0);
        for k in 0..3 {
            b.expect(k, f64::from(k) * 1000.0, 1000.0);
        }
        b.strum(0.0, 5.0);
        b
    };

    // Event 0 is over. Event 1 has its click and its strum in the stretch that the clock
    // then runs a second time (950 to 1200 ms); event 2 is after it.
    let mut b = fresh();
    b.progress(L_RT + 250.0 + MARGIN + 1.0);
    assert_eq!(verdicts(&b.strum(1000.0, 5.0))[0].timing, Timing::Center);
    b.progress(1200.0);
    let out = b.progress(950.0);
    assert_eq!(b.j.counters().clock_faults, 1);
    let v = verdicts(&out);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(v[0].id == 1 && !v[0].fin && not_judged(&v[0], Why::Glitch));
    // an attack in that stretch on the second pass: which pass is right is not known
    let t_in = b.frame(1100.0);
    assert!(
        b.push(JudgeInput::Strum { t_in, judged: true })
            .iter()
            .all(|o| { !matches!(o, JudgeOut::Verdict(v) if v.attack == Attack::Heard) })
    );
    b.strum(2000.0, 5.0);
    b.progress(9000.0);
    check_stream(&b.all);
    assert_eq!(b.result(0).timing, Timing::Center);
    assert!(not_judged(&b.result(1), Why::Glitch));
    assert_eq!(b.result(2).timing, Timing::Center);
    assert!(withdrawn(&b.all).is_empty());

    // A verdict already given inside the stretch is taken back.
    let mut b = fresh();
    b.progress(L_RT + 250.0 + MARGIN + 1.0);
    assert_eq!(b.result(0).timing, Timing::Center);
    let out = b.progress(-50.0);
    assert_eq!(withdrawn(&out), vec![(0, Why::Glitch)]);
    // the same position again is not a step
    assert!(b.progress(-50.0).is_empty());
    assert_eq!(b.j.counters().clock_faults, 1);
}

// ---------------------------------------------------------------------------------------
// (12) Property on the guitarness-sim grid

/// Detector events with the number of input samples consumed when they left it.
fn detect(take: &Take, block: usize) -> Vec<(f64, OnsetEvent)> {
    let mut d = OnsetDetector::new(take.sr, OnsetConfig::default());
    let mut out = Vec::new();
    let mut done = 0usize;
    for b in take.audio.chunks(block) {
        done += b.len();
        d.process(b, |e| out.push((done as f64, e)));
    }
    d.flush(|e| out.push((done as f64, e)));
    out
}

struct TakeRun {
    late_strums: u64,
    withdrawn: usize,
    heard: usize,
    events: usize,
}

/// Judge one take: the beat grid as expectations, the detector stream in arrival order, a
/// clock report after every block.
fn judge_take(
    take: &Take,
    ioi_s: f64,
    stream: &[(f64, OnsetEvent)],
    block: usize,
    margin_ms: f64,
) -> TakeRun {
    let sr = take.sr;
    let cfg = JudgeConfig {
        close_margin_ms: margin_ms,
        ..JudgeConfig::new(sr, Level::Apprentice)
    };
    let mut j = PulseJudge::new(cfg, loopback(), &GateTable::builtin());
    let mut out = Vec::with_capacity(4096);
    // The grid behind the take: the truth is the grid plus the player's error.
    let mut lead: Vec<f64> = take
        .truth
        .iter()
        .enumerate()
        .map(|(k, &t)| t - k as f64 * ioi_s * sr)
        .collect();
    lead.sort_by(f64::total_cmp);
    let lead = lead[lead.len() / 2];
    j.push(
        &JudgeInput::Progress {
            in_now: 0.0,
            locked: true,
        },
        &mut out,
    );
    for k in 0..take.truth.len() {
        // the click is captured `L_RT` before the strum that is on it
        j.push(
            &JudgeInput::Expect {
                id: k as u32,
                g_in: lead + k as f64 * ioi_s * sr - L_RT * sr / 1000.0,
                kind: ExpectKind::Attack,
                prev_gap_s: ioi_s,
                next_gap_s: ioi_s,
                pos_in_bar: 0,
            },
            &mut out,
        );
    }
    let total = take.audio.len();
    let mut i = 0;
    let mut at = 0;
    while at < total {
        at = (at + block).min(total);
        while i < stream.len() && stream[i].0 <= at as f64 {
            let input = match stream[i].1 {
                OnsetEvent::Heard { t, .. } => JudgeInput::Heard {
                    t_in: t,
                    judged: true,
                },
                OnsetEvent::Strum(s) => JudgeInput::Strum {
                    t_in: s.t,
                    judged: true,
                },
            };
            j.push(&input, &mut out);
            i += 1;
        }
        j.push(
            &JudgeInput::Progress {
                in_now: at as f64,
                locked: true,
            },
            &mut out,
        );
    }
    j.push(
        &JudgeInput::Progress {
            in_now: total as f64 + 2.0 * sr,
            locked: true,
        },
        &mut out,
    );
    check_stream(&out);
    let fin = finals(&out);
    TakeRun {
        late_strums: j.counters().late_strums,
        withdrawn: withdrawn(&out).len(),
        heard: fin.iter().filter(|v| v.attack == Attack::Heard).count(),
        events: fin.len(),
    }
}

#[test]
fn t12_no_strum_arrives_after_its_cell_closed_on_the_sim_grid() {
    const BLOCK: usize = 480;
    let mut worst_ms = 0.0f64;
    let mut strums = 0usize;
    let (mut heard, mut events) = (0usize, 0usize);
    let mut control_late = 0u64;
    let conds = standard_grid(true);
    assert!(conds.len() >= 48);
    for c in &conds {
        let take = make_take(c);
        let ioi = 60.0 / c.bpm / f64::from(c.per_beat);
        let stream = detect(&take, BLOCK);
        for (at, e) in &stream {
            if let OnsetEvent::Strum(s) = e {
                strums += 1;
                worst_ms = worst_ms.max((at - s.t) * 1000.0 / take.sr);
            }
        }
        let r = judge_take(&take, ioi, &stream, BLOCK, JudgeConfig::CLOSE_MARGIN_MS);
        assert_eq!(
            r.late_strums, 0,
            "{}: a strum came after its cell closed",
            c.name
        );
        assert_eq!(r.withdrawn, 0, "{}", c.name);
        assert_eq!(r.events, take.truth.len(), "{}", c.name);
        heard += r.heard;
        events += r.events;
        // Negative control: with no margin the same streams do arrive late. The fast
        // cells (sixteenths at 120 BPM, P = 62.5 ms) are where it must show.
        if c.per_beat == 4 {
            control_late += judge_take(&take, ioi, &stream, BLOCK, 0.0).late_strums;
        }
    }
    println!(
        "sim grid: {} takes, {strums} strums, longest attack-to-Strum delay {worst_ms:.1} ms, \
         heard {heard}/{events}, late with margin 0: {control_late}",
        conds.len()
    );
    assert!(strums >= 1000, "{strums}");
    // ordinary strums stay far inside the detector's bound (the block is in the figure)
    let block_ms = BLOCK as f64 / 44.1;
    assert!(
        worst_ms <= JudgeConfig::STRUM_DELAY_MS + block_ms,
        "{worst_ms:.1} ms"
    );
    assert!(heard as f64 >= 0.97 * events as f64, "{heard}/{events}");
    assert!(control_late > 0, "the control must catch late strums");
}

/// One string, struck at `t` seconds.
fn pluck(t: f64, string: usize, gain: f64) -> StrumSpec {
    let mut strings = [false; 6];
    strings[string] = true;
    StrumSpec {
        t0: t,
        voicing: voicing("E"),
        dir: Dir::Down,
        spread: 0.0,
        gain,
        muted: false,
        strings,
        attack: 0.0,
    }
}

/// A strum built to hold the detector as long as it can be held: a faint string `lead_ms`
/// before `n` strings `step_ms` apart. The strings keep the flux group open to its limit,
/// and the group is dated back to the faint one.
fn slow_strum(sr: f64, lead_ms: f64, step_ms: f64, n: usize, seed: u32) -> Vec<f32> {
    const T0: f64 = 0.6;
    let mut specs = vec![pluck(T0 - lead_ms / 1000.0, 5, 0.004)];
    for k in 0..n {
        specs.push(pluck(T0 + k as f64 * step_ms / 1000.0, k % 5, 0.5));
    }
    let len = ((T0 + 1.5) * sr) as usize;
    render_take(sr, &specs, len, &mut Mulberry32::new(seed)).audio
}

/// The strums of `x` as (input samples consumed when the detector emitted it, onset).
/// Sample by sample: the block size changes nothing in what the detector finds, only when
/// it is handed on.
fn emitted(sr: f64, x: &[f32]) -> Vec<(usize, f64)> {
    let mut d = OnsetDetector::new(sr, OnsetConfig::default());
    let mut out = Vec::new();
    for (i, v) in x.iter().enumerate() {
        d.process(std::slice::from_ref(v), |e| {
            if let OnsetEvent::Strum(st) = e {
                out.push((i + 1, st.t));
            }
        });
    }
    out
}

/// How the strums reach the judge.
#[derive(Clone, Copy, Debug)]
struct Delivery {
    /// Input block, frames: a strum is handed on when the block that emitted it is done.
    block: usize,
    /// Where the block boundaries lie: `phase` past a multiple of `block`.
    phase: usize,
    /// How far the clock report after each block is ahead of the detector, frames.
    lead: usize,
    margin_ms: f64,
}

/// Judge one event whose cell ends half a millisecond after the onset `t`: of all cells
/// that this attack can belong to, the one that closes soonest. Returns the strums that
/// came after their cell closed, the withdrawals, and the attack of the final verdict.
fn deliver(
    sr: f64,
    strums: &[(usize, f64)],
    t: f64,
    len: usize,
    d: Delivery,
) -> (u64, usize, Attack) {
    let cfg = JudgeConfig {
        close_margin_ms: d.margin_ms,
        ..JudgeConfig::new(sr, Level::Apprentice)
    };
    let mut j = PulseJudge::new(cfg, loopback(), &GateTable::builtin());
    let mut out = Vec::new();
    let progress = |frames: usize| JudgeInput::Progress {
        in_now: frames as f64,
        locked: true,
    };
    j.push(&progress(0), &mut out);
    j.push(
        &JudgeInput::Expect {
            id: 0,
            g_in: t - (L_RT + 250.0 - 0.5) * sr / 1000.0,
            kind: ExpectKind::Attack,
            prev_gap_s: 1.0,
            next_gap_s: 1.0,
            pos_in_bar: 0,
        },
        &mut out,
    );
    let mut i = 0;
    let mut end = if d.phase == 0 { d.block } else { d.phase };
    loop {
        let done = end.min(len);
        while i < strums.len() && strums[i].0 <= done {
            let t_in = strums[i].1;
            j.push(&JudgeInput::Strum { t_in, judged: true }, &mut out);
            i += 1;
        }
        j.push(&progress(done + d.lead), &mut out);
        if done == len {
            break;
        }
        end += d.block;
    }
    j.push(&progress(len + d.lead + 2 * sr as usize), &mut out);
    check_stream(&out);
    let f = finals(&out);
    assert_eq!(f.len(), 1, "{out:?}");
    (j.counters().late_strums, withdrawn(&out).len(), f[0].attack)
}

#[test]
fn t12_the_margin_covers_the_detector_at_its_slowest() {
    // The bound, from the detector's own settings: the search back from the first flux
    // peak, the longest group, the gap that closes it, one hop until the frame that sees
    // the gap, the energy window, and 3 ms for the filters.
    let cfg = OnsetConfig::default();
    for sr in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
        let d = OnsetDetector::new(sr, cfg.clone());
        let hop_ms = cfg.hop as f64 / d.fs_a() * 1000.0;
        let window_ms = cfg.contrast_cells as f64 * cfg.cell_s * 1000.0;
        let bound = (cfg.search_back_s + cfg.group_max_s + cfg.group_gap_s) * 1000.0
            + hop_ms
            + window_ms
            + 3.0;
        assert!(bound <= JudgeConfig::STRUM_DELAY_MS, "{sr}: {bound:.1} ms");
    }
    // ...and the margin leaves the clock one block of 2048 frames ahead of the detector.
    let lead_ms = MARGIN - JudgeConfig::STRUM_DELAY_MS;
    assert!(lead_ms >= 2048.0 / 44.1, "{lead_ms}");

    // The detector at its slowest, on a family of signals made for it: at each rate, a
    // faint string 34 to 42 ms before strums of 5 to 9 strings, 22 to 38 ms apart.
    let mut worst_ms = 0.0f64;
    let (mut signals, mut runs) = (0u32, 0u32);
    let (mut plan_late, mut plan_late_no_lead) = (0u64, 0u64);
    for sr in [44_100.0, 48_000.0, 96_000.0] {
        let mut worst_here = 0.0f64;
        for (step_ms, n) in [(38.0, 5), (30.0, 7), (25.0, 8), (22.0, 9)] {
            for lead_ms in [34.0, 38.0, 42.0] {
                let x = slow_strum(sr, lead_ms, step_ms, n, 1);
                let strums = emitted(sr, &x);
                let &(at, t) = strums
                    .iter()
                    .max_by(|a, b| (a.0 as f64 - a.1).total_cmp(&(b.0 as f64 - b.1)))
                    .expect("a strum");
                let delay_ms = (at as f64 - t) * 1000.0 / sr;
                assert!(
                    delay_ms <= JudgeConfig::STRUM_DELAY_MS,
                    "{sr} {step_ms} {n} {lead_ms}: {delay_ms:.1} ms"
                );
                worst_here = worst_here.max(delay_ms);
                signals += 1;
                for block in [64usize, 256, 480, 1024, 2048] {
                    // the worst phase: the block before the one that emits the strum ends
                    // on the sample before it, so the strum waits a whole block
                    for phase in [(at - 1) % block, 0, block / 2] {
                        for lead in [0, block] {
                            let d = Delivery {
                                block,
                                phase,
                                lead,
                                margin_ms: MARGIN,
                            };
                            let (late, withdrawn, attack) = deliver(sr, &strums, t, x.len(), d);
                            assert_eq!((late, withdrawn), (0, 0), "{sr} {d:?}");
                            assert_eq!(attack, Attack::Heard, "{sr} {d:?}");
                            runs += 1;
                            // the plan's 300 ms, on the same deliveries
                            let d = Delivery {
                                margin_ms: 300.0,
                                ..d
                            };
                            let (late, _, _) = deliver(sr, &strums, t, x.len(), d);
                            plan_late += late;
                            if lead == 0 {
                                plan_late_no_lead += late;
                            }
                        }
                    }
                }
            }
        }
        println!("slow strums at {sr} Hz: longest attack-to-Strum delay {worst_here:.1} ms");
        worst_ms = worst_ms.max(worst_here);
    }
    println!(
        "slow strums: {signals} signals, {runs} deliveries, longest attack-to-Strum delay \
         {worst_ms:.1} ms, late with {MARGIN} ms: 0, late with 300 ms: {plan_late} \
         ({plan_late_no_lead} with the clock level with the detector)"
    );
    // made for it indeed: more than twice the slowest strum of the sim grid
    assert!(worst_ms >= 250.0, "{worst_ms:.1} ms");
    // 300 ms hold only while the clock never runs ahead of the detector
    assert!(plan_late > 0, "the control must catch late strums");
}

// ---------------------------------------------------------------------------------------
// (13) Determinism, and no allocation

/// A long round with everything in it: uneven gaps, ghosts, missed and doubled strums,
/// early notices, glitch windows (in time and late), lost locks, disputes, resets.
fn busy_stream(seed: u64, events: u32) -> Vec<JudgeInput> {
    busy_stream_of(seed, events, Player::ROUGH)
}

/// Who plays the busy round, and how often the dispute key is pressed.
#[derive(Clone, Copy)]
struct Player {
    /// Chance that an expected attack is played.
    plays: f64,
    /// Scatter of the attacks around the click, ms.
    sigma_ms: f64,
    /// Chance of a press after an event.
    presses: f64,
}

impl Player {
    const ROUGH: Self = Self {
        plays: 0.85,
        sigma_ms: 45.0,
        presses: 0.04,
    };
    /// Mostly in time, and fond of the key: most presses find no accusation.
    const TIDY: Self = Self {
        plays: 0.95,
        sigma_ms: 30.0,
        presses: 0.3,
    };
}

fn busy_stream_of(seed: u64, events: u32, who: Player) -> Vec<JudgeInput> {
    let mut rng = Rng(seed);
    let fr = |ms: f64| BASE + ms * FS / 1000.0;
    let gaps: Vec<f64> = (0..events)
        .map(|_| [125.0, 250.0, 500.0, 750.0][(rng.next() % 4) as usize])
        .collect();
    let mut items: Vec<(f64, u8, JudgeInput)> = Vec::new();
    let mut g = 0.0;
    for k in 0..events as usize {
        let prev = if k == 0 { gaps[0] } else { gaps[k - 1] };
        let kind = if rng.chance(0.05) {
            ExpectKind::Ghost
        } else {
            ExpectKind::Attack
        };
        items.push((
            g - 2000.0,
            0,
            JudgeInput::Expect {
                id: k as u32,
                g_in: fr(g),
                kind,
                prev_gap_s: prev / 1000.0,
                next_gap_s: gaps[k] / 1000.0,
                pos_in_bar: (k % 8) as u8,
            },
        ));
        if rng.chance(who.plays) {
            let n = if rng.chance(0.1) { 2 } else { 1 };
            for _ in 0..n {
                let t = g + L_RT + who.sigma_ms * rng.normal();
                let judged = !rng.chance(0.02);
                let t_in = fr(t);
                items.push((t + 4.0, 1, JudgeInput::Heard { t_in, judged }));
                let delay = rng.range(50.0, 240.0);
                items.push((t + delay, 2, JudgeInput::Strum { t_in, judged }));
            }
        }
        if rng.chance(0.03) {
            let late = if rng.chance(0.5) { 60.0 } else { 1500.0 };
            items.push((
                g + late,
                3,
                JudgeInput::Unjudged {
                    from_in: fr(g - 100.0),
                    to_in: fr(g + 250.0),
                },
            ));
        }
        if rng.chance(who.presses) {
            items.push((g + rng.range(100.0, 900.0), 4, JudgeInput::Dispute));
        }
        if rng.chance(0.003) {
            items.push((g + 130.0, 5, JudgeInput::Reset));
        }
        g += gaps[k];
    }
    let mut now = -2500.0;
    while now < g + 2000.0 {
        items.push((
            now,
            6,
            JudgeInput::Progress {
                in_now: fr(now),
                locked: !rng.chance(0.002),
            },
        ));
        now += 10.0;
    }
    items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    items.into_iter().map(|x| x.2).collect()
}

fn run(inputs: &[JudgeInput], timing: TimingRef) -> Vec<JudgeOut> {
    let mut j = PulseJudge::new(
        JudgeConfig::new(FS, Level::Band),
        timing,
        &GateTable::builtin(),
    );
    let mut all = Vec::new();
    let mut out = Vec::new();
    for i in inputs {
        j.push(i, &mut out);
        all.append(&mut out);
    }
    all
}

#[test]
fn t13_same_input_same_output() {
    for seed in [1, 2, 3] {
        let inputs = busy_stream(seed, 3000);
        for timing in [
            loopback(),
            TimingRef::estimate(95.0, 8.0, 16),
            TimingRef::none(),
        ] {
            let a = run(&inputs, timing);
            let b = run(&inputs, timing);
            // bit for bit: `Debug` prints floats in the shortest form that reads back
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
            assert!(a.len() > 3000);
            // the stream is a real mix, not one branch
            if timing == loopback() {
                let f = finals(&a);
                let count = |p: &dyn Fn(&PulseVerdict) -> bool| f.iter().filter(|v| p(v)).count();
                assert!(count(&|v| v.timing == Timing::Center) > 100);
                assert!(count(&|v| v.timing == Timing::Outside) > 20);
                assert!(count(&|v| v.attack == Attack::NotHeard) > 100);
                assert!(count(&|v| not_judged(v, Why::Glitch)) > 20);
                assert!(count(&|v| not_judged(v, Why::ClockNotLocked)) > 5);
                assert!(count(&|v| not_judged(v, Why::Ghost)) > 50);
                assert!(extras(&a).len() > 100);
                let w = withdrawn(&a);
                assert!(w.len() > 20);
                // a dispute withdraws a final verdict; a provisional one is never contested
                assert!(w.iter().filter(|w| w.1 == Why::Disputed).count() > 5);
                assert_eq!(count(&|v| not_judged(v, Why::Disputed)), 0);
            }
            // every event ends exactly once, whatever happened to it
            let f = finals(&a);
            let mut ids: Vec<u32> = f.iter().map(|v| v.id).collect();
            ids.sort_unstable();
            assert_eq!(ids, (0..3000).collect::<Vec<u32>>(), "seed {seed}");
            // and nothing is ever said about direction
            assert!(
                verdicts(&a)
                    .iter()
                    .all(|v| v.dir == Direction::NotJudged(Why::GateLocked))
            );
        }
    }
}

#[test]
fn nothing_is_allocated_per_input() {
    // the counter counts
    let before = allocs();
    let v: Vec<u8> = Vec::with_capacity(64);
    assert!(allocs() > before, "the allocation counter is blind");
    drop(v);

    // more events than the judge remembers, so the history wraps
    let events = 3 * CLOSED_CELLS;
    let inputs = busy_stream(77, events as u32);
    for timing in [loopback(), TimingRef::none()] {
        let mut j = PulseJudge::new(
            JudgeConfig::new(FS, Level::Apprentice),
            timing,
            &GateTable::builtin(),
        );
        // the judge and the log of the round together, as the session runs them
        let mut log = VerdictLog::with_capacity(events);
        let mut out: Vec<JudgeOut> = Vec::with_capacity(2 * (OPEN_CELLS + CLOSED_CELLS));
        let mut said = 0usize;
        let before = allocs();
        for i in &inputs {
            j.push(i, &mut out);
            for o in &out {
                log.push(o);
            }
            said += out.len();
            out.clear();
        }
        let used = allocs() - before;
        assert_eq!(used, 0, "{used} allocations in {} inputs", inputs.len());
        assert!(said > inputs.len() / 100, "{said}");
        assert_eq!(log.verdicts().len(), events, "every event is in the log");
        assert_eq!(log.capacity(), events);

        // The counter does see the log: one without room has to grow.
        let mut j = PulseJudge::new(
            JudgeConfig::new(FS, Level::Apprentice),
            timing,
            &GateTable::builtin(),
        );
        let mut grown = VerdictLog::new();
        let before = allocs();
        for i in &inputs {
            j.push(i, &mut out);
            for o in &out {
                grown.push(o);
            }
            out.clear();
        }
        assert!(allocs() > before, "a log without room must have grown");
        assert_eq!(grown, log);
    }
}
