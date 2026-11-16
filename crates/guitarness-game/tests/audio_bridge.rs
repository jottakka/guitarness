//! G1: the audio bridge and the song clock, on the fake backend (virtual time, no device,
//! no microphone). `MinimalPlugins`, one update = 5 ms of virtual time.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use guitarness_audio::{BackendEvent, FakeConfig, FakeHandle};
use guitarness_core::chart::Chart as CoreChart;
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_game::audio::{
    AudioBridgePlugin, AudioCommand, AudioLink, AudioMode, AudioNote, AudioRig, AudioStatus,
    BridgeOptions, Link, REOPEN_GAP_S, REOPEN_TRIES, ReopenPolicy, Source, parse_args,
};
use guitarness_game::gfx::SongClock;
use guitarness_game::{AppConfig, LEAD_IN};
use guitarness_session::rig::PlayerModel;
use guitarness_session::session::{SessionEvent, SoundOutcome};

const STEP_S: f64 = 0.005;

#[derive(Resource, Default)]
struct Log {
    notes: Vec<SessionEvent>,
    links: Vec<AudioLink>,
}

fn collect(
    mut notes: MessageReader<AudioNote>,
    mut links: MessageReader<AudioLink>,
    mut log: ResMut<Log>,
) {
    log.notes.extend(notes.read().map(|n| n.0.clone()));
    log.links.extend(links.read().copied());
}

fn room(drift_out_ppm: f64) -> FakeConfig {
    let mut fc = FakeConfig::new(48_000, 48_000);
    fc.path.latency_s = 0.0873;
    fc.path.noise_rms = 1.0e-3;
    fc.drift_out_ppm = drift_out_ppm;
    fc
}

fn base_app() -> App {
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
    app
}

fn fake_app(fc: FakeConfig, opts: BridgeOptions) -> App {
    let mut app = base_app();
    app.add_plugins(AudioBridgePlugin::fake(fc, opts));
    app.update();
    app
}

fn send(app: &mut App, c: AudioCommand) {
    app.world_mut()
        .resource_mut::<Messages<AudioCommand>>()
        .write(c);
}

fn status(app: &App) -> AudioStatus {
    app.world().resource::<AudioStatus>().clone()
}

fn clock_t(app: &App) -> f32 {
    app.world().resource::<SongClock>().t
}

fn handle(app: &App) -> FakeHandle {
    app.world()
        .non_send::<AudioRig>()
        .fake_handle()
        .expect("a fake rig")
}

fn run_until(app: &mut App, max_s: f64, mut done: impl FnMut(&App) -> bool) -> bool {
    for _ in 0..(max_s / STEP_S).round() as u32 {
        if done(app) {
            return true;
        }
        app.update();
    }
    done(app)
}

fn quarters(bpm: f64, bars: u32) -> CoreChart {
    exercise(Exercise::OnClickQuarters, bpm, bars)
}

fn start(app: &mut App, chart: CoreChart) {
    send(
        app,
        AudioCommand::StartRound {
            chart,
            lead_in_beats: 4,
            take: None,
        },
    );
    assert!(
        run_until(app, 5.0, |a| status(a).anchor.is_some()),
        "the round was not armed"
    );
}

/// What the screen should show: the output frame sounding now (true, from the bench), seconds
/// after chart time zero.
fn truth_t(app: &App) -> f64 {
    let h = handle(app);
    let tr = h.truth().expect("streams open");
    let anchor = status(app).anchor.expect("armed") as f64;
    // Nothing sounds before frame 0 (the first block is requested ahead of its sound).
    (tr.out_frame_at(h.now_s()).max(0.0) - anchor) / tr.fs_out
}

/// Test 1: The song clock follows the audio frames: within 1 ms of the bench truth for 20 s with
/// a 120 ppm drift on the output, never backwards. The negative control: a clock that counts frame
/// time is off by more than 1 ms at the end, so the tolerance does reject it.
#[test]
fn g1_1_song_clock_tracks_audio_with_drift() {
    let mut app = fake_app(room(120.0), BridgeOptions::default());
    start(&mut app, quarters(60.0, 6));
    let h = handle(&app);
    let tr = h.truth().unwrap();
    let anchor = status(&app).anchor.unwrap() as f64;
    let start_s = tr.out_time_s(anchor);
    let (mut worst, mut worst_frame, mut prev) = (0.0f64, 0.0f64, f64::NEG_INFINITY);
    let t_end = start_s + 20.0;
    while h.now_s() < t_end {
        app.update();
        let t = f64::from(clock_t(&app));
        assert!(t >= prev, "the clock went back: {prev} -> {t}");
        prev = t;
        worst = worst.max((t - truth_t(&app)).abs());
        // The frame clock: seconds of frame time since the first click of chart time zero.
        worst_frame = worst_frame.max(((h.now_s() - start_s) - truth_t(&app)).abs());
    }
    assert!(worst <= 1.0e-3, "audio clock error {worst} s");
    assert!(
        worst_frame > 1.0e-3,
        "the negative control must fail: frame clock error {worst_frame} s"
    );
    assert!(prev > 19.0, "the clock ran: {prev}");
    let st = status(&app);
    assert_eq!(st.link, Link::Up);
    assert!(st.round_active);
}

/// Test 2: A device stop pauses the clock, the bridge reopens (epoch 1), the clock is back with
/// one new round, and the reopen made the calibration stale.
#[test]
fn g1_2_stopped_pauses_reopens_and_the_clock_returns() {
    let mut app = fake_app(room(0.0), BridgeOptions::default());
    start(&mut app, quarters(80.0, 8));
    assert!(run_until(&mut app, 8.0, |a| clock_t(a) > 2.0));
    let h = handle(&app);
    h.inject_event(BackendEvent::DeviceLost);
    assert!(run_until(&mut app, 2.0, |a| a
        .world()
        .resource::<Log>()
        .links
        .contains(&AudioLink::Lost)));
    assert!(matches!(status(&app).link, Link::Reopening { .. }));
    // Held while the streams are closed: never backwards.
    let held = clock_t(&app);
    for _ in 0..40 {
        app.update();
        assert!(clock_t(&app) >= held);
    }
    assert!(
        app.world()
            .resource::<Log>()
            .notes
            .iter()
            .any(|e| matches!(e, SessionEvent::Paused(_)))
    );
    // The first try comes after REOPEN_GAP_S.
    assert!(run_until(&mut app, 3.0, |a| status(a).link == Link::Up));
    {
        let log = app.world().resource::<Log>();
        assert!(log.links.contains(&AudioLink::Retrying { attempt: 1 }));
        assert!(log.links.contains(&AudioLink::Back { epoch: 1 }));
    }
    assert_eq!(app.world().non_send::<AudioRig>().epoch(), 1);
    assert!(app.world().non_send::<AudioRig>().calibration_stale());
    assert!(status(&app).calibration_stale);
    // No armed round in the new epoch: the clock rests at the lead-in.
    app.update();
    assert_eq!(status(&app).anchor, None);
    assert_eq!(clock_t(&app), LEAD_IN);
    // A new round: the clock runs again on the new streams, still within 1 ms.
    start(&mut app, quarters(80.0, 4));
    let mut prev = f64::NEG_INFINITY;
    let mut worst = 0.0f64;
    for _ in 0..(6.0 / STEP_S) as u32 {
        app.update();
        let t = f64::from(clock_t(&app));
        assert!(t >= prev);
        prev = t;
        worst = worst.max((t - truth_t(&app)).abs());
    }
    assert!(worst <= 1.0e-3, "error after the reopen {worst} s");
    assert!(prev > 2.0, "the clock is back: {prev}");
    assert_eq!(status(&app).epoch, 1);
}

/// Test 3: The soundcheck of the fake player ends with a basis, and the bridge clears the stale
/// flag the reopen set (the session does not).
#[test]
fn g1_3_soundcheck_clears_calibration_stale() {
    let mut app = fake_app(room(0.0), BridgeOptions::default());
    handle(&app).inject_event(BackendEvent::DeviceLost);
    assert!(run_until(&mut app, 4.0, |a| {
        status(a).link == Link::Up && a.world().non_send::<AudioRig>().epoch() == 1
    }));
    assert!(app.world().non_send::<AudioRig>().calibration_stale());
    send(&mut app, AudioCommand::Soundcheck { force: true });
    let done = |a: &App| {
        a.world()
            .resource::<Log>()
            .notes
            .iter()
            .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
    };
    assert!(
        run_until(&mut app, 60.0, done),
        "the soundcheck did not end"
    );
    let outcome = app
        .world()
        .resource::<Log>()
        .notes
        .iter()
        .find_map(|e| match e {
            SessionEvent::SoundcheckDone { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        outcome,
        SoundOutcome::Calibrated | SoundOutcome::Kept
    ));
    app.update();
    assert!(!app.world().non_send::<AudioRig>().calibration_stale());
    assert!(status(&app).basis.is_some());
}

/// Test 4: A whole round with the virtual player: judged, ended, nothing interrupted.
#[test]
fn g1_4_a_round_with_the_virtual_player() {
    let opts = BridgeOptions {
        player: Some(PlayerModel {
            tendency_ms: -5.0,
            sigma_ms: 6.0,
            ..PlayerModel::default()
        }),
        ..BridgeOptions::default()
    };
    let mut app = fake_app(room(0.0), opts);
    start(&mut app, quarters(80.0, 2));
    let done = |a: &App| {
        a.world()
            .resource::<Log>()
            .notes
            .iter()
            .any(|e| matches!(e, SessionEvent::RoundDone { .. }))
    };
    assert!(run_until(&mut app, 30.0, done));
    {
        let log = app.world().resource::<Log>();
        assert!(
            log.notes
                .iter()
                .any(|e| matches!(e, SessionEvent::Judge(_)))
        );
        assert!(log.notes.iter().any(|e| matches!(
            e,
            SessionEvent::RoundDone {
                interrupted: false,
                ..
            }
        )));
    }
    app.update();
    assert!(!status(&app).round_active);
}

/// Test 5: The reopen policy: three tries, a second apart, then gone; retry starts over.
#[test]
fn g1_5_reopen_policy() {
    let mut p = ReopenPolicy::new();
    assert!(p.is_idle());
    assert_eq!(p.tick(5.0), None, "nothing runs before a loss");
    p.start();
    p.start();
    assert_eq!(p.tick(REOPEN_GAP_S * 0.5), None);
    assert_eq!(p.tick(REOPEN_GAP_S * 0.6), Some(1));
    assert!(!p.failed());
    assert_eq!(p.tick(REOPEN_GAP_S * 0.9), None);
    assert_eq!(p.tick(REOPEN_GAP_S * 0.2), Some(2));
    assert!(!p.failed());
    assert_eq!(p.tick(REOPEN_GAP_S), Some(REOPEN_TRIES));
    assert!(p.failed());
    assert!(p.is_gone());
    assert_eq!(p.tick(10.0), None, "gone waits for the player");
    p.retry();
    assert_eq!(p.tick(0.0), Some(1));
    p.ok();
    assert!(p.is_idle());
    assert_eq!(p.attempt(), None);
}

/// Test 6: Command line.
#[test]
fn g1_6_parse_args() {
    let a = |s: &[&str]| parse_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    assert_eq!(a(&[]).source, None);
    assert_eq!(a(&["--fake"]).source, Some(Source::Fake));
    assert_eq!(a(&["--in", "mic"]).source, Some(Source::Live));
    let p = a(&["--fake", "--player", "-12.5,8", "--no-tape", "--data", "/x"]);
    let m = p.opts.player.unwrap();
    assert_eq!((m.tendency_ms, m.sigma_ms), (-12.5, 8.0));
    assert_eq!(p.opts.tape, Some(false));
    assert_eq!(p.opts.data.as_deref(), Some(std::path::Path::new("/x")));
    assert!(a(&["--fake", "--player", "none"]).opts.player.is_none());
    let bad = a(&["--fake", "--player", "oops"]);
    assert_eq!(bad.warnings.len(), 1);
    assert!(
        a(&["--fake", "--replay-wav", "a.wav"])
            .opts
            .replay_wav
            .is_some()
    );
}

/// Test 7: Mode `None` keeps the prototype: no rig, no status change, the clock is not touched, the
/// messages exist for the other units. `--fake` through `install` builds the bridge.
#[test]
fn g1_7_mode_none_and_install() {
    let mut app = base_app();
    app.insert_resource(AudioMode::None);
    guitarness_game::audio::install(&mut app, &AppConfig::default());
    app.world_mut().resource_mut::<SongClock>().t = 1.25;
    send(&mut app, AudioCommand::StopRound);
    for _ in 0..200 {
        app.update();
    }
    assert_eq!(*app.world().resource::<AudioMode>(), AudioMode::None);
    assert!(app.world().get_non_send::<AudioRig>().is_none());
    assert_eq!(status(&app).link, Link::Off);
    assert_eq!(clock_t(&app), 1.25);

    let mut app = base_app();
    app.insert_resource(AudioMode::None);
    let cfg = AppConfig::from_args(["--fake".to_string(), "--player".into(), "0,0".into()]);
    guitarness_game::audio::install(&mut app, &cfg);
    app.update();
    assert_eq!(*app.world().resource::<AudioMode>(), AudioMode::Fake);
    assert_eq!(status(&app).link, Link::Up);
    assert_eq!(status(&app).fs_out, 48_000);
    // Screenshots always run on the frame clock.
    let mut app = base_app();
    let mut cfg = AppConfig::from_args(["--fake".to_string()]);
    cfg.shot_dir = Some(std::env::temp_dir());
    guitarness_game::audio::install(&mut app, &cfg);
    assert!(app.world().get_non_send::<AudioRig>().is_none());
}
