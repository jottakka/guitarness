//! G2: the soundcheck on screen, on the fake backend (virtual time, no device, no
//! microphone). `MinimalPlugins`, one update = 5 ms of virtual time.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use guitarness_audio::FakeConfig;
use guitarness_audio::calib::{Basis, Prompt};
use guitarness_game::LEAD_IN;
use guitarness_game::audio::{AudioBridgePlugin, AudioNote, AudioRig, BridgeOptions, Link};
use guitarness_game::gfx::SongClock;
use guitarness_game::soundcheck::{
    DOTS, Phase, Pictogram, Soundcheck, SoundcheckCmd, SoundcheckPlugin, SoundcheckReady, Tone,
    bluetooth_shapes, dot_shapes, pictogram_for, play_now_shapes, shapes_of,
};
use guitarness_session::rig::PlayerModel;
use guitarness_session::session::{SessionEvent, SoundOutcome};

const STEP_S: f64 = 0.005;

#[derive(Resource, Default)]
struct Log {
    prompts: Vec<Prompt>,
    ready: Vec<SoundcheckReady>,
    dots: Vec<u8>,
}

fn collect(
    mut notes: MessageReader<AudioNote>,
    mut ready: MessageReader<SoundcheckReady>,
    sc: Res<Soundcheck>,
    mut log: ResMut<Log>,
) {
    for n in notes.read() {
        if let SessionEvent::Prompt(p) = &n.0 {
            log.prompts.push(*p);
        }
    }
    log.ready.extend(ready.read().copied());
    log.dots.push(sc.dots);
}

fn room(latency_s: f64, gain: f32) -> FakeConfig {
    let mut fc = FakeConfig::new(48_000, 48_000);
    fc.path.latency_s = latency_s;
    fc.path.gain = gain;
    fc.path.noise_rms = 1.0e-3;
    fc
}

fn app_with(fc: FakeConfig, player: Option<PlayerModel>) -> App {
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
        .add_systems(Last, collect)
        .add_plugins(AudioBridgePlugin::fake(
            fc,
            BridgeOptions {
                player,
                ..BridgeOptions::default()
            },
        ))
        .add_plugins(SoundcheckPlugin { auto_start: false });
    app.update();
    app
}

fn send(app: &mut App, c: SoundcheckCmd) {
    app.world_mut()
        .resource_mut::<Messages<SoundcheckCmd>>()
        .write(c);
}

fn sc(app: &App) -> Soundcheck {
    app.world().resource::<Soundcheck>().clone()
}

/// Updates until `done`; returns the virtual seconds spent, or `None` on timeout.
fn run_until(app: &mut App, max_s: f64, mut done: impl FnMut(&App) -> bool) -> Option<f64> {
    for i in 0..(max_s / STEP_S).round() as u32 {
        if done(app) {
            return Some(i as f64 * STEP_S);
        }
        app.update();
    }
    done(app).then_some(max_s)
}

fn finished(a: &App) -> bool {
    sc(a).phase == Phase::Finished
}

/// Test 1: a loop of 87.3 ms: the soundcheck is ready within 4.5 virtual seconds, on the
/// `Loopback` basis, and the loop it found is within 1 ms of the truth.
#[test]
fn g2_1_loopback_ready_fast_and_exact() {
    let mut app = app_with(room(0.0873, 0.5), Some(PlayerModel::default()));
    send(&mut app, SoundcheckCmd::Start { force: true });
    let took = run_until(&mut app, 4.5, finished).expect("not ready within 4.5 s");
    let s = sc(&app);
    assert_eq!(s.basis, Some(Basis::Loopback), "took {took}");
    assert_eq!(s.pictogram, Pictogram::Ready);
    assert!(s.judged() && s.tendency_allowed());
    assert_eq!(s.dots, DOTS);
    let l = app
        .world()
        .non_send::<AudioRig>()
        .session()
        .and_then(|x| x.profile())
        .and_then(|p| p.l_rt_ms)
        .expect("a measured loop");
    assert!((l - 87.3).abs() <= 1.0, "loop {l} ms");
    let log = app.world().resource::<Log>();
    assert_eq!(log.ready.len(), 1, "one ready message");
    assert!(log.ready[0].judged && !log.ready[0].skipped);
}

/// Test 2: no acoustic loop (gain 0) but a player: the soundcheck goes through the muted
/// strum, ends on `Estimate`, and a tendency is never allowed.
#[test]
fn g2_2_no_loop_goes_through_the_muted_strum() {
    let mut app = app_with(room(0.0873, 0.0), Some(PlayerModel::default()));
    send(&mut app, SoundcheckCmd::Start { force: true });
    run_until(&mut app, 90.0, finished).expect("the soundcheck did not end");
    let s = sc(&app);
    assert_eq!(s.basis, Some(Basis::Estimate), "{s:?}");
    assert!(s.judged());
    assert!(!s.tendency_allowed());
    let log = app.world().resource::<Log>();
    assert!(
        log.prompts
            .iter()
            .any(|p| matches!(p, Prompt::StrumMuted { .. })),
        "no muted-strum prompt: {:?}",
        log.prompts
    );
    // The bluetooth hint is gentle and only a hint.
    assert!(s.bluetooth_hint);
}

/// Test 3: digital silence (gain 0, nobody plays): the microphone pictogram, no failure
/// state, and "play now" works with basis `None` (shown, not judged).
#[test]
fn g2_3_digital_silence_then_play_now() {
    let mut fc = room(0.0873, 0.0);
    fc.path.noise_rms = 0.0; // digital silence, not a quiet room
    let mut app = app_with(fc, None);
    send(&mut app, SoundcheckCmd::Start { force: true });
    run_until(&mut app, 60.0, finished).expect("the soundcheck did not end");
    let s = sc(&app);
    assert_eq!(s.pictogram, Pictogram::MicBlocked);
    assert_eq!(s.basis, Some(Basis::None));
    assert!(!s.judged());
    assert!(s.shown);
    let c = s.choices();
    assert!(c.play_now && c.try_again);
    send(&mut app, SoundcheckCmd::PlayNow);
    app.update();
    app.update();
    let s = sc(&app);
    assert!(!s.shown, "the screen is gone after play now");
    assert!(s.playable());
    let log = app.world().resource::<Log>();
    let last = log.ready.last().expect("a ready message");
    assert!(!last.judged);
    assert_eq!(last.basis, Some(Basis::None));
}

/// Test 4: "play now" before the end: ready at once (skipped, not judged), and the running
/// calibration is cancelled: it never ends with a new verdict.
#[test]
fn g2_4_play_now_while_running() {
    let mut app = app_with(room(0.0873, 0.5), Some(PlayerModel::default()));
    send(&mut app, SoundcheckCmd::Start { force: true });
    run_until(&mut app, 2.0, |a| sc(a).prompt.is_some()).expect("a prompt");
    send(&mut app, SoundcheckCmd::PlayNow);
    app.update();
    app.update();
    let s = sc(&app);
    assert!(!s.shown && s.skipped && !s.judged());
    {
        let log = app.world().resource::<Log>();
        let r = log.ready.first().expect("ready at once");
        assert!(r.skipped && !r.judged);
    }
    for _ in 0..600 {
        app.update();
    }
    assert!(
        !sc(&app).judged(),
        "the cancelled soundcheck gives no verdict"
    );
    assert!(sc(&app).playable());
}

/// Test 4b: the session answers a cancelled soundcheck: the previous basis stays, no
/// outcome is invented, the player is ready and can play.
#[test]
fn g2_4b_cancelled_soundcheck_keeps_the_basis_and_is_playable() {
    for (before, judged) in [(Basis::Loopback, true), (Basis::None, false)] {
        let mut s = Soundcheck::default();
        s.begin();
        s.on_event(&SessionEvent::Prompt(Prompt::Quiet));
        let r = s
            .on_event(&SessionEvent::SoundcheckCancelled { basis: before })
            .expect("ready after a cancel");
        assert_eq!(r.basis, Some(before));
        assert_eq!(r.outcome, None);
        assert_eq!(r.judged, judged);
        assert_eq!(s.basis, Some(before));
        assert_eq!(s.judged(), judged);
        assert!(s.playable() && s.choices().play_now);
        assert_eq!(s.choices().try_again, !judged);
    }
}

/// Test 5: every `Outcome` leads to a playable state (exhaustive over the enum), with a
/// pictogram, a ready message, and play now among the choices.
#[test]
fn g2_5_every_outcome_is_playable() {
    let all = [
        SoundOutcome::Kept,
        SoundOutcome::Calibrated,
        SoundOutcome::Unstable,
        SoundOutcome::MicBlocked,
        SoundOutcome::Interrupted,
    ];
    // Fails to compile when the enum grows: add the new variant to `all`.
    for o in all {
        match o {
            SoundOutcome::Kept
            | SoundOutcome::Calibrated
            | SoundOutcome::Unstable
            | SoundOutcome::MicBlocked
            | SoundOutcome::Interrupted => {}
        }
    }
    for outcome in all {
        for basis in [Basis::Loopback, Basis::Estimate, Basis::None] {
            let mut s = Soundcheck::default();
            s.begin();
            let r = s
                .on_event(&SessionEvent::SoundcheckDone { basis, outcome })
                .expect("a ready message");
            assert_eq!(s.phase, Phase::Finished);
            assert!(s.playable() && s.choices().play_now, "{outcome:?}");
            assert_ne!(s.pictogram, Pictogram::Waiting);
            assert_eq!(r.judged, basis != Basis::None);
            assert!(!shapes_of(s.pictogram).is_empty());
            let r = s.skip();
            assert!(!s.shown);
            assert_eq!(r.basis, Some(basis));
        }
    }
    // A reopen failure is a plug with a way on.
    for link in [Link::Gone, Link::Unavailable] {
        let mut s = Soundcheck::default();
        s.begin();
        s.on_link(link);
        assert_eq!(s.pictogram, Pictogram::Plug);
        assert!(s.choices().play_now);
    }
}

/// Test 6: the progress dots only grow, whatever order the prompts come in, and nothing a
/// problem prompt says takes a dot away.
#[test]
fn g2_6_dots_never_decrease() {
    let seq = [
        Prompt::Checking,
        Prompt::Quiet,
        Prompt::TooLoud,
        Prompt::Quiet,
        Prompt::WillBeep { count: 3 },
        Prompt::WillBeepLouder { count: 3 },
        Prompt::StrumMuted {
            count_in: 2,
            beat: 0,
            of: 4,
        },
        Prompt::StrumMuted {
            count_in: 0,
            beat: 2,
            of: 4,
        },
        Prompt::RhythmLater,
        Prompt::MicBlocked,
        Prompt::Interrupted,
        Prompt::Done,
    ];
    let mut s = Soundcheck::default();
    s.begin();
    let mut prev = 0;
    for p in seq {
        s.on_event(&SessionEvent::Prompt(p));
        assert!(s.dots >= prev, "{p:?}");
        prev = s.dots;
        assert_eq!(s.pictogram, pictogram_for(p));
    }
    assert_eq!(s.dots, DOTS);
    // And in a real run.
    let mut app = app_with(room(0.0873, 0.5), Some(PlayerModel::default()));
    send(&mut app, SoundcheckCmd::Start { force: true });
    run_until(&mut app, 30.0, finished).expect("ends");
    let log = app.world().resource::<Log>();
    assert!(log.dots.windows(2).all(|w| w[0] <= w[1]), "dots went back");
}

fn saturation(c: Color) -> f32 {
    let c = c.to_srgba();
    let hi = c.red.max(c.green).max(c.blue);
    let lo = c.red.min(c.green).min(c.blue);
    hi - lo
}

fn every_pictogram() -> Vec<Pictogram> {
    vec![
        Pictogram::Waiting,
        Pictogram::Checking,
        Pictogram::Silence,
        Pictogram::TooLoud,
        Pictogram::Beeps { count: 3 },
        Pictogram::BeepsLouder { count: 3 },
        Pictogram::MutedStrum {
            count_in: 3,
            beat: 0,
            of: 4,
        },
        Pictogram::MutedStrum {
            count_in: 0,
            beat: 2,
            of: 4,
        },
        Pictogram::Ready,
        Pictogram::RhythmLater,
        Pictogram::MicBlocked,
        Pictogram::Plug,
    ]
}

/// Test 7: no red anywhere, and the problem pictograms use only white and grey (saturation
/// 0), checked by enumeration of every shape.
#[test]
fn g2_7_no_red_and_problems_are_white_or_grey() {
    for p in every_pictogram() {
        for s in shapes_of(p) {
            let c = s.tone.color().to_srgba();
            assert!(
                !(c.red > c.green + 0.15 && c.red > c.blue + 0.15),
                "{p:?} has a red-ish shape"
            );
            assert!(s.w > 0.0 && s.h > 0.0);
        }
    }
    for p in [
        Pictogram::TooLoud,
        Pictogram::RhythmLater,
        Pictogram::MicBlocked,
        Pictogram::Plug,
    ] {
        for s in shapes_of(p) {
            assert!(matches!(s.tone, Tone::White | Tone::Grey), "{p:?}");
            assert_eq!(saturation(s.tone.color()), 0.0, "{p:?}");
        }
    }
    for s in bluetooth_shapes() {
        assert!(matches!(s.tone, Tone::White | Tone::Grey));
    }
    for s in dot_shapes(2).into_iter().chain(play_now_shapes()) {
        let c = s.tone.color().to_srgba();
        assert!(!(c.red > c.green + 0.15 && c.red > c.blue + 0.15));
    }
}

/// Test 8: the pictogram is a pure function of the state: the beeps pictogram shows one
/// disc per beep, and the muted strum lights one disc per strummed beat.
#[test]
fn g2_8_pictograms_follow_their_counts() {
    use guitarness_game::soundcheck::Kind;
    let discs = |p| shapes_of(p).iter().filter(|s| s.kind == Kind::Disc).count();
    assert_eq!(discs(Pictogram::Beeps { count: 3 }), 3);
    assert_eq!(discs(Pictogram::BeepsLouder { count: 5 }), 5);
    assert_eq!(
        discs(Pictogram::MutedStrum {
            count_in: 0,
            beat: 3,
            of: 4
        }),
        3
    );
}
