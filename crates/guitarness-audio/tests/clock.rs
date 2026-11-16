//! T4: `ClockMap` on synthetic streams (drift ±200 ppm, variable blocks, jitter, lost
//! frames, outliers, bad stamps), with negative controls. Run with `--nocapture` to see
//! the measured numbers.
//!
//! Test conditions, where the plan leaves them open:
//! - "jitter of X" is Gaussian with standard deviation X, independent per stamp, on both
//!   streams;
//! - blocks are uniform in 5–15 ms per callback (about 100 stamps/s per stream);
//! - the error of `in_of_out` is sampled at the first frame of every output block;
//! - for the 1 ms gate, p95 is taken for each pair of rates and drifts separately, over
//!   the samples of 100 seeds, and over single runs of 300 s. Nothing is pooled across
//!   configurations. A single 15 s run holds only about seven independent samples of a
//!   0.1 Hz loop, so its own p95 scatters around the true value (about 0.24 ms): roughly
//!   one such run in twenty reads above 0.30 ms. The count is printed.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use guitarness_audio::clock::{
    ClockConfig, ClockMap, ClockQuality, ClockSnapshot, ClockStep, LateNotice, StampSource,
    StepCause, UNDO_STAMPS,
};
use guitarness_audio::{Side, Stamp};
use guitarness_dsp::synth::Mulberry32;

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

/// Host clock far from zero (11.6 days of uptime), to exercise the ns arithmetic.
const HOST0_NS: i128 = 1_000_000_000_000_000;

/// (input, output) clock errors in ppm. Relative drift up to ±200 ppm.
const DRIFTS: [(f64, f64); 6] = [
    (200.0, 0.0),
    (-200.0, 0.0),
    (0.0, 200.0),
    (0.0, -200.0),
    (120.0, -80.0),
    (-100.0, 100.0),
];
const RATES: [(f64, f64); 2] = [(48_000.0, 48_000.0), (44_100.0, 48_000.0)];

struct Rng {
    r: Mulberry32,
    spare: Option<f64>,
}

impl Rng {
    fn new(seed: u32) -> Self {
        Self {
            r: Mulberry32::new(seed),
            spare: None,
        }
    }
    fn uniform(&mut self) -> f64 {
        self.r.next_f64()
    }
    fn normal(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = 1.0 - self.r.next_f64();
        let u2 = self.r.next_f64();
        let rad = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        self.spare = Some(rad * s);
        rad * c
    }
}

fn ns(t: f64) -> u64 {
    (HOST0_NS + (t * 1.0e9).round() as i128) as u64
}

/// One synthetic stream: a device clock at `rate` frames per true second.
struct Synth {
    side: Side,
    rate: f64,
    t0: f64,
    frame: u64,
    lost: u64,
    blocks_s: (f64, f64),
    fs: f64,
    jitter_s: f64,
    dev_skew: f64,
    cb_skew: f64,
    extra_s: f64,
    extra_left: u32,
    stamp_shift_ns: i64,
    rng: Rng,
}

impl Synth {
    /// True time of callback frame `frame` (valid after the last loss).
    fn time_of(&self, frame: f64) -> f64 {
        self.t0 + (frame + self.lost as f64) / self.rate
    }
    /// Callback frame at true time `t` (valid after the last loss).
    fn frame_at(&self, t: f64) -> f64 {
        (t - self.t0) * self.rate - self.lost as f64
    }
    fn next_time(&self) -> f64 {
        self.time_of(self.frame as f64)
    }
    fn next(&mut self) -> Stamp {
        let (lo, hi) = self.blocks_s;
        let frames = (((lo + (hi - lo) * self.rng.uniform()) * self.fs) as u32).max(1);
        let t = self.next_time();
        let mut dev = t * (1.0 + self.dev_skew) + self.rng.normal() * self.jitter_s;
        if self.extra_left > 0 {
            self.extra_left -= 1;
            dev += self.extra_s;
        }
        let cb = match self.side {
            Side::Out => t - 0.020,
            Side::In => t + f64::from(frames) / self.rate + 0.002,
        };
        let cb = cb * (1.0 + self.cb_skew) + self.rng.normal().abs() * 100.0e-6;
        let stamp = Stamp {
            first: self.frame,
            frames,
            device_ns: (ns(dev) as i128 + i128::from(self.stamp_shift_ns)) as u64,
            callback_ns: (ns(cb) as i128 + i128::from(self.stamp_shift_ns)) as u64,
        };
        self.frame += u64::from(frames);
        stamp
    }
}

#[derive(Clone, Copy, Debug)]
enum Event {
    /// `seconds` of frames are lost on `side` at the first block boundary after `at_s`.
    Loss {
        side: Side,
        at_s: f64,
        seconds: f64,
        notice: bool,
    },
    /// `count` consecutive device stamps of `side` are off by `offset_s`.
    Outlier {
        side: Side,
        at_s: f64,
        offset_s: f64,
        count: u32,
    },
    /// The stamps of `side` are not delivered for `seconds` (no frames are lost).
    Silence { side: Side, at_s: f64, seconds: f64 },
}

impl Event {
    fn side(&self) -> Side {
        match *self {
            Event::Loss { side, .. }
            | Event::Outlier { side, .. }
            | Event::Silence { side, .. } => side,
        }
    }
    fn at_s(&self) -> f64 {
        match *self {
            Event::Loss { at_s, .. }
            | Event::Outlier { at_s, .. }
            | Event::Silence { at_s, .. } => at_s,
        }
    }
}

#[derive(Clone, Debug)]
struct Scenario {
    fs_in: f64,
    fs_out: f64,
    drift_in_ppm: f64,
    drift_out_ppm: f64,
    jitter_s: f64,
    seconds: f64,
    seed: u32,
    cfg: ClockConfig,
    blocks_s: (f64, f64),
    /// Rate error of the device stamps (input, output), ppm.
    dev_skew_ppm: (f64, f64),
    /// Rate error of the callback stamps (input, output), ppm.
    cb_skew_ppm: (f64, f64),
    /// The input stamps are reported on a clock shifted by this much.
    in_origin_shift_ns: i64,
    events: Vec<Event>,
}

impl Scenario {
    fn new(rates: (f64, f64), drift: (f64, f64), jitter_s: f64, seconds: f64, seed: u32) -> Self {
        Self {
            fs_in: rates.0,
            fs_out: rates.1,
            drift_in_ppm: drift.0,
            drift_out_ppm: drift.1,
            jitter_s,
            seconds,
            seed,
            cfg: ClockConfig::default(),
            blocks_s: (0.005, 0.015),
            dev_skew_ppm: (0.0, 0.0),
            cb_skew_ppm: (0.0, 0.0),
            in_origin_shift_ns: 0,
            events: Vec::new(),
        }
    }
    fn with(mut self, ev: Event) -> Self {
        self.events.push(ev);
        self
    }
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f64,
    err_ms: f64,
    drift_err_ppm: f64,
    quality: ClockQuality,
}

#[derive(Clone, Copy, Debug)]
struct StepSeen {
    t: f64,
    step: ClockStep,
    /// Stamps of that side fed since the last event on it, this one included.
    callbacks: u32,
}

struct Run {
    samples: Vec<Sample>,
    steps: Vec<StepSeen>,
    map: ClockMap,
    last: ClockSnapshot,
}

impl Run {
    fn errs(&self, from: f64, to: f64) -> Vec<f64> {
        self.samples
            .iter()
            .filter(|s| s.t >= from && s.t < to)
            .map(|s| s.err_ms)
            .collect()
    }
    fn max_abs_err(&self, from: f64, to: f64) -> f64 {
        self.errs(from, to).iter().fold(0.0, |m, e| m.max(e.abs()))
    }
    fn quality_at(&self, t: f64) -> ClockQuality {
        self.samples
            .iter()
            .find(|s| s.t >= t)
            .map(|s| s.quality)
            .expect("run is long enough")
    }
}

fn run(sc: &Scenario) -> Run {
    let rate_in = sc.fs_in * (1.0 + sc.drift_in_ppm * 1.0e-6);
    let rate_out = sc.fs_out * (1.0 + sc.drift_out_ppm * 1.0e-6);
    let true_drift_ppm = ((rate_in / sc.fs_in) / (rate_out / sc.fs_out) - 1.0) * 1.0e6;
    let synth = |side: Side, fs: f64, rate: f64, t0: f64, skews_ppm: (f64, f64), salt: u32| Synth {
        side,
        rate,
        t0,
        frame: 0,
        lost: 0,
        blocks_s: sc.blocks_s,
        fs,
        jitter_s: sc.jitter_s,
        dev_skew: skews_ppm.0 * 1.0e-6,
        cb_skew: skews_ppm.1 * 1.0e-6,
        extra_s: 0.0,
        extra_left: 0,
        stamp_shift_ns: 0,
        rng: Rng::new(sc.seed.wrapping_mul(2).wrapping_add(salt)),
    };
    let skews_in = (sc.dev_skew_ppm.0, sc.cb_skew_ppm.0);
    let skews_out = (sc.dev_skew_ppm.1, sc.cb_skew_ppm.1);
    // The streams do not start together, as on real hardware.
    let mut i = synth(Side::In, sc.fs_in, rate_in, 0.0037, skews_in, 1);
    let mut o = synth(Side::Out, sc.fs_out, rate_out, 0.0, skews_out, 2);
    i.stamp_shift_ns = -sc.in_origin_shift_ns;
    let mut cfg = sc.cfg;
    cfg.origin_offset_ns = sc.in_origin_shift_ns;
    let mut map = ClockMap::new(sc.fs_in, sc.fs_out, cfg);

    let mut pending: Vec<Event> = sc.events.clone();
    let mut silent_until = [f64::NEG_INFINITY; 2];
    let mut since_event = [0u32; 2];
    let mut samples = Vec::new();
    let mut steps = Vec::new();
    loop {
        let (ti, to) = (i.next_time(), o.next_time());
        if ti.min(to) > sc.seconds {
            break;
        }
        let side = if to <= ti { Side::Out } else { Side::In };
        let idx = usize::from(side == Side::Out);
        let st = if side == Side::Out { &mut o } else { &mut i };
        let now = st.next_time();
        let busy = st.extra_left > 0; // one outlier burst at a time
        if let Some(k) = pending.iter().position(|e| {
            e.side() == side && now >= e.at_s() && !(busy && matches!(e, Event::Outlier { .. }))
        }) {
            match pending.remove(k) {
                Event::Loss {
                    seconds, notice, ..
                } => {
                    st.lost += (seconds * st.rate).round() as u64;
                    if notice {
                        map.note_xrun(side);
                    }
                }
                Event::Outlier {
                    offset_s, count, ..
                } => {
                    st.extra_s = offset_s;
                    st.extra_left = count;
                }
                Event::Silence { seconds, .. } => silent_until[idx] = now + seconds,
            }
            since_event[idx] = 0;
            continue; // a loss moved this block: pick the earliest stream again
        }
        let stamp = st.next();
        if now < silent_until[idx] {
            continue;
        }
        let step = match side {
            Side::Out => map.push_out(stamp),
            Side::In => map.push_in(stamp),
        };
        since_event[idx] += 1;
        if let Some(step) = step {
            steps.push(StepSeen {
                t: now,
                step,
                callbacks: since_event[idx],
            });
        }
        if side == Side::Out {
            let f = stamp.first as f64;
            let truth = i.frame_at(o.time_of(f));
            let snap = map.snapshot();
            samples.push(Sample {
                t: now,
                err_ms: (snap.in_of_out(f) - truth) / sc.fs_in * 1.0e3,
                drift_err_ppm: snap.drift_ppm() - true_drift_ppm,
                quality: snap.quality(),
            });
        }
    }
    let last = map.snapshot();
    Run {
        samples,
        steps,
        map,
        last,
    }
}

/// 95th percentile of the absolute values.
fn p95(v: &[f64]) -> f64 {
    assert!(!v.is_empty());
    let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    a.sort_by(f64::total_cmp);
    let k = ((0.95 * a.len() as f64).ceil() as usize).clamp(1, a.len());
    a[k - 1]
}

// ---------------------------------------------------------------- T4 gate 1

#[test]
fn t4_p95_within_0_10_ms_after_5_s_with_50_us_jitter() {
    let mut worst: f64 = 0.0;
    let mut pooled = Vec::new();
    for rates in RATES {
        for drift in DRIFTS {
            for seed in 0..6 {
                let r = run(&Scenario::new(rates, drift, 50.0e-6, 20.0, 100 + seed));
                let e = r.errs(5.0, 20.0);
                let p = p95(&e);
                assert!(
                    p <= 0.10,
                    "p95 {p:.4} ms > 0.10 ms: rates {rates:?} drift {drift:?} seed {seed}"
                );
                worst = worst.max(p);
                pooled.extend(e);
            }
        }
    }
    println!(
        "T4 50 us jitter, 5..20 s: pooled p95 {:.4} ms, worst single-run p95 {:.4} ms (gate 0.10 ms, 72 runs)",
        p95(&pooled),
        worst
    );
}

// ---------------------------------------------------------------- T4 gate 2

#[test]
fn t4_p95_within_0_30_ms_after_15_s_with_1_ms_jitter() {
    // One gate per configuration: the seeds are pooled, the configurations are not.
    const SEEDS: u32 = 100;
    let (mut worst_early, mut worst_all, mut per_run) = (0.0f64, 0.0f64, Vec::new());
    for rates in RATES {
        for drift in DRIFTS {
            let (mut early, mut all) = (Vec::new(), Vec::new());
            for seed in 0..SEEDS {
                let r = run(&Scenario::new(rates, drift, 1.0e-3, 30.0, 200 + seed));
                assert_eq!(
                    r.last.quality(),
                    ClockQuality::Locked,
                    "{rates:?} {drift:?}"
                );
                early.extend(r.errs(15.0, 20.0));
                let e = r.errs(15.0, 30.0);
                per_run.push(p95(&e));
                all.extend(e);
            }
            let (p_early, p_all) = (p95(&early), p95(&all));
            println!(
                "T4 1 ms jitter, {:.1}/{:.1} kHz, drift {:+.0}/{:+.0} ppm, {SEEDS} seeds: p95 15..20 s {p_early:.4} ms, 15..30 s {p_all:.4} ms",
                rates.0 / 1e3,
                rates.1 / 1e3,
                drift.0,
                drift.1
            );
            assert!(
                p_early <= 0.30,
                "p95 15..20 s {p_early:.4} ms: {rates:?} {drift:?}"
            );
            assert!(
                p_all <= 0.30,
                "p95 15..30 s {p_all:.4} ms: {rates:?} {drift:?}"
            );
            worst_early = worst_early.max(p_early);
            worst_all = worst_all.max(p_all);
        }
    }
    let over = per_run.iter().filter(|p| **p > 0.30).count();
    println!(
        "T4 1 ms jitter: worst configuration p95 15..20 s {worst_early:.4} ms, 15..30 s {worst_all:.4} ms (gate 0.30 ms); \
         single 15 s runs above 0.30 ms: {over}/{} (worst {:.4} ms)",
        per_run.len(),
        per_run.iter().fold(0.0f64, |m, p| m.max(*p))
    );

    // Long single runs: enough independent samples for a per-run p95.
    for (k, drift) in [(0, DRIFTS[0]), (1, DRIFTS[3]), (2, DRIFTS[4])] {
        let r = run(&Scenario::new(RATES[1], drift, 1.0e-3, 315.0, 300 + k));
        let p = p95(&r.errs(15.0, 315.0));
        println!("T4 1 ms jitter, single run 15..315 s, drift {drift:?}: p95 {p:.4} ms");
        assert!(p <= 0.30, "long-run p95 {p:.4} ms");
    }
}

// ---------------------------------------------------------------- T4 gate 3

#[test]
fn t4_drift_within_10_ppm_at_20_s() {
    let mut worst: f64 = 0.0;
    for rates in RATES {
        for drift in DRIFTS {
            for seed in 0..6 {
                let r = run(&Scenario::new(rates, drift, 50.0e-6, 22.0, 400 + seed));
                let e = r
                    .samples
                    .iter()
                    .filter(|s| s.t >= 20.0)
                    .fold(0.0f64, |m, s| m.max(s.drift_err_ppm.abs()));
                assert!(
                    e <= 10.0,
                    "drift error {e:.2} ppm > 10 ppm: rates {rates:?} drift {drift:?} seed {seed}"
                );
                worst = worst.max(e);
            }
        }
    }
    println!(
        "T4 drift, 50 us jitter: worst error over 20..22 s {worst:.2} ppm (gate 10 ppm, 72 runs)"
    );

    // Not a gate: with 1 ms jitter the instantaneous rate of a 0.1 Hz loop is noisier
    // than 10 ppm, and so is any estimator over 20 s (least squares: about 5.5 ppm rms).
    let mut at_20 = Vec::new();
    for drift in DRIFTS {
        for seed in 0..8 {
            let r = run(&Scenario::new(RATES[1], drift, 1.0e-3, 20.5, 500 + seed));
            at_20.push(r.samples.last().expect("samples").drift_err_ppm.abs());
        }
    }
    at_20.sort_by(f64::total_cmp);
    println!(
        "T4 drift, 1 ms jitter (information only): at 20 s median {:.1} ppm, worst {:.1} ppm",
        at_20[at_20.len() / 2],
        at_20[at_20.len() - 1]
    );
}

// ---------------------------------------------------------------- T4 gate 4

const LOSS_AT_S: f64 = 12.0;
const LOSS_S: f64 = 0.020;

struct LossReport {
    detected_in: Option<u32>,
    step_err_ms: f64,
    max_err_after_1_s_ms: f64,
}

impl LossReport {
    /// The gate: found within 3 callbacks and within 0.2 ms from 1 s after the loss.
    fn check(&self) -> Result<(), String> {
        match self.detected_in {
            Some(n) if n <= 3 => {}
            Some(n) => return Err(format!("detected after {n} callbacks")),
            None => return Err("not detected".into()),
        }
        if self.max_err_after_1_s_ms > 0.2 {
            return Err(format!(
                "error {:.3} ms after 1 s",
                self.max_err_after_1_s_ms
            ));
        }
        Ok(())
    }
}

fn loss_run(side: Side, rates: (f64, f64), seed: u32, cfg: ClockConfig) -> (Run, LossReport) {
    let mut sc = Scenario::new(rates, (120.0, -80.0), 50.0e-6, 18.0, seed).with(Event::Loss {
        side,
        at_s: LOSS_AT_S,
        seconds: LOSS_S,
        notice: false,
    });
    sc.cfg = cfg;
    let r = run(&sc);
    let seen = r.steps.iter().find(|s| s.step.side == side);
    let report = LossReport {
        detected_in: seen.map(|s| s.callbacks),
        step_err_ms: seen.map_or(f64::NAN, |s| (s.step.step_s - LOSS_S) * 1.0e3),
        max_err_after_1_s_ms: r.max_abs_err(LOSS_AT_S + 1.0, LOSS_AT_S + 5.0),
    };
    (r, report)
}

#[test]
fn t4_frame_loss_of_20_ms_is_detected_in_3_callbacks_and_corrected() {
    let (mut worst_after, mut worst_step, mut worst_first): (f64, f64, f64) = (0.0, 0.0, 0.0);
    for side in [Side::In, Side::Out] {
        for rates in RATES {
            for seed in 0..6 {
                let (r, rep) = loss_run(side, rates, 600 + seed, ClockConfig::default());
                rep.check()
                    .unwrap_or_else(|e| panic!("{side:?} {rates:?} seed {seed}: {e}"));
                assert_eq!(r.steps.len(), 1, "exactly one step");
                let s = r.steps[0];
                assert_eq!(s.step.cause, StepCause::Detected);
                assert_eq!(s.callbacks, 3);
                assert!(
                    rep.step_err_ms.abs() <= 0.2,
                    "step size {:.3} ms off",
                    rep.step_err_ms
                );
                let nominal = if side == Side::In { rates.0 } else { rates.1 };
                assert!((s.step.frames - s.step.step_s * nominal).abs() < 1.0e-9);
                assert_eq!(r.map.stats(side).steps, 1);
                assert_eq!(r.map.stats(side).outliers, 0);
                assert_eq!(r.last.quality(), ClockQuality::Locked);
                // Stronger than the gate: right from the sample after the step.
                let first = r.max_abs_err(s.t + 1.0e-9, LOSS_AT_S + 1.0);
                worst_first = worst_first.max(first);
                worst_after = worst_after.max(rep.max_err_after_1_s_ms);
                worst_step = worst_step.max(rep.step_err_ms.abs());
            }
        }
    }
    println!(
        "T4 20 ms loss: detected on callback 3 in 24/24 runs; step size error <= {worst_step:.4} ms; \
         max error from the step to +1 s {worst_first:.4} ms; from +1 s to +5 s {worst_after:.4} ms (gate 0.2 ms)"
    );
    assert!(worst_first <= 0.2);
}

#[test]
fn t4_negative_control_the_gate_fails_without_the_jump_rule() {
    let off = ClockConfig {
        jump_rule: false,
        ..ClockConfig::default()
    };
    for side in [Side::In, Side::Out] {
        let (r, rep) = loss_run(side, RATES[1], 600, off);
        assert!(
            rep.check().is_err(),
            "the gate passed without the jump rule"
        );
        assert_eq!(rep.detected_in, None, "no step can be reported");
        assert!(r.steps.is_empty());
        assert!(
            rep.max_err_after_1_s_ms > 0.2,
            "error after 1 s is {:.3} ms",
            rep.max_err_after_1_s_ms
        );
        println!(
            "T4 negative control ({side:?}, jump rule off): no step; error 1..5 s after the loss up to {:.2} ms (gate 0.2 ms)",
            rep.max_err_after_1_s_ms
        );
        // Same seed with the rule on passes.
        let (_, on) = loss_run(side, RATES[1], 600, ClockConfig::default());
        on.check().expect("control run with the rule on");
    }
}

// ---------------------------------------------------------------- outliers

fn outlier_run(cfg: ClockConfig) -> Run {
    let mut sc = Scenario::new(RATES[1], (-100.0, 100.0), 50.0e-6, 18.0, 700)
        .with(Event::Outlier {
            side: Side::Out,
            at_s: 12.0,
            offset_s: 0.050,
            count: 1,
        })
        .with(Event::Outlier {
            side: Side::In,
            at_s: 13.0,
            offset_s: -0.007,
            count: 2,
        });
    sc.cfg = cfg;
    run(&sc)
}

#[test]
fn t4_isolated_outliers_are_ignored_and_counted() {
    let r = outlier_run(ClockConfig::default());
    assert!(
        r.steps.is_empty(),
        "an outlier must not step: {:?}",
        r.steps
    );
    assert_eq!(r.map.stats(Side::Out).outliers, 1);
    assert_eq!(r.map.stats(Side::In).outliers, 2);
    assert_eq!(
        r.map.stats(Side::Out).steps + r.map.stats(Side::In).steps,
        0
    );
    let with_rule = r.max_abs_err(12.0, 16.0);
    assert!(
        with_rule <= 0.10,
        "error {with_rule:.4} ms after the outliers"
    );
    assert_eq!(r.last.quality(), ClockQuality::Locked);

    // Negative control: without the rule the same stamps pull the map.
    let off = outlier_run(ClockConfig {
        jump_rule: false,
        ..ClockConfig::default()
    });
    let without_rule = off.max_abs_err(12.0, 16.0);
    assert_eq!(off.map.stats(Side::Out).outliers, 0);
    assert!(without_rule > 0.3, "control error {without_rule:.4} ms");
    println!(
        "outliers (+50 ms x1 out, -7 ms x2 in): max error 12..16 s {with_rule:.4} ms with the rule, {without_rule:.4} ms without"
    );
}

#[test]
fn large_errors_of_alternating_sign_never_step() {
    let mut sc = Scenario::new(RATES[0], (0.0, 0.0), 50.0e-6, 16.0, 710);
    for k in 0..6 {
        sc = sc.with(Event::Outlier {
            side: Side::In,
            at_s: 12.0 + 0.011 * f64::from(k),
            offset_s: if k % 2 == 0 { 0.02 } else { -0.02 },
            count: 1,
        });
    }
    let r = run(&sc);
    assert!(r.steps.is_empty());
    assert_eq!(r.map.stats(Side::In).outliers, 6);
    assert!(r.max_abs_err(12.0, 16.0) <= 0.10);
}

// ---------------------------------------------------------------- xrun notice

#[test]
fn xrun_notice_confirms_a_small_loss_on_the_first_stamp() {
    for (side, seconds) in [(Side::In, 0.002), (Side::Out, 0.002), (Side::Out, 0.020)] {
        let sc = Scenario::new(RATES[1], (120.0, -80.0), 50.0e-6, 18.0, 720).with(Event::Loss {
            side,
            at_s: 12.0,
            seconds,
            notice: true,
        });
        let r = run(&sc);
        assert_eq!(r.steps.len(), 1, "{side:?} {seconds}");
        let s = r.steps[0];
        assert_eq!(s.step.side, side);
        assert_eq!(s.step.cause, StepCause::Notice);
        assert_eq!(s.callbacks, 1, "confirmed by the first stamp");
        assert!(
            (s.step.step_s - seconds).abs() < 0.2e-3,
            "step {}",
            s.step.step_s
        );
        assert_eq!(r.map.stats(side).xrun_notices, 1);
        let after = r.max_abs_err(12.5, 17.0);
        assert!(after <= 0.2, "error {after:.4} ms after a noticed loss");
    }
}

#[test]
fn xrun_notice_without_a_loss_does_not_step() {
    let mut sc = Scenario::new(RATES[0], (0.0, 200.0), 50.0e-6, 16.0, 730);
    sc = sc.with(Event::Loss {
        side: Side::In,
        at_s: 12.0,
        seconds: 0.0,
        notice: true,
    });
    let r = run(&sc);
    assert!(r.steps.is_empty());
    assert_eq!(r.map.stats(Side::In).xrun_notices, 1);
    assert!(r.max_abs_err(12.0, 16.0) <= 0.10);
}

#[test]
fn small_loss_without_notice_is_absorbed_by_the_loop() {
    // Below the 3 ms threshold and with no notice there is no step: the loop converges
    // on the new offset with its own time constant (2.25 s at 0.1 Hz).
    let sc = Scenario::new(RATES[0], (0.0, 0.0), 50.0e-6, 40.0, 740).with(Event::Loss {
        side: Side::In,
        at_s: 12.0,
        seconds: 0.002,
        notice: false,
    });
    let r = run(&sc);
    assert!(r.steps.is_empty());
    let (soon, late) = (r.max_abs_err(12.1, 12.5), r.max_abs_err(30.0, 40.0));
    assert!(soon > 1.0, "the loss is visible at first: {soon:.3} ms");
    assert!(late <= 0.2, "and gone later: {late:.3} ms");
    println!("2 ms loss, no notice: error {soon:.3} ms at +0.1..0.5 s, {late:.3} ms at +18..28 s");
}

// ---------------------------------------------------------------- lock, reset, broken

#[test]
fn a_wrong_first_stamp_realigns_silently() {
    // Streams often start with an odd stamp. The jump rule realigns, but a realignment
    // within the first stamps is not a step: nothing was running yet.
    for side in [Side::In, Side::Out] {
        let sc = Scenario::new(RATES[1], (200.0, 0.0), 50.0e-6, 8.0, 790).with(Event::Outlier {
            side,
            at_s: 0.0,
            offset_s: 0.008,
            count: 1,
        });
        let r = run(&sc);
        assert!(r.steps.is_empty(), "{:?}", r.steps);
        assert_eq!(r.map.stats(side).steps, 0);
        assert_eq!(r.quality_at(2.2), ClockQuality::Locked);
        let e = r.max_abs_err(0.2, 8.0);
        assert!(
            e <= 0.10,
            "{side:?}: error {e:.4} ms after a wrong first stamp"
        );
    }
}

#[test]
fn locks_after_the_first_clean_window() {
    for (jitter, by) in [(50.0e-6, 2.2), (1.0e-3, 4.2)] {
        let r = run(&Scenario::new(RATES[1], (200.0, 0.0), jitter, 8.0, 800));
        assert_eq!(r.quality_at(0.5), ClockQuality::Acquiring);
        assert_eq!(r.quality_at(1.9), ClockQuality::Acquiring);
        assert_eq!(r.quality_at(by), ClockQuality::Locked, "jitter {jitter}");
        assert_eq!(r.last.quality(), ClockQuality::Locked);
        let st = r.map.stats(Side::In);
        assert!(st.locked && !st.degraded && st.source == StampSource::Device);
        assert!(st.mean_e_s.abs() < 0.2e-3);
        assert!(
            (st.rms_e_s / jitter - 1.0).abs() < 0.25,
            "rms {} vs jitter {jitter}",
            st.rms_e_s
        );
        assert!(
            (st.slope_ppm + 200.0).abs() < 1.0e6 * jitter * 0.5 + 5.0,
            "slope {}",
            st.slope_ppm
        );
    }
}

#[test]
fn reset_starts_a_new_epoch() {
    let sc = Scenario::new(RATES[0], (100.0, 0.0), 50.0e-6, 6.0, 810);
    let mut r = run(&sc);
    assert_eq!(r.map.epoch(), 0);
    assert_eq!(r.last.epoch(), 0);
    assert_eq!(r.last.quality(), ClockQuality::Locked);
    r.map.reset();
    assert_eq!(r.map.epoch(), 1);
    let s = r.map.snapshot();
    assert_eq!(s.epoch(), 1);
    assert_eq!(s.quality(), ClockQuality::Acquiring);
    assert_eq!(r.map.stats(Side::In).stamps, 0);
    assert!(s.drift_ppm().abs() < 1.0e-9, "line forgotten");
    // Frame counters restart at zero after a reopen: accepted in the new epoch.
    let stamp = Stamp {
        first: 0,
        frames: 480,
        device_ns: 5,
        callback_ns: 5,
    };
    assert_eq!(r.map.push_in(stamp), None);
    assert_eq!(r.map.push_out(stamp), None);
    assert_eq!(r.map.quality(), ClockQuality::Acquiring);
}

#[test]
fn frame_counter_going_backwards_is_broken_until_reset() {
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    let stamp = |first: u64, t_ms: u64| Stamp {
        first,
        frames: 480,
        device_ns: 1_000_000 * t_ms,
        callback_ns: 1_000_000 * t_ms,
    };
    for k in 0..10 {
        map.push_in(stamp(480 * k, 10 * k));
        map.push_out(stamp(480 * k, 10 * k));
    }
    assert_eq!(map.quality(), ClockQuality::Acquiring);
    // Zero-length blocks are ignored.
    map.push_in(Stamp {
        first: 4800,
        frames: 0,
        device_ns: 0,
        callback_ns: 0,
    });
    assert_eq!(map.quality(), ClockQuality::Acquiring);
    // A stream reopened without `reset`: the counter restarts.
    map.push_in(stamp(0, 100));
    assert_eq!(map.quality(), ClockQuality::Broken);
    let before = map.stats(Side::Out).stamps;
    map.push_out(stamp(4800, 100));
    assert_eq!(
        map.stats(Side::Out).stamps,
        before,
        "stamps ignored while broken"
    );
    map.reset();
    assert_eq!(map.quality(), ClockQuality::Acquiring);
}

// ---------------------------------------------------------------- bad stamps

#[test]
fn bad_device_stamps_fall_back_to_callback_stamps() {
    let mut sc = Scenario::new(RATES[1], (120.0, -80.0), 50.0e-6, 25.0, 900);
    sc.dev_skew_ppm = (5000.0, 0.0); // the input device stamps run 0.5% fast
    let r = run(&sc);
    let fallbacks: Vec<&StepSeen> = r
        .steps
        .iter()
        .filter(|s| s.step.cause == StepCause::Fallback)
        .collect();
    assert_eq!(fallbacks.len(), 1, "{:?}", r.steps);
    assert_eq!(fallbacks[0].step.side, Side::In);
    assert!(
        fallbacks[0].t > 1.9 && fallbacks[0].t < 2.3,
        "at {}",
        fallbacks[0].t
    );
    assert_eq!(fallbacks[0].step.step_s, 0.0);
    let st = r.map.stats(Side::In);
    assert_eq!(st.source, StampSource::Callback);
    assert_eq!(st.fallbacks, 1);
    assert_eq!(r.map.stats(Side::Out).source, StampSource::Device);
    assert_eq!(r.quality_at(2.5), ClockQuality::Acquiring);
    assert_eq!(r.last.quality(), ClockQuality::Locked);
    // Callback stamps carry a constant delay (2 ms here): it shifts the map, and the
    // loopback calibration absorbs it. What matters is that the map is steady.
    let e = r.errs(15.0, 25.0);
    let mean = e.iter().sum::<f64>() / e.len() as f64;
    let spread = e.iter().fold(0.0f64, |m, x| m.max((x - mean).abs()));
    assert!((mean + 2.08).abs() < 0.1, "offset {mean:.3} ms");
    assert!(spread <= 0.15, "spread {spread:.3} ms");
    let drift = r.samples.last().expect("samples").drift_err_ppm;
    assert!(
        drift.abs() <= 10.0,
        "drift error {drift:.1} ppm on callback stamps"
    );
    println!(
        "bad input device stamps: fallback at {:.2} s, offset {mean:.3} ms, spread {spread:.3} ms, drift error {drift:.1} ppm",
        fallbacks[0].t
    );
}

#[test]
fn bad_device_and_callback_stamps_degrade_to_the_nominal_ratio() {
    let mut sc = Scenario::new(RATES[1], (120.0, -80.0), 50.0e-6, 12.0, 910);
    sc.dev_skew_ppm = (5000.0, 0.0);
    sc.cb_skew_ppm = (5000.0, 0.0);
    let r = run(&sc);
    assert_eq!(r.map.stats(Side::In).source, StampSource::Callback);
    assert!(r.map.stats(Side::In).degraded);
    assert_eq!(r.quality_at(4.5), ClockQuality::Degraded);
    assert_eq!(r.last.quality(), ClockQuality::Degraded);
    assert!(r.last.drift_ppm().abs() < 1.0e-9, "nominal ratio");
    let slope = r.last.in_of_out(48_000.0) - r.last.in_of_out(0.0);
    assert!((slope - 44_100.0).abs() < 1.0e-6, "slope {slope}");

    // Without the fallback the first bad window degrades at once.
    sc.cfg.callback_fallback = false;
    let r = run(&sc);
    assert_eq!(r.map.stats(Side::In).source, StampSource::Device);
    assert_eq!(r.quality_at(2.5), ClockQuality::Degraded);
    assert!(r.steps.is_empty());
}

#[test]
fn degraded_clears_when_the_stamps_are_clean_again() {
    // A storm of losses (3 without a clean window) is treated as bad stamps; with the
    // fallback off the stream degrades, and the next clean window restores it.
    let mut sc = Scenario::new(RATES[0], (0.0, 0.0), 50.0e-6, 24.0, 920);
    sc.cfg.callback_fallback = false;
    for k in 0..3 {
        sc = sc.with(Event::Loss {
            side: Side::Out,
            at_s: 12.0 + 0.5 * f64::from(k),
            seconds: 0.010,
            notice: false,
        });
    }
    let r = run(&sc);
    assert_eq!(r.steps.len(), 3);
    assert_eq!(r.quality_at(13.2), ClockQuality::Degraded);
    assert_eq!(r.last.quality(), ClockQuality::Locked);
    assert!(r.max_abs_err(20.0, 24.0) <= 0.10);

    // With the fallback on, the same storm moves the stream to callback stamps.
    sc.cfg.callback_fallback = true;
    let r = run(&sc);
    assert_eq!(r.map.stats(Side::Out).source, StampSource::Callback);
    assert_eq!(
        r.steps.last().expect("steps").step.cause,
        StepCause::Fallback
    );
    assert_eq!(r.last.quality(), ClockQuality::Locked);
}

#[test]
fn a_stream_at_the_wrong_rate_never_locks() {
    // Reported 48 kHz, running at 44.1 kHz: both stamp sources show an 8% rate error.
    let mut sc = Scenario::new(RATES[0], (0.0, 0.0), 50.0e-6, 15.0, 930);
    sc.drift_in_ppm = (44_100.0 / 48_000.0 - 1.0) * 1.0e6;
    let r = run(&sc);
    assert!(r.samples.iter().all(|s| s.quality != ClockQuality::Locked));
    assert_eq!(r.last.quality(), ClockQuality::Degraded);
}

// ---------------------------------------------------------------- block sizes

#[test]
fn extreme_block_sizes_stay_within_the_gate() {
    for (blocks, seed) in [
        ((0.0002, 0.080), 1000),
        ((0.0002, 0.003), 1001),
        ((0.040, 0.090), 1002),
    ] {
        let mut sc = Scenario::new(RATES[1], (200.0, 0.0), 50.0e-6, 20.0, seed);
        sc.blocks_s = blocks;
        let r = run(&sc);
        let p = p95(&r.errs(5.0, 20.0));
        println!(
            "blocks {:.1}..{:.1} ms: p95 {p:.4} ms",
            blocks.0 * 1e3,
            blocks.1 * 1e3
        );
        assert!(p <= 0.10, "blocks {blocks:?}: p95 {p:.4} ms");
        assert!(r.steps.is_empty());
        assert_eq!(r.last.quality(), ClockQuality::Locked);
    }
}

#[test]
fn a_long_silence_without_lost_frames_keeps_the_line() {
    for side in [Side::In, Side::Out] {
        let sc = Scenario::new(RATES[1], (-200.0, 0.0), 50.0e-6, 24.0, 1010).with(Event::Silence {
            side,
            at_s: 12.0,
            seconds: 5.0,
        });
        let r = run(&sc);
        assert!(r.steps.is_empty(), "{:?}", r.steps);
        // Largest right after the silence: the phase then rests on single stamps (50 us
        // of jitter each) until the running mean has a few of them.
        let e = r.max_abs_err(12.0, 24.0);
        assert!(e <= 0.10, "{side:?}: error {e:.4} ms across a 5 s silence");
        // The first stamp after the silence re-seats the phase: no step, the period is
        // kept, and the map is not Locked again until a window has confirmed the line.
        assert_eq!(r.map.stats(side).gaps, 1);
        assert_eq!(r.map.stats(side).outliers, 0);
        assert_eq!(r.quality_at(11.9), ClockQuality::Locked);
        assert_eq!(r.quality_at(17.1), ClockQuality::Acquiring, "{side:?}");
        assert_eq!(r.quality_at(19.5), ClockQuality::Locked, "{side:?}");
        let drift = r
            .samples
            .iter()
            .filter(|s| s.t >= 17.1 && s.t < 19.0)
            .fold(0.0f64, |m, s| m.max(s.drift_err_ppm.abs()));
        assert!(
            drift <= 10.0,
            "{side:?}: drift error {drift:.2} ppm after the silence"
        );
        println!(
            "{side:?} silent for 5 s: max error 12..24 s {e:.4} ms, drift error after it {drift:.2} ppm"
        );
    }
}

// ---------------------------------------------------------------- snapshot, origin, allocation

#[test]
fn snapshot_maps_are_inverse_and_origin_offset_is_applied() {
    let base = Scenario::new(RATES[1], (120.0, -80.0), 50.0e-6, 10.0, 1100);
    let a = run(&base);
    for f in [0.0, 123_456.7, 480_000.0, 1.0e7] {
        let g = a.last.in_of_out(f);
        assert!(
            (a.last.out_of_in(g) - f).abs() < 1.0e-5,
            "round trip at {f}"
        );
        assert!((a.last.in_time_s(g) - a.last.out_time_s(f)).abs() < 1.0e-10);
    }
    // The input device reports on another clock, 123 s behind (and ahead): with the
    // offset in the configuration the map is the same.
    for shift in [123_456_789_000i64, -123_456_789_000] {
        let mut sc = base.clone();
        sc.in_origin_shift_ns = shift;
        let b = run(&sc);
        let (ea, eb) = (a.errs(5.0, 10.0), b.errs(5.0, 10.0));
        assert_eq!(ea.len(), eb.len());
        let d = ea
            .iter()
            .zip(&eb)
            .fold(0.0f64, |m, (x, y)| m.max((x - y).abs()));
        assert!(d < 1.0e-6, "maps differ by {d} ms with shift {shift}");
    }
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    map.set_origin_offset_ns(1_000_000_000);
    assert_eq!(map.config().origin_offset_ns, 1_000_000_000);
}

/// Regular 10 ms blocks on both streams; the stamps run `skew` fast, lose 20 ms at block
/// `loss_at`, carry one 30 ms outlier at block 50 and an xrun notice at block 900.
fn plain_series(skew: f64, loss_at: u64, blocks: u64) -> Vec<(Side, Stamp, bool)> {
    let mut v = Vec::new();
    for k in 0..blocks {
        let lost = if k >= loss_at { 20_000_000 } else { 0 };
        let t = 7_000_000_000 + (10_000_000.0 * k as f64 * (1.0 + skew)) as u64 + lost;
        let s = Stamp {
            first: 480 * k,
            frames: 480,
            device_ns: t + if k == 50 { 30_000_000 } else { 0 },
            callback_ns: t,
        };
        v.push((Side::In, s, k == 900));
        v.push((Side::Out, s, false));
    }
    v
}

#[test]
fn push_and_snapshot_do_not_allocate() {
    // Every path: start, lock, outlier, jump, notice, late notice, fallback, degraded,
    // broken, reset.
    let clean = plain_series(0.0, 600, 1200);
    let skewed = plain_series(0.01, u64::MAX, 1200);
    let mut a = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    let mut b = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    let (mut jumps, mut fallbacks, mut degraded, mut sum) = (0u32, 0u32, false, 0.0f64);
    let mut late_steps = 0u32;
    assert_no_alloc(|| {
        for (map, list) in [(&mut a, &clean), (&mut b, &skewed)] {
            for (side, s, notice) in list {
                if *notice {
                    map.note_xrun(*side);
                }
                let step = match side {
                    Side::In => map.push_in(*s),
                    Side::Out => map.push_out(*s),
                };
                match step.map(|s| s.cause) {
                    Some(StepCause::Fallback) => fallbacks += 1,
                    Some(_) => jumps += 1,
                    None => {}
                }
                // A notice for a stamp pushed long ago (the whole history is fed
                // again), and one for the stamp before this one.
                if s.first == 480 * 950 {
                    map.note_xrun_at(*side, 0, |_| late_steps += 1);
                    map.note_xrun_at(*side, 480 * 949, |_| late_steps += 1);
                }
                let snap = map.snapshot();
                degraded |= snap.quality() == ClockQuality::Degraded;
                sum += snap.in_of_out(s.first as f64) + snap.drift_ppm() + map.stats(*side).rms_e_s;
            }
            map.push_in(Stamp {
                first: 0,
                frames: 1,
                device_ns: 0,
                callback_ns: 0,
            });
            assert!(map.quality() == ClockQuality::Broken);
            map.reset();
        }
    });
    assert_eq!(jumps, 2, "one 20 ms loss per stream");
    assert_eq!(late_steps, 0, "the late notices found nothing to step over");
    assert_eq!(fallbacks, 2, "both skewed streams fell back");
    assert!(degraded, "and then degraded");
    assert!(sum.is_finite());
    assert_eq!((a.epoch(), b.epoch()), (1, 1));
}

// ---------------------------------------------------------------- review findings
//
// Hand-made stamps, so that each stamp is exactly what the case needs.

/// Block `k` of a 48 kHz stream with 480-frame blocks: exact device time plus `late_s`.
fn exact(k: u64, late_s: f64) -> Stamp {
    let t = 0.010 * k as f64;
    Stamp {
        first: 480 * k,
        frames: 480,
        device_ns: ns(t + late_s),
        callback_ns: ns(t + 0.012),
    }
}

/// A map locked on 12 s of exact stamps on both streams. The next block is 1200.
fn locked_map() -> ClockMap {
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    for k in 0..1200 {
        assert!(map.push_out(exact(k, 0.0)).is_none());
        assert!(map.push_in(exact(k, 0.0)).is_none());
    }
    assert_eq!(map.quality(), ClockQuality::Locked);
    map
}

/// Error of the input line at block `k`, seconds, when the stream is `late_s` late.
fn in_line_error(map: &ClockMap, k: u64, late_s: f64) -> f64 {
    map.snapshot().in_time_s(480.0 * k as f64) - (0.010 * k as f64 + late_s)
}

#[test]
fn a_held_outlier_does_not_bias_a_noticed_step() {
    // An outlier (+60 ms) is still pending when a real loss (+20 ms) arrives with its
    // notice. The step is the loss, not the mean of the two.
    let mut map = locked_map();
    assert!(map.push_in(exact(1200, 0.060)).is_none());
    map.note_xrun(Side::In);
    let step = map
        .push_in(exact(1201, 0.020))
        .expect("the notice confirms the loss on its first stamp");
    assert_eq!(step.cause, StepCause::Notice);
    assert_eq!(step.frame, 480 * 1201);
    assert!(
        (step.step_s - 0.020).abs() < 1.0e-4,
        "step {} s",
        step.step_s
    );
    for k in 1202..1500 {
        assert!(map.push_out(exact(k, 0.0)).is_none());
        let again = map.push_in(exact(k, 0.020));
        assert!(again.is_none(), "second step at block {k}: {again:?}");
    }
    let st = map.stats(Side::In);
    assert_eq!((st.steps, st.outliers), (1, 1));
    let err = in_line_error(&map, 1500, 0.020);
    assert!(err.abs() < 1.0e-4, "line off by {err} s");
    assert_eq!(map.quality(), ClockQuality::Locked);
}

#[test]
fn a_held_outlier_does_not_bias_a_detected_step() {
    // The same without a notice: the outlier does not count towards the three stamps
    // and does not enter their mean.
    let mut map = locked_map();
    assert!(map.push_in(exact(1200, 0.060)).is_none());
    assert!(map.push_in(exact(1201, 0.020)).is_none());
    assert!(map.push_in(exact(1202, 0.020)).is_none());
    let step = map
        .push_in(exact(1203, 0.020))
        .expect("three stamps that agree");
    assert_eq!(step.cause, StepCause::Detected);
    assert_eq!(step.frame, 480 * 1201, "the first stamp of the run");
    assert!(
        (step.step_s - 0.020).abs() < 1.0e-4,
        "step {} s",
        step.step_s
    );
    for k in 1204..1500 {
        assert!(map.push_out(exact(k, 0.0)).is_none());
        let again = map.push_in(exact(k, 0.020));
        assert!(again.is_none(), "second step at block {k}: {again:?}");
    }
    let st = map.stats(Side::In);
    assert_eq!((st.steps, st.outliers), (1, 1));
    let err = in_line_error(&map, 1500, 0.020);
    assert!(err.abs() < 1.0e-4, "line off by {err} s");
}

/// Feeds 15 s where the input device stamps are `device(k)` and everything else is
/// exact. Returns the map and the block of the fallback.
fn bad_input_device_stamps(device: impl Fn(u64) -> f64) -> (ClockMap, Option<u64>) {
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    let mut fallback_at = None;
    for k in 0..1500 {
        assert!(map.push_out(exact(k, 0.0)).is_none());
        let mut s = exact(k, 0.0);
        s.device_ns = ns(device(k));
        if let Some(step) = map.push_in(s) {
            assert_eq!(step.cause, StepCause::Fallback, "block {k}: {step:?}");
            assert!(fallback_at.replace(k).is_none(), "two fallbacks");
        }
    }
    (map, fallback_at)
}

#[test]
fn frozen_device_stamps_fall_back_to_callback_stamps() {
    // The capture time never moves; the callback stamps are healthy.
    let (map, fallback_at) = bad_input_device_stamps(|_| 0.0);
    let st = map.stats(Side::In);
    assert_eq!(
        (st.source, st.fallbacks),
        (StampSource::Callback, 1),
        "stuck on the frozen device stamps: {st:?}"
    );
    let at = fallback_at.expect("a fallback step");
    assert!(at <= 400, "fallback only at block {at}");
    assert_eq!(st.steps, 0);
    assert_eq!(map.quality(), ClockQuality::Locked);
    println!("frozen device stamps: fallback at block {at}");
}

#[test]
fn start_up_realignments_count_as_bad_stamps() {
    // Device stamps that run 18% fast: every stamp is 1.8 ms further off, so after the
    // first realignment each group of three agrees and realigns again, and no stamp is
    // ever accepted. The silent start-up realignments must still end in a fallback.
    let (map, fallback_at) = bad_input_device_stamps(|k| 0.010 * 1.18 * k as f64);
    let st = map.stats(Side::In);
    assert_eq!(
        (st.source, st.fallbacks),
        (StampSource::Callback, 1),
        "stuck on the fast device stamps: {st:?}"
    );
    let at = fallback_at.expect("a fallback step");
    assert!(at <= 400, "fallback only at block {at}");
    assert_eq!(map.quality(), ClockQuality::Locked);
    println!("device stamps 18% fast: fallback at block {at}");
}

#[test]
fn alternating_long_and_short_gaps_do_not_grow() {
    // Stamps of the output stream arrive in pairs 0.1 s apart, the pairs 120 s apart
    // (no frame is lost). One stamp is 100 us late. With gains capped for the long
    // interval the error of the line grew by 1.87 per pair.
    let mut map = locked_map();
    let at = |frame: u64, late_s: f64| {
        let t = frame as f64 / 48_000.0;
        Stamp {
            first: frame,
            frames: 480,
            device_ns: ns(t + late_s),
            callback_ns: ns(t + 0.012),
        }
    };
    let line_error = |map: &ClockMap, frame: u64| {
        map.snapshot().out_time_s(frame as f64) - frame as f64 / 48_000.0
    };
    let mut frame = 480 * 1200;
    let mut errors = Vec::new();
    for pair in 0..24 {
        frame += 120 * 48_000;
        errors.push(line_error(&map, frame).abs());
        assert!(
            map.push_out(at(frame, if pair == 0 { 100.0e-6 } else { 0.0 }))
                .is_none()
        );
        frame += 4_800;
        assert!(map.push_out(at(frame, 0.0)).is_none());
    }
    let worst = errors.iter().fold(0.0f64, |m, e| m.max(*e));
    let (third, last) = (errors[2], errors[23]);
    println!(
        "pairs 120 s apart after a 100 us late stamp: line error before each pair, worst {:.3} ms, third pair {:.3} ms, last pair {:.3} ms",
        worst * 1e3,
        third * 1e3,
        last * 1e3
    );
    assert!(worst < 1.0e-3, "worst error {worst} s");
    assert!(last <= third, "the error grows: {third} s, then {last} s");
    let st = map.stats(Side::Out);
    assert_eq!((st.outliers, st.steps, st.gaps), (0, 0, 24));
    assert_eq!(map.quality(), ClockQuality::Acquiring);
}

#[test]
fn a_frame_counter_at_the_end_of_its_range_is_broken() {
    for side in [Side::In, Side::Out] {
        let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
        let s = Stamp {
            first: u64::MAX,
            frames: 1,
            device_ns: 0,
            callback_ns: 0,
        };
        let step = match side {
            Side::In => map.push_in(s),
            Side::Out => map.push_out(s),
        };
        assert!(step.is_none());
        assert_eq!(map.quality(), ClockQuality::Broken, "{side:?}");
        map.reset();
        assert_eq!(map.quality(), ClockQuality::Acquiring);
    }
}

// ---------------------------------------------------------------- late xrun notice
//
// `note_xrun_at`: the notice arrives after the first stamp that follows the xrun.

fn push(map: &mut ClockMap, side: Side, s: Stamp) -> Option<ClockStep> {
    match side {
        Side::In => map.push_in(s),
        Side::Out => map.push_out(s),
    }
}

fn other(side: Side) -> Side {
    match side {
        Side::In => Side::Out,
        Side::Out => Side::In,
    }
}

/// Two locked maps get the same stamps: from block 1200 on, the stream of `side` is
/// `loss_s` late. Map `a` gets the notice in time (before block 1200); map `b` gets it
/// with its boundary after `late` more stamps of that stream. Returns the maps and the
/// steps of each, after 300 blocks.
fn in_time_and_late(side: Side, loss_s: f64, late: u64) -> [(ClockMap, Vec<ClockStep>); 2] {
    let (mut a, mut b) = (locked_map(), locked_map());
    let (mut steps_a, mut steps_b) = (Vec::new(), Vec::new());
    a.note_xrun(side);
    for j in 0..300 {
        let k = 1200 + j;
        for map in [&mut a, &mut b] {
            assert!(push(map, other(side), exact(k, 0.0)).is_none());
        }
        steps_a.extend(push(&mut a, side, exact(k, loss_s)));
        steps_b.extend(push(&mut b, side, exact(k, loss_s)));
        if j + 1 == late {
            b.note_xrun_at(side, 480 * 1200, |s| steps_b.push(s));
        }
    }
    [(a, steps_a), (b, steps_b)]
}

#[test]
fn a_late_notice_gives_what_a_notice_in_time_gives() {
    // The stamp at the boundary, and up to 15 more, went through the map before the
    // notice. The map goes back and feeds them again behind the notice: the same step,
    // the same lines, bit for bit. 1.5 ms is below the jump threshold (the stamps entered
    // the loop and pulled the line); 20 ms is above it (the stamps were held back: only
    // the first two can still be redone, the third one makes the step by itself).
    for side in [Side::In, Side::Out] {
        for (loss_s, lates) in [
            (0.0015, &[1u64, 2, 3, 5, 16][..]),
            (0.020, &[1, 2][..]),
            (0.0, &[1, 7, 16][..]),
        ] {
            for &late in lates {
                let [(a, steps_a), (b, steps_b)] = in_time_and_late(side, loss_s, late);
                let what = format!("{side:?}, {loss_s} s lost, notice {late} stamps late");
                assert_eq!(steps_a, steps_b, "{what}");
                assert_eq!(a.snapshot(), b.snapshot(), "{what}");
                assert_eq!(a.stats(side), b.stats(side), "{what}");
                assert_eq!(a.stats(other(side)), b.stats(other(side)), "{what}");
                assert_eq!(b.stats(side).xrun_notices, 1);
                if loss_s == 0.0 {
                    assert!(steps_b.is_empty(), "{what}: {steps_b:?}");
                    continue;
                }
                assert_eq!(steps_b.len(), 1, "{what}: {steps_b:?}");
                let s = steps_b[0];
                assert_eq!(
                    (s.side, s.frame, s.cause),
                    (side, 480 * 1200, StepCause::Notice)
                );
                assert!((s.step_s - loss_s).abs() < 1.0e-6, "{what}: {}", s.step_s);
                assert_eq!(b.quality(), ClockQuality::Locked);
            }
        }
    }
}

#[test]
fn a_late_notice_takes_the_stamp_out_of_the_loop() {
    // What the late notice is for. Without it a 1.5 ms loss is absorbed by the loop:
    // the line is wrong for seconds and the period is pulled. With it (three stamps
    // late) the line is right at once and the period never moved.
    let line_error = |map: &ClockMap, k: u64| in_line_error(map, k, 0.0015).abs();
    let mut plain = locked_map();
    let mut noticed = locked_map();
    let p0 = noticed.snapshot().parts().p_in;
    for k in 1200..1203 {
        for map in [&mut plain, &mut noticed] {
            assert!(map.push_out(exact(k, 0.0)).is_none());
            assert!(map.push_in(exact(k, 0.0015)).is_none());
        }
    }
    let mut steps = Vec::new();
    noticed.note_xrun_at(Side::In, 480 * 1200, |s| steps.push(s));
    assert_eq!(steps.len(), 1);
    assert_eq!(
        (steps[0].frame, steps[0].cause),
        (480 * 1200, StepCause::Notice)
    );
    let (e_plain, e_noticed) = (line_error(&plain, 1203), line_error(&noticed, 1203));
    let moved = |map: &ClockMap| (map.snapshot().parts().p_in / p0 - 1.0).abs() * 1.0e6;
    println!(
        "1.5 ms loss, three stamps later: line off by {:.4} ms without the notice (period moved \
         {:.3} ppm), {:.6} ms with the late notice (period moved {:.6} ppm)",
        e_plain * 1.0e3,
        moved(&plain),
        e_noticed * 1.0e3,
        moved(&noticed)
    );
    assert!(e_plain > 1.0e-3, "{e_plain}");
    assert!(moved(&plain) > 1.0, "{}", moved(&plain));
    assert!(e_noticed < 1.0e-6, "{e_noticed}");
    assert!(moved(&noticed) < 1.0e-3, "{}", moved(&noticed));
    assert_eq!(noticed.stats(Side::In).steps, 1);
    assert_eq!(plain.stats(Side::In).steps, 0);
}

#[test]
fn a_late_notice_before_its_stamp_is_a_plain_notice() {
    // Nothing at or after the boundary was pushed: the notice waits for the next stamp.
    for side in [Side::In, Side::Out] {
        let (mut a, mut b) = (locked_map(), locked_map());
        a.note_xrun(side);
        b.note_xrun_at(side, 480 * 1200, |s| panic!("nothing to redo: {s:?}"));
        for k in 1200..1210 {
            let (sa, sb) = (
                push(&mut a, side, exact(k, 0.002)),
                push(&mut b, side, exact(k, 0.002)),
            );
            assert_eq!(sa, sb);
            assert_eq!(sa.is_some(), k == 1200, "block {k}");
        }
        assert_eq!(a.snapshot(), b.snapshot());
        assert_eq!(a.stats(side), b.stats(side));
    }
    // A map with no stamps at all, and one that started over.
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    map.note_xrun_at(Side::Out, 0, |s| panic!("{s:?}"));
    assert_eq!(map.stats(Side::Out).xrun_notices, 1);
    let mut map = locked_map();
    map.reset();
    map.note_xrun_at(Side::In, 480 * 1199, |s| panic!("{s:?}"));
    assert_eq!(map.stats(Side::In).xrun_notices, 1);
    assert_eq!(map.quality(), ClockQuality::Acquiring);
}

#[test]
fn a_late_notice_after_the_step_was_found_is_only_counted() {
    // 20 ms lost: the jump rule finds it on the third stamp. A notice that comes after
    // that changes nothing but the counter.
    for side in [Side::In, Side::Out] {
        for late in [3u64, 4, 10] {
            let [(a, steps_a), (b, steps_b)] = in_time_and_late(side, 0.020, late);
            assert_eq!(steps_b.len(), 1, "{steps_b:?}");
            let s = steps_b[0];
            assert_eq!((s.frame, s.cause), (480 * 1200, StepCause::Detected));
            assert_eq!(steps_a[0].frame, s.frame);
            assert!((s.step_s - steps_a[0].step_s).abs() < 1.0e-6);
            let st = b.stats(side);
            assert_eq!((st.steps, st.xrun_notices), (1, 1));
            let late_line = in_line_or_out(&b, side, 1500, 0.020);
            assert!(late_line.abs() < 1.0e-6, "{late_line}");
            assert!(in_line_or_out(&a, side, 1500, 0.020).abs() < 1.0e-6);
        }
    }
    // The call itself: no step, same snapshot.
    let mut map = locked_map();
    for k in 1200..1206 {
        map.push_in(exact(k, 0.020));
    }
    let before = map.snapshot();
    map.note_xrun_at(Side::In, 480 * 1200, |s| panic!("{s:?}"));
    assert_eq!(map.snapshot(), before);
}

/// Error of the line of `side` at block `k`, seconds, when the stream is `late_s` late.
fn in_line_or_out(map: &ClockMap, side: Side, k: u64, late_s: f64) -> f64 {
    let (f, s) = (480.0 * k as f64, map.snapshot());
    let t = match side {
        Side::In => s.in_time_s(f),
        Side::Out => s.out_time_s(f),
    };
    t - (0.010 * k as f64 + late_s)
}

/// Like [`in_time_and_late`] for map `b` alone, returning what `note_xrun_at` said.
fn late_only(side: Side, loss_s: f64, late: u64) -> (ClockMap, Vec<ClockStep>, LateNotice) {
    let mut b = locked_map();
    let mut steps = Vec::new();
    let mut said = None;
    for j in 0..300 {
        let k = 1200 + j;
        assert!(push(&mut b, other(side), exact(k, 0.0)).is_none());
        steps.extend(push(&mut b, side, exact(k, loss_s)));
        if j + 1 == late {
            said = Some(b.note_xrun_at(side, 480 * 1200, |s| steps.push(s)));
        }
    }
    (b, steps, said.expect("the notice was given"))
}

/// The stamps of [`late_only`] with no notice at all.
fn late_only_without_notice(side: Side, loss_s: f64) -> (ClockMap, Vec<ClockStep>, ()) {
    let mut b = locked_map();
    let mut steps = Vec::new();
    for k in 1200..1500 {
        assert!(push(&mut b, other(side), exact(k, 0.0)).is_none());
        steps.extend(push(&mut b, side, exact(k, loss_s)));
    }
    (b, steps, ())
}

#[test]
fn a_late_notice_older_than_the_history_is_not_moved_to_a_later_stamp() {
    // A1 review, finding 1. The history holds UNDO_STAMPS stamps per stream. A notice
    // exactly that late still reaches the stamp at the boundary and is exact.
    for side in [Side::In, Side::Out] {
        let late = UNDO_STAMPS as u64;
        let [(a, steps_a), (b, steps_b)] = in_time_and_late(side, 0.0015, late);
        assert_eq!(steps_a, steps_b);
        assert_eq!(a.snapshot(), b.snapshot());
        assert_eq!(late_only(side, 0.0015, late).2, LateNotice::Replayed);
        // One stamp later the stamp at the boundary is gone. The map used to apply the
        // notice at the oldest stamp it still had, a later one, and report the step
        // there: a wrong place, after the first stamps had biased the loop. Now it says
        // so, and claims no place between the boundary and the notice.
        for late in [UNDO_STAMPS as u64 + 1, UNDO_STAMPS as u64 + 4] {
            let (b, steps, said) = late_only(side, 0.0015, late);
            let what = format!("{side:?}, notice {late} stamps late");
            assert_eq!(said, LateNotice::Unavailable, "{what}");
            let notice_at = 480 * (1200 + late);
            assert!(
                steps.iter().all(|s| s.frame >= notice_at),
                "{what}: a step placed before the notice came: {steps:?}"
            );
            // What is left of the loss re-anchors the line at the next stamp. The period
            // the stamps pulled meanwhile is not undone: the line keeps a small error
            // for a while (the caller does not judge up to the re-anchor), below the one
            // the loop alone leaves.
            assert_eq!(steps.len(), 1, "{what}: {steps:?}");
            assert_eq!(
                (steps[0].frame, steps[0].cause),
                (notice_at, StepCause::Notice)
            );
            assert!(steps[0].step_s > 0.25e-3 && steps[0].step_s < 1.5e-3);
            let err = in_line_or_out(&b, side, 1500, 0.0015);
            let (plain, _, _) = late_only_without_notice(side, 0.0015);
            let err_plain = in_line_or_out(&plain, side, 1500, 0.0015);
            println!(
                "{what}: step of {:.4} ms at the notice; line off by {:.4} ms 3 s later \
                 ({:.4} ms with no notice at all)",
                steps[0].step_s * 1.0e3,
                err * 1.0e3,
                err_plain * 1.0e3
            );
            assert!(err.abs() < 0.25e-3, "{what}: {err}");
            assert!(err.abs() < err_plain.abs(), "{what}: {err} vs {err_plain}");
            assert_eq!(b.stats(side).xrun_notices, 1);
        }
    }
}

#[test]
fn a_late_notice_is_not_hidden_by_a_later_unrelated_step() {
    // A1 review, finding 2. Locked; the stream loses 1.5 ms at block 1200 and another
    // 20 ms at block 1204. The jump rule finds the 20 ms on block 1206 (step at 1204).
    // The notice of the first loss arrives after block 1207, with its boundary. It used
    // to be dropped because a step sat somewhere after its boundary; now the stream goes
    // back to 1200, the 1.5 ms becomes its own step, and the 20 ms step, found again on
    // the way, is not reported twice. The result is what a notice in time gives.
    let late_s = |k: u64| match k {
        ..1200 => 0.0,
        1200..1204 => 0.0015,
        _ => 0.0215,
    };
    for side in [Side::In, Side::Out] {
        let (mut a, mut b) = (locked_map(), locked_map());
        let (mut steps_a, mut steps_b) = (Vec::new(), Vec::new());
        let mut said = None;
        a.note_xrun(side);
        for k in 1200..1500 {
            for map in [&mut a, &mut b] {
                assert!(push(map, other(side), exact(k, 0.0)).is_none());
            }
            steps_a.extend(push(&mut a, side, exact(k, late_s(k))));
            steps_b.extend(push(&mut b, side, exact(k, late_s(k))));
            if k == 1207 {
                assert_eq!(steps_b.len(), 1, "{steps_b:?}");
                said = Some(b.note_xrun_at(side, 480 * 1200, |s| steps_b.push(s)));
                // The second notice, late too: a step already sits on its stamp.
                let again = b.note_xrun_at(side, 480 * 1204, |s| panic!("{s:?}"));
                assert_eq!(again, LateNotice::Found);
            }
        }
        assert_eq!(said, Some(LateNotice::Replayed));
        steps_b.sort_by_key(|s| s.frame);
        let what = format!("{side:?}");
        // Same steps, same places. The size of the 20 ms step was reported before the
        // notice came (measured against the line the 1.5 ms had pulled) and is not
        // reported again.
        let places = |v: &[ClockStep]| v.iter().map(|s| (s.frame, s.cause)).collect::<Vec<_>>();
        assert_eq!(steps_a.len(), 2, "{what}: {steps_a:?}");
        assert_eq!(places(&steps_a), places(&steps_b), "{what}: {steps_b:?}");
        assert_eq!(
            (steps_b[0].frame, steps_b[0].cause),
            (480 * 1200, StepCause::Notice)
        );
        assert!((steps_b[0].step_s - 0.0015).abs() < 1.0e-6, "{what}");
        assert_eq!(
            (steps_b[1].frame, steps_b[1].cause),
            (480 * 1204, StepCause::Detected)
        );
        assert_eq!(a.snapshot(), b.snapshot(), "{what}");
        let (sa, sb) = (a.stats(side), b.stats(side));
        assert_eq!((sa.steps, sb.steps), (2, 2), "{what}");
        assert_eq!((sa.xrun_notices, sb.xrun_notices), (1, 2), "{what}");
    }
}

#[test]
fn a_late_notice_keeps_the_notices_armed_after_it() {
    // Two notices, the first one late. The second one stays armed where it was, both
    // when it sits between two stamps that are fed again and when it is still waiting
    // for its stamp. Seen through a 1 ms offset on the last stamp the second notice
    // covers (a notice lasts three stamps): it steps only if that notice is still armed.
    for second_before in [1201u64, 1202] {
        let (mut a, mut b) = (locked_map(), locked_map());
        let (mut steps_a, mut steps_b) = (Vec::new(), Vec::new());
        let late_s = |k: u64| if k >= second_before + 2 { 0.001 } else { 0.0 };
        // In time.
        a.note_xrun(Side::In);
        for k in 1200..1210 {
            if k == second_before {
                a.note_xrun(Side::In);
            }
            steps_a.extend(a.push_in(exact(k, late_s(k))));
        }
        // The first notice comes when blocks 1200 and 1201 are in.
        for k in 1200..1210 {
            if k == second_before {
                b.note_xrun(Side::In);
            }
            if k == 1202 {
                b.note_xrun_at(Side::In, 480 * 1200, |s| steps_b.push(s));
            }
            steps_b.extend(b.push_in(exact(k, late_s(k))));
        }
        assert_eq!(steps_a.len(), 1, "{second_before}: {steps_a:?}");
        assert_eq!(steps_a[0].frame, 480 * (second_before + 2));
        assert_eq!(steps_a, steps_b, "{second_before}");
        assert_eq!(a.snapshot(), b.snapshot());
        assert_eq!(a.stats(Side::In), b.stats(Side::In));
        assert_eq!(b.stats(Side::In).xrun_notices, 2);
    }
    // The control: without the second notice that stamp does not step.
    let mut c = locked_map();
    c.note_xrun(Side::In);
    for k in 1200..1210 {
        let late_s = if k >= 1203 { 0.001 } else { 0.0 };
        assert!(c.push_in(exact(k, late_s)).is_none(), "block {k}");
    }
}

#[test]
fn a_late_notice_on_a_broken_map_does_nothing() {
    let mut map = locked_map();
    map.push_in(exact(3, 0.0)); // the frame counter went back
    assert_eq!(map.quality(), ClockQuality::Broken);
    map.note_xrun_at(Side::In, 480 * 1199, |s| panic!("{s:?}"));
    map.note_xrun_at(Side::Out, 0, |s| panic!("{s:?}"));
    assert_eq!(map.quality(), ClockQuality::Broken);
}

// ---------------------------------------------------------------- parts and host time

#[test]
fn snapshot_parts_round_trip() {
    let r = run(&Scenario::new(
        RATES[1],
        (120.0, -80.0),
        50.0e-6,
        10.0,
        1100,
    ));
    let (s, p) = (r.last, r.last.parts());
    assert_eq!(ClockSnapshot::from_parts(p), s);
    assert_eq!((p.epoch, p.quality), (s.epoch(), s.quality()));
    assert_eq!((p.fs_in, p.fs_out), (44_100.0, 48_000.0));
    assert_eq!(
        (p.src_in, p.src_out),
        (s.source(Side::In), s.source(Side::Out))
    );
    assert_eq!((p.n_in, p.n_out), (s.last_in_frame(), s.last_out_frame()));
    // The parts are the lines: T(n) = t + p·(n − n0).
    for f in [0.0, 4321.5, 400_000.0] {
        assert_eq!(s.out_time_s(f), p.t_out + p.p_out * (f - p.n_out));
        assert_eq!(s.in_time_s(f), p.t_in + p.p_in * (f - p.n_in));
    }
    // The base is the host time of the first stamp of the epoch.
    let base = p.base_ns.expect("stamps were pushed");
    assert!((base - HOST0_NS).abs() < 1_000_000, "{base}");
    assert_eq!(locked_map().snapshot().parts().base_ns, Some(HOST0_NS));
    // A changed part is a changed snapshot.
    let mut q = p;
    q.p_out *= 1.0 + 1.0e-6;
    assert_ne!(ClockSnapshot::from_parts(q), s);
    assert!((ClockSnapshot::from_parts(q).drift_ppm() - s.drift_ppm() - 1.0).abs() < 1.0e-3);
    // After a reset the map has no base again, and the epoch is in the parts.
    let mut map = r.map;
    map.reset();
    let p = map.snapshot().parts();
    assert_eq!((p.epoch, p.base_ns), (1, None));
    assert_eq!(ClockSnapshot::from_parts(p), map.snapshot());
}

#[test]
fn snapshot_reads_frames_at_host_times() {
    // Before the first stamp of an epoch there is no host time.
    let mut map = ClockMap::new(48_000.0, 48_000.0, ClockConfig::default());
    let s = map.snapshot();
    assert_eq!(s.out_frame_at(ns(1.0)), None);
    assert_eq!(s.in_frame_at(ns(1.0)), None);
    assert_eq!(s.host_ns_of_out(0.0), None);
    assert_eq!((s.last_in_frame(), s.last_out_frame()), (0.0, 0.0));
    // One stamp is enough for a base.
    assert!(map.push_in(exact(0, 0.0)).is_none());
    assert!(map.snapshot().out_frame_at(ns(1.0)).is_some());

    // Exact stamps: frame f of both streams is at host time HOST0 + f/48000.
    let s = locked_map().snapshot();
    assert_eq!(
        (s.last_in_frame(), s.last_out_frame()),
        (480.0 * 1199.0, 480.0 * 1199.0)
    );
    for t in [0.0, 0.0101, 5.0, 11.99, 12.5, 60.0] {
        let f = s.out_frame_at(ns(t)).unwrap();
        assert!((f - 48_000.0 * t).abs() < 1.0e-3, "out at {t}: {f}");
        let g = s.in_frame_at(ns(t)).unwrap();
        assert!((g - 48_000.0 * t).abs() < 1.0e-3, "in at {t}: {g}");
        let back = s.host_ns_of_out(f).unwrap();
        assert!((back - ns(t) as f64).abs() <= 1.0, "{back} at {t}");
    }
    // A host time before the base is a frame before the first one.
    let early = s.out_frame_at(ns(-0.5)).unwrap();
    assert!((early + 24_000.0).abs() < 1.0e-3, "{early}");

    // Real stamps: the two lines read at one instant agree with `in_of_out`.
    let r = run(&Scenario::new(
        RATES[1],
        (120.0, -80.0),
        50.0e-6,
        10.0,
        1100,
    ));
    for t in [2.0, 9.5, 10.2] {
        let (f, g) = (
            r.last.out_frame_at(ns(t)).unwrap(),
            r.last.in_frame_at(ns(t)).unwrap(),
        );
        assert!((r.last.in_of_out(f) - g).abs() < 1.0e-6, "at {t}");
        assert!((r.last.host_ns_of_out(f).unwrap() - ns(t) as f64).abs() <= 1.0);
    }
}
