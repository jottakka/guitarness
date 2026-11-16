//! S3: replay of tapes made by the bench (no device, no microphone).

use std::fs;
use std::path::{Path, PathBuf};

use guitarness_audio::{BeatPlan, EngineConfig, FakeConfig};
use guitarness_core::chart::Chart;
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_session::reader::Tape;
use guitarness_session::rig::{ChartHit, FakeRig, HitKind, PlayerModel};
use guitarness_session::session::{
    Session, SessionCmd, SessionConfig, SessionEvent, SessionPort, TapeNote,
};

const FS: u32 = 48_000;
const SLICE_S: f64 = 0.005;

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "guitarness-s3-{}-{}-{name}",
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

/// One bench round: the chart and the virtual player.
struct Play {
    chart: Chart,
    model: PlayerModel,
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

fn eighths(bpm: f64, bars: u32) -> Chart {
    exercise(Exercise::OnClickEighths, bpm, bars)
}

struct Bench {
    rig: FakeRig,
    s: Session,
    port: SessionPort,
    log: Vec<SessionEvent>,
    ticks: u64,
}

impl Bench {
    fn new(fc: FakeConfig, cfg: SessionConfig) -> Bench {
        let mut rig = FakeRig::with_engine(fc, EngineConfig::default());
        let io = std::mem::replace(&mut rig.io, FakeRig::new(FakeConfig::new(FS, FS)).io);
        let info = rig.engine.info().clone();
        let (s, port) = Session::new(io, info, cfg);
        let mut b = Bench {
            rig,
            s,
            port,
            log: Vec::new(),
            ticks: 0,
        };
        b.run(0.3);
        b
    }

    fn tick(&mut self) {
        // Give the tape thread real time to drain the raw ring (virtual time is far faster).
        self.ticks += 1;
        if self.ticks.is_multiple_of(10) {
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
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

    fn soundcheck(&mut self) {
        let from = self.log.len();
        assert!(self.port.send(SessionCmd::Soundcheck { force: false }));
        let mut scheduled = false;
        for _ in 0..(200.0 / SLICE_S) as u32 {
            self.tick();
            if !scheduled && let Some(plan) = self.s.calibration_beat_plan() {
                scheduled = true;
                let hits = calibration_hits(&plan);
                self.rig
                    .schedule_player(plan.start, &hits, &PlayerModel::default());
            }
            if self.log[from..]
                .iter()
                .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
            {
                return;
            }
        }
        panic!("the soundcheck did not end");
    }

    /// A whole round with the virtual player, its truth on the tape.
    fn round(&mut self, p: &Play) {
        let from = self.log.len();
        assert!(self.port.send(SessionCmd::StartRound {
            chart: p.chart.clone(),
            lead_in_beats: 4,
            take: None,
        }));
        assert!(self.run_until(5.0, |b| armed(&b.log[from..]).is_some()));
        let anchor = armed(&self.log[from..]).unwrap();
        let truth = self.rig.schedule_player(anchor, &p.chart, &p.model);
        self.s.tape_truth(&truth);
        assert!(
            self.run_until(200.0, |b| b.log[from..]
                .iter()
                .any(|e| matches!(e, SessionEvent::RoundDone { .. }))),
            "the round did not end"
        );
    }

    fn close(mut self) -> PathBuf {
        assert!(self.port.send(SessionCmd::Shutdown));
        self.run(0.05);
        assert!(self.s.is_done());
        self.log
            .iter()
            .find_map(|e| match e {
                SessionEvent::Tape(TapeNote::Started { dir, .. }) => Some(dir.clone()),
                _ => None,
            })
            .expect("a tape")
    }
}

fn armed(log: &[SessionEvent]) -> Option<u64> {
    log.iter().rev().find_map(|e| match e {
        SessionEvent::RoundArmed { anchor, .. } => Some(*anchor),
        _ => None,
    })
}

fn calibration_hits(plan: &BeatPlan) -> Vec<ChartHit> {
    (0..plan.beats)
        .map(|i| ChartHit {
            id: i,
            t_s: (plan.strum_frame(i) - plan.start) as f64 / f64::from(FS),
            kind: HitKind::Mute,
        })
        .collect()
}

/// Records a tape: a soundcheck, then each play in turn. `noise` is the microphone noise
/// RMS (the virtual strums have RMS 0.1).
///
/// The bench runs in virtual time, far faster than the disk thread may keep up with when the
/// machine is busy; a tape whose raw ring overflowed is not complete and is made again
/// (up to five times) because a gate counts only complete tapes.
fn record(root: &Path, latency_ms: f64, noise: f32, plays: &[Play], dispute: bool) -> Tape {
    for attempt in 0..5 {
        let dir = root.join(format!("try{attempt}"));
        fs::create_dir_all(&dir).unwrap();
        let t = record_once(&dir, latency_ms, noise, plays, dispute);
        if t.complete {
            return t;
        }
    }
    panic!("no complete tape in five tries");
}

fn record_once(root: &Path, latency_ms: f64, noise: f32, plays: &[Play], dispute: bool) -> Tape {
    let mut fc = FakeConfig::new(FS, FS);
    fc.path.latency_s = latency_ms / 1.0e3;
    fc.path.noise_rms = noise;
    let cfg = SessionConfig {
        tapes: Some(root.to_path_buf()),
        record: true,
        ..SessionConfig::new("fake", "2026-11-16")
    };
    let mut b = Bench::new(fc, cfg);
    b.soundcheck();
    for p in plays {
        b.round(p);
    }
    if dispute {
        assert!(b.port.send(SessionCmd::Dispute));
        b.run(0.05);
    }
    let dir = b.close();
    Tape::open(&dir).expect("the tape opens")
}

use std::collections::BTreeSet;

use guitarness_core::gates::GateTable;
use guitarness_core::judge::{JudgeConfig, PulseJudge};
use guitarness_core::pulse;
use guitarness_core::timing::Level;
use guitarness_core::verdict::{JudgeOut, Timing};
use guitarness_session::format::TapeEvent;
use guitarness_session::replay::{ReplayLevel, ReplayOptions, evidence, replay, replay_with};

/// Six bench tapes: tempo, tendency, spread, noise, two rounds, a dispute, misses and doubles.
fn bench_tapes(tmp: &Tmp) -> Vec<Tape> {
    let p = |c: Chart, t: f64, s: f64, seed: u32| Play {
        chart: c,
        model: model(t, s, seed),
    };
    let mut missy = model(0.0, 10.0, 6);
    missy.miss = 0.15;
    missy.double = 0.15;
    let root = |n: &str| {
        let d = tmp.path().join(n);
        fs::create_dir_all(&d).unwrap();
        d
    };
    vec![
        record(
            &root("a"),
            87.3,
            1.0e-3,
            &[p(quarters(80.0, 4), -30.0, 9.0, 1)],
            false,
        ),
        record(
            &root("b"),
            40.0,
            1.0e-3,
            &[p(eighths(100.0, 3), 0.0, 9.0, 2)],
            false,
        ),
        record(
            &root("c"),
            60.0,
            3.0e-3,
            &[p(eighths(120.0, 3), 20.0, 25.0, 3)],
            false,
        ),
        record(
            &root("d"),
            87.3,
            1.0e-3,
            &[p(quarters(100.0, 4), -15.0, 12.0, 4)],
            true,
        ),
        record(
            &root("e"),
            20.0,
            1.0e-3,
            &[
                p(quarters(80.0, 2), 5.0, 8.0, 5),
                p(eighths(100.0, 2), 5.0, 8.0, 7),
            ],
            false,
        ),
        record(
            &root("f"),
            87.3,
            1.0e-3,
            &[Play {
                chart: quarters(90.0, 4),
                model: missy,
            }],
            false,
        ),
    ]
}

/// 1. The same code gives the recorded verdicts back: zero differences on six tapes.
#[test]
fn s3_1_events_replay_is_exact() {
    let tmp = Tmp::new("events");
    let tapes = bench_tapes(&tmp);
    let gates = GateTable::builtin();
    let mut total = 0;
    for (i, t) in tapes.iter().enumerate() {
        assert!(
            t.complete,
            "tape {i} is complete: {:?} unknown {} bad {} lost runs {}",
            t.header.counts,
            t.unknown_lines,
            t.bad_lines,
            t.runs().iter().filter(|r| r.lost).count()
        );
        let r = replay(t, ReplayLevel::Events, &gates).unwrap();
        assert!(
            r.is_exact(),
            "tape {i}: {:?}",
            &r.diffs[..r.diffs.len().min(4)]
        );
        assert!(!r.rounds.is_empty());
        for round in &r.rounds {
            assert!(
                round.recorded.len() >= 8,
                "tape {i}: {}",
                round.recorded.len()
            );
            assert_eq!(round.recorded, round.replayed);
            total += round.recorded.len();
        }
    }
    assert_eq!(
        tapes[4]
            .events
            .iter()
            .filter(|(_, e)| matches!(e, TapeEvent::Anchor { .. }))
            .count(),
        2
    );
    assert!(total > 200, "{total} outputs");
    // The replay is not a copy: changing one recorded output is a difference.
    let mut t = Tape::open(tapes[0].dir()).unwrap();
    let i = t
        .events
        .iter()
        .position(|(_, e)| matches!(e, TapeEvent::Out(JudgeOut::Verdict(v)) if v.fin))
        .unwrap();
    if let TapeEvent::Out(JudgeOut::Verdict(v)) = &mut t.events[i].1 {
        v.timing = Timing::Outside;
        v.dt_ms = Some(500.0);
    }
    let r = replay(&t, ReplayLevel::Events, &gates).unwrap();
    assert_eq!(r.diffs.len(), 1, "{:?}", r.diffs);
}

/// 2. The microphone through the detector: the same onsets and the same verdicts.
#[test]
fn s3_2_audio_replay_is_exact() {
    let tmp = Tmp::new("audio");
    let tapes = bench_tapes(&tmp);
    let gates = GateTable::builtin();
    for (i, t) in tapes.iter().enumerate() {
        let r = replay(t, ReplayLevel::Audio, &gates).unwrap();
        let o = r.onsets.as_ref().unwrap();
        assert!(o.recorded > 20, "tape {i}: {} onsets", o.recorded);
        assert!(
            o.is_exact(),
            "tape {i}: missing {:?} extra {:?}",
            o.missing,
            o.extra
        );
        assert_eq!(o.matched, o.recorded);
        assert!(
            r.is_exact(),
            "tape {i}: {:?}",
            &r.diffs[..r.diffs.len().min(4)]
        );
    }
}

/// 3. A judge with another window differs exactly at the events on the new boundary.
#[test]
fn s3_3_a_changed_window_lists_the_boundary_events() {
    let tmp = Tmp::new("window");
    let d = tmp.path().join("w");
    fs::create_dir_all(&d).unwrap();
    let play = Play {
        chart: quarters(80.0, 6),
        model: model(-30.0, 40.0, 9),
    };
    let tape = record(&d, 87.3, 1.0e-3, &[play], false);
    let gates = GateTable::builtin();
    let base = replay(&tape, ReplayLevel::Events, &gates).unwrap();
    assert!(base.is_exact());
    let round = &base.rounds[0];
    let stage = JudgeConfig {
        level: Level::Stage,
        ..round.config
    };
    let opts = ReplayOptions {
        judge: Some(stage),
        ..ReplayOptions::default()
    };
    let r = replay_with(&tape, ReplayLevel::Events, &gates, &opts).unwrap();
    let probe = PulseJudge::new(stage, round.timing, &gates);
    let exps = pulse::expectations(&round.chart);
    let mut expect = BTreeSet::new();
    for o in &round.recorded {
        if let JudgeOut::Verdict(v) = o
            && matches!(v.timing, Timing::Center | Timing::InWindow)
        {
            let e = &exps[v.id as usize];
            let w = probe.shape(e.prev_gap_s, e.next_gap_s).window_ms;
            let dt = f64::from(v.dt_ms.unwrap()).abs();
            assert!((dt - w).abs() > 0.05, "event {} sits on the edge", v.id);
            if dt > w {
                expect.insert(v.id);
            }
        }
    }
    assert!(
        !expect.is_empty(),
        "the tape has events past the narrower window"
    );
    let got: BTreeSet<u32> = r.diffs.iter().filter_map(|d| d.id).collect();
    assert_eq!(got, expect);
    assert!(
        r.diffs
            .iter()
            .all(|d| d.recorded.is_some() && d.replayed.is_some())
    );
}

/// 5. An incomplete tape stays out of the gate and the report says so.
#[test]
fn s3_5_an_incomplete_tape_is_left_out() {
    let tmp = Tmp::new("incomplete");
    let tapes = bench_tapes(&tmp);
    let dir = tapes[0].dir().to_path_buf();
    let path = dir.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    // A crash cuts the last line in two.
    fs::write(&path, &text[..text.len() - 20]).unwrap();
    let cut = Tape::open(&dir).unwrap();
    assert!(!cut.complete);
    let gates = GateTable::builtin();
    let whole = evidence(&tapes[1..2], &gates);
    let mixed = evidence(&[cut, Tape::open(tapes[1].dir()).unwrap()], &gates);
    assert_eq!(mixed.used, vec![1]);
    assert_eq!(mixed.excluded.len(), 1);
    assert_eq!(mixed.excluded[0].index, 0);
    assert!(mixed.to_text().contains("left out"), "{}", mixed.to_text());
    // Leaving it out changes nothing in the counts.
    assert_eq!(mixed.report.to_json(), whole.report.to_json());
}
