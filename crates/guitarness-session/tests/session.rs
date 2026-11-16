//! S2: the session loop on the fake rig (virtual time, no device, no microphone).
//!
//! Every test drives the real engine on the fake backend with the virtual player of the S4
//! bench: `rig.advance(5 ms)`, then `session.step()`, as the app would do on its thread.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use guitarness_audio::calib::{Basis, BeatStep};
use guitarness_audio::{
    BackendEvent, BeatPlan, CalibConfig, Calibrator, EngineConfig, EngineEvent, FakeConfig,
    MixEvent, Outcome, ProfileKey, Side,
};
use guitarness_core::chart::{Chart, Loc};
use guitarness_core::contract::{Aspect, Check};
use guitarness_core::gates::{GateState, GateTable};
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_core::stats::{NoClaim, RoundStats, Tendency, VerdictLog};
use guitarness_core::timing::{Level, TimingRef};
use guitarness_core::verdict::{
    Attack, Direction, JudgeOut, PulseVerdict, Timing, VerdictType, Why,
};
use guitarness_session::format::TapeEvent;
use guitarness_session::reader::Tape;
use guitarness_session::rig::{ChartHit, FakeRig, HitKind, HitSource, PlayerModel, TruthStrum};
use guitarness_session::session::{
    Advice, CLICK_TAG, Deferred, Notice, ROUND_TAG, Refusal, Session, SessionCmd, SessionConfig,
    SessionEvent, SessionPort, SoundOutcome, TapeNote,
};
use guitarness_session::writer::TapeSummary;

const FS: u32 = 48_000;
const SLICE_S: f64 = 0.005;
/// Ping tags of the calibrator start here; they come from a counter shared by the process.
const PING_TAG: u32 = 0xCA11_B000;

// ------------------------------------------------------------------ harness

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "guitarness-session-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn room(latency_ms: f64) -> FakeConfig {
    let mut fc = FakeConfig::new(FS, FS);
    fc.path.latency_s = latency_ms / 1.0e3;
    fc.path.noise_rms = 1.0e-3;
    fc
}

fn config() -> SessionConfig {
    SessionConfig::new("fake", "2026-11-16")
}

fn recording(root: &Path) -> SessionConfig {
    SessionConfig {
        tapes: Some(root.to_path_buf()),
        record: true,
        ..config()
    }
}

struct Bench {
    rig: FakeRig,
    s: Session,
    port: SessionPort,
    log: Vec<SessionEvent>,
}

impl Bench {
    fn new(fc: FakeConfig, cfg: SessionConfig) -> Bench {
        Bench::with_engine(fc, EngineConfig::default(), cfg)
    }

    fn with_engine(fc: FakeConfig, ec: EngineConfig, cfg: SessionConfig) -> Bench {
        let mut rig = FakeRig::with_engine(fc, ec);
        // The session owns the client side of the engine under test.
        let io = std::mem::replace(&mut rig.io, FakeRig::new(FakeConfig::new(FS, FS)).io);
        let info = rig.engine.info().clone();
        let (s, port) = Session::new(io, info, cfg);
        let mut b = Bench {
            rig,
            s,
            port,
            log: Vec::new(),
        };
        b.run(0.3);
        b
    }

    fn tick(&mut self) {
        self.rig.advance(SLICE_S);
        self.s.step();
        self.log.extend(self.port.drain());
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds / SLICE_S).round() as u32 {
            self.tick();
        }
    }

    fn run_until(&mut self, max_s: f64, mut done: impl FnMut(&Bench) -> bool) -> bool {
        for _ in 0..(max_s / SLICE_S).round() as u32 {
            if done(self) {
                return true;
            }
            self.tick();
        }
        done(self)
    }

    fn send(&self, c: SessionCmd) {
        assert!(self.port.send(c));
    }

    /// A soundcheck with a virtual player for the strum step, run to its end.
    fn soundcheck(&mut self, force: bool, model: &PlayerModel) -> (Basis, SoundOutcome) {
        let from = self.log.len();
        self.send(SessionCmd::Soundcheck { force });
        let mut scheduled = false;
        for _ in 0..(200.0 / SLICE_S) as u32 {
            self.tick();
            if !scheduled && let Some(plan) = self.s.calibration_beat_plan() {
                scheduled = true;
                let hits = calibration_hits(&plan);
                self.rig.schedule_player(plan.start, &hits, model);
            }
            if let Some(done) = self.log[from..].iter().find_map(|e| match e {
                SessionEvent::SoundcheckDone { basis, outcome } => Some((*basis, *outcome)),
                _ => None,
            }) {
                return done;
            }
        }
        panic!("the soundcheck did not end");
    }

    fn quiet_model() -> PlayerModel {
        PlayerModel::default()
    }

    /// Asks for a round and waits for its anchor; the player is scheduled by the caller.
    fn arm(&mut self, chart: &Chart, lead_in: u32) -> (usize, u64) {
        let from = self.log.len();
        self.send(SessionCmd::StartRound {
            chart: chart.clone(),
            lead_in_beats: lead_in,
            take: None,
        });
        assert!(
            self.run_until(5.0, |b| armed(&b.log[from..]).is_some()),
            "the round was not armed: {:?}",
            &self.log[from..]
        );
        let anchor = armed(&self.log[from..]).unwrap();
        (from, anchor)
    }

    /// Runs to the end of the round armed after `from`.
    fn finish(&mut self, from: usize, truth: Vec<TruthStrum>, anchor: u64) -> Run {
        assert!(
            self.run_until(120.0, |b| done(&b.log[from..]).is_some()),
            "the round did not end"
        );
        Run::new(&self.log[from..], truth, anchor)
    }

    /// A whole round: arm, schedule the virtual player, run to the end.
    fn play(
        &mut self,
        chart: &Chart,
        lead_in: u32,
        player: impl FnOnce(&mut FakeRig, u64, &Chart) -> Vec<TruthStrum>,
    ) -> Run {
        let (from, anchor) = self.arm(chart, lead_in);
        let truth = player(&mut self.rig, anchor, chart);
        self.finish(from, truth, anchor)
    }
}

fn armed(log: &[SessionEvent]) -> Option<u64> {
    log.iter().rev().find_map(|e| match e {
        SessionEvent::RoundArmed { anchor, .. } => Some(*anchor),
        _ => None,
    })
}

fn done(log: &[SessionEvent]) -> Option<&SessionEvent> {
    log.iter()
        .find(|e| matches!(e, SessionEvent::RoundDone { .. }))
}

/// The muted strums of the calibration's step 5, as hits of a chart that starts at the
/// first click.
fn calibration_hits(plan: &BeatPlan) -> Vec<ChartHit> {
    (0..plan.beats)
        .map(|i| ChartHit {
            id: i,
            t_s: (plan.strum_frame(i) - plan.start) as f64 / f64::from(FS),
            kind: HitKind::Mute,
        })
        .collect()
}

/// What one round gave.
struct Run {
    truth: Vec<TruthStrum>,
    anchor: u64,
    stats: RoundStats,
    interrupted: bool,
    outs: Vec<JudgeOut>,
    log: VerdictLog,
}

impl Run {
    fn new(events: &[SessionEvent], truth: Vec<TruthStrum>, anchor: u64) -> Run {
        let mut outs = Vec::new();
        let mut log = VerdictLog::new();
        let mut end = None;
        for e in events {
            match e {
                SessionEvent::Judge(o) => {
                    outs.push(*o);
                    log.push(o);
                }
                SessionEvent::RoundDone {
                    stats, interrupted, ..
                } => {
                    end = Some((*stats, *interrupted));
                    break;
                }
                _ => {}
            }
        }
        let (stats, interrupted) = end.expect("a round ended");
        Run {
            truth,
            anchor,
            stats,
            interrupted,
            outs,
            log,
        }
    }

    fn verdict(&self, id: u32) -> PulseVerdict {
        *self
            .log
            .verdicts()
            .iter()
            .find(|v| v.id == id)
            .unwrap_or_else(|| panic!("no verdict for event {id}"))
    }

    fn flashes(&self) -> Vec<(Option<u32>, Option<f32>)> {
        self.outs
            .iter()
            .filter_map(|o| match o {
                JudgeOut::Flash { id, dt_ms } => Some((*id, *dt_ms)),
                _ => None,
            })
            .collect()
    }

    fn count(&self, f: impl Fn(&JudgeOut) -> bool) -> usize {
        self.outs.iter().filter(|o| f(o)).count()
    }

    /// The strums that belong to an event (not the second strum of a double hit).
    fn strums(&self) -> Vec<&TruthStrum> {
        self.truth
            .iter()
            .filter(|t| t.played && !t.extra && t.id.is_some())
            .collect()
    }

    /// The dt of each verdict that has one, ms.
    fn dts(&self) -> Vec<f64> {
        self.log
            .verdicts()
            .iter()
            .filter_map(|v| v.dt_ms.map(f64::from))
            .collect()
    }
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    }
}

fn model(tendency_ms: f64, sigma_ms: f64, seed: u32) -> PlayerModel {
    PlayerModel {
        tendency_ms,
        sigma_ms,
        seed,
        ..PlayerModel::default()
    }
}

fn quarters(bpm: f64, bars: u32) -> Chart {
    exercise(Exercise::OnClickQuarters, bpm, bars)
}

/// The tape of the folder a `Tape(Started)` event named.
fn tape_dirs(log: &[SessionEvent]) -> Vec<PathBuf> {
    log.iter()
        .filter_map(|e| match e {
            SessionEvent::Tape(TapeNote::Started { dir, .. }) => Some(dir.clone()),
            _ => None,
        })
        .collect()
}

fn shut_down(b: &mut Bench) {
    b.send(SessionCmd::Shutdown);
    b.run(0.05);
    assert!(b.s.is_done());
}

// ------------------------------------------------------------------ the 12 tests of S2

/// 1. Truth per strum: L = 87.3 ms, the player at -30 ± 9 ms, 32 quarter notes at 80 BPM.
#[test]
fn s2_01_truth_per_strum() {
    let mut b = Bench::new(room(87.3), config());
    let (basis, outcome) = b.soundcheck(false, &Bench::quiet_model());
    assert_eq!(
        (basis, outcome),
        (Basis::Loopback, SoundOutcome::Calibrated)
    );
    let chart = quarters(80.0, 8);
    let m = model(-30.0, 9.0, 11);
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert!(!run.interrupted);
    assert_eq!(run.stats.events, 32);
    assert_eq!(run.stats.heard, 32, "{:?}", run.stats);
    let mut worst = 0.0f64;
    for t in run.strums() {
        let v = run.verdict(t.id.unwrap());
        assert_eq!(v.attack, Attack::Heard, "event {:?}", t.id);
        let got = f64::from(v.dt_ms.expect("a heard event has a dt"));
        let err = (got - t.dt_ms.unwrap()).abs();
        worst = worst.max(err);
        assert!(
            err <= 3.5,
            "event {:?}: {got:.2} vs {:.2}",
            t.id,
            t.dt_ms.unwrap()
        );
    }
    eprintln!("worst |dt - truth| = {worst:.2} ms");
    let med = median(&run.dts());
    assert!((med + 30.0).abs() <= 3.0, "median {med:.2}");
    assert!(
        matches!(run.stats.tendency, Tendency::Early(m) if (m + 30.0).abs() <= 3.0),
        "{:?}",
        run.stats.tendency
    );
}

/// The player's tendency as the calibrator reads it with the strum step run always, on
/// the same room and the same player model, on all `beats` strums (no early stop). The
/// virtual player draws its errors from the seed, the number of hits and their order only,
/// so a round of `beats` quarter notes at the calibrator's tempo gets the very same strums.
fn calibrator_tendency(fc: FakeConfig, m: &PlayerModel, beats: u32) -> f64 {
    let mut rig = FakeRig::new(fc);
    let key = ProfileKey::new("fake", rig.engine.info());
    let short = CalibConfig::short(key, "2026-11-16");
    assert_eq!(
        short.bpm, 80.0,
        "the round below plays at the calibrator's tempo"
    );
    let cfg = CalibConfig {
        beat_step: BeatStep::Always,
        beats,
        early_stop_pairs: beats as usize,
        ..short
    };
    let mut cal = Calibrator::new(cfg, None);
    let mut scheduled = false;
    for _ in 0..(200.0 / SLICE_S) as u32 {
        rig.advance(SLICE_S);
        cal.step_io(&mut rig.io);
        if !scheduled && let Some(plan) = cal.beat_plan() {
            scheduled = true;
            rig.schedule_player(plan.start, &calibration_hits(&plan), m);
        }
        if cal.is_done() && cal.flushed() {
            break;
        }
    }
    assert_eq!(cal.details().strums_used, beats, "every strum was used");
    match cal.outcome() {
        Some(Outcome::Calibrated(p)) => p.tendency_ms.expect("the strum step ran"),
        other => panic!("calibration: {other:?}"),
    }
}

/// 2. The judge's median agrees with the calibrator's `tendency_ms` on the same strums,
///    same count and same draws: two independent readings of one performance (<= 1 ms).
#[test]
fn s2_02_judge_agrees_with_the_calibrator() {
    let m = model(-30.0, 9.0, 11);
    let tendency = calibrator_tendency(room(87.3), &m, 16);
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let run = b.play(&quarters(80.0, 4), 4, |rig, a, c| {
        rig.schedule_player(a, c, &m)
    });
    assert_eq!(run.stats.heard, 16, "{:?}", run.stats);
    let med = median(&run.dts());
    eprintln!("calibrator {tendency:.3} ms, judge {med:.3} ms");
    assert!((med - tendency).abs() <= 1.0, "{med:.3} vs {tendency:.3}");
}

/// 3. No loop: the basis is an estimate and no tendency is claimed.
#[test]
fn s2_03_no_loop_is_an_estimate_without_tendency() {
    let mut fc = room(87.3);
    fc.path.gain = 0.0;
    let mut b = Bench::new(fc, config());
    let m = model(-30.0, 9.0, 11);
    let (basis, outcome) = b.soundcheck(false, &m);
    assert_eq!(
        (basis, outcome),
        (Basis::Estimate, SoundOutcome::Calibrated)
    );
    let run = b.play(&quarters(80.0, 4), 4, |rig, a, c| {
        rig.schedule_player(a, c, &m)
    });
    assert!(
        run.stats.timed >= 12,
        "timing is judged on an estimate: {:?}",
        run.stats
    );
    assert_eq!(
        run.stats.tendency,
        Tendency::NotClaimed(NoClaim::EstimateOnly)
    );
    assert!(
        !b.log
            .iter()
            .any(|e| matches!(e, SessionEvent::Notice(Notice::SoundcheckAdvised(_)))),
        "a verified estimate needs no advice"
    );
}

/// 4. No calibration: nothing is judged, the flashes are still there.
#[test]
fn s2_04_no_calibration_judges_nothing_and_flashes() {
    let mut b = Bench::new(room(87.3), config());
    let chart = quarters(80.0, 4);
    let m = model(-30.0, 9.0, 11);
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert!(b.log.iter().any(|e| matches!(
        e,
        SessionEvent::Notice(Notice::SoundcheckAdvised(Advice::NoProfile))
    )));
    for v in run.log.verdicts() {
        assert_eq!(v.attack, Attack::NotJudged(Why::NoCalibration), "{v:?}");
        assert_eq!(v.timing, Timing::NotJudged(Why::NoCalibration), "{v:?}");
        assert_eq!(v.dt_ms, None);
    }
    assert_eq!(run.stats.heard, 0);
    assert_eq!(run.stats.not_judged, 16);
    let flashes = run.flashes();
    assert!(flashes.len() >= 16, "{} flashes", flashes.len());
    assert!(flashes.iter().all(|(_, dt)| dt.is_none()));
}

/// The windows the engine announced, from the tape: input frames.
fn windows(tape: &Tape) -> Vec<(f64, f64)> {
    tape.events
        .iter()
        .filter_map(|(_, e)| match e {
            TapeEvent::Engine(EngineEvent::Unjudged { from_in, to_in, .. }) => {
                Some((*from_in, *to_in))
            }
            _ => None,
        })
        .collect()
}

/// An input xrun 10 s into a round of 32 quarters. Returns how many events were deep in
/// the window (all `Glitch`), how many were well outside it (none `Glitch`), and how many
/// final verdicts were withdrawn as `Glitch`.
fn xrun_round(ec: EngineConfig) -> (usize, usize, usize) {
    let tmp = Tmp::new("xrun");
    let mut b = Bench::with_engine(room(87.3), ec, recording(tmp.path()));
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 8);
    let m = model(-20.0, 6.0, 4);
    let (from, anchor) = b.arm(&chart, 4);
    let truth = b.rig.schedule_player(anchor, &chart, &m);
    let at = b.rig.now_s();
    // 10 s into the chart.
    let target = at + (anchor as f64 / f64::from(FS) - at) + 10.0;
    while b.rig.now_s() < target {
        b.tick();
    }
    assert!(b.rig.handle.inject_xrun(Side::In, 2_400));
    let run = b.finish(from, truth, anchor);
    shut_down(&mut b);
    let dir = tape_dirs(&b.log).pop().expect("a tape");
    let tape = Tape::open(&dir).unwrap();
    let ws = windows(&tape);
    assert_eq!(ws.len(), 1, "one clock step: {ws:?}");
    let (lo, hi) = ws[0];
    let margin = 0.06 * f64::from(FS);
    let inside = |x: f64| x >= lo - margin && x <= hi + margin;
    let deep = |x: f64| x >= lo + margin && x <= hi - margin;
    let outside = |x: f64| x < lo - margin || x > hi + margin;
    let (mut n_in, mut n_out) = (0, 0);
    for t in run.strums() {
        let v = run.verdict(t.id.unwrap());
        let (g, o) = (t.g_in, t.onset_in.unwrap());
        let glitch = v.attack == Attack::NotJudged(Why::Glitch);
        if deep(g) || deep(o) {
            assert!(glitch, "event {:?} is in the window: {v:?}", t.id);
            n_in += 1;
        } else if outside(g) && outside(o) && !inside(g) && !inside(o) {
            assert!(!glitch, "event {:?} is outside the window: {v:?}", t.id);
            n_out += 1;
        }
    }
    eprintln!("window {lo:.0}..{hi:.0}: {n_in} glitched, {n_out} untouched");
    let withdrawn = run.count(|o| {
        matches!(
            o,
            JudgeOut::Withdrawn {
                why: Why::Glitch,
                ..
            }
        )
    });
    (n_in, n_out, withdrawn)
}

/// Test 5: an input xrun in the middle of a round. The events in the window, and only
/// those, become `Glitch`, by withdrawal too.
#[test]
fn s2_05_xrun_mid_round_glitches_only_the_window() {
    // A long look-back, so the window reaches events that already have a final verdict.
    let (n_in, n_out, withdrawn) = xrun_round(EngineConfig {
        glitch_before_s: 1.5,
        ..EngineConfig::default()
    });
    assert!(n_in >= 1 && n_out >= 20, "{n_in} in, {n_out} out");
    assert!(withdrawn >= 1, "a final verdict was taken back");
}

/// Test 5 with the engine's default look-back (0.1 s): the events in the window are still
/// `Glitch` (asserted inside), the ones outside are not, and the window is too short to
/// reach an event that already has a final verdict: nothing is withdrawn.
#[test]
fn s2_05b_xrun_with_the_default_config_withdraws_nothing() {
    let (_, n_out, withdrawn) = xrun_round(EngineConfig::default());
    assert!(n_out >= 20, "{n_out} out");
    assert_eq!(withdrawn, 0);
}

/// 6. Three skipped strums and two doubled ones: exactly 3 `NotHeard` and 2 `Extra`.
#[test]
fn s2_06_skipped_and_doubled_strums() {
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 8);
    let m = model(-10.0, 6.0, 9);
    let run = b.play(&chart, 4, |rig, a, c| {
        let all = c.hits();
        let mut hits: Vec<ChartHit> = all
            .iter()
            .copied()
            .filter(|h| ![5, 12, 20].contains(&h.id))
            .collect();
        for (n, id) in [8usize, 25].into_iter().enumerate() {
            hits.push(ChartHit {
                id: 1000 + n as u32,
                t_s: all[id].t_s + 0.08,
                kind: HitKind::Mute,
            });
        }
        hits.sort_by(|a, b| a.t_s.partial_cmp(&b.t_s).unwrap());
        rig.schedule_player(a, &hits, &m)
    });
    let missed: Vec<u32> = run
        .log
        .verdicts()
        .iter()
        .filter(|v| v.attack == Attack::NotHeard)
        .map(|v| v.id)
        .collect();
    assert_eq!(missed, vec![5, 12, 20]);
    assert_eq!(run.log.extras(), 2);
    assert_eq!(run.count(|o| matches!(o, JudgeOut::Extra { .. })), 2);
    assert_eq!(run.stats.not_heard, 3);
    assert_eq!(run.stats.heard, 29);
    for id in [4, 6, 8, 11, 13, 19, 21, 24, 25, 26] {
        assert_eq!(run.verdict(id).attack, Attack::Heard, "neighbour {id}");
    }
}

/// 7. The click alone, no player: nothing is heard and nothing is extra.
#[test]
fn s2_07_click_only_hears_nothing() {
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let run = b.play(&quarters(80.0, 4), 4, |_, _, _| Vec::new());
    assert_eq!(run.stats.heard, 0, "{:?}", run.stats);
    assert_eq!(run.stats.not_heard, 16);
    assert_eq!(run.log.extras(), 0);
    assert_eq!(run.count(|o| matches!(o, JudgeOut::Extra { .. })), 0);
}

/// 8. I5: a soundcheck sent during a round waits for its end.
#[test]
fn s2_08_soundcheck_during_a_round_is_deferred() {
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 4);
    let m = model(-10.0, 6.0, 5);
    let (from, anchor) = b.arm(&chart, 4);
    let truth = b.rig.schedule_player(anchor, &chart, &m);
    b.run(4.0);
    b.send(SessionCmd::Soundcheck { force: true });
    let run = b.finish(from, truth, anchor);
    assert_eq!(run.stats.heard, 16, "the round was not disturbed");
    let events = &b.log[from..];
    let end = events
        .iter()
        .position(|e| matches!(e, SessionEvent::RoundDone { .. }))
        .unwrap();
    assert!(events[..end].iter().any(|e| matches!(
        e,
        SessionEvent::Notice(Notice::Deferred(Deferred::Soundcheck))
    )));
    assert!(
        !events[..end]
            .iter()
            .any(|e| matches!(e, SessionEvent::Prompt(_))),
        "no calibration prompt during the round"
    );
    // It runs after the round.
    assert!(b.run_until(60.0, |b| {
        b.log[from..]
            .iter()
            .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
    }));
    let late = &b.log[from..];
    let done_at = late
        .iter()
        .position(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
        .unwrap();
    assert!(done_at > end);
    assert!(
        late[end..done_at]
            .iter()
            .any(|e| matches!(e, SessionEvent::Prompt(_)))
    );
}

/// Mix events of a tape between two sequence numbers.
fn mix_tags(tape: &Tape) -> Vec<(u64, u32)> {
    tape.events
        .iter()
        .filter_map(|(seq, e)| match e {
            TapeEvent::Engine(EngineEvent::Mix(
                MixEvent::Started { tag, .. }
                | MixEvent::Beat { tag, .. }
                | MixEvent::Late { tag, .. }
                | MixEvent::Rejected { tag },
            )) => Some((*seq, *tag)),
            _ => None,
        })
        .collect()
}

/// `events.jsonl` with every calibrator ping tag written as `PING`. The calibrator numbers
/// its pings from a counter shared by the whole test process (`PING_TAG` and up), so two
/// runs of one scenario in one process get different numbers for the same pings; any other
/// difference is a real one. Round and click tags are fixed and stay as they are.
fn normalised(events_jsonl: &str) -> String {
    let mut out = String::new();
    for line in events_jsonl.lines() {
        let mut rest = line;
        while let Some(i) = rest.find("\"tag\":") {
            let (head, tail) = rest.split_at(i + 6);
            out.push_str(head);
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            let n: u64 = digits.parse().unwrap_or(0);
            if n >= u64::from(PING_TAG) {
                out.push_str("PING");
            } else {
                out.push_str(&digits);
            }
            rest = &tail[digits.len()..];
        }
        out.push_str(rest);
        out.push('\n');
    }
    out
}

fn scenario(root: &Path) -> String {
    let mut b = Bench::new(room(87.3), recording(root));
    b.soundcheck(false, &Bench::quiet_model());
    let m = model(-15.0, 8.0, 21);
    let _ = b.play(&quarters(80.0, 2), 4, |rig, a, c| {
        rig.schedule_player(a, c, &m)
    });
    b.send(SessionCmd::Dispute);
    b.run(0.05);
    shut_down(&mut b);
    let dir = tape_dirs(&b.log).pop().expect("a tape");
    normalised(&fs::read_to_string(dir.join("events.jsonl")).unwrap())
}

/// 9. I2: the same seed twice gives a byte-identical `events.jsonl`.
#[test]
fn s2_09_same_seed_same_tape() {
    let (a, b) = (Tmp::new("i2a"), Tmp::new("i2b"));
    let (ta, tb) = (scenario(a.path()), scenario(b.path()));
    assert!(ta.lines().count() > 100, "{} lines", ta.lines().count());
    assert!(ta.contains("PING"), "the calibration is on the tape");
    assert!(ta == tb, "the tapes differ");
}

/// 10. The device stops mid-round; `Rewire`; a new round in epoch 1.
#[test]
fn s2_10_stopped_rewire_and_a_new_epoch() {
    let tmp = Tmp::new("rewire");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 4);
    let m = model(-10.0, 6.0, 7);
    let (from, anchor) = b.arm(&chart, 4);
    let _ = b.rig.schedule_player(anchor, &chart, &m);
    let target = anchor as f64 / f64::from(FS) + 5.0;
    while b.rig.now_s() < target {
        b.tick();
    }
    b.rig.handle.inject_event(BackendEvent::DeviceLost);
    assert!(b.run_until(5.0, |b| done(&b.log[from..]).is_some()));
    let interrupted = b.log[from..].iter().any(|e| {
        matches!(
            e,
            SessionEvent::RoundDone {
                interrupted: true,
                ..
            }
        )
    });
    assert!(interrupted);
    assert!(
        b.log[from..]
            .iter()
            .any(|e| matches!(e, SessionEvent::Paused(_)))
    );
    // The old player's strums run out on the dead streams.
    b.run(12.0);
    assert!(!b.s.verified());
    let (mix, tap) = b.rig.engine.reopen().expect("the fake device comes back");
    let info = b.rig.engine.info().clone();
    b.send(SessionCmd::Rewire { mix, tap, info });
    b.run(1.0);
    assert!(b.rig.engine.epoch() >= 1);
    // The old tape is closed, a new one is open.
    let closed = b
        .log
        .iter()
        .filter(|e| matches!(e, SessionEvent::Tape(TapeNote::Closed(_))))
        .count();
    assert_eq!(closed, 1);
    assert_eq!(tape_dirs(&b.log).len(), 2);
    let (basis, _) = b.soundcheck(true, &Bench::quiet_model());
    assert_eq!(basis, Basis::Loopback);
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert!(!run.interrupted);
    assert_eq!(run.stats.heard, 16, "{:?}", run.stats);
    shut_down(&mut b);
    let dirs = tape_dirs(&b.log);
    let tape = Tape::open(&dirs[1]).unwrap();
    let epochs: Vec<u32> = tape
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            TapeEvent::Engine(EngineEvent::Onset(n)) => Some(n.epoch),
            _ => None,
        })
        .collect();
    assert!(
        !epochs.is_empty() && epochs.iter().all(|e| *e >= 1),
        "{epochs:?}"
    );
    assert!(
        tape.events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Engine(EngineEvent::Reopened { epoch: 1 })))
    );
}

/// 11. Eighths at 100 BPM, sigma 12 ms: every hit's id is the truth id.
#[test]
fn s2_11_eighths_keep_their_ids() {
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let chart = exercise(Exercise::OnClickEighths, 100.0, 4);
    let m = model(0.0, 12.0, 17);
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert_eq!(run.stats.events, 32);
    assert_eq!(run.stats.heard, 32, "{:?}", run.stats);
    for t in run.strums() {
        let id = t.id.unwrap();
        let v = run.verdict(id);
        assert_eq!(v.attack, Attack::Heard);
        let err = (f64::from(v.dt_ms.unwrap()) - t.dt_ms.unwrap()).abs();
        assert!(
            err <= 3.5,
            "event {id}: off by {err:.1} ms (a wrong pairing is 300 ms)"
        );
    }
    assert_eq!(run.log.extras(), 0);
}

/// 12. The click of event `k` sounds within one frame of `f_k`.
#[test]
fn s2_12_clicks_land_on_their_frames() {
    let tmp = Tmp::new("clicks");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 8);
    let m = model(0.0, 5.0, 3);
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    shut_down(&mut b);
    let tape = Tape::open(&tape_dirs(&b.log)[0]).unwrap();
    let mut beats: BTreeMap<u32, u64> = BTreeMap::new();
    for (_, e) in &tape.events {
        if let TapeEvent::Engine(EngineEvent::Mix(MixEvent::Beat { tag, index, at })) = e
            && *tag == ROUND_TAG
        {
            // Count-in beats and chart beats are separate metronomes; the chart's is the
            // one that starts at the anchor.
            if *at >= run.anchor {
                beats.insert(*index, *at);
            }
        }
    }
    assert_eq!(
        beats.len(),
        32,
        "every click of the chart was heard by the mixer"
    );
    let mut worst = 0u64;
    for (k, ev) in chart.events().iter().enumerate() {
        let f_k = run.anchor as f64 + (chart.seconds_of_tick(ev.tick) * f64::from(FS)).round();
        let at = beats[&(k as u32)] as f64;
        worst = worst.max((at - f_k).abs() as u64);
        assert!((at - f_k).abs() <= 1.0, "beat {k}: {at} vs {f_k}");
    }
    eprintln!("worst click offset: {worst} frames");
}

// ------------------------------------------------------------------ extra tests

fn locked_timing() -> GateTable {
    let mut g = GateTable::builtin();
    let mut e = g.entry(VerdictType::Timing).clone();
    e.state = GateState::Locked;
    g.set(VerdictType::Timing, e);
    g
}

fn assert_contract_agrees(b: &Bench, chart: &Chart, run: &Run) {
    for i in 0..chart.events().len() {
        let c = b.s.contract(chart, Loc { tier: 0, index: i });
        let v = run.verdict(i as u32);
        // Direction is never judged by the pulse judge, whatever the gate says.
        assert_eq!(
            c.get(Aspect::Direction),
            Some(Check::Guide(Why::GateLocked))
        );
        assert_eq!(v.dir, Direction::NotJudged(Why::GateLocked));
        match c.get(Aspect::Attack) {
            Some(Check::Judged(_)) => {
                assert!(
                    !matches!(v.attack, Attack::NotJudged(_)),
                    "event {i}: {v:?}"
                )
            }
            Some(Check::Guide(why)) => assert_eq!(v.attack, Attack::NotJudged(why), "event {i}"),
            None => panic!("a mute asks for an attack"),
        }
        match c.get(Aspect::Timing) {
            Some(Check::Judged(_)) => {
                assert!(
                    !matches!(v.timing, Timing::NotJudged(_)),
                    "event {i}: {v:?}"
                )
            }
            Some(Check::Guide(why)) => assert_eq!(v.timing, Timing::NotJudged(why), "event {i}"),
            None => panic!("a mute asks for timing"),
        }
    }
}

/// K2: what the contract promises is what the judge delivers (calibrated, not calibrated,
/// and with the `Timing` gate locked).
#[test]
fn contract_and_judge_agree() {
    let chart = quarters(80.0, 4);
    let m = model(-10.0, 6.0, 2);
    // Calibrated.
    let mut b = Bench::new(room(87.3), config());
    b.soundcheck(false, &Bench::quiet_model());
    let c = b.s.contract(&chart, Loc { tier: 0, index: 0 });
    assert!(c.judged(Aspect::Attack) && c.judged(Aspect::Timing));
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert_eq!(run.stats.heard, 16);
    assert_contract_agrees(&b, &chart, &run);
    // Not calibrated.
    let mut b = Bench::new(room(87.3), config());
    let c = b.s.contract(&chart, Loc { tier: 0, index: 0 });
    assert_eq!(
        c.get(Aspect::Attack),
        Some(Check::Guide(Why::NoCalibration))
    );
    assert_eq!(
        c.get(Aspect::Timing),
        Some(Check::Guide(Why::NoCalibration))
    );
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert_contract_agrees(&b, &chart, &run);
    // Calibrated, `Timing` gate locked: the attack is judged, its timing is not.
    let cfg = SessionConfig {
        gates: locked_timing(),
        ..config()
    };
    let mut b = Bench::new(room(87.3), cfg);
    b.soundcheck(false, &Bench::quiet_model());
    let c = b.s.contract(&chart, Loc { tier: 0, index: 0 });
    assert!(c.judged(Aspect::Attack));
    assert_eq!(c.get(Aspect::Timing), Some(Check::Guide(Why::GateLocked)));
    let run = b.play(&chart, 4, |rig, a, c| rig.schedule_player(a, c, &m));
    assert_eq!(run.stats.heard, 16);
    assert!(
        run.log
            .verdicts()
            .iter()
            .all(|v| v.timing == Timing::NotJudged(Why::GateLocked))
    );
    assert_contract_agrees(&b, &chart, &run);
}

/// A disputed verdict leaves the statistics, on screen and on the tape.
#[test]
fn a_dispute_leaves_the_stats_and_marks_the_tape() {
    let tmp = Tmp::new("dispute");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.soundcheck(false, &Bench::quiet_model());
    let m = model(-10.0, 6.0, 5);
    let (from, anchor) = b.arm(&quarters(80.0, 4), 4);
    let truth = b.rig.schedule_player(anchor, &quarters(80.0, 4), &m);
    let run = b.finish(from, truth, anchor);
    assert_eq!((run.stats.heard, run.stats.disputed), (16, 0));
    let mark = b.log.len();
    b.send(SessionCmd::Dispute);
    b.run(0.05);
    let after = &b.log[mark..];
    let withdrawn: Vec<u32> = after
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Judge(JudgeOut::Withdrawn {
                id,
                why: Why::Disputed,
            }) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(withdrawn, vec![15], "the last final verdict");
    let revised = after.iter().find_map(|e| match e {
        SessionEvent::RoundRevised { stats } => Some(*stats),
        _ => None,
    });
    let stats = revised.expect("the statistics were revised");
    assert_eq!((stats.disputed, stats.heard), (1, 15), "{stats:?}");
    shut_down(&mut b);
    let tape = Tape::open(&tape_dirs(&b.log)[0]).unwrap();
    assert!(tape.header.disputed);
    assert!(tape.events.iter().any(|(_, e)| matches!(
        e,
        TapeEvent::Cmd(guitarness_session::format::SessionCmdLog::Dispute)
    )));
    assert!(tape.events.iter().any(|(_, e)| matches!(
        e,
        TapeEvent::Out(JudgeOut::Withdrawn {
            why: Why::Disputed,
            ..
        })
    )));
}

/// Tapes are off unless asked for: no folder, then one when recording starts.
#[test]
fn tapes_are_off_by_default_and_on_when_asked() {
    let tmp = Tmp::new("tapes");
    let cfg = SessionConfig {
        tapes: Some(tmp.path().to_path_buf()),
        ..config()
    };
    assert!(!cfg.record);
    let mut b = Bench::new(room(87.3), cfg);
    b.soundcheck(false, &Bench::quiet_model());
    let m = model(-10.0, 6.0, 5);
    let _ = b.play(&quarters(80.0, 2), 4, |rig, a, c| {
        rig.schedule_player(a, c, &m)
    });
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "no tape, no folder"
    );
    assert!(b.s.tape_dir().is_none());
    b.send(SessionCmd::Record(true));
    b.run(0.05);
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    let _ = b.play(&quarters(80.0, 2), 4, |rig, a, c| {
        rig.schedule_player(a, c, &m)
    });
    b.send(SessionCmd::Record(false));
    b.run(0.05);
    let summary = b
        .log
        .iter()
        .find_map(|e| match e {
            SessionEvent::Tape(TapeNote::Closed(s)) => Some(s.clone()),
            _ => None,
        })
        .expect("the tape was closed");
    assert!(summary.events > 50, "{summary:?}");
    assert!(summary.mic_frames > 0, "the raw audio went to the tape");
    assert!(b.s.tape_dir().is_none());
    let tape = Tape::open(&summary.dir).unwrap();
    assert!(
        tape.events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Anchor { .. }))
    );
    assert!(
        tape.events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Round(_)))
    );
    // No click or calibration tag of this run lies in a tape that never saw it.
    assert!(mix_tags(&tape).iter().all(|(_, t)| *t < PING_TAG));
}

/// A round asked for during a soundcheck waits; a second round during a round is refused;
/// a bad chart is refused; the level is taken at the next round.
#[test]
fn rounds_wait_and_refuse() {
    let mut b = Bench::new(room(87.3), config());
    let m = model(-10.0, 6.0, 5);
    let chart = quarters(80.0, 2);
    b.send(SessionCmd::Soundcheck { force: false });
    b.run(0.05);
    b.send(SessionCmd::StartRound {
        chart: chart.clone(),
        lead_in_beats: 2,
        take: None,
    });
    b.run(0.05);
    assert!(
        b.log
            .iter()
            .any(|e| matches!(e, SessionEvent::Notice(Notice::Deferred(Deferred::Round))))
    );
    assert!(
        b.run_until(60.0, |b| armed(&b.log).is_some()),
        "the round starts after it"
    );
    let anchor = armed(&b.log).unwrap();
    let truth = b.rig.schedule_player(anchor, &chart, &m);
    b.send(SessionCmd::StartRound {
        chart: chart.clone(),
        lead_in_beats: 2,
        take: None,
    });
    b.run(0.05);
    assert!(b.log.iter().any(|e| matches!(
        e,
        SessionEvent::Notice(Notice::Refused(Refusal::RoundRunning))
    )));
    let from = b
        .log
        .iter()
        .position(|e| matches!(e, SessionEvent::RoundArmed { .. }))
        .unwrap();
    let run = b.finish(from, truth, anchor);
    assert_eq!(run.stats.heard, 8);
    let mut bad = chart.clone();
    bad.tiers[0].events.clear();
    b.send(SessionCmd::StartRound {
        chart: bad,
        lead_in_beats: 2,
        take: None,
    });
    b.run(0.05);
    assert!(
        b.log
            .iter()
            .any(|e| matches!(e, SessionEvent::Notice(Notice::Refused(Refusal::BadChart))))
    );
}

// ------------------------------------------------------------------ review fixes (S2b)

fn closed(log: &[SessionEvent]) -> Vec<TapeSummary> {
    log.iter()
        .filter_map(|e| match e {
            SessionEvent::Tape(TapeNote::Closed(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn paused(log: &[SessionEvent]) -> bool {
    log.iter().any(|e| matches!(e, SessionEvent::Paused(_)))
}

/// P1-2: every `Anchor` line carries the timing reference and the judge settings of its own
/// round; the header keeps only the latest.
#[test]
fn every_anchor_carries_its_round_reference() {
    let tmp = Tmp::new("anchors");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    let m = model(-10.0, 6.0, 5);
    let chart = quarters(80.0, 2);
    // Round 1: no calibration, apprentice level.
    let _ = b.play(&chart, 2, |rig, a, c| rig.schedule_player(a, c, &m));
    b.soundcheck(false, &Bench::quiet_model());
    b.send(SessionCmd::SetLevel(Level::Band));
    let _ = b.play(&chart, 2, |rig, a, c| rig.schedule_player(a, c, &m));
    shut_down(&mut b);
    let tape = Tape::open(&tape_dirs(&b.log)[0]).unwrap();
    let anchors: Vec<(
        Option<TimingRef>,
        Option<guitarness_core::judge::JudgeConfig>,
    )> = tape
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            TapeEvent::Anchor { timing, judge, .. } => Some((*timing, *judge)),
            _ => None,
        })
        .collect();
    assert_eq!(anchors.len(), 2);
    assert_eq!(anchors[0].0, Some(TimingRef::none()));
    let t2 = anchors[1].0.expect("round 2 has its reference");
    assert!(t2.judges_timing(), "{t2:?}");
    assert_eq!(anchors[0].1.map(|j| j.level), Some(Level::Apprentice));
    assert_eq!(anchors[1].1.map(|j| j.level), Some(Level::Band));
    assert_eq!(tape.header.timing, t2, "the header holds the latest round");
}

/// P2-3: after the device stops and comes back, a round asked for with the `Rewire` is armed
/// on the new streams' map, never on the old one.
#[test]
fn a_round_after_a_rewire_is_armed_on_the_new_map() {
    let mut b = Bench::new(room(87.3), config());
    b.run(3.0);
    b.rig.handle.inject_event(BackendEvent::DeviceLost);
    assert!(b.run_until(2.0, |b| paused(&b.log)), "the session pauses");
    b.run(1.0);
    let (mix, tap) = b.rig.engine.reopen().expect("the fake device comes back");
    let info = b.rig.engine.info().clone();
    let from = b.log.len();
    b.send(SessionCmd::Rewire { mix, tap, info });
    let chart = quarters(80.0, 2);
    b.send(SessionCmd::StartRound {
        chart,
        lead_in_beats: 2,
        take: None,
    });
    let mut out_now = None;
    assert!(b.run_until(5.0, |b| { armed(&b.log[from..]).is_some() }));
    // The output frame the new streams have reached, just after the arming.
    if let Some((_, o)) = b.rig.handle.next_frames() {
        out_now = Some(o as f64);
    }
    let anchor = armed(&b.log[from..]).unwrap() as f64;
    let out_now = out_now.expect("the new streams run");
    // anchor = last_out + 0.5 s + 2 beats of 0.75 s: within 2 s of "now" on the new map.
    let lead = (0.5 + 1.5) * f64::from(FS);
    assert!(
        (anchor - out_now - lead).abs() < 0.5 * f64::from(FS),
        "anchor {anchor} vs new output frame {out_now}"
    );
}

/// P2-4: a voluntary reopen (no `Stopped`) and its `Rewire` leave nothing behind: a later
/// device loss still pauses the session.
#[test]
fn a_stop_after_a_voluntary_reopen_still_pauses() {
    let mut b = Bench::new(room(87.3), config());
    b.run(0.5);
    let (mix, tap) = b.rig.engine.reopen().expect("reopen");
    let info = b.rig.engine.info().clone();
    b.send(SessionCmd::Rewire { mix, tap, info });
    b.run(1.0);
    assert!(!paused(&b.log), "a reopen alone is not a stop");
    b.rig.handle.inject_event(BackendEvent::DeviceLost);
    assert!(
        b.run_until(2.0, |b| paused(&b.log)),
        "the stop of the new streams pauses the session"
    );
}

/// P2-7: recording off and on again keeps the raw audio on every tape of the epoch.
#[test]
fn every_tape_of_an_epoch_has_audio() {
    let tmp = Tmp::new("reaudio");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.run(1.0);
    b.send(SessionCmd::Record(false));
    b.run(0.1);
    b.send(SessionCmd::Record(true));
    b.run(1.0);
    b.send(SessionCmd::Record(false));
    b.run(0.1);
    let started: Vec<bool> = b
        .log
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Tape(TapeNote::Started { audio, .. }) => Some(*audio),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec![true, true]);
    let sums = closed(&b.log);
    assert_eq!(sums.len(), 2);
    for s in &sums {
        assert!(s.mic_frames as f64 > 0.8 * f64::from(FS), "{s:?}");
        assert!(s.complete, "{s:?}");
    }
}

/// P2-7: a take label without a round.
#[test]
fn a_take_needs_no_round() {
    let tmp = Tmp::new("take");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.send(SessionCmd::Take("scales".into()));
    b.run(0.5);
    shut_down(&mut b);
    let tape = Tape::open(&tape_dirs(&b.log)[0]).unwrap();
    assert_eq!(tape.header.takes.len(), 1);
    assert_eq!(tape.header.takes[0].label, "scales");
    assert!(
        !tape
            .events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Anchor { .. }))
    );
}

/// P2-7: a click track with no judge: the clicks sound, nothing is judged.
#[test]
fn a_click_track_has_no_judge() {
    let tmp = Tmp::new("click");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.send(SessionCmd::Click {
        bpm: 120.0,
        beats: 8,
        accent_every: 4,
    });
    b.run(5.5);
    shut_down(&mut b);
    assert!(!b.log.iter().any(|e| matches!(
        e,
        SessionEvent::Judge(_) | SessionEvent::RoundArmed { .. } | SessionEvent::RoundDone { .. }
    )));
    let tape = Tape::open(&tape_dirs(&b.log)[0]).unwrap();
    let beats: Vec<u64> = tape
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            TapeEvent::Engine(EngineEvent::Mix(MixEvent::Beat { tag, at, .. }))
                if *tag == CLICK_TAG =>
            {
                Some(*at)
            }
            _ => None,
        })
        .collect();
    assert_eq!(beats.len(), 8, "{beats:?}");
    for w in beats.windows(2) {
        assert!(((w[1] - w[0]) as f64 - 0.5 * f64::from(FS)).abs() <= 1.0);
    }
    assert!(
        !tape
            .events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Anchor { .. } | TapeEvent::Out(_)))
    );
}

/// P3: `Record(false)` during a round closes the tape when the round ends, not on the judge
/// thread in the middle of it.
#[test]
fn record_off_mid_round_closes_at_the_round_end() {
    let tmp = Tmp::new("recoff");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.soundcheck(false, &Bench::quiet_model());
    let chart = quarters(80.0, 4);
    let m = model(-10.0, 6.0, 5);
    let (from, anchor) = b.arm(&chart, 4);
    let truth = b.rig.schedule_player(anchor, &chart, &m);
    b.run(5.0);
    b.send(SessionCmd::Record(false));
    b.run(0.1);
    assert!(closed(&b.log).is_empty(), "the tape stays open mid-round");
    let run = b.finish(from, truth, anchor);
    assert_eq!(run.stats.heard, 16);
    b.run(0.05);
    let end = b
        .log
        .iter()
        .position(|e| matches!(e, SessionEvent::RoundDone { .. }))
        .unwrap();
    let close = b
        .log
        .iter()
        .position(|e| matches!(e, SessionEvent::Tape(TapeNote::Closed(_))))
        .expect("closed after the round");
    assert!(close > end);
    let tape = Tape::open(&closed(&b.log)[0].dir).unwrap();
    // Not `complete`: the bench runs far faster than real time, so over a whole soundcheck
    // and round the raw ring (4 s) outruns the tape thread. The lines are whole.
    assert_eq!((tape.bad_lines, tape.unknown_lines), (0, 0));
    assert!(
        tape.events
            .iter()
            .any(|(_, e)| matches!(e, TapeEvent::Round(_)))
    );
    assert!(b.s.tape_dir().is_none());
}

/// P3: a forced soundcheck that finds the microphone blocked leaves nothing verified.
#[test]
fn a_failed_forced_soundcheck_unverifies() {
    let mut fc = room(87.3);
    fc.path.noise_rms = 0.0;
    let mut b = Bench::new(fc, config());
    let (basis, _) = b.soundcheck(false, &Bench::quiet_model());
    assert_eq!(basis, Basis::Loopback);
    assert!(b.s.verified());
    b.rig.handle.set_path_gain(0.0);
    let (basis, outcome) = b.soundcheck(true, &Bench::quiet_model());
    assert_eq!((basis, outcome), (Basis::None, SoundOutcome::MicBlocked));
    assert!(!b.s.verified());
    assert_eq!(b.s.next_timing(), TimingRef::none());
}

/// P3: a dispute with nothing to take back does not mark the tape.
#[test]
fn a_dispute_with_nothing_to_withdraw_leaves_the_tape_undisputed() {
    let tmp = Tmp::new("nodispute");
    let mut b = Bench::new(room(87.3), recording(tmp.path()));
    b.send(SessionCmd::Dispute);
    b.run(0.6);
    shut_down(&mut b);
    let sums = closed(&b.log);
    assert!(!sums[0].disputed, "{:?}", sums[0]);
    let tape = Tape::open(&sums[0].dir).unwrap();
    assert!(!tape.header.disputed);
    assert!(
        tape.events.iter().any(|(_, e)| matches!(
            e,
            TapeEvent::Cmd(guitarness_session::format::SessionCmdLog::Dispute)
        )),
        "the command is still on the tape"
    );
}

/// `CancelSoundcheck`: the calibration stops, nothing is saved, the verification in force
/// stays, and the round that waited for it starts at once.
#[test]
fn a_cancelled_soundcheck_keeps_the_basis_and_lets_the_round_start() {
    let tmp = Tmp::new("cancel");
    let profiles = tmp.path().join("profiles.json");
    let cfg = SessionConfig {
        profiles: Some(profiles.clone()),
        ..config()
    };
    let mut b = Bench::new(room(87.3), cfg);
    assert_eq!(
        b.soundcheck(false, &Bench::quiet_model()),
        (Basis::Loopback, SoundOutcome::Calibrated)
    );
    let saved = fs::read(&profiles).unwrap();
    let before = b.s.next_timing();
    assert!(before.judges_timing());
    // A full measurement again, and a round asked for while it runs.
    let from = b.log.len();
    b.send(SessionCmd::Soundcheck { force: true });
    b.run(1.0);
    assert!(b.s.soundcheck_active());
    b.send(SessionCmd::StartRound {
        chart: quarters(80.0, 2),
        lead_in_beats: 2,
        take: None,
    });
    b.run(0.05);
    assert!(armed(&b.log[from..]).is_none(), "the round waits (I5)");
    b.send(SessionCmd::CancelSoundcheck);
    b.tick();
    assert!(!b.s.soundcheck_active());
    assert!(b.s.round_active(), "the round starts in the same turn");
    let after = &b.log[from..];
    assert!(after.iter().any(|e| matches!(
        e,
        SessionEvent::SoundcheckCancelled {
            basis: Basis::Loopback
        }
    )));
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. })),
        "a cancel is not a result"
    );
    assert!(armed(after).is_some());
    assert!(b.s.verified());
    assert_eq!(b.s.next_timing(), before, "the basis in force is kept");
    assert_eq!(fs::read(&profiles).unwrap(), saved, "nothing was written");
    // With nothing running, a cancel says nothing.
    let mark = b.log.len();
    b.send(SessionCmd::CancelSoundcheck);
    b.run(0.05);
    assert!(
        !b.log[mark..]
            .iter()
            .any(|e| matches!(e, SessionEvent::SoundcheckCancelled { .. }))
    );
}
