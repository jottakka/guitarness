//! G3: the Pulse screen's logic on the fake backend (virtual time, no window, no device).

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use tonica_audio::{FakeConfig, Side};
use tonica_core::verdict::{Attack, JudgeOut, Why};
use tonica_game::LEAD_IN;
use tonica_game::audio::{AudioBridgePlugin, AudioCommand, AudioNote, AudioRig, BridgeOptions};
use tonica_game::gfx::SongClock;
use tonica_game::pulse::assets::{ErrorMark, PulseMats, build_mats, material_colors, saturation};
use tonica_game::pulse::feedback::{MarkKind, PulseFeedback, shift_of};
use tonica_game::pulse::flow::{
    Offer, Offers, Phase, PulseAction, PulseFlow, PulseLogicPlugin, PulseRound, PulseTotals,
};
use tonica_session::rig::PlayerModel;
use tonica_session::session::{SessionEvent, SoundOutcome};

const STEP_S: f64 = 0.005;

#[derive(Resource, Default)]
struct Log {
    notes: Vec<SessionEvent>,
}

fn collect(mut notes: MessageReader<AudioNote>, mut log: ResMut<Log>) {
    log.notes.extend(notes.read().map(|n| n.0.clone()));
}

fn room() -> FakeConfig {
    let mut fc = FakeConfig::new(48_000, 48_000);
    fc.path.latency_s = 0.0873;
    fc.path.noise_rms = 1.0e-3;
    fc
}

fn app_with(model: PlayerModel, bpm: f64, target_s: f64) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            STEP_S,
        )))
        .insert_resource(SongClock {
            t: LEAD_IN,
            running: true,
        })
        .init_resource::<Log>()
        .add_systems(Last, collect);
    let opts = BridgeOptions {
        player: Some(model),
        ..Default::default()
    };
    app.add_plugins(AudioBridgePlugin::fake(room(), opts));
    app.add_plugins(PulseLogicPlugin);
    {
        let mut f = app.world_mut().resource_mut::<PulseFlow>();
        f.bpm = bpm;
        f.target_s = target_s;
        f.screen_on = true;
    }
    app.update();
    app
}

fn model(tendency_ms: f64, sigma_ms: f64, miss: f64) -> PlayerModel {
    PlayerModel {
        tendency_ms,
        sigma_ms,
        miss,
        double: 0.0,
        seed: 7,
    }
}

fn run_until(app: &mut App, max_s: f64, mut done: impl FnMut(&mut App) -> bool) -> bool {
    for _ in 0..(max_s / STEP_S).round() as u32 {
        if done(app) {
            return true;
        }
        app.update();
    }
    done(app)
}

fn act(app: &mut App, a: PulseAction) {
    app.world_mut()
        .resource_mut::<Messages<PulseAction>>()
        .write(a);
}

fn soundcheck(app: &mut App) {
    app.world_mut()
        .resource_mut::<Messages<AudioCommand>>()
        .write(AudioCommand::Soundcheck { force: true });
    assert!(
        run_until(app, 60.0, |a| {
            a.world()
                .resource::<Log>()
                .notes
                .iter()
                .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
        }),
        "the soundcheck did not end"
    );
    let ok = app
        .world()
        .resource::<Log>()
        .notes
        .iter()
        .find_map(|e| match e {
            SessionEvent::SoundcheckDone { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .unwrap();
    assert!(matches!(ok, SoundOutcome::Calibrated | SoundOutcome::Kept));
}

fn play_round(app: &mut App) {
    let before = app.world().resource::<PulseTotals>().rounds();
    act(app, PulseAction::Play);
    assert!(run_until(app, 90.0, |a| {
        a.world().resource::<PulseTotals>().rounds() > before
    }));
    assert_eq!(app.world().resource::<PulseFlow>().phase, Phase::Between);
}

fn judge_outs(app: &App) -> Vec<JudgeOut> {
    app.world()
        .resource::<Log>()
        .notes
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Judge(j) => Some(*j),
            _ => None,
        })
        .collect()
}

/// Test 1: one return per strike, a negative shift for an early player, mean within 3 ms.
#[test]
fn g3_1_one_return_per_strike_signed_shift() {
    let mut app = app_with(model(-30.0, 9.0, 0.0), 120.0, 30.0);
    soundcheck(&mut app);
    play_round(&mut app);
    let n = app.world().resource::<PulseRound>().times.len();
    let fb = app.world().resource::<PulseFeedback>();
    let flashes = judge_outs(&app)
        .iter()
        .filter(|j| matches!(j, JudgeOut::Flash { .. }))
        .count();
    assert_eq!(fb.flashes.len(), flashes);
    assert_eq!(fb.flashes.len(), n, "one return per strike played");
    assert_eq!(app.world().resource::<PulseTotals>().strikes(), n as u64);
    let timed: Vec<_> = fb.flashes.iter().filter(|f| f.dt_ms.is_some()).collect();
    assert!(timed.len() >= n * 9 / 10, "{} of {n} timed", timed.len());
    let mean = fb.mean_dt_ms().unwrap();
    assert!((mean + 30.0).abs() <= 3.0, "mean {mean} ms");
    let neg = timed.iter().filter(|f| f.shift.unwrap() < 0.0).count();
    assert!(
        neg * 10 >= timed.len() * 9,
        "negative shifts {neg}/{}",
        timed.len()
    );
    assert!(timed.iter().all(|f| f.shift.unwrap().abs() <= 1.0));
    eprintln!("mean {mean:.2} ms over {} timed of {n}", timed.len());
}

#[test]
fn g3_2_shift_is_proportional_and_capped() {
    assert_eq!(shift_of(-60.0, 120.0), -0.5);
    assert_eq!(shift_of(30.0, 120.0), 0.25);
    assert_eq!(shift_of(-400.0, 120.0), -1.0);
    assert_eq!(shift_of(400.0, 120.0), 1.0);
}

/// Test 3: an xrun in the middle of a round: the events in its window show a lock and only
/// they; nothing becomes a miss because of it.
#[test]
fn g3_3_glitch_shows_a_lock() {
    let mut app = app_with(model(0.0, 5.0, 0.0), 120.0, 16.0);
    soundcheck(&mut app);
    act(&mut app, PulseAction::Play);
    assert!(run_until(&mut app, 30.0, |a| a
        .world()
        .resource::<SongClock>()
        .t
        > 7.0));
    let h = app.world().non_send::<AudioRig>().fake_handle().unwrap();
    assert!(h.inject_xrun(Side::In, 2_400));
    assert!(run_until(&mut app, 60.0, |a| {
        a.world().resource::<PulseFlow>().phase == Phase::Between
    }));
    let outs = judge_outs(&app);
    let fb = app.world().resource::<PulseFeedback>();
    let mut locked = 0;
    for j in &outs {
        if let JudgeOut::Verdict(v) = j
            && v.attack == Attack::NotJudged(Why::Glitch)
            && v.fin
        {
            {
                // The last word on the event decides what is drawn.
                let last = outs.iter().rev().find_map(|x| match x {
                    JudgeOut::Verdict(w) if w.id == v.id => Some(w.attack),
                    JudgeOut::Withdrawn { id, .. } if *id == v.id => {
                        Some(Attack::NotJudged(Why::Glitch))
                    }
                    _ => None,
                });
                if last == Some(Attack::NotJudged(Why::Glitch)) {
                    assert_eq!(fb.events[v.id as usize].kind, MarkKind::NotJudged);
                    locked += 1;
                }
            }
        }
    }
    assert!(locked >= 1, "no lock shown");
    let not_heard = fb
        .events
        .iter()
        .filter(|m| m.kind == MarkKind::NotHeard)
        .count();
    assert_eq!(not_heard, 0, "the xrun made misses");
}

/// Test 4: every error mark material is white or grey (saturation 0), by enumeration; and no
/// material of the screen is red.
#[test]
fn g3_4_error_marks_have_no_colour() {
    for m in ErrorMark::ALL {
        for c in material_colors(&m.material()) {
            assert_eq!(saturation(c), 0.0, "{m:?}");
        }
    }
    let mut assets = Assets::<StandardMaterial>::default();
    let mats: PulseMats = build_mats(&mut assets);
    for m in ErrorMark::ALL {
        let mat = assets.get(mats.error(m)).unwrap();
        for c in material_colors(mat) {
            assert_eq!(saturation(c), 0.0, "{m:?}");
        }
    }
    for (_, mat) in assets.iter() {
        let l = mat.base_color.to_linear();
        assert!(
            !(l.red > 1.4 * l.green && l.red > 1.4 * l.blue),
            "red-ish material"
        );
    }
}

/// A match with no failure arm: every phase is a calm one.
#[test]
fn g3_5_phases_have_no_failure_state() {
    for p in Phase::ALL {
        let calm = match p {
            Phase::Home | Phase::Counting | Phase::Playing | Phase::Between => true,
        };
        assert!(calm);
    }
    for k in MarkKind::ALL {
        let neutral = match k {
            MarkKind::Waiting
            | MarkKind::Heard(_)
            | MarkKind::NotHeard
            | MarkKind::NotJudged
            | MarkKind::Ghost
            | MarkKind::Contested => true,
        };
        assert!(neutral);
    }
}

/// Test 6: three bad rounds in a row. Counters only go up, the tempo stays, and every round
/// ends in a calm state with play available.
#[test]
fn g3_6_counters_only_grow_in_bad_rounds() {
    let mut app = app_with(model(0.0, 8.0, 0.7), 120.0, 8.0);
    soundcheck(&mut app);
    let mut prev = (0u64, 0u64, 0u64);
    for r in 1..=3u64 {
        act(&mut app, PulseAction::Play);
        let rounds_before = app.world().resource::<PulseTotals>().rounds();
        let mut done = false;
        for _ in 0..(60.0 / STEP_S) as u32 {
            app.update();
            let t = *app.world().resource::<PulseTotals>();
            let now = (t.strikes(), t.rounds(), t.heard());
            assert!(now.0 >= prev.0 && now.1 >= prev.1 && now.2 >= prev.2);
            prev = now;
            if t.rounds() > rounds_before {
                done = true;
                break;
            }
        }
        assert!(done, "round {r} did not end");
        let f = app.world().resource::<PulseFlow>();
        assert_eq!(f.phase, Phase::Between);
        assert_eq!(f.bpm, 120.0, "the tempo changed by itself");
    }
    assert_eq!(app.world().resource::<PulseTotals>().rounds(), 3);
    assert!(prev.0 > 0);
}

/// Test 7: the tempo changes only after the chip is accepted, and can be taken back.
#[test]
fn g3_7_tempo_only_by_accepted_offer() {
    let mut app = app_with(model(0.0, 3.0, 0.0), 120.0, 8.0);
    soundcheck(&mut app);
    // No offer on the table: the key does nothing.
    act(&mut app, PulseAction::Accept(Offer::TempoUp));
    app.update();
    assert_eq!(app.world().resource::<PulseFlow>().bpm, 120.0);
    play_round(&mut app);
    let f = app.world().resource::<PulseFlow>().clone();
    assert_eq!(f.bpm, 120.0, "unchanged until accepted");
    assert!(
        f.offers.up && !f.offers.down,
        "a good round offers faster: {:?}",
        f.offers
    );
    act(&mut app, PulseAction::Accept(Offer::TempoUp));
    app.update();
    let f = app.world().resource::<PulseFlow>().clone();
    assert_eq!(f.bpm, 125.0);
    assert_eq!(
        f.offers,
        Offers {
            up: false,
            down: true
        }
    );
    // Reversible.
    act(&mut app, PulseAction::Accept(Offer::TempoDown));
    app.update();
    assert_eq!(app.world().resource::<PulseFlow>().bpm, 120.0);
    // The next round plays at the accepted tempo.
    act(&mut app, PulseAction::Play);
    app.update();
    assert_eq!(app.world().resource::<PulseRound>().bpm, 120.0);
}
