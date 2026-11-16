//! S3: the synthetic gate. Twelve bench tapes go through the whole path (rig, session, tape,
//! replay, report); `Attack` and `Timing` must pass the rule, and a judge whose offset is
//! 40 ms wrong must not.

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

use guitarness_core::gates::{GateCheck, GateTable};
use guitarness_core::verdict::VerdictType;
use guitarness_session::replay::{ReplayOptions, evidence, evidence_replayed};

/// Strum RMS of the virtual player (`rig.rs`), for the noise levels.
const STRUM_RMS: f64 = 0.1;

fn twelve_tapes(tmp: &Tmp) -> (Vec<Tape>, usize) {
    let mut tapes = Vec::new();
    let mut strums = 0;
    for i in 0..12usize {
        // 3 charts x 2 noise levels x 2 spreads x 3 tendencies, each tape one mix of them.
        let chart = match i % 3 {
            0 => quarters(80.0, 13),
            1 => eighths(100.0, 7),
            _ => eighths(120.0, 7),
        };
        let snr_db = [40.0, 20.0][(i / 3) % 2];
        let sigma = [9.0, 25.0][(i / 6) % 2];
        let tendency = [-30.0, 0.0, 20.0][i % 3];
        let seed = (i % 3) as u32 + 1;
        let noise = (STRUM_RMS * 10f64.powf(-snr_db / 20.0)) as f32;
        let d = tmp.path().join(format!("t{i}"));
        fs::create_dir_all(&d).unwrap();
        let play = Play {
            chart,
            model: model(tendency, sigma, seed * 100 + i as u32),
        };
        strums += guitarness_core::pulse::expectations(&play.chart).len();
        tapes.push(record(&d, 50.0 + 3.0 * i as f64, noise, &[play], false));
    }
    (tapes, strums)
}

/// 4. Twelve tapes, at least 600 strums: `Attack` and `Timing` pass.
///
/// Negative control: with the offset 40 ms off, `Timing` fails.
#[test]
fn gates_synthetic() {
    let tmp = Tmp::new("gate");
    let (tapes, strums) = twelve_tapes(&tmp);
    assert!(strums >= 600, "{strums} strums");
    assert!(tapes.iter().all(|t| t.complete));
    let gates = GateTable::builtin();
    let ev = evidence(&tapes, &gates);
    assert_eq!(ev.used.len(), 12, "{}", ev.to_text());
    assert!(ev.excluded.is_empty());
    for t in [VerdictType::Attack, VerdictType::Timing] {
        let r = ev.report.get(t);
        eprintln!(
            "{t:?}: n {} false_wrong {} abstain {}",
            r.n, r.false_wrong, r.abstain
        );
        assert!(r.n >= 200, "{t:?}: n = {}", r.n);
        assert_eq!(r.gate, GateCheck::Pass, "{t:?}\n{}", ev.to_text());
        assert_eq!(r.synthetic_tapes, 12);
    }
    // The replay of the recorded judge says the same as the tape does.
    let again = evidence_replayed(&tapes, &gates, &ReplayOptions::default());
    assert_eq!(again.report.to_json(), ev.report.to_json());

    // Negative control: the same tapes judged with the offset 40 ms too large.
    let tampered = ReplayOptions {
        offset_shift_ms: 40.0,
        ..ReplayOptions::default()
    };
    let bad = evidence_replayed(&tapes, &gates, &tampered);
    let r = bad.report.get(VerdictType::Timing);
    assert_ne!(
        r.gate,
        GateCheck::Pass,
        "a judge 40 ms off must not pass\n{}",
        bad.to_text()
    );
    assert!(r.false_wrong > 0);
}
