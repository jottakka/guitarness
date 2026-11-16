//! Unit A1 of the Pulso plan: the host clock, the playhead and the xrun boundary, on the
//! fake backend (virtual time, no device). Run with `--nocapture` to see the measured
//! numbers.
//!
//! The checks of the plan (section 5, A1):
//!
//! 1. seqlock under a concurrent writer: `src/playhead.rs`, unit test
//!    `seqlock_never_returns_a_torn_mark` (the writer is crate-private), and
//!    `the_playhead_is_consistent_while_the_engine_runs` here, through the engine;
//! 2. `out_frame_at(now)` against the truth of the fake, 12 rate/drift pairs;
//! 3. `PlayClock`: monotonic, error gates in `Line` and `Stamp`, recovery from an output
//!    underrun (also gate F3 of the plan: within 1 ms for 20 s at +120 ppm);
//! 4. negative control: a frame clock with a 0.1 % rate error must not pass the gate;
//! 5. callback stamps: `out_frame_at(now)` runs ahead by `out_lead_s`;
//! 6. the xrun boundary with a late error callback, the control without a boundary, and
//!    60 xruns in a row;
//! 7. `EngineIo`, `Playhead` and `RawOutput` are `Send`;
//! 8. T7 with the mark published: `tests/no_alloc.rs`.

use guitarness_audio::capture::RawOutput;
use guitarness_audio::clock::{ClockQuality, ClockSnapshot, ClockStep, StampSource, StepCause};
use guitarness_audio::fake::{BlockModel, FakeTruth};
use guitarness_audio::playhead::{OutMark, PlayClock, PlayMode, PlayPos, Playhead};
use guitarness_audio::{
    AudioEngine, BackendEvent, EngineConfig, EngineEvent, EngineIo, FakeBackend, FakeConfig,
    FakeHandle, Pump, Side, SoundBank,
};

type Eng = AudioEngine<FakeBackend>;

struct Rig {
    h: FakeHandle,
    eng: Eng,
    io: EngineIo,
    ev: Vec<EngineEvent>,
    /// The last snapshot the engine sent: what a client of the engine has.
    clock: Option<ClockSnapshot>,
}

fn rig(fc: FakeConfig) -> Rig {
    let fs_out = f64::from(fc.fs_out);
    let backend = FakeBackend::new(fc);
    let h = backend.handle();
    let (eng, io) = AudioEngine::start(
        backend,
        EngineConfig::default(),
        SoundBank::new(fs_out),
        Pump::Manual,
    )
    .unwrap();
    Rig {
        h,
        eng,
        io,
        ev: Vec::new(),
        clock: None,
    }
}

impl Rig {
    /// One slice of virtual time, then the analysis step, then the events.
    fn tick(&mut self, slice_s: f64) {
        self.h.pump(slice_s);
        self.eng.pump();
        while let Ok(e) = self.io.events.pop() {
            if let EngineEvent::Clock(s) = e {
                self.clock = Some(s);
            }
            self.ev.push(e);
        }
    }

    fn run(&mut self, seconds: f64, slice_s: f64) {
        for _ in 0..(seconds / slice_s).round() as usize {
            self.tick(slice_s);
        }
    }

    fn snap(&self) -> ClockSnapshot {
        self.eng.clock_map().expect("manual pump").snapshot()
    }

    fn truth(&self) -> FakeTruth {
        self.h.truth().expect("streams are open")
    }

    /// Ticks until the engine sends a snapshot that `ok` accepts; panics after `limit_s`.
    fn run_until(&mut self, limit_s: f64, what: &str, ok: impl Fn(&ClockSnapshot) -> bool) {
        let end = self.h.now_s() + limit_s;
        while !self.clock.as_ref().is_some_and(&ok) {
            assert!(self.h.now_s() < end, "{what}: not in {limit_s} s");
            self.tick(0.005);
        }
    }

    /// The host clock, through the engine. It is the clock of the fake.
    fn now_ns(&self) -> u64 {
        let now = self.eng.host_now_ns().expect("streams are open");
        assert_eq!(now, self.h.now_ns());
        now
    }

    /// The output frame leaving the loudspeaker now. Before frame 0 plays (the first
    /// block is requested ahead of its sound) nothing does: 0.
    fn true_out_now(&self) -> f64 {
        self.truth().out_frame_at(self.h.now_s()).max(0.0)
    }

    /// Error of `in_of_out` at the output frame being requested now, in ms.
    fn map_error_ms(&self) -> f64 {
        let truth = self.truth();
        let f = self.h.next_frames().unwrap().1 as f64;
        (self.snap().in_of_out(f) - truth.in_of_out(f)) / truth.fs_in * 1.0e3
    }
}

// ---------------------------------------------------------------- check 6: xrun boundary

/// What the engine made of one xrun.
#[derive(Debug)]
struct Outcome {
    /// Frame counter of the stream at the xrun: the first frame of the next block.
    boundary: u64,
    block: u64,
    step: ClockStep,
    /// Next frame of that stream when the `Step` came out of the engine.
    next_at_step: u64,
    /// Map error right after the first block after the xrun went through the analysis
    /// and the notice was delivered, before the second block: ms.
    err_between_ms: f64,
    /// Map error when the `Step` came out, ms.
    err_at_step_ms: f64,
    /// How much the estimated drift moved from before the xrun to the `Step`, ppm.
    drift_moved_ppm: f64,
    /// The `Unjudged` window that followed.
    window: (f64, f64),
    notices: u64,
}

/// Locks the clock, loses `lost` frames on `side` and follows the engine in 1 ms slices
/// until the step is out.
fn xrun_case(side: Side, lost: u32, delay_s: f64, with_boundary: bool) -> Outcome {
    const BLOCK: u64 = 480;
    let fc = FakeConfig {
        stamp_jitter_in_s: 5.0e-6,
        stamp_jitter_out_s: 5.0e-6,
        error_delay_s: delay_s,
        error_boundary: with_boundary,
        seed: 77,
        ..FakeConfig::default()
    };
    let mut r = rig(fc);
    r.run(12.0, 0.005);
    assert_eq!(r.snap().quality(), ClockQuality::Locked);
    assert!(r.map_error_ms().abs() <= 0.05, "{}", r.map_error_ms());
    r.ev.clear();

    let next = |r: &Rig| {
        let (i, o) = r.h.next_frames().unwrap();
        if side == Side::In { i } else { o }
    };
    let drift_before = r.snap().drift_ppm();
    let boundary = next(&r);
    assert!(r.h.inject_xrun(side, lost));

    let mut found: Option<(ClockStep, u64, f64, f64)> = None;
    let mut err_between_ms = f64::NAN;
    let mut since_first = None;
    for _ in 0..400 {
        r.tick(0.001);
        // Slices since the first block after the xrun ran.
        if next(&r) > boundary {
            since_first = Some(since_first.map_or(0, |n: u32| n + 1));
        }
        // 7 ms after it: the 5 ms notice is in, the next block (10 ms) is not.
        if since_first == Some(7) {
            assert_eq!(next(&r), boundary + BLOCK, "the second block is early");
            err_between_ms = r.map_error_ms();
        }
        if found.is_none()
            && let Some(step) = r.ev.iter().find_map(|e| match e {
                EngineEvent::Step(s) if s.side == side => Some(*s),
                _ => None,
            })
        {
            let moved = r.snap().drift_ppm() - drift_before;
            found = Some((step, next(&r), r.map_error_ms(), moved));
        }
        if found.is_some() && since_first >= Some(30) {
            break;
        }
    }
    let (step, next_at_step, err_at_step_ms, drift_moved_ppm) = found.expect("no step");
    let steps =
        r.ev.iter()
            .filter(|e| matches!(e, EngineEvent::Step(_)))
            .count();
    assert_eq!(steps, 1, "{:?}", r.ev);
    let window =
        r.ev.iter()
            .find_map(|e| match e {
                EngineEvent::Unjudged { from_in, to_in, .. } => Some((*from_in, *to_in)),
                _ => None,
            })
            .expect("no window");
    let xrun = match side {
        Side::In => BackendEvent::XrunIn,
        Side::Out => BackendEvent::XrunOut,
    };
    assert!(r.ev.contains(&EngineEvent::Backend(xrun)));
    let notices = r.eng.clock_map().unwrap().stats(side).xrun_notices;
    Outcome {
        boundary,
        block: BLOCK,
        step,
        next_at_step,
        err_between_ms,
        err_at_step_ms,
        drift_moved_ppm,
        window,
        notices,
    }
}

/// Check 6. The error callback is delivered 5 ms after the xrun, as the dispatcher of the
/// cpal backend does: the first block after the xrun has been through the analysis by
/// then. With the boundary the step is still reported on that block, and the block never
/// enters the clock filter.
///
/// This test fails on the engine as it was before unit A1 (the boundary ignored, the
/// notice applied to the first stamp after the delivery): see the control below, which
/// keeps that behaviour alive through `error_boundary = false`.
#[test]
fn a_late_xrun_notice_is_placed_on_its_boundary() {
    let fs = 48_000.0;
    // 1.5 ms lost: below the 3 ms jump threshold, so only the notice can find it.
    let o = xrun_case(Side::In, 72, 0.005, true);
    println!(
        "late notice, 72 frames, boundary: step at {} (boundary {}), {:.4} ms, {:?}; next block {} \
         when it came out; map error {:+.4} ms between the blocks, {:+.4} ms at the step; drift \
         moved {:+.3} ppm",
        o.step.frame,
        o.boundary,
        o.step.step_s * 1.0e3,
        o.step.cause,
        o.next_at_step,
        o.err_between_ms,
        o.err_at_step_ms,
        o.drift_moved_ppm
    );
    assert_eq!(
        o.step.frame, o.boundary,
        "the step is not on the first stamp after the xrun"
    );
    assert_eq!(o.step.cause, StepCause::Notice);
    assert!((o.step.step_s * 1.0e3 - 1.5).abs() <= 0.05, "{:?}", o.step);
    // It came out before the second block after the xrun ran.
    assert_eq!(o.next_at_step, o.boundary + o.block);
    // The map is right at once: the first stamp did not pull the line or the period.
    assert!(
        o.err_at_step_ms.abs() <= 0.05,
        "at the step: {}",
        o.err_at_step_ms
    );
    assert!(
        o.err_between_ms.abs() <= 0.05,
        "between the blocks: {}",
        o.err_between_ms
    );
    assert!(
        o.drift_moved_ppm.abs() <= 0.5,
        "the stamp entered the filter: drift moved {} ppm",
        o.drift_moved_ppm
    );
    // The window sits on the boundary: 100 ms before to 250 ms after.
    let c = o.boundary as f64;
    assert!(
        (o.window.0 - (c - 0.1 * fs)).abs() < 1.0e-6,
        "{:?}",
        o.window
    );
    assert!(
        (o.window.1 - (c + 0.25 * fs)).abs() < 1.0e-6,
        "{:?}",
        o.window
    );
    assert_eq!(o.notices, 1);

    // The same xrun with the notice in time gives the same step.
    let t = xrun_case(Side::In, 72, 0.0, true);
    assert_eq!(
        (t.step.frame, t.step.cause),
        (t.boundary, StepCause::Notice)
    );
    assert!((t.step.step_s - o.step.step_s).abs() < 20.0e-6);

    // A loss above the jump threshold (20 ms), late notice: on the boundary too, and out
    // before the second block.
    let big = xrun_case(Side::In, 960, 0.005, true);
    assert_eq!(
        (big.step.frame, big.step.cause),
        (big.boundary, StepCause::Notice)
    );
    assert_eq!(big.next_at_step, big.boundary + big.block);
    assert!(
        (big.step.step_s * 1.0e3 - 20.0).abs() <= 0.05,
        "{:?}",
        big.step
    );
    assert!(big.err_between_ms.abs() <= 0.05, "{}", big.err_between_ms);

    // The output side, reported by the backend (CoreAudio style), late.
    let out = xrun_case(Side::Out, 72, 0.005, true);
    assert_eq!(
        (out.step.frame, out.step.cause),
        (out.boundary, StepCause::Notice)
    );
    assert_eq!(out.next_at_step, out.boundary + out.block);
    assert!(out.err_between_ms.abs() <= 0.05, "{}", out.err_between_ms);

    // A notice that is several blocks late (25 ms): still on the boundary.
    let later = xrun_case(Side::In, 72, 0.025, true);
    assert_eq!(
        (later.step.frame, later.step.cause),
        (later.boundary, StepCause::Notice)
    );
    assert!(
        later.err_at_step_ms.abs() <= 0.05,
        "{}",
        later.err_at_step_ms
    );
    assert!(
        later.drift_moved_ppm.abs() <= 0.5,
        "{}",
        later.drift_moved_ppm
    );
}

/// The control of check 6: without the boundary (`error_boundary = false`) the engine can
/// only do what it did before unit A1, and the late notice lands one block late. If this
/// stops failing the way it does, the test above no longer proves anything.
#[test]
fn without_the_boundary_a_late_notice_is_one_block_late() {
    let o = xrun_case(Side::In, 72, 0.005, false);
    println!(
        "late notice, 72 frames, no boundary: step at {} (boundary {}), {:.4} ms; next block {} \
         when it came out; map error {:+.4} ms between the blocks; drift moved {:+.3} ppm",
        o.step.frame,
        o.boundary,
        o.step.step_s * 1.0e3,
        o.next_at_step,
        o.err_between_ms,
        o.drift_moved_ppm
    );
    assert_eq!(o.step.frame, o.boundary + o.block, "one block late");
    assert_eq!(o.next_at_step, o.boundary + 2 * o.block);
    // Until then the map is off by the whole loss, and the stamp went through the filter.
    assert!(o.err_between_ms.abs() >= 1.0, "{}", o.err_between_ms);
    assert!(o.drift_moved_ppm.abs() >= 2.0, "{}", o.drift_moved_ppm);

    // In time, the missing boundary costs nothing: the notice precedes its block.
    let t = xrun_case(Side::In, 72, 0.0, false);
    assert_eq!(
        (t.step.frame, t.step.cause),
        (t.boundary, StepCause::Notice)
    );

    // Above the jump threshold the stamp is held back by the jump rule, so the step is
    // on the right frame even so; it only comes out a block later.
    let big = xrun_case(Side::In, 960, 0.005, false);
    assert_eq!(big.step.frame, big.boundary);
    assert_eq!(big.next_at_step, big.boundary + 2 * big.block);
}

/// Check 6 at length: 60 xruns on both sides, blocks of 5 to 15 ms, two rates, the error
/// callback 3 ms late (the cpal dispatcher wakes every 2 ms), the analysis every 5 ms. A
/// notice then reaches the analysis together with its block or one step after it,
/// depending on where the block falls; every one gives exactly one step, on its
/// boundary, of the size of the loss. (The xruns of a stream are 5.2 s apart: three
/// jumps without a clean 2 s window in between are bad stamps for the map, by design.)
#[test]
fn every_late_notice_lands_on_its_boundary() {
    let mut fc = FakeConfig::new(44_100, 48_000);
    fc.drift_in_ppm = 60.0;
    fc.drift_out_ppm = -90.0;
    fc.blocks_in = BlockModel::Uniform { min: 220, max: 661 };
    fc.blocks_out = BlockModel::Uniform { min: 240, max: 720 };
    fc.error_delay_s = 0.003;
    fc.seed = 31;
    let mut r = rig(fc);
    r.run_until(30.0, "lock", locked);
    r.ev.clear();

    let mut want = Vec::new();
    for k in 0..60u32 {
        let side = if k.is_multiple_of(2) {
            Side::In
        } else {
            Side::Out
        };
        // 0.7 ms to 40 ms: below and above the jump threshold of the map.
        let lost = 30 + (k * 197) % 1900;
        let (i, o) = r.h.next_frames().unwrap();
        want.push((side, if side == Side::In { i } else { o }, lost));
        assert!(r.h.inject_xrun(side, lost));
        r.run(2.6, 0.005);
    }
    let steps: Vec<ClockStep> =
        r.ev.iter()
            .filter_map(|e| match e {
                EngineEvent::Step(s) => Some(*s),
                _ => None,
            })
            .collect();
    assert_eq!(steps.len(), want.len(), "one step per xrun");
    let mut worst_ms = 0.0f64;
    for (s, (side, at, lost)) in steps.iter().zip(&want) {
        assert_eq!(
            (s.side, s.frame, s.cause),
            (*side, *at, StepCause::Notice),
            "{s:?}"
        );
        let fs = if *side == Side::In {
            44_100.0
        } else {
            48_000.0
        };
        let off_ms = (s.frames - f64::from(*lost)) / fs * 1.0e3;
        worst_ms = worst_ms.max(off_ms.abs());
        assert!(off_ms.abs() <= 0.15, "{s:?} for {lost} frames");
    }
    let windows =
        r.ev.iter()
            .filter(|e| matches!(e, EngineEvent::Unjudged { .. }))
            .count();
    assert_eq!(windows, steps.len());
    let map = r.eng.clock_map().unwrap();
    assert_eq!(map.stats(Side::In).xrun_notices, 30);
    assert_eq!(map.stats(Side::Out).xrun_notices, 30);
    assert_eq!(
        (map.stats(Side::In).steps, map.stats(Side::Out).steps),
        (30, 30)
    );
    r.run(10.0, 0.005);
    assert!(locked(&r.snap()));
    println!(
        "60 xruns, notices 3 ms late: every step on its boundary, sizes within {worst_ms:.4} ms; \
         map error {:+.4} ms at the end",
        r.map_error_ms()
    );
    assert!(r.map_error_ms().abs() <= 0.05, "{}", r.map_error_ms());
}

// ---------------------------------------------------------------- gates and readers

/// Gate of a playhead on the line of a locked clock, 99th percentile, ms.
const LINE_GATE_MS: f64 = 0.5;
/// Gate of a playhead on the last output stamp, 99th percentile, ms.
const STAMP_GATE_MS: f64 = 5.0;
/// One frame of a 240 Hz display, seconds. The readings do not line up with the blocks.
const FRAME_S: f64 = 1.0 / 240.0;

/// 99th percentile of the absolute values.
fn p99(v: &[f64]) -> f64 {
    assert!(!v.is_empty());
    let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    a.sort_by(f64::total_cmp);
    let k = ((0.99 * a.len() as f64).ceil() as usize).clamp(1, a.len());
    a[k - 1]
}

fn max_abs(v: &[f64]) -> f64 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

/// The gate every playhead goes through: 99 % of its readings are within `limit_ms` of
/// the frame at the loudspeaker. Returns the percentile.
fn gate(errs_ms: &[f64], limit_ms: f64) -> Result<f64, String> {
    let p = p99(errs_ms);
    if p <= limit_ms {
        Ok(p)
    } else {
        Err(format!(
            "p99 {p:.4} ms above {limit_ms} ms over {} readings",
            errs_ms.len()
        ))
    }
}

fn locked(s: &ClockSnapshot) -> bool {
    s.quality() == ClockQuality::Locked
}

/// A drawing thread: it owns a `PlayClock`, feeds it every snapshot the engine sends and
/// reads the playhead once per display frame.
struct Viewer {
    ph: Playhead,
    pc: PlayClock,
    fed: usize,
    last: Option<PlayPos>,
}

impl Viewer {
    fn new(r: &Rig) -> Self {
        Self {
            ph: r.eng.playhead(),
            pc: PlayClock::new(r.truth().fs_out),
            fed: 0,
            last: None,
        }
    }

    /// One reading. Inside an epoch the position never goes back; epochs only grow.
    fn look(&mut self, r: &Rig) -> PlayPos {
        for e in &r.ev[self.fed..] {
            if let EngineEvent::Clock(s) = e {
                self.pc.on_snapshot(*s);
            }
        }
        self.fed = r.ev.len();
        // With the streams closed the engine has no clock to read: the drawing thread
        // goes on with the one it had (here, the same virtual clock).
        let now = match r.eng.host_now_ns() {
            Some(now) => {
                assert_eq!(now, r.h.now_ns());
                now
            }
            None => r.h.now_ns(),
        };
        let pos = self.pc.frame_at(now, self.ph.last_out());
        if let Some(last) = self.last {
            assert!(pos.epoch >= last.epoch, "{last:?} then {pos:?}");
            if pos.epoch == last.epoch {
                assert!(
                    pos.out_frame >= last.out_frame,
                    "went back: {last:?} then {pos:?}"
                );
            }
        }
        self.last = Some(pos);
        pos
    }
}

/// Errors of the readings of one stretch, ms, by the source the `PlayClock` used.
#[derive(Default)]
struct Readings {
    line: Vec<f64>,
    stamp: Vec<f64>,
    hold: usize,
    /// Every reading: true time, source, error in ms.
    all: Vec<(f64, PlayMode, f64)>,
}

/// Follows the engine for `seconds`, one reading per display frame.
fn watch(r: &mut Rig, v: &mut Viewer, seconds: f64) -> Readings {
    let mut out = Readings::default();
    for _ in 0..(seconds / FRAME_S).round() as usize {
        r.tick(FRAME_S);
        let pos = v.look(r);
        let err = (pos.out_frame - r.true_out_now()) / r.truth().fs_out * 1.0e3;
        match pos.mode {
            PlayMode::Line => out.line.push(err),
            PlayMode::Stamp => out.stamp.push(err),
            PlayMode::Hold => out.hold += 1,
        }
        out.all.push((r.h.now_s(), pos.mode, err));
    }
    out
}

// ---------------------------------------------------------------- check 2: out_frame_at

/// Check 2. `out_frame_at(now)` of the last snapshot the engine sent (up to 100 ms old)
/// against the frame at the loudspeaker, in 12 pairs of rates and drifts, with 50 µs of
/// stamp jitter and blocks of 5 to 15 ms: 99 % within 0.2 ms once the clock is locked.
/// The input line and the inverse are held to the same numbers.
#[test]
fn out_frame_at_now_is_the_frame_at_the_loudspeaker() {
    let rates = [(48_000u32, 48_000u32), (44_100, 48_000), (48_000, 44_100)];
    let drifts = [(0.0, 0.0), (200.0, -200.0), (-120.0, 80.0), (50.0, 200.0)];
    let mut case = 0u32;
    let (mut worst_out, mut worst_in) = (0.0f64, 0.0f64);
    for (fs_in, fs_out) in rates {
        for (d_in, d_out) in drifts {
            case += 1;
            let mut fc = FakeConfig::new(fs_in, fs_out);
            fc.drift_in_ppm = d_in;
            fc.drift_out_ppm = d_out;
            fc.stamp_jitter_in_s = 50.0e-6;
            fc.stamp_jitter_out_s = 50.0e-6;
            fc.blocks_in = BlockModel::Uniform {
                min: fs_in / 200,
                max: 3 * fs_in / 200,
            };
            fc.blocks_out = BlockModel::Uniform {
                min: fs_out / 200,
                max: 3 * fs_out / 200,
            };
            fc.seed = 100 + case;
            let mut r = rig(fc);
            r.run_until(30.0, "lock", locked);
            let locked_at = r.h.now_s();

            let (fs_i, fs_o) = (f64::from(fs_in), f64::from(fs_out));
            let (mut out_ms, mut in_ms) = (Vec::new(), Vec::new());
            for _ in 0..(10.0 / FRAME_S) as usize {
                r.tick(FRAME_S);
                let s = r.clock.expect("a snapshot");
                assert!(locked(&s), "case {case}: the lock was lost");
                assert_eq!(s.source(Side::Out), StampSource::Device);
                let (now, t, truth) = (r.now_ns(), r.h.now_s(), r.truth());
                let f = s.out_frame_at(now).expect("the map has a base");
                out_ms.push((f - truth.out_frame_at(t)) / fs_o * 1.0e3);
                let g = s.in_frame_at(now).expect("the map has a base");
                in_ms.push((g - truth.in_frame_at(t)) / fs_i * 1.0e3);
                // The inverse gives the instant back, to the nanosecond.
                let back = s.host_ns_of_out(f).expect("the map has a base");
                assert!((back - now as f64).abs() <= 1.0, "{back} for {now}");
                // The progress marks are behind "now" by less than the snapshot period
                // plus the lead of the output (the stamps run ahead of the sound).
                assert!(s.last_out_frame() <= f + 0.03 * fs_o);
                assert!(s.last_out_frame() >= f - 0.2 * fs_o);
                assert!(s.last_in_frame() <= g && s.last_in_frame() >= g - 0.2 * fs_i);
            }
            let p_out = gate(&out_ms, 0.2).unwrap_or_else(|e| panic!("case {case}, out: {e}"));
            let p_in = gate(&in_ms, 0.2).unwrap_or_else(|e| panic!("case {case}, in: {e}"));
            println!(
                "case {case:2}: {fs_in}/{fs_out} Hz, {d_in:+.0}/{d_out:+.0} ppm: locked at {locked_at:.2} s; \
                 out_frame_at p99 {p_out:.4} ms (max {:.4}), in_frame_at p99 {p_in:.4} ms (max {:.4})",
                max_abs(&out_ms),
                max_abs(&in_ms)
            );
            worst_out = worst_out.max(p_out);
            worst_in = worst_in.max(p_in);
        }
    }
    assert_eq!(case, 12);
    println!("worst p99: out {worst_out:.4} ms, in {worst_in:.4} ms (gate 0.2 ms)");
}

// ---------------------------------------------------------------- check 3: PlayClock

/// Check 3 (and gate F3 of the plan: within 1 ms of the truth for 20 s with the output
/// device 120 ppm fast, monotonic). A drawing thread follows the engine from the first
/// block: the position never goes back, it is within 5 ms (p99) on the stamps before the
/// lock and within 0.5 ms (p99) on the line after it. A 20 ms output underrun that
/// nobody reports puts it at most 20 ms ahead and it is back within 1 ms in less than a
/// second. A lost device freezes it; the reopened streams make it jump to their frame
/// counter.
#[test]
fn play_clock_follows_the_loudspeaker_and_never_goes_back() {
    let fc = FakeConfig {
        drift_in_ppm: -80.0,
        drift_out_ppm: 120.0,
        stamp_jitter_in_s: 50.0e-6,
        stamp_jitter_out_s: 50.0e-6,
        notify_out_xrun: false, // as on WASAPI: the underrun is not reported
        seed: 5,
        ..FakeConfig::default()
    };
    let mut r = rig(fc);
    let mut v = Viewer::new(&r);

    // Nothing has played yet.
    let p = v.look(&r);
    assert_eq!((p.mode, p.out_frame, p.epoch), (PlayMode::Hold, 0.0, 0));

    // 20 s from the first block: on the stamps until the clock locks, then on the line.
    let a = watch(&mut r, &mut v, 20.0);
    assert!(r.clock.as_ref().is_some_and(locked), "no lock in 20 s");
    assert_eq!(a.hold, 0, "a stamp is enough to show a position");
    assert!(
        a.stamp.len() >= 100,
        "{} readings on the stamps",
        a.stamp.len()
    );
    assert!(
        a.line.len() >= 2000,
        "{} readings on the line",
        a.line.len()
    );
    let p_stamp = gate(&a.stamp, STAMP_GATE_MS).expect("on the stamps");
    let p_line = gate(&a.line, LINE_GATE_MS).expect("on the line");
    let worst = a.all.iter().fold(0.0f64, |m, x| m.max(x.2.abs()));
    println!(
        "PlayClock, 20 s: stamps p99 {p_stamp:.4} ms (max {:.4}, {} readings), line p99 \
         {p_line:.4} ms (max {:.4}, {} readings); worst reading {worst:.4} ms",
        max_abs(&a.stamp),
        a.stamp.len(),
        max_abs(&a.line),
        a.line.len()
    );
    assert!(worst <= 1.0, "more than 1 ms from the loudspeaker: {worst}");
    assert_eq!(v.last.unwrap().mode, PlayMode::Line);

    // The output device plays 20 ms of silence. Nobody says so: the jump rule of the map
    // finds it three stamps later. Until then the line runs 20 ms ahead; the position
    // then waits for the sound instead of going back.
    assert!(r.h.inject_xrun(Side::Out, 960));
    let t_xrun = r.h.now_s();
    let b = watch(&mut r, &mut v, 3.0);
    let early: Vec<f64> = b
        .all
        .iter()
        .filter(|x| x.0 < t_xrun + 1.0)
        .map(|x| x.2)
        .collect();
    let late: Vec<f64> = b
        .all
        .iter()
        .filter(|x| x.0 >= t_xrun + 1.0)
        .map(|x| x.2)
        .collect();
    let back_at = b
        .all
        .iter()
        .rev()
        .find(|x| x.2.abs() > 1.0)
        .map_or(t_xrun, |x| x.0);
    println!(
        "20 ms output underrun: up to {:.3} ms off, within 1 ms again {:.3} s later; then max \
         {:.4} ms ({} on the line, {} on the stamps)",
        max_abs(&early),
        back_at - t_xrun,
        max_abs(&late),
        b.line.len(),
        b.stamp.len()
    );
    assert!(
        (15.0..=21.0).contains(&max_abs(&early)),
        "the underrun did not show as expected: {}",
        max_abs(&early)
    );
    assert!(
        early.iter().all(|e| *e >= -1.0),
        "behind the sound: {early:?}"
    );
    assert!(max_abs(&late) <= 1.0, "not back in 1 s: {}", max_abs(&late));
    assert_eq!(b.hold, 0);
    let steps =
        r.ev.iter()
            .filter(|e| matches!(e, EngineEvent::Step(s) if s.side == Side::Out))
            .count();
    assert_eq!(steps, 1, "the map stepped once");
    r.run_until(20.0, "lock after the underrun", locked);

    // The device goes away: the streams close, the engine has no clock, the position
    // stops at most at the last rendered frame and holds.
    v.look(&r);
    r.h.inject_event(BackendEvent::DeviceLost);
    for _ in 0..24 {
        r.tick(FRAME_S);
        v.look(&r);
    }
    assert_eq!(r.eng.host_now_ns(), None, "the streams are closed");
    let mark = r.eng.playhead().last_out().expect("the last stamp stays");
    let rendered = (mark.first + u64::from(mark.frames)) as f64;
    let mut pos = v.look(&r);
    for _ in 0..240 {
        r.tick(FRAME_S);
        pos = v.look(&r);
        assert!(pos.out_frame <= rendered, "{pos:?} past {rendered}");
    }
    assert_eq!((pos.mode, pos.epoch), (PlayMode::Hold, 0));
    let held = pos.out_frame;
    assert!(held > rendered - 0.05 * 48_000.0, "{held} of {rendered}");
    r.run(0.5, FRAME_S);
    assert_eq!(v.look(&r), pos, "a held position does not move");

    // New streams: the stamp of the old ones is hidden, the position stays until the
    // first block of the new ones, then jumps to their frame counter.
    let (_mix, _tap) = r.eng.reopen().expect("reopen");
    let ph = r.eng.playhead();
    assert_eq!(ph.epoch(), 1);
    assert_eq!(ph.last_out(), None);
    assert_eq!(v.look(&r), pos);
    assert!(r.eng.host_now_ns().is_some());
    let mut d = watch(&mut r, &mut v, 5.0);
    assert_eq!(ph.last_out().map(|m| m.epoch), Some(1));
    while !r
        .clock
        .as_ref()
        .is_some_and(|s| locked(s) && s.epoch() == 1)
    {
        assert!(d.all.len() < 30 * 240, "no lock after the reopen");
        let more = watch(&mut r, &mut v, 0.5);
        d.line.extend(more.line);
        d.stamp.extend(more.stamp);
        d.hold += more.hold;
        d.all.extend(more.all);
    }
    let more = watch(&mut r, &mut v, 5.0);
    d.line.extend(more.line);
    d.stamp.extend(more.stamp);
    d.hold += more.hold;
    let last = v.last.unwrap();
    assert_eq!((last.mode, last.epoch), (PlayMode::Line, 1));
    assert!(last.out_frame < held, "the counter restarted: {last:?}");
    assert_eq!(d.hold, 0);
    assert!(d.stamp.len() >= 100 && d.line.len() >= 1000);
    let p_stamp = gate(&d.stamp, STAMP_GATE_MS).expect("on the stamps, epoch 1");
    let p_line = gate(&d.line, LINE_GATE_MS).expect("on the line, epoch 1");
    println!("after the reopen: stamps p99 {p_stamp:.4} ms, line p99 {p_line:.4} ms");
}

// ---------------------------------------------------------------- check 4: negative control

/// Check 4, the negative control of the gates. A clock that counts frames from the host
/// time at a rate 0.1 % off (a game clock that trusts the nominal rate of a bad device)
/// is 10 ms away after 10 s. The same gate that the `PlayClock` passes, on the same run,
/// must not let it through.
#[test]
fn a_frame_clock_with_a_rate_error_does_not_pass_the_gate() {
    let fc = FakeConfig {
        stamp_jitter_in_s: 50.0e-6,
        stamp_jitter_out_s: 50.0e-6,
        seed: 9,
        ..FakeConfig::default()
    };
    let mut r = rig(fc);
    let mut v = Viewer::new(&r);
    while !r.clock.as_ref().is_some_and(locked) {
        assert!(r.h.now_s() < 30.0, "no lock");
        r.tick(FRAME_S);
        v.look(&r);
    }

    // Exact at the start, then the elapsed host time at the wrong rate.
    let truth = r.truth();
    let (ns0, f0) = (r.now_ns(), r.true_out_now());
    let rate = truth.rate_out() * 1.001;
    let (mut naive, mut play) = (Vec::new(), Vec::new());
    for _ in 0..(10.0 / FRAME_S) as usize {
        r.tick(FRAME_S);
        let real = truth.out_frame_at(r.h.now_s());
        let counted = f0 + (r.now_ns() - ns0) as f64 * 1.0e-9 * rate;
        naive.push((counted - real) / truth.fs_out * 1.0e3);
        play.push((v.look(&r).out_frame - real) / truth.fs_out * 1.0e3);
    }
    println!(
        "frame clock 0.1 % fast: {:.3} ms off after 10 s, p99 {:.3} ms; PlayClock p99 {:.4} ms",
        naive.last().unwrap(),
        p99(&naive),
        p99(&play)
    );
    assert!(max_abs(&naive) > 5.0, "{}", max_abs(&naive));
    let verdict = gate(&naive, STAMP_GATE_MS);
    assert!(
        verdict.is_err(),
        "the loosest gate let it pass: {verdict:?}"
    );
    assert!(gate(&naive, LINE_GATE_MS).is_err());
    gate(&play, LINE_GATE_MS).expect("the PlayClock passes the gate that rejects it");
}

// ---------------------------------------------------------------- check 5: callback stamps

/// Check 5. When the device stamps of the output are useless the map falls back to the
/// callback stamps, and then `out_frame_at(now)` is the frame being *requested* now: it
/// runs ahead of the loudspeaker by the lead of the output (`out_lead_s`, minus the
/// scheduling delay of the callbacks, 0.16 ms on average here). Nothing in the engine
/// knows that lead: this is the documented meaning, held by a test.
#[test]
fn with_callback_stamps_the_frame_is_the_one_being_requested() {
    for lead_s in [0.020, 0.035] {
        let fc = FakeConfig {
            stamp_skew_out_ppm: 4000.0,
            out_lead_s: lead_s,
            seed: 21,
            ..FakeConfig::default()
        };
        let mut r = rig(fc);
        let mut v = Viewer::new(&r);
        r.run_until(60.0, "locked on callback stamps", |s| {
            locked(s) && s.source(Side::Out) == StampSource::Callback
        });
        let at = r.h.now_s();
        let fs = r.truth().fs_out;
        let (mut ahead, mut shown) = (Vec::new(), Vec::new());
        for _ in 0..(5.0 / FRAME_S) as usize {
            r.tick(FRAME_S);
            let s = r.clock.expect("a snapshot");
            assert_eq!(s.source(Side::Out), StampSource::Callback);
            assert_eq!(s.source(Side::In), StampSource::Device);
            let real = r.true_out_now();
            let f = s.out_frame_at(r.now_ns()).expect("the map has a base");
            ahead.push((f - real) / fs * 1.0e3 - lead_s * 1.0e3);
            let pos = v.look(&r);
            assert_eq!(pos.mode, PlayMode::Line);
            shown.push((pos.out_frame - real) / fs * 1.0e3 - lead_s * 1.0e3);
        }
        let mean = ahead.iter().sum::<f64>() / ahead.len() as f64;
        println!(
            "callback stamps, lead {:.0} ms (on them at {at:.2} s): out_frame_at(now) is ahead of \
             the sound by the lead {mean:+.4} ms on average, p99 of the difference {:.4} ms; \
             PlayClock p99 {:.4} ms",
            lead_s * 1.0e3,
            p99(&ahead),
            p99(&shown)
        );
        assert!((-0.4..=0.0).contains(&mean), "{mean}");
        gate(&ahead, 0.5).expect("ahead by the lead");
        // The PlayClock shows the same frame, never past the rendered ones.
        gate(&shown, 0.6).expect("the PlayClock too");
    }
}

// ---------------------------------------------------------------- check 1, through the engine

/// Check 1 through the engine (the tight writer loop is the unit test in
/// `src/playhead.rs`). A thread reads the playhead while the output callback of the
/// engine publishes 12 000 stamps. Every field of a stamp of this fake is a function of
/// the block number, so a copy mixed from two stamps cannot pass.
#[test]
fn the_playhead_is_consistent_while_the_engine_runs() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const BLOCK: u64 = 480;
    let fc = FakeConfig {
        stamp_jitter_out_s: 0.0, // the device time is then exact
        seed: 3,
        ..FakeConfig::default()
    };
    let lead_ns = (fc.out_lead_s * 1.0e9) as i64;
    let origin_ns = fc.host_origin_ns;
    let mut r = rig(fc);
    let t0 = r.truth().out_time_s(0.0);
    let ph = r.eng.playhead();
    assert_eq!(ph.last_out(), None);

    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let (mut reads, mut changes) = (0u64, 0u64);
            let mut last: Option<OutMark> = None;
            while !stop.load(Ordering::Relaxed) {
                reads += 1;
                let Some(m) = ph.last_out() else { continue };
                assert_eq!((m.frames, m.epoch), (BLOCK as u32, 0), "{m:?}");
                assert_eq!(m.first % BLOCK, 0, "{m:?}");
                let k = m.first / BLOCK;
                let device = origin_ns as f64 + (t0 + k as f64 * 0.01) * 1.0e9;
                assert!((m.device_ns as f64 - device).abs() <= 2.0, "{m:?}");
                // Requested 10 to 20 ms before it plays (the first blocks are late).
                let lead = m.device_ns as i64 - m.callback_ns as i64;
                if k >= 3 {
                    assert!((lead_ns / 2 - 1..=lead_ns + 1).contains(&lead), "{m:?}");
                }
                if let Some(l) = last {
                    assert!(m.first >= l.first, "{l:?} then {m:?}");
                    changes += u64::from(m != l);
                }
                last = Some(m);
            }
            (reads, changes, last)
        })
    };
    r.run(120.0, 0.005);
    stop.store(true, Ordering::Relaxed);
    let (reads, changes, last) = reader.join().expect("the reader saw a torn stamp");
    let published = r.h.stats().callbacks_out;
    println!(
        "playhead under the engine: {published} stamps published, {reads} reads, {changes} changes seen"
    );
    assert!(published >= 12_000, "{published}");
    assert!(
        changes >= 100,
        "the reader barely saw the writer: {changes}"
    );
    let end = r.h.next_frames().unwrap().1;
    assert_eq!(
        r.eng.playhead().last_out().map(|m| m.first),
        Some(end - BLOCK)
    );
    assert!(last.is_some());
}

// ---------------------------------------------------------------- check 7: Send

/// Check 7. What the game moves to other threads can be moved: the engine handles to the
/// audio bridge, the playhead to the drawing thread, the raw ring to the tape writer.
#[test]
fn the_handles_cross_threads() {
    fn send<T: Send>() {}
    fn sync<T: Sync>() {}
    send::<EngineIo>();
    send::<Playhead>();
    sync::<Playhead>();
    send::<RawOutput>();
    send::<PlayClock>();
    send::<OutMark>();

    // And for real: the raw ring read on another thread, the playhead on a third.
    let mut r = rig(FakeConfig::default());
    r.io.tap.set_raw(true);
    let mut raw = r.io.tap.take_raw().expect("the raw ring, once");
    let ph = r.eng.playhead();
    r.run(1.0, 0.005);
    let frames = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut frames = 0u64;
        while let Some(mark) = raw.read_block(&mut buf) {
            assert_eq!(buf.len(), mark.frames as usize);
            frames += u64::from(mark.frames);
        }
        frames
    })
    .join()
    .unwrap();
    assert!(frames >= 40_000, "{frames} raw frames in 1 s");
    let mark = std::thread::spawn(move || ph.last_out()).join().unwrap();
    assert!(mark.is_some());
    let io = std::thread::spawn(move || r.io).join().unwrap();
    drop(io);
}
