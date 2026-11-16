//! U9: the live calibration on the fake backend, with the manual pump (deterministic).
//! Run with `--nocapture` to see the measured numbers (T5 and T6 of the M2 plan).
//!
//! Unless a test says otherwise, a run starts the calibration at the same instant as the
//! engine: the clock map is still acquiring, which is the hardest case and what
//! `tonica calibrate` does.
//!
//! A2 of the Pulso plan (the calibration keeps the latency of the system apart from the
//! player's tendency) is at the end of the file. The tests from before it that need the
//! strum step on top of a measured loop ask for it with [`config_always`]: by default
//! the step only runs where there is no loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tonica_audio::calib::{Basis, BeatStep};
use tonica_audio::{
    AudioEngine, BackendEvent, BeatPlan, CalibConfig, Calibrator, ClockQuality, Details,
    EngineConfig, EngineEvent, EngineIo, FakeBackend, FakeConfig, FakeHandle, FakeTruth, MixEvent,
    Outcome, Profile, ProfileKey, Prompt, Pump, RoundKind, Side, SoundBank, SoundId, StampSource,
    StepCause, Warning, find_profile, profiles_from_json, profiles_to_json, upsert_profile,
};
use tonica_dsp::latency::CalError;
use tonica_dsp::synth::Mulberry32;
use tonica_sim::fx::rms;
use tonica_sim::strum::{Dir, StrumSpec, render_take, voicing};

type Eng = AudioEngine<FakeBackend>;

/// Virtual time per loop turn: one `step` every 5 ms.
const SLICE_S: f64 = 0.005;
/// Microphone noise of the fake room (−60 dBFS RMS).
const NOISE: f32 = 1.0e-3;

struct Rig {
    h: FakeHandle,
    eng: Eng,
    io: EngineIo,
}

fn rig(fc: FakeConfig) -> Rig {
    rig_with(fc, EngineConfig::default())
}

fn rig_with(fc: FakeConfig, ec: EngineConfig) -> Rig {
    let fs_out = f64::from(fc.fs_out);
    let backend = FakeBackend::new(fc);
    let h = backend.handle();
    let (eng, io) = AudioEngine::start(backend, ec, SoundBank::new(fs_out), Pump::Manual).unwrap();
    Rig { h, eng, io }
}

fn room(fs_in: u32, fs_out: u32, latency_ms: f64) -> FakeConfig {
    let mut fc = FakeConfig::new(fs_in, fs_out);
    fc.path.latency_s = latency_ms / 1.0e3;
    fc.path.noise_rms = NOISE;
    fc
}

fn config(r: &Rig) -> CalibConfig {
    CalibConfig::new(ProfileKey::new("fake", r.eng.info()), "2026-11-16")
}

/// The whole chain of the M2 plan: the strum step runs on top of a measured loop too,
/// and what it reads is the player's tendency.
fn config_always(r: &Rig) -> CalibConfig {
    CalibConfig {
        beat_step: BeatStep::Always,
        ..config(r)
    }
}

/// The short check of the Pulso plan (§2.2).
fn config_short(r: &Rig) -> CalibConfig {
    CalibConfig::short(ProfileKey::new("fake", r.eng.info()), "2026-11-16")
}

/// The latency a perfect calibration reports, in ms.
///
/// `L_rt = g_arrival − in_of_out(f_emission)` (plan §3). For an emission at output frame
/// `f`, `FakeTruth::arrival_in(f)` is the input frame at which that frame reaches the
/// microphone through the acoustic path, and `FakeTruth::in_of_out(f)` is the input frame
/// captured at the instant `f` leaves the loudspeaker. Their difference is the path
/// latency counted in input frames: `latency_s · rate_in`, the same for every `f`. The
/// profile stores it in ms of *nominal* input frames (that is how the game uses it), so
/// the truth is `latency_s · rate_in / fs_in`: 142.6 ms on an input clock that runs
/// 120 ppm fast is 142.617 ms.
///
/// It is read for an emission half a second before "now", that is at the end of the run:
/// after a moved path (`set_latency_s`) or an injected xrun, which shifts the frame
/// counters of the truth, this is the latency the saved profile has to describe.
fn truth_l_rt_ms(h: &FakeHandle) -> f64 {
    let t: FakeTruth = h.truth().expect("the streams are open");
    let f = t.out_frame_at(h.now_s() - 0.5);
    (t.arrival_in(f) - t.in_of_out(f)) / t.fs_in * 1.0e3
}

// ------------------------------------------------------------------ virtual player

/// Inverse of the standard normal distribution (P. J. Acklam's rational approximation).
fn probit(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    if p < 0.024_25 {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - 0.024_25 {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

fn gauss(r: &mut Mulberry32) -> f64 {
    let u1 = r.next_f64().max(1.0e-12);
    let u2 = r.next_f64();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// `n` timing errors of standard deviation `sigma_s`.
///
/// `stratified`: the `n` quantiles of the normal distribution in a random order, so the
/// player has exactly the stated spread whatever the seed. Otherwise independent draws.
fn timing_errors(n: usize, sigma_s: f64, seed: u32, stratified: bool) -> Vec<f64> {
    let mut rng = Mulberry32::new(seed);
    if !stratified {
        return (0..n).map(|_| sigma_s * gauss(&mut rng)).collect();
    }
    let mut z: Vec<f64> = (0..n)
        .map(|i| sigma_s * probit((i as f64 + 0.5) / n as f64))
        .collect();
    for i in (1..n).rev() {
        let j = (rng.next_f64() * (i + 1) as f64) as usize;
        z.swap(i, j.min(i));
    }
    z
}

/// Four muted strums ("chucks") of the simulator, at the input rate, and the time of the
/// attack inside each of them.
fn muted_takes(fs_in: f64) -> (Vec<Arc<[f32]>>, f64) {
    let attack_s = 0.02;
    let mut rng = Mulberry32::new(77);
    let takes = (0..4usize)
        .map(|k| {
            let spec = StrumSpec {
                t0: attack_s,
                voicing: voicing(["E", "A", "G", "D"][k]),
                dir: if k % 2 == 0 { Dir::Down } else { Dir::Up },
                spread: 0.006 + 0.003 * k as f64,
                gain: 1.0,
                muted: true,
                strings: [true; 6],
                attack: 0.0005,
            };
            let mut audio = render_take(fs_in, &[spec], (0.35 * fs_in) as usize, &mut rng).audio;
            let a = (attack_s * fs_in) as usize;
            let level = rms(&audio[a..a + (0.05 * fs_in) as usize]).max(1.0e-9);
            audio
                .iter_mut()
                .for_each(|v| *v = (f64::from(*v) * 0.1 / level) as f32);
            Arc::from(audio)
        })
        .collect();
    (takes, attack_s)
}

/// A player who strums `bias_s` after every click *as heard*: the click leaves the
/// loudspeaker at `out_time_s(f)`, reaches player and microphone `heard_s` later (the
/// latency the stamps do not know about, which the fake models as its path latency), and
/// the strum is captured `bias_s + error` after that.
struct Player {
    bias_s: f64,
    heard_s: f64,
    errors_s: Vec<f64>,
    takes: Vec<Arc<[f32]>>,
    attack_s: f64,
    scheduled: bool,
    /// Scored beats at the start that get no strum (a late start).
    skip: u32,
    /// Strums on the clicks of the count-in too.
    count_in: bool,
}

impl Player {
    fn new(fc: &FakeConfig, bias_ms: f64, errors_s: Vec<f64>) -> Self {
        let (takes, attack_s) = muted_takes(f64::from(fc.fs_in));
        Self {
            bias_s: bias_ms / 1.0e3,
            heard_s: fc.path.latency_s,
            errors_s,
            takes,
            attack_s,
            scheduled: false,
            skip: 0,
            count_in: false,
        }
    }

    /// The strum on scored beat `i` of `plan`.
    fn strum(&self, h: &FakeHandle, plan: &BeatPlan, i: u32) {
        if i < self.skip {
            return;
        }
        self.strum_on(h, plan.strum_frame(i), i);
    }

    /// A strum on the click at output frame `frame`, with the timing error number `i`.
    fn strum_on(&self, h: &FakeHandle, frame: u64, i: u32) {
        let truth = h.truth().unwrap();
        let click = truth.out_time_s(frame as f64);
        let error = self.errors_s[i as usize % self.errors_s.len()];
        let at = click + self.heard_s + self.bias_s + error - self.attack_s;
        let take = Arc::clone(&self.takes[i as usize % self.takes.len()]);
        h.play_external(at, take, 1.0).expect("strum accepted");
    }

    /// Every strum of `plan` at once (they play even if the clicks stop early).
    fn schedule(&mut self, h: &FakeHandle, plan: &BeatPlan) {
        for i in 0..plan.beats {
            self.strum(h, plan, i);
        }
        self.scheduled = true;
    }
}

// ------------------------------------------------------------------ driver

struct Run {
    seconds: f64,
    prompts: Vec<Prompt>,
    outcome: Outcome,
    details: Details,
}

impl Run {
    fn profile(&self) -> &Profile {
        self.outcome.profile().expect("a profile")
    }
}

/// Drives the calibration to its end, one step per 5 ms of virtual time. `each` runs after
/// every step with the time since the start.
fn calibrate_with(
    r: &mut Rig,
    cal: &mut Calibrator,
    mut player: Option<&mut Player>,
    mut each: impl FnMut(f64, &FakeHandle, &Calibrator),
) -> Run {
    let start = r.h.now_s();
    let mut prompts = Vec::new();
    loop {
        r.h.pump(SLICE_S);
        r.eng.pump();
        let up = cal.step_io(&mut r.io);
        if up.changed {
            prompts.push(up.prompt);
        }
        if let (Some(p), Some(plan)) = (player.as_deref_mut(), cal.beat_plan())
            && !p.scheduled
        {
            p.schedule(&r.h, &plan);
        }
        let t = r.h.now_s() - start;
        each(t, &r.h, cal);
        if up.done {
            break;
        }
        assert!(t < 60.0, "the calibration did not end: {:?}", cal.prompt());
    }
    Run {
        seconds: r.h.now_s() - start,
        prompts,
        outcome: cal.outcome().expect("an outcome").clone(),
        details: cal.details().clone(),
    }
}

fn calibrate(r: &mut Rig, cal: &mut Calibrator, player: Option<&mut Player>) -> Run {
    calibrate_with(r, cal, player, |_, _, _| {})
}

/// Like [`calibrate_with`], with two differences. The test drains the event queue itself
/// and hands the events to [`Calibrator::step`], so `each` sees them. And the player
/// strums beat by beat, when a click starts: such a player stops when the clicks stop and
/// starts again with a new count-in.
fn calibrate_seeing(
    r: &mut Rig,
    cal: &mut Calibrator,
    player: Option<&mut Player>,
    each: impl FnMut(f64, &FakeHandle, &Calibrator, &[EngineEvent]),
) -> Run {
    calibrate_filtered(r, cal, player, |_| true, each)
}

/// [`calibrate_seeing`], where the calibrator only gets the events that `keep` lets by.
fn calibrate_filtered(
    r: &mut Rig,
    cal: &mut Calibrator,
    player: Option<&mut Player>,
    mut keep: impl FnMut(&EngineEvent) -> bool,
    each: impl FnMut(f64, &FakeHandle, &Calibrator, &[EngineEvent]),
) -> Run {
    calibrate_mapped(r, cal, player, |_, e| keep(&e).then_some(e), each)
}

/// [`calibrate_seeing`], where the calibrator gets what `map` makes of each event (none
/// for `None`). `map` sees the calibrator as the step before left it.
fn calibrate_mapped(
    r: &mut Rig,
    cal: &mut Calibrator,
    mut player: Option<&mut Player>,
    mut map: impl FnMut(&Calibrator, EngineEvent) -> Option<EngineEvent>,
    mut each: impl FnMut(f64, &FakeHandle, &Calibrator, &[EngineEvent]),
) -> Run {
    let start = r.h.now_s();
    let mut prompts = Vec::new();
    let mut events = Vec::new();
    let mut plan: Option<BeatPlan> = None;
    let mut next = 0u32;
    loop {
        r.h.pump(SLICE_S);
        r.eng.pump();
        events.clear();
        while let Ok(e) = r.io.events.pop() {
            events.extend(map(cal, e));
        }
        let up = cal.step(&mut r.io.mix, &mut r.io.tap, &events);
        if up.changed {
            prompts.push(up.prompt);
        }
        if let Some(p) = player.as_deref_mut() {
            if cal.beat_plan() != plan {
                plan = cal.beat_plan();
                next = 0;
                if let (Some(plan), true) = (plan, p.count_in) {
                    for k in 0..plan.count_in {
                        p.strum_on(&r.h, plan.click_frame(k), k);
                    }
                }
            }
            if let (Some(plan), Prompt::StrumMuted { beat, .. }) = (plan, up.prompt) {
                while next < u32::from(beat) {
                    p.strum(&r.h, &plan, next);
                    next += 1;
                }
            }
        }
        let t = r.h.now_s() - start;
        each(t, &r.h, cal, &events);
        if up.done {
            break;
        }
        assert!(t < 90.0, "the calibration did not end: {:?}", cal.prompt());
    }
    Run {
        seconds: r.h.now_s() - start,
        prompts,
        outcome: cal.outcome().expect("an outcome").clone(),
        details: cal.details().clone(),
    }
}

fn strumming(p: &Prompt) -> bool {
    matches!(p, Prompt::StrumMuted { .. })
}

/// A steady player: +18 ms, σ 9 ms (the stratified sample).
fn steady_player(fc: &FakeConfig, seed: u32) -> Player {
    Player::new(fc, 18.0, timing_errors(16, 0.009, seed, true))
}

// ------------------------------------------------------------------ T5

#[test]
fn t5_full_calibration_finds_the_latency() {
    eprintln!(
        "T5  fs_in/fs_out   L(ms)  drift in/out   truth      L_rt     error    MAD   tend.   time"
    );
    let mut worst = 0.0f64;
    let (mut bias_sum, mut bias_worst) = (0.0f64, 0.0f64);
    let mut seed = 0;
    for (fs_in, fs_out) in [(48_000, 48_000), (44_100, 48_000)] {
        for latency_ms in [35.0, 87.3, 142.6] {
            // The drift of the plan on the input clock, and once on the output clock.
            for (drift_in, drift_out) in [(0.0, 0.0), (120.0, 0.0), (-80.0, 0.0), (0.0, 120.0)] {
                seed += 1;
                let mut fc = room(fs_in, fs_out, latency_ms);
                fc.drift_in_ppm = drift_in;
                fc.drift_out_ppm = drift_out;
                fc.seed = seed;
                let mut player = steady_player(&fc, seed);
                let mut r = rig(fc);
                let mut cal = Calibrator::new(config_always(&r), None);
                let run = calibrate(&mut r, &mut cal, Some(&mut player));

                let truth = truth_l_rt_ms(&r.h);
                let p = run.profile();
                let l_rt = p.l_rt_ms.expect("the loop is measured");
                let err = l_rt - truth;
                worst = worst.max(err.abs());
                eprintln!(
                    "    {fs_in}/{fs_out}  {latency_ms:6.1}  {drift_in:+5.0}/{drift_out:+5.0}  {truth:8.3}  {l_rt:8.3}  {err:+7.3}  {:5.3}  {:+6.2}  {:5.2}",
                    p.mad_ms.unwrap(),
                    p.tendency_ms.unwrap_or(f64::NAN),
                    run.seconds,
                );
                assert!(matches!(run.outcome, Outcome::Calibrated(_)));
                assert!(err.abs() <= 0.3, "L_rt {l_rt:.3} ms, truth {truth:.3} ms");
                // One round, every probe on time and heard, the line fitted and in
                // agreement with the live clock map.
                assert_eq!(run.details.rounds.len(), 1);
                let round = &run.details.rounds[0];
                assert_eq!((round.sent, round.started, round.late), (10, 10, 0));
                assert_eq!(round.found, Some(10));
                assert!(round.fitted && round.level_dbfs == -12.0);
                assert!(round.fit_vs_live_ms.unwrap() <= 0.3, "{round:?}");
                assert!(
                    run.details.warnings.is_empty(),
                    "{:?}",
                    run.details.warnings
                );
                assert!((-62.0..-58.0).contains(&run.details.noise_floor_dbfs.unwrap()));
                assert_eq!(p.clock_quality, ClockQuality::Locked);
                assert!((p.drift_ppm - (drift_in - drift_out)).abs() < 15.0, "{p:?}");
                // The player of T6 on top: the whole chain ends judged. T5 gates the
                // latency; the tendency of these 24 players is a median of 12 strums with
                // σ = 9 ms (standard error about 3 ms), so here it is only held to two
                // standard errors. The 3 ms limit of T6 is checked in the T6 tests.
                assert!(p.judgeable, "{p:?}");
                // What the strums read is the player's: it never enters the offset.
                assert_eq!(
                    (p.basis(), p.judge_offset_ms()),
                    (Basis::Loopback, p.l_rt_ms)
                );
                let bias = p.tendency_ms.unwrap();
                bias_sum += bias - 18.0;
                bias_worst = bias_worst.max((bias - 18.0).abs());
                assert!((bias - 18.0).abs() <= 6.0, "{p:?}");
                assert_eq!(
                    run.prompts[..2],
                    [Prompt::Quiet, Prompt::WillBeep { count: 10 }]
                );
                assert_eq!(run.prompts.last(), Some(&Prompt::Done));
                // Plan §4: the whole run within 25 s.
                assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);
            }
        }
    }
    eprintln!("T5  worst |error| {worst:.3} ms (limit 0.3)");
    eprintln!(
        "T5  tendency − 18 ms over {seed} players: mean {:+.2} ms, worst {bias_worst:.2} ms",
        bias_sum / f64::from(seed)
    );
    // No systematic error of the chain: the mean over 24 players is within 2 ms.
    assert!((bias_sum / f64::from(seed)).abs() <= 2.0);
}

// ------------------------------------------------------------------ refusals

#[test]
fn path_jitter_of_8_ms_is_refused_and_the_strums_take_the_latency() {
    // The latency of the path moves by σ = 8 ms between probes (a Bluetooth link).
    let fc = room(48_000, 48_000, 90.0);
    let mut player = steady_player(&fc, 3);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let mut rng = Mulberry32::new(21);
    let mut next = 0.0;
    let run = calibrate_with(&mut r, &mut cal, Some(&mut player), |t, h, _| {
        if t >= next {
            next += 0.1;
            h.set_latency_s(0.090 + 0.008 * gauss(&mut rng));
        }
    });
    let round = &run.details.rounds[0];
    eprintln!("path jitter 8 ms: {:?}; {:?}", round.refused, run.profile());
    assert!(
        matches!(
            round.refused,
            Some(CalError::UnstableLatency { .. } | CalError::TooFewProbes { .. })
        ),
        "{round:?}"
    );
    // No L_rt, no failure prompt: the strums go on and hold latency and tendency in one
    // number, which is all there is to judge with.
    let p = run.profile();
    assert!(p.l_rt_ms.is_none() && run.details.no_loop);
    assert!(run.prompts.iter().any(strumming));
    assert!(
        (p.combined_ms.unwrap() - (90.0 + 18.0)).abs() <= 3.0,
        "{p:?}"
    );
    assert!(p.judgeable);
    assert_eq!((p.basis(), p.tendency_ms), (Basis::Estimate, None));
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);
}

#[test]
fn stamp_jitter_of_8_ms_keeps_the_judgement_right() {
    // Device stamps that shake by σ = 8 ms on both streams. The clock map drops them and
    // falls back to the callback times. That map is steady, so the round is not refused,
    // but it is offset by a constant (the buffers: about 22 ms here), and the constant
    // enters `L_rt` (plan §3). The game judges with the same map, where it cancels:
    // `(t_onset − L_rt) − in_of_out(f_beat)` is what the strum step measures, and it must
    // still read the player's 18 ms.
    let mut fc = room(48_000, 48_000, 87.3);
    fc.stamp_jitter_in_s = 8.0e-3;
    fc.stamp_jitter_out_s = 8.0e-3;
    let mut player = steady_player(&fc, 4);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    let round = &run.details.rounds[0];
    eprintln!(
        "stamp jitter 8 ms: L_rt {:?} ms (acoustic path {:.3}), MAD {:?} ms, tendency {:?} ms, \
         combined {:?} ms, refused {:?}, warnings {:?}, {:.2} s",
        p.l_rt_ms,
        truth_l_rt_ms(&r.h),
        p.mad_ms,
        p.tendency_ms,
        p.combined_ms,
        round.refused,
        run.details.warnings,
        run.seconds
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    assert!(p.judgeable, "{p:?}");
    if p.l_rt_ms.is_some() {
        // Kept: then the latency is steady and the judgement built on it is right.
        assert!(p.mad_ms.unwrap() <= 3.0, "{p:?}");
        assert!((p.tendency_ms.unwrap() - 18.0).abs() <= 3.0, "{p:?}");
    } else {
        // Refused: the strums hold the whole latency (at least the acoustic path).
        assert!(p.combined_ms.unwrap() >= 87.3 + 18.0 - 3.0, "{p:?}");
    }
    assert!(run.seconds <= 30.0);
}

#[test]
fn silent_path_is_refused_twice_then_strums_alone() {
    // Headphones: the microphone never hears the loudspeaker.
    let mut fc = room(48_000, 48_000, 87.3);
    fc.path.gain = 0.0;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    let rounds = &run.details.rounds;
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert_eq!(
        (rounds[0].kind, rounds[0].level_dbfs),
        (RoundKind::First, -12.0)
    );
    assert_eq!(
        (rounds[1].kind, rounds[1].level_dbfs),
        (RoundKind::Retry, -6.0)
    );
    for round in rounds {
        assert!(
            matches!(
                round.refused,
                Some(CalError::TooFewProbes {
                    found: 0,
                    needed: 7
                })
            ),
            "{round:?}"
        );
    }
    // Nobody strummed: a profile without numbers, "not yet", no failure prompt.
    let p = run.profile();
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!((p.l_rt_ms, p.combined_ms, p.judgeable), (None, None, false));
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    assert!(run.details.no_loop);
    assert_eq!(
        run.prompts[..3],
        [
            Prompt::Quiet,
            Prompt::WillBeep { count: 10 },
            Prompt::WillBeepLouder { count: 10 }
        ]
    );
    assert!(run.prompts.iter().any(strumming));
    assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
    assert!(!run.prompts.contains(&Prompt::MicBlocked));
    // Worst case with the second round: under 30 s.
    eprintln!("silent path: {:.2} s", run.seconds);
    assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);
}

#[test]
fn all_zero_input_is_a_blocked_microphone() {
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 0.0;
    fc.path.noise_rms = 0.0;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::MicBlocked);
    assert_eq!(run.prompts, [Prompt::Quiet, Prompt::MicBlocked]);
    assert!(run.details.rounds.is_empty());
    eprintln!("blocked microphone: {:.2} s", run.seconds);
    assert!(run.seconds <= 5.0);
}

#[test]
fn digitally_silent_path_that_carries_sound_is_not_blocked() {
    // A loopback cable or a noise gate: zeros until something plays. Blocking here would
    // be a needless refusal.
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.noise_rms = 0.0;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    let truth = truth_l_rt_ms(&r.h);
    let p = run.profile();
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert_eq!(run.details.noise_floor_dbfs, None);
    assert!(!run.prompts.contains(&Prompt::MicBlocked));
}

#[test]
fn weak_path_is_heard_on_the_louder_round() {
    // The first round is not heard (the path opens only afterwards): the round at −6 dBFS
    // measures the latency.
    let mut fc = room(44_100, 48_000, 87.3);
    fc.path.gain = 0.0;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate_with(&mut r, &mut cal, None, |_, h, cal| {
        if matches!(cal.prompt(), Prompt::WillBeepLouder { .. }) {
            h.set_path_gain(0.5);
        }
    });
    let truth = truth_l_rt_ms(&r.h);
    let rounds = &run.details.rounds;
    assert_eq!(rounds.len(), 2);
    assert!(matches!(
        rounds[0].refused,
        Some(CalError::TooFewProbes { .. })
    ));
    assert_eq!((rounds[1].level_dbfs, rounds[1].found), (-6.0, Some(10)));
    let l_rt = run.profile().l_rt_ms.unwrap();
    assert!(
        (l_rt - truth).abs() <= 0.3,
        "L_rt {l_rt:.3}, truth {truth:.3}"
    );
    assert!(!run.details.no_loop);
    assert!(run.seconds <= 30.0);
}

// ------------------------------------------------------------------ T6

#[test]
fn t6_bias_of_a_steady_player() {
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "T6  +18 ms, σ 9 ms: tendency {:+.2} ms, MAD {:.2} ms, {} of {} strums used, {} beats, {:.2} s",
        p.tendency_ms.unwrap(),
        p.tendency_mad_ms.unwrap(),
        run.details.strums_used,
        run.details.strums_heard,
        run.details.beats_played,
        run.seconds
    );
    assert!((p.tendency_ms.unwrap() - 18.0).abs() <= 3.0, "{p:?}");
    assert!(p.judgeable && p.tendency_mad_ms.unwrap() <= 15.0);
    // 12 paired strums with a small MAD stop the step early.
    assert_eq!(run.details.strums_used, 12);
    assert!(run.details.beats_played < 16);
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    // The count-in counts down, then the beats count up.
    let beat_prompts: Vec<&Prompt> = run.prompts.iter().filter(|p| strumming(p)).collect();
    assert_eq!(
        beat_prompts[..6],
        [
            &Prompt::StrumMuted {
                count_in: 4,
                beat: 0,
                of: 16
            },
            &Prompt::StrumMuted {
                count_in: 3,
                beat: 0,
                of: 16
            },
            &Prompt::StrumMuted {
                count_in: 2,
                beat: 0,
                of: 16
            },
            &Prompt::StrumMuted {
                count_in: 1,
                beat: 0,
                of: 16
            },
            &Prompt::StrumMuted {
                count_in: 0,
                beat: 1,
                of: 16
            },
            &Prompt::StrumMuted {
                count_in: 0,
                beat: 2,
                of: 16
            },
        ]
    );
    // What the game subtracts is the latency of the system, and only that: the 18 ms are
    // the player's, to be shown, never to be taken out of the judgement. (Before P8 the
    // profile gave `L_rt + bias` here, 105.3 ms.)
    let off = p.judge_offset_ms().expect("judged with the loop");
    assert_eq!(p.judge_offset_ms(), p.l_rt_ms);
    assert!((off - truth_l_rt_ms(&r.h)).abs() <= 0.3, "{off:.3} ms");
}

/// A2 (3). Before P8 this player got "not yet" (`t6_scattered_player_is_not_judged`):
/// the bias was part of the offset and a scattered bias could not be trusted. With a
/// loop the offset is the latency alone, so the scatter is the player's to work on and
/// the game judges it.
#[test]
fn t6_scattered_player_with_a_loop_is_judged() {
    let fc = room(48_000, 48_000, 87.3);
    let mut player = Player::new(&fc, 18.0, timing_errors(16, 0.025, 1, true));
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "T6  +18 ms, σ 25 ms: tendency {:+.2} ms, MAD {:.2} ms, {} strums used, judgeable {}, {:.2} s",
        p.tendency_ms.unwrap(),
        p.tendency_mad_ms.unwrap(),
        run.details.strums_used,
        p.judgeable,
        run.seconds
    );
    // The strums are as scattered as before, and written down as such.
    assert!(p.tendency_mad_ms.unwrap() > 15.0, "{p:?}");
    assert!(p.judgeable, "{p:?}");
    assert_eq!(p.basis(), Basis::Loopback);
    assert_eq!(p.judge_offset_ms(), p.l_rt_ms);
    assert!((p.l_rt_ms.unwrap() - truth_l_rt_ms(&r.h)).abs() <= 0.3);
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);

    // The same player without a loop (headphones): the strums are the only offset there
    // is, and strums this scattered do not give one. The rule of before: "not yet".
    let mut fc = fc;
    fc.path.gain = 0.0;
    let mut player = Player::new(&fc, 18.0, timing_errors(16, 0.025, 1, true));
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "T6  +18 ms, σ 25 ms, no loop: combined {:?} ms, MAD {:?} ms, judgeable {}",
        p.combined_ms, p.combined_mad_ms, p.judgeable
    );
    assert!(!p.judgeable && p.combined_mad_ms.unwrap() > 15.0, "{p:?}");
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
}

#[test]
fn t6_without_a_loop_the_strums_hold_the_whole_latency() {
    // Headphones (path gain 0) with 35, 142.6 and 300 ms between the click and the ear:
    // the last one is beyond the pairing window of `player_bias` (262 ms) on purpose.
    for latency_ms in [35.0, 142.6, 300.0] {
        let mut fc = room(48_000, 48_000, latency_ms);
        fc.path.gain = 0.0;
        let mut player = steady_player(&fc, 2);
        let mut r = rig(fc);
        let mut cal = Calibrator::new(config(&r), None);
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let p = run.profile();
        eprintln!(
            "T6  no loop, {latency_ms} ms + 18 ms: combined {:+.2} ms, MAD {:.2} ms, n {}, {:.2} s",
            p.combined_ms.unwrap(),
            p.combined_mad_ms.unwrap(),
            p.combined_n.unwrap(),
            run.seconds
        );
        assert_eq!(p.l_rt_ms, None);
        assert!(
            (p.combined_ms.unwrap() - (latency_ms + 18.0)).abs() <= 3.0,
            "{p:?}"
        );
        assert!(p.judgeable);
        // An estimate: judged with, and nothing in it is called the player's tendency.
        assert_eq!(p.basis(), Basis::Estimate);
        assert_eq!(p.judge_offset_ms(), p.combined_ms);
        assert_eq!((p.tendency_ms, p.tendency_mad_ms), (None, None));
        assert_eq!(p.combined_n, Some(run.details.strums_used));
        assert!(p.combined_n.unwrap() >= 12);
        assert_eq!(run.prompts.last(), Some(&Prompt::Done));
        assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);
    }
}

/// Not a gate of the plan: how the strum step behaves for players drawn at random. With
/// 12 to 16 strums the median of a σ = 9 ms player has a standard error near 3 ms, so the
/// 3 ms of T6 is one standard error, not a bound for every player.
///
/// `steady` counts the players whose strums are consistent (MAD ≤ 15 ms). Before P8 that
/// was the condition to be judged at all; with a loop every one of them is judged now.
#[test]
fn t6_random_players_statistics() {
    let seeds = 12;
    let mut errs = Vec::new();
    let mut judged_steady = 0;
    let mut judged_scattered = 0;
    let mut judgeable = 0;
    for seed in 0..seeds {
        for (sigma, judged) in [(0.009, &mut judged_steady), (0.025, &mut judged_scattered)] {
            let mut fc = room(48_000, 48_000, 87.3);
            fc.seed = 100 + seed;
            let mut player = Player::new(&fc, 18.0, timing_errors(16, sigma, 500 + seed, false));
            let mut r = rig(fc);
            let mut cal = Calibrator::new(config_always(&r), None);
            let run = calibrate(&mut r, &mut cal, Some(&mut player));
            let p = run.profile();
            *judged += u32::from(p.tendency_mad_ms.is_some_and(|mad| mad <= 15.0));
            judgeable += u32::from(p.judgeable && p.judge_offset_ms() == p.l_rt_ms);
            if sigma < 0.01 {
                errs.push(p.tendency_ms.unwrap() - 18.0);
            }
        }
    }
    let within = errs.iter().filter(|e| e.abs() <= 3.0).count();
    let worst = errs.iter().fold(0.0f64, |m, e| m.max(e.abs()));
    errs.sort_by(f64::total_cmp);
    eprintln!(
        "T6  random players, {seeds} seeds: σ 9 ms steady {judged_steady}/{seeds}, |tendency − 18| ≤ 3 ms \
         in {within}/{seeds}, median error {:+.2} ms, worst {worst:.2} ms; σ 25 ms steady \
         {judged_scattered}/{seeds}; judged with the latency alone {judgeable}/{}",
        errs[errs.len() / 2],
        2 * seeds
    );
    assert_eq!(
        judged_steady, seeds,
        "a σ = 9 ms player always reads steady"
    );
    assert!(worst <= 10.0, "three standard errors: {worst:.2} ms");
    assert!(errs[errs.len() / 2].abs() <= 3.0);
    assert!(
        judged_scattered * 2 <= seeds,
        "σ = 25 ms mostly reads scattered"
    );
    // With a loop, all 24 are judged, and with the latency alone.
    assert_eq!(judgeable, 2 * seeds);
}

// ------------------------------------------------------------------ profile, quick check

#[test]
fn profile_json_round_trip() {
    let fc = room(48_000, 48_000, 87.3);
    let r = rig(fc);
    let key = ProfileKey::new("fake", r.eng.info());
    let full = Profile {
        key: key.clone(),
        l_rt_ms: Some(87.31),
        mad_ms: Some(0.012),
        tendency_ms: Some(18.4),
        tendency_mad_ms: Some(6.1),
        combined_ms: None,
        combined_mad_ms: None,
        combined_n: None,
        judgeable: true,
        date: "2026-11-16".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: -79.6,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    };
    let no_loop = Profile {
        key: ProfileKey {
            out_id: "headphones".into(),
            ..key.clone()
        },
        l_rt_ms: None,
        mad_ms: None,
        tendency_ms: None,
        tendency_mad_ms: None,
        judgeable: false,
        clock_quality: ClockQuality::Acquiring,
        ..full.clone()
    };
    // Headphones with the strums read: an estimate (on a locked map, as every
    // judgeable profile).
    let estimate = Profile {
        key: ProfileKey {
            out_id: "bluetooth".into(),
            ..key.clone()
        },
        combined_ms: Some(212.75),
        combined_mad_ms: Some(7.25),
        combined_n: Some(14),
        judgeable: true,
        clock_quality: ClockQuality::Locked,
        ..no_loop.clone()
    };
    assert_eq!(
        (full.basis(), no_loop.basis(), estimate.basis()),
        (Basis::Loopback, Basis::None, Basis::Estimate)
    );
    assert_eq!(
        (
            full.judge_offset_ms(),
            no_loop.judge_offset_ms(),
            estimate.judge_offset_ms()
        ),
        (Some(87.31), None, Some(212.75))
    );
    // An older profile of the same pair is replaced, not kept and not doubled.
    let older = Profile {
        l_rt_ms: Some(64.2),
        tendency_ms: Some(-7.5),
        date: "2026-10-02".into(),
        ..full.clone()
    };
    let mut list = Vec::new();
    upsert_profile(&mut list, older.clone());
    upsert_profile(&mut list, no_loop.clone());
    assert_eq!(list, [older.clone(), no_loop.clone()]);
    upsert_profile(&mut list, full.clone());
    assert_eq!(list, [full.clone(), no_loop.clone()]);
    assert_ne!(find_profile(&list, &key), Some(&older));
    upsert_profile(&mut list, estimate.clone());

    let text = profiles_to_json(&list);
    for field in [
        "\"version\": 2",
        "\"host\": \"fake\"",
        "\"in_id\": \"fake-in\"",
        "\"out_id\": \"fake-out\"",
        "\"fs_in\": 48000",
        "\"fs_out\": 48000",
        "\"l_rt_ms\": 87.31",
        "\"mad_ms\": 0.012",
        "\"tendency_ms\": 18.4",
        "\"tendency_mad_ms\": 6.1",
        "\"combined_ms\": 212.75",
        "\"combined_mad_ms\": 7.25",
        "\"combined_n\": 14",
        "\"judgeable\": true",
        "\"date\": \"2026-11-16\"",
        "\"clock_quality\": \"locked\"",
        "\"l_rt_ms\": null",
        "\"combined_ms\": null",
    ] {
        assert!(text.contains(field), "{field} missing in\n{text}");
    }
    // The names of version 1 are gone: nothing can read the sum of the two again.
    assert!(!text.contains("bias"), "{text}");
    let back = profiles_from_json(&text);
    assert_eq!(back, list);
    assert_eq!(find_profile(&back, &key), Some(&full));
    assert_eq!(find_profile(&back, &no_loop.key), Some(&no_loop));
    assert_eq!(find_profile(&back, &estimate.key), Some(&estimate));
    let other = ProfileKey {
        fs_in: 44_100,
        ..key
    };
    assert_eq!(find_profile(&back, &other), None);
    // Anything else reads as "no profile", never as an error.
    for bad in [
        "",
        "{",
        "[]",
        "{\"version\": 2, \"profiles\": []}",
        "{\"version\": 1}",
        "{\"version\": 2}",
        "{\"version\": \"2\", \"profiles\": []}",
    ] {
        assert!(profiles_from_json(bad).is_empty(), "{bad}");
    }
    let partial = "{\"version\":2,\"profiles\":[{\"host\":\"x\"},".to_owned()
        + &full.to_json().to_string()
        + "]}";
    assert_eq!(profiles_from_json(&partial), vec![full.clone()]);
    // A version this code does not know is not guessed at.
    let newer = partial.replace("\"version\":2", "\"version\":3");
    assert!(profiles_from_json(&newer).is_empty());
}

#[test]
fn quick_check_keeps_the_profile_or_calibrates_again() {
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 6);
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config_always(&r), None);
    let first = calibrate(&mut r, &mut cal, Some(&mut player));
    // The profile goes through the file format, as the CLI will do.
    let saved = profiles_from_json(&profiles_to_json(&[first.profile().clone()]));
    let saved = find_profile(&saved, &config(&r).key).cloned().unwrap();
    assert_eq!(&saved, first.profile());

    // Same engine, same path: three probes, the profile stays.
    let mut cal = Calibrator::new(config(&r), Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    let quick = run.details.quick.clone().unwrap();
    eprintln!(
        "quick check, running engine: Δ {:+.3} ms in {:.2} s",
        quick.delta_ms.unwrap(),
        run.seconds
    );
    assert_eq!(run.outcome, Outcome::Kept(saved.clone()));
    assert!(quick.kept && quick.delta_ms.unwrap().abs() <= 0.3);
    assert_eq!(run.details.rounds.len(), 1);
    let round = &run.details.rounds[0];
    assert_eq!(
        (round.kind, round.sent, round.found),
        (RoundKind::Quick, 3, Some(3))
    );
    assert_eq!(run.prompts, [Prompt::Checking, Prompt::Done]);
    assert!(run.seconds <= 2.0, "took {:.2} s", run.seconds);

    // A new engine on the same devices (the game was restarted): the clock has to lock
    // first, then the same three probes.
    let mut r2 = rig(fc.clone());
    let mut cal = Calibrator::new(config(&r2), Some(saved.clone()));
    let run = calibrate(&mut r2, &mut cal, None);
    eprintln!("quick check, fresh engine: {:.2} s", run.seconds);
    assert_eq!(run.outcome, Outcome::Kept(saved.clone()));
    assert_eq!(run.details.rounds.len(), 1);
    assert!(run.seconds <= 4.0, "took {:.2} s", run.seconds);

    // The path moved by 10 ms: the check fails and the whole calibration runs.
    r.h.set_latency_s(0.0973);
    let mut moved = fc.clone();
    moved.path.latency_s = 0.0973; // the player hears the click through the new path
    let mut player = steady_player(&moved, 7);
    let mut cal = Calibrator::new(config_always(&r), Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let quick = run.details.quick.clone().unwrap();
    let truth = truth_l_rt_ms(&r.h);
    let p = run.profile();
    eprintln!(
        "quick check, path moved: Δ {:+.3} ms, new L_rt {:.3} ms (truth {truth:.3}), tendency {:+.2} ms, {:.2} s",
        quick.delta_ms.unwrap(),
        p.l_rt_ms.unwrap(),
        p.tendency_ms.unwrap_or(f64::NAN),
        run.seconds
    );
    assert!(!quick.kept && (quick.delta_ms.unwrap() - 10.0).abs() <= 0.3);
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!((truth - 97.3).abs() < 0.01);
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert!((p.tendency_ms.unwrap() - 18.0).abs() <= 3.0 && p.judgeable);
    assert_eq!(
        run.details
            .rounds
            .iter()
            .map(|x| x.kind)
            .collect::<Vec<_>>(),
        [RoundKind::Quick, RoundKind::First]
    );
    assert_eq!(
        run.prompts[..3],
        [
            Prompt::Checking,
            Prompt::Quiet,
            Prompt::WillBeep { count: 10 }
        ]
    );
    assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);

    // A profile of another device pair is not checked at all.
    let mut foreign = saved;
    foreign.key.in_id = "another microphone".into();
    let cal = Calibrator::new(config(&r), Some(foreign));
    assert_eq!(cal.prompt(), Prompt::Quiet);
}

#[test]
fn quick_check_of_a_profile_without_a_loop() {
    // Saved on headphones: no L_rt, no strums read yet, rhythm not judged yet.
    let mut fc = room(48_000, 48_000, 60.0);
    fc.path.gain = 0.0;
    let mut r = rig(fc.clone());
    let saved = Profile {
        key: config(&r).key,
        l_rt_ms: None,
        mad_ms: None,
        tendency_ms: None,
        tendency_mad_ms: None,
        combined_ms: None,
        combined_mad_ms: None,
        combined_n: None,
        judgeable: false,
        date: "2026-11-01".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: 0.0,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    };
    // Still no loop: kept, and the ending says "not yet".
    let mut cal = Calibrator::new(config(&r), Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::Kept(saved.clone()));
    assert_eq!(run.prompts, [Prompt::Checking, Prompt::RhythmLater]);

    // "Never" is never: the kept profile stays as it is, without the strums.
    let mut cfg = config(&r);
    cfg.retry_rhythm = true;
    cfg.beat_step = BeatStep::Never;
    let mut cal = Calibrator::new(cfg, Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::Kept(saved.clone()));
    assert_eq!(run.prompts, [Prompt::Checking, Prompt::RhythmLater]);

    // "Try again later": only the strums, on top of the kept profile.
    let mut cfg = config(&r);
    cfg.retry_rhythm = true;
    let mut player = steady_player(&fc, 8);
    let mut cal = Calibrator::new(cfg, Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(run.details.rounds.len(), 1);
    assert_eq!(p.l_rt_ms, None);
    assert!(
        (p.combined_ms.unwrap() - 78.0).abs() <= 3.0 && p.judgeable,
        "{p:?}"
    );
    assert_eq!(p.basis(), Basis::Estimate);
    assert_eq!(p.date, "2026-11-16");

    // Loudspeakers now: the probes are heard, so the whole calibration runs.
    r.h.set_path_gain(0.5);
    let mut cal = Calibrator::new(config(&r), Some(saved));
    let run = calibrate(&mut r, &mut cal, None);
    let truth = truth_l_rt_ms(&r.h);
    assert!(!run.details.quick.clone().unwrap().kept);
    assert!((run.profile().l_rt_ms.unwrap() - truth).abs() <= 0.3);
}

// ------------------------------------------------------------------ levels, glitches

#[test]
fn nothing_plays_above_minus_six_dbfs() {
    // The microphone hears the loudspeaker at unit gain; a spy on the microphone signal
    // keeps the largest sample. The configuration asks for 0 dBFS everywhere.
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 1.0;
    fc.path.noise_rms = 1.0e-4;
    let mut r = rig(fc);
    let peak = Arc::new(AtomicU32::new(0));
    let spy = Arc::clone(&peak);
    r.h.set_external_fn(Some(Box::new(move |_, mic: &mut [f32]| {
        let m = mic.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        if m > f32::from_bits(spy.load(Ordering::Relaxed)) {
            spy.store(m.to_bits(), Ordering::Relaxed);
        }
    })));
    // With the strum step, so that the clicks play too.
    let mut cfg = config_always(&r);
    cfg.probe_dbfs = 0.0;
    cfg.retry_dbfs = 0.0;
    cfg.click_dbfs = 0.0;
    let mut cal = Calibrator::new(cfg, None);
    let run = calibrate(&mut r, &mut cal, None);
    assert!(run.details.beats_played >= 12, "the clicks played");
    let peak = f64::from(f32::from_bits(peak.load(Ordering::Relaxed)));
    let dbfs = 20.0 * peak.log10();
    eprintln!("loudest sample at the microphone: {peak:.4} ({dbfs:.2} dBFS)");
    assert_eq!(run.details.rounds[0].level_dbfs, -6.0);
    // The path interpolates between the samples; the levels are set against the true
    // peak, so even that stays under −6 dBFS. Slack: the 1e-4 of noise (0.01 dB).
    assert!(dbfs <= -5.98, "{dbfs:.2} dBFS");
    assert!(dbfs >= -6.5, "the sounds should reach −6 dBFS: {dbfs:.2}");
}

#[test]
fn clipping_input_asks_for_distance_and_goes_on() {
    // Something drives the microphone into the rails. `loud_s`: how long it lasts.
    for (loud_s, stays) in [(2.0, false), (1.0e9, true)] {
        let fc = room(48_000, 48_000, 87.3);
        let mut player = steady_player(&fc, 1);
        let mut r = rig(fc);
        let until = (loud_s * 48_000.0) as u64;
        r.h.set_external_fn(Some(Box::new(move |pos, mic: &mut [f32]| {
            for (i, x) in mic.iter_mut().enumerate() {
                let g = pos + i as u64;
                if g < until && g % 7 < 3 {
                    *x += if g.is_multiple_of(2) { 1.5 } else { -1.5 };
                }
            }
        })));
        let mut cal = Calibrator::new(config_always(&r), None);
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let p = run.profile();
        eprintln!(
            "clipping for {loud_s} s: prompts {:?}, L_rt {:?}, warnings {:?}, {:.2} s",
            &run.prompts[..3.min(run.prompts.len())],
            p.l_rt_ms,
            run.details.warnings,
            run.seconds
        );
        // Asked once, right after the silence, and never a failure screen.
        assert_eq!(run.prompts[..2], [Prompt::Quiet, Prompt::TooLoud]);
        assert_eq!(
            run.prompts
                .iter()
                .filter(|p| **p == Prompt::TooLoud)
                .count(),
            1
        );
        assert!(matches!(run.outcome, Outcome::Calibrated(_)));
        assert!(run.details.clipped);
        assert_eq!(run.details.warnings.contains(&Warning::InputClipped), stays);
        if !stays {
            // The room went quiet: the usual calibration, 1.5 s later.
            let truth = truth_l_rt_ms(&r.h);
            assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
            assert!(
                p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
                "{p:?}"
            );
            assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);
        } else if let Some(l_rt) = p.l_rt_ms {
            // Still loud: a latency is reported only if it is the right one.
            assert!((l_rt - truth_l_rt_ms(&r.h)).abs() <= 0.3, "{p:?}");
        }
        assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);
    }
}

#[test]
fn xrun_inside_the_probe_round_repeats_it() {
    let fc = room(48_000, 48_000, 87.3);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let mut done = false;
    let run = calibrate_with(&mut r, &mut cal, None, |t, h, cal| {
        if !done && t > 5.0 && matches!(cal.prompt(), Prompt::WillBeep { .. }) {
            done = true;
            assert!(h.inject_xrun(Side::In, 960));
        }
    });
    let truth = truth_l_rt_ms(&r.h);
    let rounds = &run.details.rounds;
    eprintln!(
        "xrun in the round: L_rt {:.3} ms (truth {truth:.3}), {:.2} s",
        run.profile().l_rt_ms.unwrap(),
        run.seconds
    );
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert!(rounds[0].glitch && !rounds[1].glitch);
    assert_eq!(rounds[1].kind, RoundKind::First);
    assert!(run.details.warnings.contains(&Warning::RoundGlitch));
    assert!((run.profile().l_rt_ms.unwrap() - truth).abs() <= 0.3);
    assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);
}

#[test]
fn stalled_driver_overflows_the_tap_and_the_round_repeats() {
    let fc = room(48_000, 48_000, 35.0);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    // Up to the middle of the probe round.
    while !matches!(cal.prompt(), Prompt::WillBeep { .. }) {
        r.h.pump(SLICE_S);
        r.eng.pump();
        cal.step_io(&mut r.io);
    }
    for _ in 0..200 {
        r.h.pump(SLICE_S);
        r.eng.pump();
        cal.step_io(&mut r.io);
    }
    // The UI thread hangs for 2.6 s: the tap (2 s) overflows.
    for _ in 0..520 {
        r.h.pump(SLICE_S);
        r.eng.pump();
    }
    let run = calibrate(&mut r, &mut cal, None);
    let truth = truth_l_rt_ms(&r.h);
    let rounds = &run.details.rounds;
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert!(rounds[0].glitch);
    assert_eq!(rounds[1].found, Some(10));
    let l_rt = run.profile().l_rt_ms.unwrap();
    assert!(
        (l_rt - truth).abs() <= 0.3,
        "L_rt {l_rt:.3}, truth {truth:.3}"
    );
}

#[test]
fn short_stalls_of_the_driver_do_not_spoil_the_numbers() {
    // The UI thread hangs while the engine keeps running: in the probe round, then in the
    // beat step. Whatever the machine does about it (go on, repeat the round), the
    // latency it reports is right and the steady player is still read as +18 ms.
    for (in_beat, stall_s) in [(false, 0.4), (false, 1.2), (true, 0.4), (true, 1.2)] {
        let fc = room(48_000, 48_000, 87.3);
        let mut player = steady_player(&fc, 1);
        let mut r = rig(fc);
        let mut cal = Calibrator::new(config_always(&r), None);
        let mut prompts = Vec::new();
        let mut wait = 160; // 0.8 s into the phase
        loop {
            r.h.pump(SLICE_S);
            r.eng.pump();
            let up = cal.step_io(&mut r.io);
            if up.changed {
                prompts.push(up.prompt);
            }
            if let Some(plan) = cal.beat_plan()
                && !player.scheduled
            {
                player.schedule(&r.h, &plan);
            }
            let there = match up.prompt {
                Prompt::WillBeep { .. } => !in_beat,
                Prompt::StrumMuted { beat, .. } => in_beat && beat >= 3,
                _ => false,
            };
            if there {
                wait -= 1;
                if wait == 0 {
                    break;
                }
            }
            assert!(!up.done, "ended before the stall: {prompts:?}");
        }
        for _ in 0..(stall_s / SLICE_S) as usize {
            r.h.pump(SLICE_S);
            r.eng.pump();
        }
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let truth = truth_l_rt_ms(&r.h);
        let p = run.profile();
        eprintln!(
            "stall of {stall_s} s in the {}: rounds {}, L_rt {:?} (truth {truth:.3}), tendency {:?}, \
             used {}, judgeable {}, warnings {:?}",
            if in_beat { "beat step" } else { "probe round" },
            run.details.rounds.len(),
            p.l_rt_ms,
            p.tendency_ms,
            run.details.strums_used,
            p.judgeable,
            run.details.warnings
        );
        let l_rt = p.l_rt_ms.expect("the loop is still measured");
        assert!(
            (l_rt - truth).abs() <= 0.3,
            "L_rt {l_rt:.3}, truth {truth:.3}"
        );
        assert!(p.judgeable, "{p:?}");
        assert!((p.tendency_ms.unwrap() - 18.0).abs() <= 3.0, "{p:?}");
    }
}

// ------------------------------------------------------------------ review findings

#[test]
fn stamp_fallback_in_the_beat_step_restarts_the_measurement() {
    // The output stamps go bad while the player strums: the clock map falls back to the
    // callback times of that stream, and `in_of_out` moves by a constant (the 20 ms
    // between an output callback and its sound). Pairs from before and after are on two
    // different maps, and so is the latency measured before.
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config_always(&r), None);
    let mut rng = Mulberry32::new(77);
    let mut fell = None;
    // The prompt on screen when a step begins (the one the step before it left).
    let mut shown = cal.prompt();
    let run = calibrate_seeing(&mut r, &mut cal, Some(&mut player), |t, h, cal, events| {
        if events
            .iter()
            .any(|e| matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback))
        {
            fell.get_or_insert((t, shown));
        }
        shown = cal.prompt();
        let strumming = matches!(cal.prompt(), Prompt::StrumMuted { beat, .. } if beat >= 3);
        if strumming && fell.is_none() {
            // Every output stamp is off by 30 to 70 ms, either way: none agrees with the
            // one before, so the map rejects them all.
            let size = 0.030 + 0.040 * rng.next_f64();
            let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
            h.inject_stamp_outlier(Side::Out, sign * size);
        }
    });
    let (fell_at, fell_in) = fell.expect("the test must produce a fallback");
    assert!(matches!(fell_in, Prompt::StrumMuted { .. }), "{fell_in:?}");

    // The reference: a calibration made wholly on the map now in force.
    let mut player = steady_player(&fc, 2);
    let mut again = Calibrator::new(config_always(&r), None);
    let reference = calibrate_seeing(&mut r, &mut again, Some(&mut player), |_, _, _, _| {});
    let (p, q) = (run.profile(), reference.profile());
    eprintln!(
        "fallback at {fell_at:.2} s in {fell_in:?}: L_rt {:?}, tendency {:?}, judgeable {}, rounds {}, \
         {:.2} s, stamps {:?}/{:?}, warnings {:?}, prompts {:?}; on the new map L_rt {:?}, \
         tendency {:?}",
        p.l_rt_ms,
        p.tendency_ms,
        p.judgeable,
        run.details.rounds.len(),
        run.seconds,
        p.stamps_in,
        p.stamps_out,
        run.details.warnings,
        run.prompts,
        q.l_rt_ms,
        q.tendency_ms
    );
    let on_new_map = q.l_rt_ms.expect("the reference measures the loop");
    assert!((on_new_map - (87.3 + 20.0)).abs() <= 0.5, "{q:?}");
    assert!(
        q.judgeable && (q.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
        "{q:?}"
    );
    // What the first calibration saved must hold on that map: the same latency and the
    // player's own 18 ms, or no claim at all.
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    match p.l_rt_ms {
        Some(l_rt) => {
            assert!((l_rt - on_new_map).abs() <= 0.3, "{p:?} against {q:?}");
            assert!(
                p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
                "{p:?}"
            );
            // The offset a judge takes is the latency on the map now in force: the one
            // from before the fallback (87.3 ms) is 20 ms short on it.
            assert_eq!(p.judge_offset_ms(), Some(l_rt));
        }
        None => assert!(!p.judgeable && p.tendency_ms.is_none(), "{p:?}"),
    }
    // How: latency and rhythm were measured again, once, and the profile says on which
    // stamps it was made.
    assert!(p.l_rt_ms.is_some(), "{p:?}");
    assert_eq!(
        run.details
            .rounds
            .iter()
            .map(|x| x.kind)
            .collect::<Vec<_>>(),
        [RoundKind::First, RoundKind::First]
    );
    assert_eq!(run.details.warnings, [Warning::StampFallback]);
    assert_eq!(
        (p.stamps_in, p.stamps_out),
        (StampSource::Device, StampSource::Callback)
    );
    assert_eq!((q.stamps_in, q.stamps_out), (p.stamps_in, p.stamps_out));
    assert!(
        reference.details.warnings.is_empty(),
        "{:?}",
        reference.details.warnings
    );
    assert!(run.seconds <= 40.0, "took {:.2} s", run.seconds);
}

#[test]
fn stamp_fallback_without_a_loop_restarts_the_rhythm_step() {
    // No loop: only the rhythm step is measured again, on the map now in force.
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 0.0;
    let mut player = steady_player(&fc, 2);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let mut rng = Mulberry32::new(77);
    let mut fell = false;
    let run = calibrate_seeing(&mut r, &mut cal, Some(&mut player), |_, h, cal, events| {
        fell |= events
            .iter()
            .any(|e| matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback));
        if matches!(cal.prompt(), Prompt::StrumMuted { beat, .. } if beat >= 3) && !fell {
            let size = 0.030 + 0.040 * rng.next_f64();
            let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
            h.inject_stamp_outlier(Side::Out, sign * size);
        }
    });
    let p = run.profile();
    eprintln!(
        "fallback without a loop: combined {:?}, MAD {:?}, judgeable {}, stamps {:?}/{:?}, \
         warnings {:?}, {:.2} s",
        p.combined_ms,
        p.combined_mad_ms,
        p.judgeable,
        p.stamps_in,
        p.stamps_out,
        run.details.warnings,
        run.seconds
    );
    assert!(fell);
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(run.details.warnings, [Warning::StampFallback]);
    assert_eq!((p.l_rt_ms, p.stamps_out), (None, StampSource::Callback));
    assert_eq!(run.details.rounds.len(), 2);
    // 35 ms of path, 18 ms of player, 20 ms between the output callback and its sound:
    // every pair is on the callback map, none on the one from before (which reads 53).
    assert!(
        p.judgeable && (p.combined_ms.unwrap() - 73.0).abs() <= 3.0,
        "{p:?}"
    );
    assert_eq!(p.basis(), Basis::Estimate);
}

#[test]
fn a_clock_step_without_its_window_restarts_the_rhythm_step() {
    // An xrun in the rhythm step is a clock step. Which strums it spoiled is known from
    // the `Unjudged` window that follows it; here that window never reaches the
    // calibrator, so the step ends without a number and runs again.
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let mut done = false;
    let run = calibrate_filtered(
        &mut r,
        &mut cal,
        Some(&mut player),
        |e| !matches!(e, EngineEvent::Unjudged { .. }),
        |_, h, cal, _| {
            if !done && matches!(cal.prompt(), Prompt::StrumMuted { beat, .. } if beat >= 5) {
                done = true;
                assert!(h.inject_xrun(Side::In, 960));
            }
        },
    );
    let truth = truth_l_rt_ms(&r.h);
    let p = run.profile();
    eprintln!(
        "step without a window: L_rt {:?} (truth {truth:.3}), tendency {:?}, rounds {}, warnings {:?}, \
         {:.2} s",
        p.l_rt_ms,
        p.tendency_ms,
        run.details.rounds.len(),
        run.details.warnings,
        run.seconds
    );
    assert!(done);
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(run.details.warnings, [Warning::BeatGlitch]);
    assert_eq!(run.details.rounds.len(), 2);
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert!(
        p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
        "{p:?}"
    );
}

/// Bad output stamps until the clock map gives them up for the callback times.
fn force_output_fallback(r: &mut Rig) {
    let mut rng = Mulberry32::new(78);
    for _ in 0..(10.0 / SLICE_S) as usize {
        let size = 0.030 + 0.040 * rng.next_f64();
        let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
        r.h.inject_stamp_outlier(Side::Out, sign * size);
        r.h.pump(SLICE_S);
        r.eng.pump();
        let mut fell = false;
        while let Ok(e) = r.io.events.pop() {
            fell |= matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback);
        }
        if fell {
            return;
        }
    }
    panic!("no fallback in 10 s");
}

#[test]
fn a_second_fallback_in_the_beat_step_ends_without_a_verdict() {
    // The restart is allowed once. A second change of the stamps (here the input side,
    // during the second rhythm step) ends the calibration: no profile, no failure screen.
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config_always(&r), None);
    let mut rng = Mulberry32::new(79);
    let mut falls = 0;
    let run = calibrate_seeing(&mut r, &mut cal, Some(&mut player), |_, h, cal, events| {
        falls += events
            .iter()
            .filter(|e| matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback))
            .count();
        if matches!(cal.prompt(), Prompt::StrumMuted { beat, .. } if beat >= 3) && falls < 2 {
            let side = if falls == 0 { Side::Out } else { Side::In };
            let size = 0.030 + 0.040 * rng.next_f64();
            let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
            h.inject_stamp_outlier(side, sign * size);
        }
    });
    eprintln!(
        "two fallbacks: {:?}, {:?}, warnings {:?}, {:.2} s",
        run.outcome,
        run.prompts.last(),
        run.details.warnings,
        run.seconds
    );
    assert_eq!(falls, 2);
    assert_eq!(run.outcome, Outcome::Unstable);
    assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
    assert!(run.seconds <= 45.0, "took {:.2} s", run.seconds);
}

#[test]
fn quick_check_measures_again_when_the_stamps_changed() {
    // A latency measured on device stamps does not hold on callback stamps (and the
    // other way round): `in_of_out` differs by the time between a callback and its sound.
    let fc = room(48_000, 48_000, 87.3);
    let mut r = rig(fc.clone());
    force_output_fallback(&mut r);
    let mut player = steady_player(&fc, 3);
    let mut cal = Calibrator::new(config_always(&r), None);
    let made = calibrate(&mut r, &mut cal, Some(&mut player));
    let on_callback = made.profile().clone();
    assert_eq!(on_callback.stamps_out, StampSource::Callback);

    // Made on these stamps: three probes, kept.
    let mut cal = Calibrator::new(config(&r), Some(on_callback.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::Kept(on_callback.clone()));

    // The same numbers, but made on device stamps: not kept, whatever the probes would
    // say, and measured again.
    let other = Profile {
        stamps_out: StampSource::Device,
        ..on_callback.clone()
    };
    let mut player = steady_player(&fc, 4);
    let mut cal = Calibrator::new(config_always(&r), Some(other));
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "profile of other stamps: quick {:?}, warnings {:?}, new L_rt {:?} (made on these: {:?}), \
         {:.2} s",
        run.details.quick, run.details.warnings, p.l_rt_ms, on_callback.l_rt_ms, run.seconds
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!(!run.details.quick.clone().unwrap().kept);
    assert_eq!(run.details.warnings, [Warning::ProfileOtherStamps]);
    assert_eq!(run.details.rounds[0].kind, RoundKind::First);
    assert_eq!(p.stamps_out, StampSource::Callback);
    assert!(
        (p.l_rt_ms.unwrap() - on_callback.l_rt_ms.unwrap()).abs() <= 0.3,
        "{p:?}"
    );
    assert!(
        p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
        "{p:?}"
    );

    // Without a loop the probes cannot tell at all: the saved estimate holds the latency
    // as the device stamps saw it, and on callback stamps it is 20 ms short.
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 0.0;
    let mut r = rig(fc.clone());
    let saved = Profile {
        key: config(&r).key,
        l_rt_ms: None,
        mad_ms: None,
        tendency_ms: None,
        tendency_mad_ms: None,
        combined_ms: Some(54.2),
        combined_mad_ms: Some(6.5),
        combined_n: Some(12),
        judgeable: true,
        date: "2026-11-01".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: 0.0,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    };
    // On device stamps it is kept.
    let mut cal = Calibrator::new(config(&r), Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::Kept(saved.clone()));
    force_output_fallback(&mut r);
    let mut player = steady_player(&fc, 2);
    let mut cal = Calibrator::new(config(&r), Some(saved.clone()));
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "no loop, profile of other stamps: combined {:?} (saved {:?}), warnings {:?}, {:.2} s",
        p.combined_ms, saved.combined_ms, run.details.warnings, run.seconds
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!(run.details.warnings.contains(&Warning::ProfileOtherStamps));
    assert_eq!((p.l_rt_ms, p.stamps_out), (None, StampSource::Callback));
    // 35 ms of path, 18 ms of player, 20 ms between the output callback and its sound.
    assert!(
        p.judgeable && (p.combined_ms.unwrap() - 73.0).abs() <= 3.0,
        "{p:?}"
    );
}

#[test]
fn a3_offset_near_a_whole_beat_is_not_read_as_an_early_player() {
    // Headphones on a slow wireless link: the click reaches the ear 682 ms after its
    // stamp. With the player's 18 ms every strum comes 700 ms after "its" click, which is
    // 50 ms before the next one (80 BPM: 750 ms). Read as a player 50 ms early, every
    // strum would be judged against the wrong beat.
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 0.0;
    let mut player = steady_player(&fc, 2);
    player.heard_s = 0.682;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate_seeing(&mut r, &mut cal, Some(&mut player), |_, _, _, _| {});
    let p = run.profile();
    eprintln!(
        "A3, 700 ms: combined {:?}, MAD {:?}, judgeable {}, {:?}, {:.2} s",
        p.combined_ms,
        p.combined_mad_ms,
        p.judgeable,
        run.prompts.last(),
        run.seconds
    );
    // No failure screen, and no judgement on an offset that is not known.
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!(!p.judgeable, "{p:?}");
    assert_eq!(p.combined_ms, None, "{p:?}");
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
    // Every strum fits the click before the one it was read against.
    assert_eq!(run.details.warnings, [Warning::OffsetUnresolved]);
    let read = run
        .details
        .unresolved_offset_ms
        .expect("the reading is reported");
    assert!((read + 50.0).abs() <= 5.0, "read {read:.2} ms");
}

/// One calibration without a loop: the click is heard `heard_ms` after its stamp.
fn a3_run(heard_ms: f64, ceiling_ms: Option<f64>, prepare: impl FnOnce(&mut Player)) -> Run {
    let mut fc = room(48_000, 48_000, 35.0);
    fc.path.gain = 0.0;
    let mut player = steady_player(&fc, 2);
    player.heard_s = heard_ms / 1.0e3;
    prepare(&mut player);
    let mut r = rig(fc);
    let mut cfg = config(&r);
    if let Some(ms) = ceiling_ms {
        cfg.a3_max_offset_ms = ms;
    }
    let mut cal = Calibrator::new(cfg, None);
    calibrate_seeing(&mut r, &mut cal, Some(&mut player), |_, _, _, _| {})
}

#[test]
fn a3_offsets_are_judged_under_the_ceiling_and_when_the_first_beat_anchors_them() {
    let show = |what: &str, run: &Run| {
        let p = run.profile();
        eprintln!(
            "A3, {what}: combined {:?}, MAD {:?}, judgeable {}, reading left open {:?}, warnings {:?},              {:?}, {:.2} s",
            p.combined_ms,
            p.combined_mad_ms,
            p.judgeable,
            run.details.unresolved_offset_ms,
            run.details.warnings,
            run.prompts.last(),
            run.seconds
        );
    };
    let judged = |run: &Run, ms: f64| {
        let p = run.profile();
        assert!(
            p.judgeable && (p.combined_ms.unwrap() - ms).abs() <= 3.0,
            "{p:?}"
        );
        assert_eq!(p.judge_offset_ms(), p.combined_ms);
        assert!(
            run.details.warnings.is_empty(),
            "{:?}",
            run.details.warnings
        );
        assert_eq!(run.details.unresolved_offset_ms, None);
        assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    };
    let not_judged = |run: &Run, why: Warning, read_ms: f64| {
        let p = run.profile();
        assert!(matches!(run.outcome, Outcome::Calibrated(_)));
        assert!(
            !p.judgeable && p.combined_ms.is_none() && p.combined_mad_ms.is_none(),
            "{p:?}"
        );
        assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
        assert_eq!(run.details.warnings, [why]);
        let read = run
            .details
            .unresolved_offset_ms
            .expect("the reading is reported");
        assert!((read - read_ms).abs() <= 3.0, "read {read:.2} ms");
        assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
    };

    // A slow wireless link, 432 + 18 = 450 ms: under the ceiling of 500 ms.
    let run = a3_run(432.0, None, |_| {});
    show("450 ms", &run);
    judged(&run, 450.0);

    // 542 + 18 = 560 ms: read without doubt (the first beat has its strum), but above
    // the ceiling.
    let run = a3_run(542.0, None, |_| {});
    show("560 ms", &run);
    not_judged(&run, Warning::OffsetBeyondCeiling, 560.0);

    // The ceiling is a setting.
    let run = a3_run(542.0, Some(600.0), |_| {});
    show("560 ms, ceiling 600 ms", &run);
    judged(&run, 560.0);

    // 35 + 18 ms, and strums on the four clicks of the count-in as well: they answer no
    // scored beat under any reading, and the first scored beat still has its strum.
    let run = a3_run(35.0, None, |p| p.count_in = true);
    show("53 ms, strums on the count-in", &run);
    judged(&run, 53.0);

    // 35 + 18 ms, but the first scored beat is missed. These strums are exactly those of
    // a player who strums every beat 803 ms late: the offset is not known.
    let run = a3_run(35.0, None, |p| p.skip = 1);
    show("53 ms, late start", &run);
    not_judged(&run, Warning::OffsetUnresolved, 53.0);
}

#[test]
fn a_tiny_command_queue_still_calibrates() {
    for cmd_ring in [1, 4, 7] {
        let fc = room(48_000, 48_000, 87.3);
        let mut player = steady_player(&fc, 1);
        let ec = EngineConfig {
            cmd_ring,
            ..EngineConfig::default()
        };
        let mut r = rig_with(fc, ec);
        let mut cal = Calibrator::new(config_always(&r), None);
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let truth = truth_l_rt_ms(&r.h);
        let p = run.profile();
        let round = &run.details.rounds[0];
        eprintln!(
            "command queue of {cmd_ring}: L_rt {:?} (truth {truth:.3}), probes sent {} started {} \
             found {:?}, tendency {:?}, warnings {:?}, {:.2} s",
            p.l_rt_ms,
            round.sent,
            round.started,
            round.found,
            p.tendency_ms,
            run.details.warnings,
            run.seconds
        );
        assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
        assert_eq!(run.details.rounds.len(), 1);
        assert_eq!((round.sent, round.started, round.found), (10, 10, Some(10)));
        assert!(
            p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
            "{p:?}"
        );
        assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);
    }
}

#[test]
fn no_answer_from_the_mixer_ends_the_calibration() {
    // The driver reopened the streams and still steps with the handles from before: no
    // command can be sent and no answer will come, while the engine runs on.
    let mut r = rig(room(48_000, 48_000, 87.3));
    for _ in 0..100 {
        r.h.pump(SLICE_S);
        r.eng.pump();
    }
    let fresh = r.eng.reopen().unwrap();
    while r.io.events.pop().is_ok() {}
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    eprintln!("dead handles: {:?} after {:.2} s", run.outcome, run.seconds);
    assert_eq!(run.outcome, Outcome::Interrupted);
    assert_eq!(run.prompts.last(), Some(&Prompt::Interrupted));
    assert!(run.seconds <= 6.0, "took {:.2} s", run.seconds);
    // A handle that refuses a command with room in its queue says so at once.
    assert_eq!(run.details.warnings, [Warning::NoMixer]);
    assert!(run.seconds <= 0.1, "took {:.2} s", run.seconds);
    drop(fresh);

    // The engine runs and the commands leave, but the answers of the mixer stop coming
    // (from the start, or after 8 s, just before the strum step): 4 s of input later the
    // calibration ends.
    for quiet_from in [0.0, 8.0] {
        let mut r = rig(room(48_000, 48_000, 87.3));
        let mut cal = Calibrator::new(config_always(&r), None);
        let start = r.h.now_s();
        let mut events = Vec::new();
        loop {
            r.h.pump(SLICE_S);
            r.eng.pump();
            let t = r.h.now_s() - start;
            events.clear();
            while let Ok(e) = r.io.events.pop() {
                if t < quiet_from || !matches!(e, EngineEvent::Mix(_)) {
                    events.push(e);
                }
            }
            if cal.step(&mut r.io.mix, &mut r.io.tap, &events).done {
                break;
            }
            assert!(t < 20.0, "no state may wait for ever: {:?}", cal.prompt());
        }
        let t = r.h.now_s() - start - quiet_from;
        eprintln!(
            "no answers from {quiet_from} s: {:?} {t:.2} s later",
            cal.outcome()
        );
        assert_eq!(cal.outcome(), Some(&Outcome::Interrupted));
        assert_eq!(cal.details().warnings, [Warning::NoMixer]);
        assert!((3.5..=5.0).contains(&t), "{t:.2} s");
    }

    // Nothing runs at all (no callback, no analysis): only the steps can be counted.
    let mut r = rig(room(48_000, 48_000, 87.3));
    let mut cal = Calibrator::new(config(&r), None);
    let mut steps = 0u32;
    while !cal.step_io(&mut r.io).done {
        steps += 1;
        assert!(
            steps < 100_000,
            "no state may wait for ever: {:?}",
            cal.prompt()
        );
    }
    eprintln!("silent engine: {:?} after {steps} steps", cal.outcome());
    assert_eq!(cal.outcome(), Some(&Outcome::Interrupted));
    assert_eq!(cal.details().warnings, [Warning::NoMixer]);
    assert!(steps <= config(&r).max_silent_steps, "{steps} steps");
}

#[test]
fn a_lost_answer_at_a_slow_step_rate_is_not_a_silent_mixer() {
    // A driver at the slow end of what is allowed (one step every 100 ms) loses the
    // answers of the mixer for three steps, twice. The calibration asks again and goes on.
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let start = r.h.now_s();
    let mut events = Vec::new();
    let mut lost = 0;
    loop {
        for _ in 0..20 {
            r.h.pump(SLICE_S);
            r.eng.pump();
        }
        let t = r.h.now_s() - start;
        let deaf = (2.0..2.25).contains(&t) || (9.0..9.25).contains(&t);
        events.clear();
        while let Ok(e) = r.io.events.pop() {
            if deaf && matches!(e, EngineEvent::Mix(_)) {
                lost += 1;
            } else {
                events.push(e);
            }
        }
        let up = cal.step(&mut r.io.mix, &mut r.io.tap, &events);
        if let Some(plan) = cal.beat_plan()
            && !player.scheduled
        {
            player.schedule(&r.h, &plan);
        }
        if up.done {
            break;
        }
        assert!(t < 60.0, "the calibration did not end: {:?}", cal.prompt());
    }
    let seconds = r.h.now_s() - start;
    let truth = truth_l_rt_ms(&r.h);
    let p = cal.outcome().and_then(|o| o.profile()).cloned();
    eprintln!(
        "100 ms steps, {lost} answers lost: {:?}, warnings {:?}, {seconds:.2} s",
        p.as_ref().map(|p| (p.l_rt_ms, p.tendency_ms, p.judgeable)),
        cal.details().warnings
    );
    assert!(lost > 0);
    let p = p.expect("a profile");
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert!(
        p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
        "{p:?}"
    );
    assert!(!cal.details().warnings.contains(&Warning::NoMixer));
}

#[test]
fn profile_json_rejects_a_mistyped_or_incomplete_measurement() {
    let r = rig(room(48_000, 48_000, 87.3));
    let key = ProfileKey::new("fake", r.eng.info());
    let good = profiles_from_json(&profiles_to_json(&[Profile {
        key,
        l_rt_ms: Some(87.31),
        mad_ms: Some(0.012),
        tendency_ms: Some(18.4),
        tendency_mad_ms: Some(6.1),
        combined_ms: None,
        combined_mad_ms: None,
        combined_n: None,
        judgeable: true,
        date: "2026-11-16".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: -79.6,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    }]))
    .remove(0);
    let base = good.to_json();
    assert_eq!(Profile::from_json(&base), Some(good.clone()));
    let null = serde_json::Value::Null;
    let edit = |base: &serde_json::Value, changes: &[(&str, Option<serde_json::Value>)]| {
        let mut v = base.clone();
        for (field, value) in changes {
            match value {
                Some(value) => v[*field] = value.clone(),
                None => {
                    v.as_object_mut().unwrap().remove(*field);
                }
            }
        }
        v
    };
    let with = |field: &str, value: serde_json::Value| edit(&base, &[(field, Some(value))]);
    let without = |field: &str| edit(&base, &[(field, None)]);
    // A number written as text is not "no measurement".
    let bad = [
        ("tendency_ms as text", with("tendency_ms", "18.4".into())),
        ("l_rt_ms as text", with("l_rt_ms", "87.31".into())),
        ("mad_ms as a flag", with("mad_ms", true.into())),
        (
            "tendency_mad_ms as a list",
            with("tendency_mad_ms", serde_json::json!([6.1])),
        ),
        ("drift_ppm as text", with("drift_ppm", "-79.6".into())),
        ("clock_quality as a number", with("clock_quality", 1.into())),
        (
            "clock_quality unknown",
            with("clock_quality", "splendid".into()),
        ),
        ("date as a number", with("date", 20261116.into())),
        ("judgeable as text", with("judgeable", "true".into())),
        ("judgeable missing", without("judgeable")),
        // The latency is the basis of this profile: without it, nothing to judge with.
        (
            "judgeable without a latency",
            edit(
                &base,
                &[
                    ("l_rt_ms", Some(null.clone())),
                    ("mad_ms", Some(null.clone())),
                    ("tendency_ms", Some(null.clone())),
                    ("tendency_mad_ms", Some(null.clone())),
                ],
            ),
        ),
        ("judgeable, l_rt_ms missing", without("l_rt_ms")),
        ("latency without its MAD", with("mad_ms", null.clone())),
        ("tendency without its MAD", without("tendency_mad_ms")),
        (
            "MAD without its tendency",
            with("tendency_ms", null.clone()),
        ),
        // A tendency is what is left after the latency: there is none without one.
        (
            "tendency without a latency",
            edit(
                &base,
                &[
                    ("l_rt_ms", Some(null.clone())),
                    ("mad_ms", Some(null.clone())),
                    ("judgeable", Some(false.into())),
                ],
            ),
        ),
        // Both readings of the strums at once: which one is subtracted?
        (
            "combined next to a latency",
            edit(
                &base,
                &[
                    ("combined_ms", Some(105.7.into())),
                    ("combined_mad_ms", Some(6.1.into())),
                    ("combined_n", Some(12.into())),
                ],
            ),
        ),
        // The old sum under its old name, in an entry of the new form.
        ("fields of both versions", with("bias_ms", 18.4.into())),
        ("stamps_in as a number", with("stamps_in", 1.into())),
        (
            "stamps_out unknown",
            with("stamps_out", "wall clock".into()),
        ),
        ("stamps_out null", with("stamps_out", null.clone())),
    ];
    for (what, v) in &bad {
        assert_eq!(Profile::from_json(v), None, "{what}: {v}");
    }
    // With a loop the strums are an observation: a profile without them is whole, and
    // judged with the latency. (Version 1 refused `judgeable` without a bias, because
    // the bias was part of the offset. It no longer is.)
    let bare = edit(
        &base,
        &[
            ("tendency_ms", Some(null.clone())),
            ("tendency_mad_ms", Some(null.clone())),
        ],
    );
    for v in [
        bare.clone(),
        edit(&bare, &[("tendency_ms", None), ("tendency_mad_ms", None)]),
    ] {
        let p = Profile::from_json(&v).expect("a profile with the latency alone");
        assert_eq!((p.tendency_ms, p.judgeable), (None, true));
        assert_eq!(
            (p.basis(), p.judge_offset_ms()),
            (Basis::Loopback, Some(87.31))
        );
    }

    // Explicitly no loop (addendum A3): judged with what the strums gave, which then has
    // to be there, whole and of the right type.
    let a3 = edit(
        &base,
        &[
            ("l_rt_ms", Some(null.clone())),
            ("mad_ms", Some(null.clone())),
            ("tendency_ms", Some(null.clone())),
            ("tendency_mad_ms", Some(null.clone())),
            ("combined_ms", Some(105.7.into())),
            ("combined_mad_ms", Some(6.1.into())),
            ("combined_n", Some(14.into())),
        ],
    );
    let p = Profile::from_json(&a3).expect("a profile without a loop");
    assert_eq!(
        (p.l_rt_ms, p.combined_ms, p.combined_mad_ms, p.combined_n),
        (None, Some(105.7), Some(6.1), Some(14))
    );
    assert!(p.judgeable);
    assert_eq!(
        (p.basis(), p.judge_offset_ms()),
        (Basis::Estimate, Some(105.7))
    );
    assert_eq!(Profile::from_json(&p.to_json()), Some(p));
    let bad_a3 = [
        // A2 (5): a judgeable estimate without its number is rejected.
        (
            "judgeable estimate, combined_ms null",
            edit(
                &a3,
                &[
                    ("combined_ms", Some(null.clone())),
                    ("combined_mad_ms", Some(null.clone())),
                    ("combined_n", Some(null.clone())),
                ],
            ),
        ),
        (
            "judgeable estimate, combined fields missing",
            edit(
                &a3,
                &[
                    ("combined_ms", None),
                    ("combined_mad_ms", None),
                    ("combined_n", None),
                ],
            ),
        ),
        (
            "combined_ms as text",
            edit(&a3, &[("combined_ms", Some("105.7".into()))]),
        ),
        ("combined_ms missing", edit(&a3, &[("combined_ms", None)])),
        (
            "combined without its MAD",
            edit(&a3, &[("combined_mad_ms", Some(null.clone()))]),
        ),
        (
            "combined without its count",
            edit(&a3, &[("combined_n", None)]),
        ),
        (
            "combined_n zero",
            edit(&a3, &[("combined_n", Some(0.into()))]),
        ),
        (
            "combined_n negative",
            edit(&a3, &[("combined_n", Some((-3).into()))]),
        ),
        (
            "combined_n fractional",
            edit(&a3, &[("combined_n", Some(13.5.into()))]),
        ),
        (
            "combined_n as text",
            edit(&a3, &[("combined_n", Some("14".into()))]),
        ),
        (
            "judgeable estimate, l_rt_ms missing",
            edit(&a3, &[("l_rt_ms", None), ("mad_ms", None)]),
        ),
    ];
    for (what, v) in &bad_a3 {
        assert_eq!(Profile::from_json(v), None, "{what}: {v}");
    }
    // Not judgeable, nothing read: still a profile ("not yet").
    let nothing = edit(
        &a3,
        &[
            ("combined_ms", Some(null.clone())),
            ("combined_mad_ms", Some(null.clone())),
            ("combined_n", Some(null.clone())),
            ("judgeable", Some(false.into())),
        ],
    );
    let p = Profile::from_json(&nothing).expect("a profile without numbers");
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));

    // The stamp source: written by name; a file from before it existed means device
    // stamps (the only ones a calibration ran on then).
    assert_eq!(
        (base["stamps_in"].as_str(), base["stamps_out"].as_str()),
        (Some("device"), Some("device"))
    );
    let mut old_file = without("stamps_in");
    old_file.as_object_mut().unwrap().remove("stamps_out");
    assert_eq!(Profile::from_json(&old_file), Some(good.clone()));
    let on_callback = Profile::from_json(&with("stamps_out", "callback".into())).unwrap();
    assert_eq!(
        (on_callback.stamps_in, on_callback.stamps_out),
        (StampSource::Device, StampSource::Callback)
    );
    assert_eq!(
        on_callback.to_json()["stamps_out"].as_str(),
        Some("callback")
    );
    assert_eq!(
        Profile::from_json(&on_callback.to_json()),
        Some(on_callback)
    );
    // In a file, the bad entry is skipped and the good one stays.
    let text = format!("{{\"version\":2,\"profiles\":[{},{}]}}", bad[0].1, base);
    assert_eq!(profiles_from_json(&text), vec![good]);
}

#[test]
fn a_calibrator_built_after_a_reopen_is_not_interrupted() {
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc);
    // A first calibration is cancelled half way and nobody reads the event queue after
    // it: its last ping answer, the snapshots of epoch 0 and then the notice of the
    // reopen are all waiting when the next calibration starts on the fresh handles.
    let mut old = Calibrator::new(config(&r), None);
    for _ in 0..300 {
        r.h.pump(SLICE_S);
        r.eng.pump();
        old.step_io(&mut r.io);
    }
    old.cancel(&mut r.io.mix, &mut r.io.tap);
    for _ in 0..40 {
        r.h.pump(SLICE_S);
        r.eng.pump();
    }
    let (mix, tap) = r.eng.reopen().unwrap();
    r.io.mix = mix;
    r.io.tap = tap;
    assert_eq!(r.eng.epoch(), 1);
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    eprintln!(
        "after a reopen: {:?}, {:.2} s",
        run.outcome.profile().map(|p| (p.l_rt_ms, p.tendency_ms)),
        run.seconds
    );
    assert!(
        matches!(run.outcome, Outcome::Calibrated(_)),
        "{:?}",
        run.outcome
    );
    let truth = truth_l_rt_ms(&r.h);
    let p = run.profile();
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert!(
        p.judgeable && (p.tendency_ms.unwrap() - 18.0).abs() <= 3.0,
        "{p:?}"
    );
    assert!(run.seconds <= 25.0, "took {:.2} s", run.seconds);

    // A reopen in the middle of a calibration still ends it.
    let mut cal = Calibrator::new(config_always(&r), None);
    for _ in 0..600 {
        r.h.pump(SLICE_S);
        r.eng.pump();
        assert!(!cal.step_io(&mut r.io).done);
    }
    let (mix, tap) = r.eng.reopen().unwrap();
    r.io.mix = mix;
    r.io.tap = tap;
    let mut steps = 0;
    while !cal.step_io(&mut r.io).done {
        r.h.pump(SLICE_S);
        r.eng.pump();
        steps += 1;
        assert!(
            steps < 50,
            "a reopen under a running calibration interrupts it"
        );
    }
    assert_eq!(cal.outcome(), Some(&Outcome::Interrupted));
}

#[test]
fn device_loss_and_cancel_end_cleanly() {
    let fc = room(48_000, 48_000, 35.0);
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config(&r), None);
    let mut sent = false;
    let run = calibrate_with(&mut r, &mut cal, None, |t, h, _| {
        if !sent && t > 4.0 {
            sent = true;
            h.inject_event(BackendEvent::DeviceLost);
        }
    });
    assert_eq!(run.outcome, Outcome::Interrupted);
    assert_eq!(run.prompts.last(), Some(&Prompt::Interrupted));
    assert!(run.seconds < 4.5);

    let mut r = rig(fc);
    let mut cal = Calibrator::new(config(&r), None);
    for _ in 0..800 {
        r.h.pump(SLICE_S);
        r.eng.pump();
        cal.step_io(&mut r.io);
    }
    assert!(!cal.is_done());
    cal.cancel(&mut r.io.mix, &mut r.io.tap);
    assert_eq!(cal.outcome(), Some(&Outcome::Interrupted));
    let up = cal.step_io(&mut r.io);
    assert!(up.done && up.prompt == Prompt::Interrupted);
    // The tap is off again: nothing accumulates.
    for _ in 0..100 {
        r.h.pump(SLICE_S);
        r.eng.pump();
    }
    let mut sink = vec![0.0f32; 48_000];
    while r.io.tap.read(&mut sink) > 0 {}
    for _ in 0..100 {
        r.h.pump(SLICE_S);
        r.eng.pump();
    }
    assert_eq!(r.io.tap.available(), 0);
}

// ------------------------------------------------------------------ A2: system and player

/// Steps the engine until the clock map is locked and returns how long that took. The
/// events are thrown away, as by a game that opened the audio when it started.
fn pump_until_locked(r: &mut Rig) -> f64 {
    let start = r.h.now_s();
    loop {
        r.h.pump(SLICE_S);
        r.eng.pump();
        let mut locked = false;
        while let Ok(e) = r.io.events.pop() {
            if let EngineEvent::Clock(s) = e {
                locked = s.quality() == ClockQuality::Locked;
            }
        }
        let t = r.h.now_s() - start;
        if locked {
            return t;
        }
        assert!(t < 10.0, "the clock map did not lock");
    }
}

fn kinds(run: &Run) -> Vec<RoundKind> {
    run.details.rounds.iter().map(|x| x.kind).collect()
}

/// A2 (1), F5: the short check on the 24 rooms of T5. The player does nothing; about
/// 3.4 s after the start on a locked clock, and no more than 4 s after the lock when the
/// streams were only just opened.
#[test]
fn short_check_finds_the_latency_within_four_seconds() {
    eprintln!(
        "short  fs_in/fs_out   L(ms)  drift in/out   truth      L_rt     error    MAD   time | \
         cold start:  error   time  (lock at)"
    );
    let (mut worst, mut slowest) = (0.0f64, 0.0f64);
    let (mut worst_cold, mut slowest_cold, mut slowest_after_lock) = (0.0f64, 0.0f64, 0.0f64);
    let mut seed = 0;
    for (fs_in, fs_out) in [(48_000, 48_000), (44_100, 48_000)] {
        for latency_ms in [35.0, 87.3, 142.6] {
            for (drift_in, drift_out) in [(0.0, 0.0), (120.0, 0.0), (-80.0, 0.0), (0.0, 120.0)] {
                seed += 1;
                let mut fc = room(fs_in, fs_out, latency_ms);
                fc.drift_in_ppm = drift_in;
                fc.drift_out_ppm = drift_out;
                fc.seed = seed;

                // The game opened the audio when it started: the clock map is locked.
                let mut r = rig(fc.clone());
                pump_until_locked(&mut r);
                let mut cal = Calibrator::new(config_short(&r), None);
                let run = calibrate(&mut r, &mut cal, None);
                let truth = truth_l_rt_ms(&r.h);
                let p = run.profile();
                let l_rt = p.l_rt_ms.expect("the loop is measured");
                let err = l_rt - truth;
                worst = worst.max(err.abs());
                slowest = slowest.max(run.seconds);
                assert!(matches!(run.outcome, Outcome::Calibrated(_)));
                assert!(err.abs() <= 1.0, "L_rt {l_rt:.3} ms, truth {truth:.3} ms");
                assert!(run.seconds <= 4.0, "took {:.2} s", run.seconds);
                assert!(run.details.seconds <= 4.0, "{:.2} s", run.details.seconds);
                // One short round, every probe on time and heard.
                assert_eq!(kinds(&run), [RoundKind::Short]);
                let round = &run.details.rounds[0];
                assert_eq!((round.sent, round.started, round.late), (6, 6, 0));
                assert_eq!(round.found, Some(6));
                assert!(round.fitted && round.level_dbfs == -12.0, "{round:?}");
                assert!(
                    run.details.warnings.is_empty(),
                    "{:?}",
                    run.details.warnings
                );
                assert!((-62.0..-58.0).contains(&run.details.noise_floor_dbfs.unwrap()));
                // Nothing was asked of the player, and nothing is said about them.
                assert_eq!(
                    run.prompts,
                    [Prompt::Quiet, Prompt::WillBeep { count: 6 }, Prompt::Done]
                );
                assert_eq!(run.details.beats_played, 0);
                assert!(p.judgeable, "{p:?}");
                assert_eq!(
                    (p.basis(), p.judge_offset_ms()),
                    (Basis::Loopback, p.l_rt_ms)
                );
                assert_eq!((p.tendency_ms, p.combined_ms), (None, None));
                assert_eq!(run.details.basis, Basis::Loopback);
                assert_eq!(p.clock_quality, ClockQuality::Locked);

                // Right after the streams opened: the lock comes first.
                let mut r = rig(fc);
                let mut cal = Calibrator::new(config_short(&r), None);
                let mut locked_at = None;
                let cold = calibrate_seeing(&mut r, &mut cal, None, |t, _, _, events| {
                    for e in events {
                        if let EngineEvent::Clock(s) = e
                            && s.quality() == ClockQuality::Locked
                        {
                            locked_at.get_or_insert(t);
                        }
                    }
                });
                let locked_at = locked_at.expect("the clock map locks");
                let q = cold.profile();
                let err_cold = q.l_rt_ms.expect("the loop is measured") - truth_l_rt_ms(&r.h);
                worst_cold = worst_cold.max(err_cold.abs());
                slowest_cold = slowest_cold.max(cold.seconds);
                slowest_after_lock = slowest_after_lock.max(cold.seconds - locked_at);
                eprintln!(
                    "       {fs_in}/{fs_out}  {latency_ms:6.1}  {drift_in:+5.0}/{drift_out:+5.0}  {truth:8.3}  {l_rt:8.3}  {err:+7.3}  {:5.3}  {:5.2} |             {err_cold:+7.3}  {:5.2}  ({locked_at:.2})",
                    p.mad_ms.unwrap(),
                    run.seconds,
                    cold.seconds,
                );
                assert!(err_cold.abs() <= 1.0, "{q:?}");
                assert_eq!(kinds(&cold), [RoundKind::Short]);
                assert_eq!(cold.details.rounds[0].found, Some(6));
                assert!(q.judgeable && q.basis() == Basis::Loopback, "{q:?}");
                assert!(
                    cold.seconds - locked_at <= 4.0,
                    "{:.2} s after the lock",
                    cold.seconds - locked_at
                );
                assert!(cold.seconds <= 6.0, "took {:.2} s", cold.seconds);
            }
        }
    }
    eprintln!(
        "short  locked clock: worst |error| {worst:.3} ms (limit 1.0), slowest {slowest:.2} s \
         (limit 4.0); cold start: worst |error| {worst_cold:.3} ms, slowest {slowest_cold:.2} s, \
         {slowest_after_lock:.2} s after the lock (limit 4.0)"
    );
}

/// A2 (2), F4: with a loop the judge is given the latency of the system and nothing of
/// the player. On the semantics from before P8 this fails: the offset was
/// `L_rt + bias`, 105.3 ms here, so a player who strums 18 ms late was told "on time".
#[test]
fn with_a_loop_the_offset_is_the_latency_and_the_tendency_is_only_observed() {
    let fc = room(48_000, 48_000, 87.3);
    let mut player = steady_player(&fc, 1);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let truth = truth_l_rt_ms(&r.h);
    let p = run.profile();
    let off = p.judge_offset_ms().expect("judged with the loop");
    eprintln!(
        "P8  loop, +18 ms σ 9 ms: L_rt {:.3} ms (truth {truth:.3}), offset to judge with {off:.3} ms, \
         tendency observed {:+.2} ms (MAD {:.2})",
        p.l_rt_ms.unwrap(),
        p.tendency_ms.unwrap(),
        p.tendency_mad_ms.unwrap()
    );
    // The player's 18 ms are seen...
    assert!((p.tendency_ms.unwrap() - 18.0).abs() <= 3.0, "{p:?}");
    // ...and stay out of the offset: it is the latency, to the last bit, and the true one.
    assert_eq!(p.judge_offset_ms(), p.l_rt_ms);
    assert!(
        (off - truth).abs() <= 0.3,
        "offset {off:.3}, truth {truth:.3}"
    );
    assert_eq!(p.basis(), Basis::Loopback);
    assert!(p.judgeable);
    // Nothing of the form "latency plus player" exists in a profile made with a loop.
    assert_eq!(
        (p.combined_ms, p.combined_mad_ms, p.combined_n),
        (None, None, None)
    );
    assert_eq!(
        (run.details.basis, run.details.tendency_ms.is_some()),
        (Basis::Loopback, true)
    );
    assert_eq!(run.details.combined_ms, None);
    // The same after the file.
    let back = profiles_from_json(&profiles_to_json(std::slice::from_ref(p)));
    assert_eq!(back, std::slice::from_ref(p));
    assert_eq!(back[0].judge_offset_ms(), p.l_rt_ms);
}

/// A2 (4), F4: the two worlds of plan §2.1. (87.3 ms of system, a player 30 ms early)
/// and (57.3 ms of system, a player on the beat) put every strum at the same place in
/// the microphone signal. With the loop the calibration tells them apart. Without it
/// there is nothing to tell them apart with: both give the same estimate, and neither
/// profile says anything about the player.
#[test]
fn two_worlds_differ_with_a_loop_and_read_the_same_without() {
    let worlds = [(87.3, -30.0), (57.3, 0.0)];
    let strums = || timing_errors(16, 0.009, 5, true);

    let mut with_loop = Vec::new();
    for (latency_ms, tendency_ms) in worlds {
        let fc = room(48_000, 48_000, latency_ms);
        let mut player = Player::new(&fc, tendency_ms, strums());
        let mut r = rig(fc);
        let mut cal = Calibrator::new(config_always(&r), None);
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let truth = truth_l_rt_ms(&r.h);
        let p = run.profile().clone();
        eprintln!(
            "P8  world ({latency_ms} ms, {tendency_ms:+} ms) with the loop: offset {:?}, tendency {:?} \
             (MAD {:?})",
            p.judge_offset_ms(),
            p.tendency_ms,
            p.tendency_mad_ms
        );
        assert_eq!(p.basis(), Basis::Loopback);
        assert_eq!(p.judge_offset_ms(), p.l_rt_ms);
        assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
        assert!((truth - latency_ms).abs() < 0.01);
        assert!((p.tendency_ms.unwrap() - tendency_ms).abs() <= 3.0, "{p:?}");
        assert!(p.judgeable && p.combined_ms.is_none(), "{p:?}");
        with_loop.push(p);
    }

    let mut alone = Vec::new();
    for (latency_ms, tendency_ms) in worlds {
        // Headphones: the click reaches the ear after the same latency, and the
        // microphone never hears it.
        let mut fc = room(48_000, 48_000, latency_ms);
        fc.path.gain = 0.0;
        let mut player = Player::new(&fc, tendency_ms, strums());
        let mut r = rig(fc);
        let mut cal = Calibrator::new(config(&r), None);
        let run = calibrate(&mut r, &mut cal, Some(&mut player));
        let p = run.profile().clone();
        eprintln!(
            "P8  world ({latency_ms} ms, {tendency_ms:+} ms) without it: estimate {:?} (MAD {:?}, \
             n {:?}), tendency {:?}",
            p.combined_ms, p.combined_mad_ms, p.combined_n, p.tendency_ms
        );
        assert_eq!(p.l_rt_ms, None);
        assert_eq!(p.basis(), Basis::Estimate);
        assert_eq!(p.judge_offset_ms(), p.combined_ms);
        assert!(p.judgeable, "{p:?}");
        // No claim about the player, in the profile or in the details.
        assert_eq!((p.tendency_ms, p.tendency_mad_ms), (None, None));
        assert_eq!(
            (run.details.tendency_ms, run.details.basis),
            (None, Basis::Estimate)
        );
        assert_eq!(run.prompts.last(), Some(&Prompt::Done));
        alone.push(p);
    }
    let (a, b) = (alone[0].combined_ms.unwrap(), alone[1].combined_ms.unwrap());
    assert!((a - b).abs() <= 3.0, "{a:.2} ms against {b:.2} ms");
    for x in [a, b] {
        assert!((x - 57.3).abs() <= 3.0, "{x:.2} ms");
    }
    // With the loop, 30 ms of system and 30 ms of player sit where they belong.
    let (a, b) = (&with_loop[0], &with_loop[1]);
    assert!((a.l_rt_ms.unwrap() - b.l_rt_ms.unwrap() - 30.0).abs() <= 0.6);
    assert!((a.tendency_ms.unwrap() - b.tendency_ms.unwrap() + 30.0).abs() <= 3.0);
}

/// A2 (5): a profile file of version 1 is read, and its one `bias_ms` goes where its
/// meaning was: an observation next to a latency, the estimate where there is none.
#[test]
fn version_1_profiles_are_read_and_converted() {
    let entry = |out_id: &str, l_rt: Option<(f64, f64)>, bias: Option<(f64, f64)>, ok: bool| {
        serde_json::json!({
            "host": "fake", "in_id": "fake-in", "out_id": out_id,
            "fs_in": 48_000, "fs_out": 48_000,
            "l_rt_ms": l_rt.map(|x| x.0), "mad_ms": l_rt.map(|x| x.1),
            "bias_ms": bias.map(|x| x.0), "bias_mad_ms": bias.map(|x| x.1),
            "judgeable": ok,
            "date": "2026-11-15", "clock_quality": "locked", "drift_ppm": -79.6,
            "stamps_in": "device", "stamps_out": "callback",
        })
    };
    let looped = entry("fake-out", Some((87.31, 0.012)), Some((18.4, 6.1)), true);
    let alone = entry("headphones", None, Some((108.2, 6.5)), true);
    let file = serde_json::json!({ "version": 1, "profiles": [looped, alone] }).to_string();
    let list = profiles_from_json(&file);
    assert_eq!(list.len(), 2, "{list:?}");

    // Form 1, with a loop: the bias was the player's own.
    let p = &list[0];
    assert_eq!((p.l_rt_ms, p.mad_ms), (Some(87.31), Some(0.012)));
    assert_eq!((p.tendency_ms, p.tendency_mad_ms), (Some(18.4), Some(6.1)));
    assert_eq!(
        (p.combined_ms, p.combined_mad_ms, p.combined_n),
        (None, None, None)
    );
    assert!(p.judgeable);
    // Version 1 subtracted 87.31 + 18.4. No more.
    assert_eq!(
        (p.basis(), p.judge_offset_ms()),
        (Basis::Loopback, Some(87.31))
    );
    assert_eq!(
        (p.date.as_str(), p.clock_quality, p.drift_ppm),
        ("2026-11-15", ClockQuality::Locked, -79.6)
    );
    assert_eq!(
        (p.stamps_in, p.stamps_out),
        (StampSource::Device, StampSource::Callback)
    );
    assert_eq!(p.key.out_id, "fake-out");

    // Form 2, without a loop: the bias held latency and tendency together.
    let q = &list[1];
    assert_eq!((q.l_rt_ms, q.mad_ms), (None, None));
    assert_eq!((q.tendency_ms, q.tendency_mad_ms), (None, None));
    assert_eq!(
        (q.combined_ms, q.combined_mad_ms, q.combined_n),
        (Some(108.2), Some(6.5), Some(12))
    );
    assert!(q.judgeable);
    assert_eq!(
        (q.basis(), q.judge_offset_ms()),
        (Basis::Estimate, Some(108.2))
    );

    // Saved again, the file is of version 2 and says the same.
    let text = profiles_to_json(&list);
    assert!(
        text.contains("\"version\": 2") && !text.contains("bias"),
        "{text}"
    );
    assert_eq!(profiles_from_json(&text), list);

    // With a loop, "not yet" was a verdict on the strums (σ 25 ms, or nobody played). The
    // latency never depended on them: such a profile is judged with now, unless the
    // clock map was not sound when it was made.
    let scattered = entry("fake-out", Some((87.31, 0.012)), Some((17.9, 17.0)), false);
    let p = Profile::from_json(&scattered).expect("a version 1 profile");
    assert!(p.judgeable, "{p:?}");
    assert_eq!(
        (p.tendency_mad_ms, p.judge_offset_ms()),
        (Some(17.0), Some(87.31))
    );
    let silent = entry("fake-out", Some((87.31, 0.012)), None, false);
    let p = Profile::from_json(&silent).expect("a version 1 profile");
    assert_eq!((p.tendency_ms, p.basis()), (None, Basis::Loopback));
    for quality in ["degraded", "broken"] {
        let mut v = scattered.clone();
        v["clock_quality"] = quality.into();
        let p = Profile::from_json(&v).expect("a version 1 profile");
        assert!(!p.judgeable, "{p:?}");
        assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    }
    // Without a loop, "not yet" stays "not yet", with or without a reading.
    let unsure = entry("headphones", None, Some((108.2, 21.0)), false);
    let p = Profile::from_json(&unsure).expect("a version 1 profile");
    assert_eq!((p.combined_ms, p.judgeable), (Some(108.2), false));
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    let nothing = entry("headphones", None, None, false);
    let p = Profile::from_json(&nothing).expect("a version 1 profile");
    assert_eq!((p.combined_ms, p.basis()), (None, Basis::None));

    // The reading of version 1 was strict, and still is.
    let change = |base: &serde_json::Value, field: &str, value: Option<serde_json::Value>| {
        let mut v = base.clone();
        match value {
            Some(value) => v[field] = value,
            None => {
                v.as_object_mut().unwrap().remove(field);
            }
        }
        v
    };
    let null = serde_json::Value::Null;
    let bad = [
        (
            "bias_ms as text",
            change(&looped, "bias_ms", Some("18.4".into())),
        ),
        (
            "bias_mad_ms as a list",
            change(&looped, "bias_mad_ms", Some(serde_json::json!([6.1]))),
        ),
        (
            "judgeable without a bias",
            change(&looped, "bias_ms", Some(null.clone())),
        ),
        (
            "judgeable, bias_ms missing",
            change(&looped, "bias_ms", None),
        ),
        ("bias without its MAD", change(&looped, "bias_mad_ms", None)),
        (
            "judgeable, l_rt_ms missing",
            change(&alone, "l_rt_ms", None),
        ),
        (
            "estimate as text",
            change(&alone, "bias_ms", Some("108.2".into())),
        ),
        (
            "judgeable estimate without its number",
            change(
                &change(&alone, "bias_ms", Some(null.clone())),
                "bias_mad_ms",
                Some(null.clone()),
            ),
        ),
        (
            "fields of both versions",
            change(&alone, "combined_ms", Some(108.2.into())),
        ),
        (
            "fields of both versions, as null",
            change(&looped, "tendency_ms", Some(null.clone())),
        ),
    ];
    for (what, v) in &bad {
        assert_eq!(Profile::from_json(v), None, "{what}: {v}");
    }
    // A2 (5), in the form of version 2: a judgeable estimate without `combined_ms`.
    let mut v2 = list[1].to_json();
    assert_eq!(Profile::from_json(&v2).as_ref(), Some(&list[1]));
    v2.as_object_mut().unwrap().remove("combined_ms");
    assert_eq!(Profile::from_json(&v2), None);
    v2["combined_ms"] = null.clone();
    assert_eq!(Profile::from_json(&v2), None);
    // In a file of version 1, the bad entry is skipped and the good ones stay.
    let file = serde_json::json!({ "version": 1, "profiles": [bad[0].1, looped, alone] });
    assert_eq!(profiles_from_json(&file.to_string()), list);
}

/// A2 (6): when the strum step runs.
#[test]
fn the_strum_step_runs_only_where_the_setting_says() {
    let looped = room(48_000, 48_000, 87.3);
    let mut headphones = looped.clone();
    headphones.path.gain = 0.0;

    // The default, with a loop: the player is never asked to strum. Probes, then done.
    let mut r = rig(looped.clone());
    let cfg = config(&r);
    assert_eq!(cfg.beat_step, BeatStep::WhenNoLoop);
    let mut cal = Calibrator::new(cfg, None);
    let run = calibrate(&mut r, &mut cal, None);
    let p = run.profile();
    eprintln!(
        "beat step, loop, WhenNoLoop: {:?}, {:.2} s",
        run.prompts, run.seconds
    );
    assert_eq!(
        run.prompts,
        [Prompt::Quiet, Prompt::WillBeep { count: 10 }, Prompt::Done]
    );
    assert!(!run.prompts.iter().any(strumming));
    assert!(cal.beat_plan().is_none());
    assert_eq!((run.details.beats_played, run.details.strums_heard), (0, 0));
    assert!((p.l_rt_ms.unwrap() - truth_l_rt_ms(&r.h)).abs() <= 0.3);
    assert!(p.judgeable && p.tendency_ms.is_none(), "{p:?}");
    assert_eq!(
        (p.basis(), p.judge_offset_ms()),
        (Basis::Loopback, p.l_rt_ms)
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!(run.seconds <= 8.5, "took {:.2} s", run.seconds);
    let default_profile = p.clone();

    // `Never`, with a loop: the same calibration.
    let mut r = rig(looped.clone());
    let mut cfg = config(&r);
    cfg.beat_step = BeatStep::Never;
    let mut cal = Calibrator::new(cfg, None);
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.profile(), &default_profile);
    assert!(!run.prompts.iter().any(strumming));

    // `Always`, with a loop: the strums are asked for, and change nothing but the
    // observation (nobody strums here: there is none).
    let mut r = rig(looped);
    let mut cal = Calibrator::new(config_always(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    let p = run.profile();
    assert!(run.prompts.iter().any(strumming));
    assert_eq!(run.details.beats_played, 16);
    assert_eq!(
        (p.l_rt_ms, p.judgeable, p.tendency_ms),
        (default_profile.l_rt_ms, true, None)
    );
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));

    // `Never`, without a loop: no strums, so nothing to judge with. A profile all the
    // same, and "not yet" rather than a failure.
    let mut r = rig(headphones.clone());
    let mut cfg = config(&r);
    cfg.beat_step = BeatStep::Never;
    let mut cal = Calibrator::new(cfg, None);
    let run = calibrate(&mut r, &mut cal, None);
    let p = run.profile();
    eprintln!(
        "beat step, no loop, Never: {:?}, {:.2} s",
        run.prompts, run.seconds
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(
        run.prompts,
        [
            Prompt::Quiet,
            Prompt::WillBeep { count: 10 },
            Prompt::WillBeepLouder { count: 10 },
            Prompt::RhythmLater
        ]
    );
    assert!(cal.beat_plan().is_none());
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    assert_eq!(
        (p.l_rt_ms, p.combined_ms, p.tendency_ms, p.judgeable),
        (None, None, None, false)
    );
    assert_eq!(run.details.basis, Basis::None);
    assert!(run.details.no_loop);

    // The default, without a loop: the strums are the only offset there is, so they
    // are asked for.
    let mut player = steady_player(&headphones, 2);
    let mut r = rig(headphones);
    let mut cal = Calibrator::new(config(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    assert!(run.prompts.iter().any(strumming));
    assert_eq!(p.basis(), Basis::Estimate);
    assert!(
        (p.combined_ms.unwrap() - (87.3 + 18.0)).abs() <= 3.0,
        "{p:?}"
    );
}

/// Runs the short check with the path changed by `spoil` while the short round plays
/// (`spoil` gets the time, the start frames of the probes known so far and the handle)
/// and restored to `gain` and `latency_s` as soon as the full round is announced.
fn short_check_spoiled(
    fc: FakeConfig,
    mut spoil: impl FnMut(f64, &[u64], &FakeHandle),
) -> (Run, f64) {
    let (gain, latency_s) = (fc.path.gain, fc.path.latency_s);
    let mut r = rig(fc);
    pump_until_locked(&mut r);
    let cfg = config_short(&r);
    let tags = cfg.tag_base..cfg.tag_base + 64;
    let mut cal = Calibrator::new(cfg, None);
    let mut ats: Vec<u64> = Vec::new();
    let mut restored = false;
    let run = calibrate_seeing(&mut r, &mut cal, None, |_, h, cal, events| {
        match cal.prompt() {
            Prompt::WillBeep { count: 6 } => {
                for e in events {
                    if let EngineEvent::Mix(MixEvent::Started { tag, at }) = e
                        && tags.contains(tag)
                        && !ats.contains(at)
                    {
                        ats.push(*at);
                        ats.sort_unstable();
                    }
                }
                spoil(h.now_s(), &ats, h);
            }
            Prompt::WillBeep { .. } if !restored => {
                restored = true;
                h.set_path_gain(gain);
                h.set_latency_s(latency_s);
            }
            _ => {}
        }
    });
    let truth = truth_l_rt_ms(&r.h);
    (run, truth)
}

/// A2 (7), F5: a short round that is refused is not an ending. The full round runs and
/// reads the same latency.
#[test]
fn a_refused_short_round_goes_on_to_the_full_round() {
    let fc = room(48_000, 48_000, 87.3);
    // What the short round reads in this room when nothing disturbs it.
    let (clean, truth) = short_check_spoiled(fc.clone(), |_, _, _| {});
    assert_eq!(kinds(&clean), [RoundKind::Short]);
    let short_l_rt = clean.profile().l_rt_ms.unwrap();
    assert!((short_l_rt - truth).abs() <= 1.0);

    // Four of the six probes are heard: something covers the microphone while the last
    // two arrive.
    let (run, truth) = short_check_spoiled(fc.clone(), |now, ats, h| {
        if ats.len() == 6 {
            let fifth = h.truth().unwrap().out_time_s(ats[4] as f64);
            if now >= fifth - 0.02 {
                h.set_path_gain(0.0);
            }
        }
    });
    let p = run.profile();
    let rounds = &run.details.rounds;
    eprintln!(
        "short round, 4 of 6 heard: {:?} -> full round found {:?}, L_rt {:?} (short alone {short_l_rt:.3}, \
         truth {truth:.3}), {:.2} s",
        rounds[0].refused, rounds[1].found, p.l_rt_ms, run.seconds
    );
    assert_eq!(kinds(&run), [RoundKind::Short, RoundKind::First]);
    assert_eq!((rounds[0].sent, rounds[0].started), (6, 6));
    assert!(
        matches!(
            rounds[0].refused,
            Some(CalError::TooFewProbes {
                found: 4,
                needed: 5
            })
        ),
        "{:?}",
        rounds[0]
    );
    assert_eq!(
        (rounds[1].sent, rounds[1].found, rounds[1].level_dbfs),
        (10, Some(10), -12.0)
    );
    let l_rt = p.l_rt_ms.expect("the full round measures the loop");
    assert!(
        (l_rt - truth).abs() <= 0.3,
        "L_rt {l_rt:.3}, truth {truth:.3}"
    );
    assert!(
        (l_rt - short_l_rt).abs() <= 1.0,
        "{l_rt:.3} / {short_l_rt:.3}"
    );
    assert!(p.judgeable && p.basis() == Basis::Loopback, "{p:?}");
    // More beeps, and no word of a failure.
    assert_eq!(
        run.prompts,
        [
            Prompt::Quiet,
            Prompt::WillBeep { count: 6 },
            Prompt::WillBeep { count: 10 },
            Prompt::Done
        ]
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert!(run.seconds <= 9.5, "took {:.2} s", run.seconds);

    // The other refusal: every probe heard, but the latency moves by σ = 8 ms from one
    // to the next while the short round plays (MAD above 3 ms).
    let mut rng = Mulberry32::new(21);
    let mut next = 0.0;
    let (run, truth) = short_check_spoiled(fc, |now, _, h| {
        if now >= next {
            next = now + 0.1;
            h.set_latency_s(0.0873 + 0.008 * gauss(&mut rng));
        }
    });
    let p = run.profile();
    let rounds = &run.details.rounds;
    eprintln!(
        "short round, moving latency: {:?} -> L_rt {:?} (truth {truth:.3}), {:.2} s",
        rounds[0].refused, p.l_rt_ms, run.seconds
    );
    assert_eq!(kinds(&run), [RoundKind::Short, RoundKind::First]);
    assert!(
        matches!(rounds[0].refused, Some(CalError::UnstableLatency { .. })),
        "{:?}",
        rounds[0]
    );
    assert!((p.l_rt_ms.unwrap() - truth).abs() <= 0.3, "{p:?}");
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
}

/// The probes of the short round are 300 ms apart, so from about 210 ms of latency on
/// the window searched for a probe also holds the probe before it, which comes first. A
/// short round that took those for its own would report a latency one gap too small,
/// and every strum would then be judged against the wrong place. It must be refused
/// (the gaps differ by 20 ms, so such readings never agree), whatever the seed, and the
/// full round, whose probes are further apart, measures the loop.
#[test]
fn short_round_does_not_take_a_neighbour_probe_for_its_own() {
    for latency_ms in [200.0, 230.0, 250.0, 270.0, 300.0, 330.0] {
        for seed in [0x7061_6C6D, 1, 2, 3, 4, 5, 6, 7] {
            let mut r = rig(room(48_000, 48_000, latency_ms));
            pump_until_locked(&mut r);
            let mut cfg = config_short(&r);
            cfg.seed = seed;
            let mut cal = Calibrator::new(cfg, None);
            let run = calibrate(&mut r, &mut cal, None);
            let truth = truth_l_rt_ms(&r.h);
            let p = run.profile();
            let short = &run.details.rounds[0];
            if !(3..=7).contains(&seed) {
                eprintln!(
                    "short round at {latency_ms} ms, seed {seed}: found {:?}, {:?} -> {:?}, L_rt {:?}",
                    short.found,
                    short.refused,
                    kinds(&run),
                    p.l_rt_ms
                );
            }
            let l_rt = p.l_rt_ms.expect("one of the rounds measures the loop");
            match kinds(&run)[..] {
                // Accepted: then it is the latency (most windows held their own probe).
                [RoundKind::Short] => {
                    assert!(latency_ms < 250.0, "accepted at {latency_ms} ms");
                    assert!(
                        (l_rt - truth).abs() <= 1.0,
                        "L_rt {l_rt:.3}, truth {truth:.3}"
                    );
                }
                [RoundKind::Short, RoundKind::First] => {
                    assert!(short.refused.is_some(), "{short:?}");
                    assert!(
                        (l_rt - truth).abs() <= 0.3,
                        "L_rt {l_rt:.3}, truth {truth:.3}"
                    );
                    assert_eq!(run.details.rounds[1].found, Some(10));
                }
                ref other => panic!("{latency_ms} ms, seed {seed}: {other:?}"),
            }
            assert!(p.judgeable && p.judge_offset_ms() == p.l_rt_ms, "{p:?}");
            assert_eq!(run.prompts.last(), Some(&Prompt::Done));
        }
    }
}

/// The short check where there is no loop (headphones): each round is refused in turn
/// and the calibration ends where it always did without a loop, with the strums. And
/// with a microphone that delivers nothing it says so, sooner than the full check.
#[test]
fn short_check_without_a_loop_walks_down_to_the_strums() {
    let mut fc = room(48_000, 48_000, 60.0);
    fc.path.gain = 0.0;
    let mut player = steady_player(&fc, 2);
    let mut r = rig(fc.clone());
    let mut cal = Calibrator::new(config_short(&r), None);
    let run = calibrate(&mut r, &mut cal, Some(&mut player));
    let p = run.profile();
    eprintln!(
        "short check, no loop: {:?}, estimate {:?}, {:.2} s",
        kinds(&run),
        p.combined_ms,
        run.seconds
    );
    assert_eq!(
        kinds(&run),
        [RoundKind::Short, RoundKind::First, RoundKind::Retry]
    );
    for (round, needed) in run.details.rounds.iter().zip([5, 7, 7]) {
        assert_eq!(
            round.refused,
            Some(CalError::TooFewProbes { found: 0, needed }),
            "{round:?}"
        );
    }
    assert_eq!(
        run.prompts[..4],
        [
            Prompt::Quiet,
            Prompt::WillBeep { count: 6 },
            Prompt::WillBeep { count: 10 },
            Prompt::WillBeepLouder { count: 10 }
        ]
    );
    assert!(run.prompts.iter().any(strumming));
    assert_eq!(run.prompts.last(), Some(&Prompt::Done));
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!((p.l_rt_ms, p.tendency_ms), (None, None));
    assert_eq!(p.basis(), Basis::Estimate);
    assert!(
        p.judgeable && (p.combined_ms.unwrap() - 78.0).abs() <= 3.0,
        "{p:?}"
    );
    assert!(run.seconds <= 30.0, "took {:.2} s", run.seconds);

    // A blocked microphone.
    fc.path.noise_rms = 0.0;
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_short(&r), None);
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::MicBlocked);
    assert_eq!(run.prompts, [Prompt::Quiet, Prompt::MicBlocked]);
    assert!(run.details.rounds.is_empty());
    assert!(run.seconds <= 2.5, "took {:.2} s", run.seconds);
}

/// A saved latency that may not be judged with (the clock map was degraded when it was
/// measured) is not confirmed by three probes: it is measured again.
#[test]
fn quick_check_measures_again_a_latency_that_was_not_valid() {
    let mut r = rig(room(48_000, 48_000, 87.3));
    let mut cal = Calibrator::new(config(&r), None);
    let made = calibrate(&mut r, &mut cal, None);
    let good = made.profile().clone();
    assert!(good.judgeable);

    let doubtful = Profile {
        judgeable: false,
        clock_quality: ClockQuality::Degraded,
        ..good.clone()
    };
    assert_eq!(
        (doubtful.basis(), doubtful.judge_offset_ms()),
        (Basis::None, None)
    );
    let mut cal = Calibrator::new(config(&r), Some(doubtful));
    let run = calibrate(&mut r, &mut cal, None);
    let quick = run.details.quick.clone().unwrap();
    let p = run.profile();
    // The probes agree with the saved number, and that is not the point.
    assert!(
        !quick.kept && quick.delta_ms.unwrap().abs() <= 0.3,
        "{quick:?}"
    );
    assert!(matches!(run.outcome, Outcome::Calibrated(_)));
    assert_eq!(kinds(&run), [RoundKind::Quick, RoundKind::First]);
    assert!((p.l_rt_ms.unwrap() - truth_l_rt_ms(&r.h)).abs() <= 0.3);
    assert!(
        p.judgeable && p.clock_quality == ClockQuality::Locked,
        "{p:?}"
    );
    assert_eq!(
        run.prompts,
        [
            Prompt::Checking,
            Prompt::Quiet,
            Prompt::WillBeep { count: 10 },
            Prompt::Done
        ]
    );

    // The valid one is kept, as before, and with the short check too.
    let mut cal = Calibrator::new(config_short(&r), Some(good.clone()));
    let run = calibrate(&mut r, &mut cal, None);
    assert_eq!(run.outcome, Outcome::Kept(good));
    assert_eq!(run.prompts, [Prompt::Checking, Prompt::Done]);
    assert_eq!(run.details.basis, Basis::Loopback);
    assert!(run.seconds <= 2.0, "took {:.2} s", run.seconds);
}

/// With a loop and the default setting the calibration ends with the probe round, so
/// no later step can notice that the map changed under the probes. The output stamps go
/// bad at different moments around the round (the full one, then the short one). What
/// is saved must be right on the stamps the profile names: the latency of the map in
/// force at the end, never the one from before the fallback, which is 20 ms short on
/// it. Or no latency at all. (When a round was only discarded on a clock step, the
/// short check with the first bad stamp 1 s before it saved 127.5 ms, judgeable, read
/// off a map that had lost its lock under the probes.)
#[test]
fn stamp_fallback_around_a_probe_round_never_saves_the_latency_of_the_old_map() {
    let is_fallback =
        |e: &EngineEvent| matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback);
    let before = (StampSource::Device, StampSource::Device);
    let after = (StampSource::Device, StampSource::Callback);
    for short in [false, true] {
        let (mut on_new_map_runs, mut during_a_round) = (0, 0);
        // When the first bad stamp comes, seconds after the calibration started: from
        // 1.75 s before it to 4 s into it. (The fallback follows 2 to 5 s later.)
        for quarter in -7..=16 {
            let first_bad_s = f64::from(quarter) * 0.25;
            let lead_s = (-first_bad_s).max(0.0);
            let mut r = rig(room(48_000, 48_000, 87.3));
            pump_until_locked(&mut r);
            let mut rng = Mulberry32::new(77);
            // Every output stamp is off by 30 to 70 ms, either way: none agrees with the
            // one before, so the map rejects them all.
            let mut spoil = |h: &FakeHandle| {
                let size = 0.030 + 0.040 * rng.next_f64();
                let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
                h.inject_stamp_outlier(Side::Out, sign * size);
            };
            let mut fell = None;
            let start = r.h.now_s();
            let mut early = false;
            while r.h.now_s() - start < lead_s {
                spoil(&r.h);
                r.h.pump(SLICE_S);
                r.eng.pump();
                while let Ok(e) = r.io.events.pop() {
                    early |= is_fallback(&e);
                }
            }
            if early {
                // The map fell back before the calibration began: nothing to see.
                continue;
            }
            let cfg = if short { config_short(&r) } else { config(&r) };
            let mut cal = Calibrator::new(cfg, None);
            let mut shown = cal.prompt();
            let run = calibrate_seeing(&mut r, &mut cal, None, |t, h, cal, events| {
                if events.iter().any(is_fallback) {
                    fell.get_or_insert((t, shown));
                }
                shown = cal.prompt();
                if fell.is_none() && t >= first_bad_s {
                    spoil(h);
                }
            });
            // A fallback after the end is not the calibration's to see.
            let fell_in = fell.map(|f| f.1);
            while fell.is_none() {
                spoil(&r.h);
                r.h.pump(SLICE_S);
                r.eng.pump();
                while let Ok(e) = r.io.events.pop() {
                    if is_fallback(&e) {
                        fell = Some((r.h.now_s() - start - lead_s, Prompt::Done));
                    }
                }
                assert!(
                    r.h.now_s() - start < 60.0,
                    "the test must produce a fallback"
                );
            }

            // The reference: a calibration made wholly on the map now in force.
            let mut again = Calibrator::new(config(&r), None);
            let reference = calibrate(&mut r, &mut again, None);
            let q = reference.profile();
            let on_new_map = q.l_rt_ms.expect("the reference measures the loop");
            assert!((on_new_map - (87.3 + 20.0)).abs() <= 0.5, "{q:?}");
            assert_eq!((q.stamps_in, q.stamps_out), after);

            let saved = match &run.outcome {
                Outcome::Calibrated(p) => format!(
                    "L_rt {:?} on {:?}/{:?} stamps, judgeable {}",
                    p.l_rt_ms, p.stamps_in, p.stamps_out, p.judgeable
                ),
                other => format!("{other:?}"),
            };
            eprintln!(
                "bad stamps from {first_bad_s:+.2} s of the {} check, fallback at {:.2} s in \
                 {fell_in:?}: {saved}, rounds {:?}, warnings {:?}, {:.2} s",
                if short { "short" } else { "full" },
                fell.unwrap().0,
                kinds(&run),
                run.details.warnings,
                run.seconds
            );
            if matches!(fell_in, Some(Prompt::WillBeep { .. })) {
                during_a_round += 1;
            }
            let Outcome::Calibrated(p) = &run.outcome else {
                // Two voided rounds and then a fallback in the strum step: no profile.
                assert_eq!(run.outcome, Outcome::Unstable);
                continue;
            };
            match p.l_rt_ms {
                Some(l_rt) if (p.stamps_in, p.stamps_out) == after => {
                    assert!(fell_in.is_some());
                    assert!((l_rt - on_new_map).abs() <= 0.3, "{p:?} against {q:?}");
                    assert_eq!(p.judge_offset_ms(), Some(l_rt));
                    on_new_map_runs += 1;
                }
                // The calibration ended before the fallback: right on the stamps it
                // names, which `Profile::holds_on` then finds are not the ones in use.
                Some(l_rt) => {
                    assert_eq!((p.stamps_in, p.stamps_out), before);
                    assert_eq!(fell_in, None, "{p:?}");
                    assert!((l_rt - 87.3).abs() <= 0.3, "{p:?}");
                }
                // No latency (the repeat was spent): no claim at all.
                None => {
                    assert!(!p.judgeable && p.judge_offset_ms().is_none(), "{p:?}");
                    assert_eq!(run.prompts.last(), Some(&Prompt::RhythmLater));
                }
            }
        }
        // The sweep does put the fallback inside probe rounds, and does reach the
        // repeat on the new map.
        assert!(during_a_round >= 1, "short check: {short}");
        assert!(on_new_map_runs >= 1, "short check: {short}");
    }
}

// ------------------------------------------------------------------ review of A2

/// The short check in a room where the microphone does not hear the loudspeaker while
/// the short round plays. Each probe "arrives" when `delays_ms` says instead: for
/// probe `k`, so many ms after its emission (`None`: never). `delays_ms` is given the
/// gaps between the probes, ms. The room is an ordinary one (87.3 ms) again as soon as
/// the full round is announced. Returns the run, the gaps and the delays.
fn short_check_with_arrivals(
    mut delays_ms: impl FnMut(&[f64]) -> Vec<Option<f64>>,
) -> (Run, Vec<f64>, Vec<Option<f64>>) {
    let fc = room(48_000, 48_000, 87.3);
    let fs_out = f64::from(fc.fs_out);
    let probe: Arc<[f32]> = SoundBank::new(f64::from(fc.fs_in))
        .get(SoundId::Probe)
        .into();
    let (mut gaps, mut delays) = (Vec::new(), Vec::new());
    let (run, _) = short_check_spoiled(fc, |now, ats, h| {
        h.set_path_gain(0.0);
        if ats.len() == 6 && delays.is_empty() {
            gaps = ats
                .windows(2)
                .map(|w| (w[1] - w[0]) as f64 / fs_out * 1.0e3)
                .collect();
            delays = delays_ms(&gaps);
            assert_eq!(delays.len(), 6);
            let truth = h.truth().unwrap();
            for (at, delay) in ats.iter().zip(&delays) {
                if let Some(ms) = delay {
                    let arrival = truth.out_time_s(*at as f64) + ms / 1.0e3;
                    assert!(arrival > now + 0.02, "planned too late");
                    h.play_external(arrival, Arc::clone(&probe), 0.25)
                        .expect("probe accepted");
                }
            }
        }
    });
    assert_eq!(delays.len(), 6, "the short round was not seen");
    (run, gaps, delays)
}

/// Review of A2, finding 1: the probes are all the same sound, so a window cannot know
/// whose arrival it holds. Here every probe but the last arrives 10 ms after the *next*
/// one is emitted (a latency of 270 to 350 ms that follows the gaps). Five windows
/// then read 10 ms and agree to the microsecond, and none of the six probes had that
/// latency. When agreement was all the short round asked for, it saved 9.995 ms as
/// judgeable.
#[test]
fn short_round_is_not_fooled_by_a_latency_that_follows_the_gaps() {
    let (run, gaps, delays) = short_check_with_arrivals(|gaps| {
        let mut d: Vec<Option<f64>> = gaps.iter().map(|g| Some(g + 10.0)).collect();
        d.push(Some(300.0));
        d
    });
    let short = &run.details.rounds[0];
    eprintln!(
        "gaps {gaps:?} ms, delays {delays:?} ms: short round read {:?}, per probe {:?}, refused {:?}; \
         then {:?}, L_rt {:?}",
        short.l_rt_ms,
        short.per_probe_ms,
        short.refused,
        &kinds(&run)[1..],
        run.profile().l_rt_ms
    );
    assert_eq!(short.kind, RoundKind::Short);
    // The six were heard, five of them where another probe was expected.
    assert_eq!(short.found, Some(6));
    let read: Vec<f64> = short.per_probe_ms.iter().map(|x| x.unwrap()).collect();
    assert!((read[0] - delays[0].unwrap()).abs() <= 0.1, "{read:?}");
    for x in &read[1..] {
        assert!((x - 10.0).abs() <= 0.1, "{read:?}");
    }
    // Not one of them can be told from its neighbour: no latency from this round.
    assert_eq!(
        short.refused,
        Some(CalError::TooFewProbes {
            found: 0,
            needed: 5
        })
    );
    assert_eq!((short.l_rt_ms, short.mad_ms), (None, None));
    // The full round, in the room as it is afterwards, decides.
    assert_eq!(kinds(&run), [RoundKind::Short, RoundKind::First]);
    let p = run.profile();
    assert!((p.l_rt_ms.unwrap() - 87.3).abs() <= 0.3, "{p:?}");
    assert!(p.judgeable && p.basis() == Basis::Loopback, "{p:?}");
    assert_eq!(
        run.prompts,
        [
            Prompt::Quiet,
            Prompt::WillBeep { count: 6 },
            Prompt::WillBeep { count: 10 },
            Prompt::Done
        ]
    );

    // The same behind a first probe that is read as itself (10 ms): the second is the
    // late one, and what the windows after it hold is not theirs.
    let (run, _, _) = short_check_with_arrivals(|gaps| {
        let mut d = vec![Some(10.0)];
        d.extend(gaps[1..].iter().map(|g| Some(g + 10.0)));
        d.push(Some(300.0));
        d
    });
    let short = &run.details.rounds[0];
    assert_eq!(
        short.refused,
        Some(CalError::TooFewProbes {
            found: 1,
            needed: 5
        }),
        "{short:?}"
    );
    assert_eq!(kinds(&run), [RoundKind::Short, RoundKind::First]);
    assert!((run.profile().l_rt_ms.unwrap() - 87.3).abs() <= 0.3);
}

/// Review of A2, finding 1: delays that vary independently from probe to probe. What
/// the short round accepts is the latency its probes had; what it cannot vouch for
/// goes to the full round.
#[test]
fn short_round_with_delays_that_vary_from_probe_to_probe() {
    // A steady latency, heard by all six, or by all but the last: accepted.
    for (lost, vouched) in [(None, 6), (Some(5), 5)] {
        let (run, _, _) = short_check_with_arrivals(|_| {
            let mut d = vec![Some(60.0); 6];
            if let Some(k) = lost {
                d[k] = None;
            }
            d
        });
        let short = &run.details.rounds[0];
        assert_eq!(kinds(&run), [RoundKind::Short], "{short:?}");
        assert_eq!(short.found, Some(vouched));
        let p = run.profile();
        assert!((p.l_rt_ms.unwrap() - 60.0).abs() <= 0.1, "{p:?}");
        assert!(p.judgeable && p.judge_offset_ms() == p.l_rt_ms, "{p:?}");
    }
    // A probe lost in the middle: the three after it are heard at the same 60 ms, and
    // still cannot be vouched for (the lost one may be what their windows hold).
    let (run, _, _) = short_check_with_arrivals(|_| {
        let mut d = vec![Some(60.0); 6];
        d[2] = None;
        d
    });
    let short = &run.details.rounds[0];
    // Six windows read something: the window of the lost probe holds the next one.
    assert_eq!(short.found, Some(6));
    let lost = short.per_probe_ms[2].unwrap();
    assert!(lost > 300.0, "{:?}", short.per_probe_ms);
    assert_eq!(
        short.refused,
        Some(CalError::TooFewProbes {
            found: 2,
            needed: 5
        })
    );
    assert_eq!(kinds(&run), [RoundKind::Short, RoundKind::First]);
    assert!((run.profile().l_rt_ms.unwrap() - 87.3).abs() <= 0.3);

    // Any delays: a common latency with a little jitter, one that follows the gap
    // after the probe, anything from −40 to 450 ms, or never.
    let (mut accepted, mut refused) = (0, 0);
    for seed in 1..=40u32 {
        let mut rng = Mulberry32::new(seed);
        let (run, gaps, delays) = short_check_with_arrivals(|gaps| {
            let base = -30.0 + 220.0 * rng.next_f64();
            let wild = if seed % 4 == 0 { 0.0 } else { rng.next_f64() };
            (0..6)
                .map(|k| {
                    let next = gaps.get(k).copied().unwrap_or(300.0);
                    let jitter = 2.0 * rng.next_f64() - 1.0;
                    let style = rng.next_f64();
                    if style > wild {
                        Some(base + jitter)
                    } else if style < 0.4 * wild {
                        Some(base + next + jitter)
                    } else if style < 0.8 * wild {
                        Some(-40.0 + 490.0 * rng.next_f64())
                    } else {
                        None
                    }
                })
                .collect()
        });
        let short = &run.details.rounds[0];
        assert_eq!(short.kind, RoundKind::Short);
        match short.refused {
            None => {
                accepted += 1;
                let l_rt = short.l_rt_ms.unwrap();
                // At least five were counted, from the first on: each of them had the
                // latency that is reported (3 ms around the first, so 6 around the
                // median).
                for (k, delay) in delays[..5].iter().enumerate() {
                    let delay = delay.unwrap_or(f64::NAN);
                    assert!(
                        (delay - l_rt).abs() <= 6.1 && (delay - delays[0].unwrap()).abs() <= 3.1,
                        "seed {seed}, probe {k}: read {l_rt:.3} ms; gaps {gaps:?}, delays {delays:?}"
                    );
                }
                assert_eq!(kinds(&run), [RoundKind::Short]);
                assert_eq!(
                    run.profile().l_rt_ms.map(|x| (x - l_rt).abs() < 1.0e-3),
                    Some(true)
                );
            }
            Some(_) => {
                refused += 1;
                assert_eq!(kinds(&run)[..2], [RoundKind::Short, RoundKind::First]);
                assert!((run.profile().l_rt_ms.unwrap() - 87.3).abs() <= 0.3);
            }
        }
    }
    eprintln!(
        "delays varying from probe to probe: {accepted} short rounds accepted, {refused} refused"
    );
    assert!(
        accepted >= 8 && refused >= 8,
        "{accepted} accepted, {refused} refused"
    );
}

/// The strum step on top of a measured loop, without a player, where the clock map is
/// shown acquiring again from beat 14 on (each snapshot the calibrator gets is replaced
/// by one from the start of the run, when the map had not locked yet) until `relock`
/// says the true ones may pass again (given the seconds since the strum step ended).
/// `each` also gets the time since the strum step ended. Returns the run and the time
/// from the end of the strum step to the end.
fn lock_lost_in_the_strum_step(
    mut relock: impl FnMut(f64) -> bool,
    mut each: impl FnMut(Option<f64>, &FakeHandle, &Calibrator, &[EngineEvent]),
) -> (Run, f64) {
    let fc = room(48_000, 48_000, 87.3);
    let mut r = rig(fc);
    let mut cal = Calibrator::new(config_always(&r), None);
    let mut early: Option<tonica_audio::ClockSnapshot> = None;
    let (mut lost, mut strummed) = (false, false);
    // Shared by the two closures: when the strum step ended, and the time now.
    let ended: std::cell::Cell<Option<f64>> = std::cell::Cell::new(None);
    let now = std::cell::Cell::new(0.0);
    let run = calibrate_mapped(
        &mut r,
        &mut cal,
        None,
        |cal, e| {
            let EngineEvent::Clock(s) = e else {
                return Some(e);
            };
            if s.quality() == ClockQuality::Acquiring && early.is_none() {
                early = Some(s);
            }
            lost |= matches!(cal.prompt(), Prompt::StrumMuted { beat, .. } if beat >= 14);
            let back = ended.get().is_some_and(|t| relock(now.get() - t));
            Some(match early {
                Some(old) if lost && !back => EngineEvent::Clock(old),
                _ => e,
            })
        },
        |t, h, cal, events| {
            now.set(t);
            let strumming = strumming(&cal.prompt());
            if strummed && !strumming && ended.get().is_none() {
                ended.set(Some(t));
            }
            strummed |= strumming;
            each(ended.get().map(|e| t - e), h, cal, events);
        },
    );
    let since = run.seconds - ended.get().expect("the strum step ran");
    (run, since)
}

/// Review of A2, finding 3: with [`BeatStep::Always`] no probe round watches the clock
/// map while the player strums. A map that is acquiring again when step 5 ends is waited
/// for; one that does not lock again leaves no judgeable profile, even with a latency.
#[test]
fn a_clock_map_acquiring_at_the_end_is_not_judged_with() {
    let (run, waited) = lock_lost_in_the_strum_step(|_| false, |_, _, _, _| {});
    let p = run.profile();
    eprintln!(
        "lock lost for good: waited {waited:.2} s, judgeable {}, quality {:?}, prompts {:?}",
        p.judgeable, p.clock_quality, run.prompts
    );
    assert!(p.l_rt_ms.is_some(), "{p:?}");
    assert_eq!(p.clock_quality, ClockQuality::Acquiring);
    assert!(!p.judgeable, "{p:?}");
    assert_eq!((p.basis(), p.judge_offset_ms()), (Basis::None, None));
    assert_eq!(run.details.basis, Basis::None);
    assert!(!run.details.judgeable);
    // It waited for the lock, quietly, and ended on "later", not on a failure.
    assert!((4.9..6.0).contains(&waited), "waited {waited:.2} s");
    assert_eq!(
        run.prompts[run.prompts.len() - 2..],
        [Prompt::Quiet, Prompt::RhythmLater]
    );
    // Such a profile is measured again next time, not kept.
    let mut r = rig(room(48_000, 48_000, 87.3));
    let text = profiles_to_json(std::slice::from_ref(p));
    let saved = profiles_from_json(&text);
    assert_eq!(saved.len(), 1);
    let mut cal = Calibrator::new(config(&r), saved.into_iter().next());
    let again = calibrate(&mut r, &mut cal, None);
    let q = again.details.quick.clone().expect("the quick check ran");
    assert!(!q.kept, "{q:?}");
    assert!(again.profile().judgeable);
}

/// Review of A2, finding 3: the map locks again a second after step 5. The profile is
/// made on the locked map, and judged with.
#[test]
fn a_clock_map_that_locks_again_after_the_strum_step_is_waited_for() {
    let (run, waited) = lock_lost_in_the_strum_step(|since| since > 1.0, |_, _, _, _| {});
    let p = run.profile();
    eprintln!(
        "lock back after 1 s: waited {waited:.2} s, judgeable {}, quality {:?}, prompts {:?}",
        p.judgeable, p.clock_quality, run.prompts
    );
    assert_eq!(p.clock_quality, ClockQuality::Locked);
    assert!(p.judgeable && p.basis() == Basis::Loopback, "{p:?}");
    assert!((p.l_rt_ms.unwrap() - 87.3).abs() <= 0.3, "{p:?}");
    assert!((1.0..2.0).contains(&waited), "waited {waited:.2} s");
    assert_eq!(
        run.prompts[run.prompts.len() - 2..],
        [Prompt::Quiet, Prompt::Done]
    );
    assert!(
        run.details.warnings.is_empty(),
        "{:?}",
        run.details.warnings
    );
}

/// Review of A2, finding 3, apart from the lock: the stamps fall back while the map is
/// waited for. The latency was read on the old map: it is measured again.
#[test]
fn a_stamp_fallback_while_the_lock_is_waited_for_measures_again() {
    let mut rng = Mulberry32::new(79);
    // From the fallback on, the map in force is shown as it is.
    let fell = std::cell::Cell::new(false);
    let (run, _) = lock_lost_in_the_strum_step(
        |_| fell.get(),
        |since, h, _, events| {
            if events
                .iter()
                .any(|e| matches!(e, EngineEvent::Step(s) if s.cause == StepCause::Fallback))
            {
                fell.set(true);
            }
            if since.is_some() && !fell.get() {
                let size = 0.030 + 0.040 * rng.next_f64();
                let sign = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
                h.inject_stamp_outlier(Side::Out, sign * size);
            }
        },
    );
    assert!(fell.get(), "the test must produce a fallback");
    let p = run.profile();
    eprintln!(
        "fallback while waiting: rounds {:?}, warnings {:?}, {p:?}",
        kinds(&run),
        run.details.warnings
    );
    assert_eq!(run.details.warnings, [Warning::StampFallback]);
    assert_eq!(kinds(&run), [RoundKind::First, RoundKind::First]);
    assert_eq!(
        (p.stamps_in, p.stamps_out),
        (StampSource::Device, StampSource::Callback)
    );
}

/// Review of A2, finding 2: a file is read for numbers that are judged with. It may say
/// `judgeable` of numbers the calibrator would never judge with; such an entry is not
/// trusted, in either version, and a MAD below zero is no measurement at all.
#[test]
fn profile_json_rejects_numbers_that_contradict_judgeable() {
    let looped = Profile {
        key: ProfileKey {
            host: "fake".into(),
            in_id: "fake-in".into(),
            out_id: "fake-out".into(),
            fs_in: 48_000,
            fs_out: 48_000,
        },
        l_rt_ms: Some(87.31),
        mad_ms: Some(0.012),
        tendency_ms: Some(18.4),
        tendency_mad_ms: Some(6.1),
        combined_ms: None,
        combined_mad_ms: None,
        combined_n: None,
        judgeable: true,
        date: "2026-11-16".into(),
        clock_quality: ClockQuality::Locked,
        drift_ppm: -79.6,
        stamps_in: StampSource::Device,
        stamps_out: StampSource::Device,
    };
    let alone = Profile {
        l_rt_ms: None,
        mad_ms: None,
        tendency_ms: None,
        tendency_mad_ms: None,
        combined_ms: Some(108.0),
        combined_mad_ms: Some(6.5),
        combined_n: Some(14),
        ..looped.clone()
    };
    let (base, a3) = (looped.to_json(), alone.to_json());
    assert_eq!(Profile::from_json(&base), Some(looped.clone()));
    assert_eq!(Profile::from_json(&a3), Some(alone.clone()));
    let edit = |v: &serde_json::Value, changes: &[(&str, serde_json::Value)]| {
        let mut v = v.clone();
        for (field, value) in changes {
            v[*field] = value.clone();
        }
        v
    };
    let no = serde_json::Value::Bool(false);
    // Judgeable, and beyond what the calibrator judges with: rejected. The same numbers
    // as "not yet" are a profile, and stay "not yet".
    let beyond = [
        // The counter-example of the review: 25 ms of strums judged with.
        (
            "estimate MAD 25 ms",
            edit(&a3, &[("combined_mad_ms", 25.0.into())]),
        ),
        (
            "estimate of 11 strums",
            edit(&a3, &[("combined_n", 11.into())]),
        ),
        (
            "estimate above 500 ms",
            edit(&a3, &[("combined_ms", 520.0.into())]),
        ),
        ("loop MAD 4 ms", edit(&base, &[("mad_ms", 4.0.into())])),
        // Finding 3: only a locked map is judged with.
        (
            "loop, acquiring",
            edit(&base, &[("clock_quality", "acquiring".into())]),
        ),
        (
            "loop, degraded",
            edit(&base, &[("clock_quality", "degraded".into())]),
        ),
        (
            "estimate, broken",
            edit(&a3, &[("clock_quality", "broken".into())]),
        ),
    ];
    for (what, v) in &beyond {
        assert_eq!(Profile::from_json(v), None, "{what}: {v}");
        let p = Profile::from_json(&edit(v, &[("judgeable", no.clone())]))
            .unwrap_or_else(|| panic!("{what}, not judgeable, is a profile"));
        assert_eq!((p.judgeable, p.basis()), (false, Basis::None), "{what}");
    }
    // An absent quality reads as acquiring: not judged with either.
    let mut v = base.clone();
    v.as_object_mut().unwrap().remove("clock_quality");
    assert_eq!(Profile::from_json(&v), None);
    // At the limits: still judged with.
    for v in [
        edit(
            &a3,
            &[("combined_mad_ms", 15.0.into()), ("combined_n", 12.into())],
        ),
        edit(&a3, &[("combined_ms", 500.0.into())]),
        edit(&a3, &[("combined_ms", (-60.0).into())]),
        edit(&base, &[("mad_ms", 3.0.into())]),
        // The tendency is an observation: no limit applies to it.
        edit(&base, &[("tendency_mad_ms", 40.0.into())]),
    ] {
        let p = Profile::from_json(&v).unwrap_or_else(|| panic!("{v}"));
        assert!(p.judgeable && p.basis() != Basis::None, "{p:?}");
    }
    // A MAD below zero, judgeable or not.
    for (v, field) in [
        (&base, "mad_ms"),
        (&base, "tendency_mad_ms"),
        (&a3, "combined_mad_ms"),
    ] {
        for judgeable in [true, false] {
            let bad = edit(
                v,
                &[(field, (-0.5).into()), ("judgeable", judgeable.into())],
            );
            assert_eq!(Profile::from_json(&bad), None, "{bad}");
        }
    }

    // Version 1: a judgeable entry was judgeable in all its numbers, the strums too.
    let entry = |l_rt: Option<(f64, f64)>, bias: Option<(f64, f64)>, ok: bool, q: &str| {
        serde_json::json!({
            "host": "fake", "in_id": "fake-in", "out_id": "fake-out",
            "fs_in": 48_000, "fs_out": 48_000,
            "l_rt_ms": l_rt.map(|x| x.0), "mad_ms": l_rt.map(|x| x.1),
            "bias_ms": bias.map(|x| x.0), "bias_mad_ms": bias.map(|x| x.1),
            "judgeable": ok, "date": "2026-11-15", "clock_quality": q, "drift_ppm": 0.0,
        })
    };
    let lp = Some((87.31, 0.012));
    let v1_bad = [
        (
            "loop MAD 4 ms",
            entry(Some((87.31, 4.0)), Some((18.4, 6.1)), true, "locked"),
        ),
        (
            "strum MAD 25 ms",
            entry(lp, Some((18.4, 25.0)), true, "locked"),
        ),
        (
            "estimate MAD 25 ms",
            entry(None, Some((108.0, 25.0)), true, "locked"),
        ),
        (
            "estimate above 500 ms",
            entry(None, Some((520.0, 6.5)), true, "locked"),
        ),
        (
            "loop MAD below zero",
            entry(Some((87.31, -0.1)), None, false, "locked"),
        ),
        (
            "bias MAD below zero",
            entry(lp, Some((18.4, -6.1)), false, "locked"),
        ),
    ];
    for (what, v) in &v1_bad {
        assert_eq!(Profile::from_json(v), None, "version 1, {what}: {v}");
    }
    // The conversion promotes a "not yet" with a loop only when the loop is within its
    // limit and the map was locked (the review's case: a false profile, a loop MAD of
    // 4 ms).
    for (loop_mad, quality, judged) in [
        (0.012, "locked", true),
        (4.0, "locked", false),
        (0.012, "acquiring", false),
        (0.012, "degraded", false),
    ] {
        let v = entry(Some((87.31, loop_mad)), Some((17.9, 17.0)), false, quality);
        let p = Profile::from_json(&v).expect("a version 1 profile");
        assert_eq!(p.judgeable, judged, "MAD {loop_mad} ms, {quality}: {p:?}");
    }
    let mut v = entry(lp, Some((18.4, 6.1)), false, "locked");
    v.as_object_mut().unwrap().remove("clock_quality");
    assert!(!Profile::from_json(&v).unwrap().judgeable);
    // Without a loop, "judgeable" was written of the strums; it holds on a locked map.
    for (quality, judged) in [("locked", true), ("acquiring", false)] {
        let v = entry(None, Some((108.0, 6.5)), true, quality);
        let p = Profile::from_json(&v).expect("a version 1 profile");
        assert_eq!(p.judgeable, judged, "{quality}: {p:?}");
    }
}
