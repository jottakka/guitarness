//! `--screenshot <dir>`: scripted captures with blank-frame rejection.
//!
//! Render pipelines compile asynchronously, so a capture can come out blank. Each
//! capture waits for N rendered frames, is validated in the observer (a frame with
//! almost no bright pixels is dropped) and is retried until a valid file exists.
//! A retry starts only after the previous readback has reported (or after
//! `LOST_AFTER_S`), so slow renderers do not pile up pending readbacks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};

use tonica_audio::calib::{Basis, Prompt};
use tonica_core::stats::{Improvement, NoClaim, RoundStats, Tendency};
use tonica_core::timing::Level;
use tonica_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use tonica_session::session::SessionEvent;

use crate::audio::AudioNote;
use crate::gfx::{ActiveView, SongClock, View, ViewCam};
use crate::pulse::assets::PulseView;
use crate::pulse::flow::{Phase, PulseAction, PulseFlow, PulseRound};
use crate::pulse::{PulseCam, PulsePlugin};
use crate::soundcheck::{Soundcheck, SoundcheckCmd};
use crate::summary::{Dispute, SummaryAction, SummaryState};
use crate::{PendingChart, chart::Chart, fretboard::Zone};

const SETTLE_FRAMES: u32 = 45;
const MAX_ATTEMPTS: u32 = 40;
/// A readback with no answer after this long counts as lost.
const LOST_AFTER_S: f32 = 10.0;

const PENDING: u8 = 0;
const BLANK: u8 = 1;
const DONE: u8 = 2;

#[derive(Resource)]
pub struct Shooting;

/// Captures render offscreen: a locked or occluded macOS display presents no
/// frames to the window, which made window captures come out blank.
#[derive(Resource)]
struct ShotTarget(Handle<Image>);

fn retarget(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    cams: Query<Entity, With<ViewCam>>,
) {
    let image = Image::new_target_texture(
        1280,
        720,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    let handle = images.add(image);
    for e in &cams {
        commands
            .entity(e)
            .insert(RenderTarget::Image(handle.clone().into()));
    }
    commands.insert_resource(ShotTarget(handle));
}

/// What a capture shows. `Gallery` is the old note-highway views; the rest are the Pulse screen
/// driven by injected audio notes (the frame clock runs, no device is ever opened).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scene {
    Gallery,
    /// Mid-round: the strokes before `t` were judged by the scripted player.
    Round(PulseView),
    /// Mid-round with no calibration: the strokes are instructions only, nothing is marked.
    Guide(PulseView),
    /// A whole round, then the summary.
    Summary,
    /// The summary with the Details panel open.
    Details,
    /// Mid-round, the last stroke contested: the "?" state.
    Dispute,
    /// The soundcheck screen on its beeps step.
    Beeps,
    /// The soundcheck screen on its muted-strum step.
    Strum,
}

struct Spec {
    name: &'static str,
    scene: Scene,
    view: View,
    chart: &'static str,
    t: f32,
}

#[derive(Resource)]
struct Driver {
    dir: PathBuf,
    specs: Vec<Spec>,
    idx: usize,
    phase: u32,
    frames: u32,
    waited: f32,
    attempts: u32,
    /// State of the last readback: PENDING, BLANK or DONE.
    cap: Arc<AtomicU8>,
    /// Step of the Pulse script (phase 20) and the next stroke to replay.
    step: u32,
    cursor: usize,
}

pub fn install(app: &mut App, dir: PathBuf) {
    std::fs::create_dir_all(&dir).expect("create screenshot dir");
    let s = |name, view, chart, t| Spec {
        name,
        scene: Scene::Gallery,
        view,
        chart,
        t,
    };
    let p = |name, scene, t| Spec {
        name,
        scene,
        view: View::D3,
        chart: "demo",
        t,
    };
    let specs = vec![
        s("fretboard-3d", View::Fret3, "demo", 2.9),
        s("lanes-3d", View::D3, "demo", 2.9),
        s("lane-2d", View::D2, "demo", 2.9),
        s("strokes-fretboard-3d", View::Fret3, "strokes", 2.4),
        s("strokes-fretboard-3d-2", View::Fret3, "strokes", 6.5),
        s("strokes-fretboard-3d-3", View::Fret3, "strokes", 12.0),
        s("strokes-fretboard-3d-4", View::Fret3, "strokes", 17.2),
        s("strokes-2d", View::D2, "strokes", 1.3),
        s("strokes-2d-2", View::D2, "strokes", 8.5),
        s("strokes-2d-3", View::D2, "strokes", 15.5),
        p("pulse-3d", Scene::Round(PulseView::D3), PULSE_T),
        p("pulse-2d", Scene::Round(PulseView::D2), 4.45),
        p("pulse-guide-3d", Scene::Guide(PulseView::D3), PULSE_T),
        p("pulse-guide-2d", Scene::Guide(PulseView::D2), 4.45),
        p("pulse-summary", Scene::Summary, 0.0),
        p("pulse-details", Scene::Details, 0.0),
        p("pulse-dispute", Scene::Dispute, 10.9),
        p("soundcheck-beeps", Scene::Beeps, 0.0),
        p("soundcheck-strum", Scene::Strum, 0.0),
    ];
    let specs: Vec<Spec> = match std::env::var("SHOT_ONLY") {
        Ok(n) => specs.into_iter().filter(|s| s.name == n).collect(),
        Err(_) => specs,
    };
    for sp in &specs {
        let _ = std::fs::remove_file(dir.join(format!("{}.png", sp.name)));
    }
    // Fixed 60 Hz frame time (real and virtual): easing and fades depend on the frame count,
    // not on how fast this machine renders, so captures repeat byte for byte.
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_micros(16_667),
    ))
    .insert_resource(Shooting)
    .insert_resource(Driver {
        dir,
        specs,
        idx: 0,
        phase: 0,
        frames: 0,
        waited: 0.0,
        attempts: 0,
        cap: Arc::new(AtomicU8::new(PENDING)),
        step: 0,
        cursor: 0,
    })
    .add_systems(Startup, retarget.after(crate::setup_scene))
    .add_systems(
        Update,
        (retarget_pulse, script, drive)
            .chain()
            .after(crate::judge_fake::update_flash),
    );
    // The Pulse screen stays off (flow.screen_on = false) until a Pulse scene turns it on.
    if app.world().get_resource::<PulseFlow>().is_none() {
        app.add_plugins(PulsePlugin);
    }
}

/// The Pulse cameras render offscreen too; the HUD camera also carries the UI (summary,
/// soundcheck, dispute line).
fn retarget_pulse(
    mut commands: Commands,
    target: Option<Res<ShotTarget>>,
    cams: Query<(Entity, &PulseCam), Added<PulseCam>>,
) {
    let Some(target) = target else { return };
    for (e, p) in &cams {
        commands
            .entity(e)
            .insert(RenderTarget::Image(target.0.clone().into()));
        if p.0.is_none() {
            commands.entity(e).insert(bevy::ui::IsDefaultUiCamera);
        }
    }
}

/// Song time of the Pulse round captures: a few strokes in.
const PULSE_T: f32 = 6.9;

/// Tiny deterministic generator (fixed seed): the scripted player is the same on every run.
struct Lcg(u64);

impl Lcg {
    /// Uniform in -1..1.
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// What the scripted player did with stroke `i`: `None` is a stroke not heard.
fn player(i: usize) -> Option<f32> {
    let mut g = Lcg(7 ^ (i as u64).wrapping_mul(0x9E37_79B9));
    g.next();
    let r = g.next();
    match i {
        // Not heard: a quiet stroke.
        5 | 14 | 22 => None,
        // A little early or late, within the window.
        2 | 9 | 17 => Some(-58.0 + r * 8.0),
        4 | 11 | 19 => Some(52.0 + r * 8.0),
        _ => Some(r * 20.0),
    }
}

fn verdict_of(i: usize, dt: Option<f32>, pos: u8) -> PulseVerdict {
    let (attack, timing) = match dt {
        None => (Attack::NotHeard, Timing::NotJudged(Why::NoAttack)),
        Some(d) if d.abs() <= 15.0 => (Attack::Heard, Timing::Center),
        Some(_) => (Attack::Heard, Timing::InWindow),
    };
    PulseVerdict {
        id: i as u32,
        attack,
        timing,
        dt_ms: dt,
        dir: Direction::NotJudged(Why::Ghost),
        onset_in: None,
        pos_in_bar: pos,
        fin: true,
    }
}

/// Round numbers of the first `n` strokes, as the session would report them.
fn stats_of(n: usize) -> RoundStats {
    let outs: Vec<Option<f32>> = (0..n).map(player).collect();
    let mut dts: Vec<f64> = outs.iter().flatten().map(|d| f64::from(*d)).collect();
    dts.sort_by(|a, b| a.total_cmp(b));
    let heard = dts.len() as u32;
    let center = dts.iter().filter(|d| d.abs() <= 15.0).count() as u32;
    let med = dts.get(dts.len() / 2).copied().unwrap_or(0.0);
    let mut dev: Vec<f64> = dts.iter().map(|d| (d - med).abs()).collect();
    dev.sort_by(|a, b| a.total_cmp(b));
    let mad = dev.get(dev.len() / 2).copied().unwrap_or(0.0);
    RoundStats {
        events: n as u32,
        heard,
        not_heard: n as u32 - heard,
        not_judged: 0,
        disputed: 0,
        center,
        in_window: heard - center,
        outside: 0,
        timed: heard,
        median_ms: Some(med),
        spread_ms: Some(1.4826 * mad),
        uncertainty_ms: Some(4.0),
        p90_abs_ms: Some(62.0),
        constancy_focus: false,
        tendency: if med.abs() < 8.0 {
            Tendency::Centered
        } else if med < 0.0 {
            Tendency::Early(med)
        } else {
            Tendency::NotClaimed(NoClaim::Uncertain)
        },
    }
}

/// The Pulse scenes, step by step, one injected event per frame so the views see them in order.
#[allow(clippy::too_many_arguments)]
fn script(
    mut d: ResMut<Driver>,
    time: Res<Time<Real>>,
    mut clock: ResMut<SongClock>,
    mut flow: ResMut<PulseFlow>,
    round: Res<PulseRound>,
    mut summary: ResMut<SummaryState>,
    mut sound: ResMut<Soundcheck>,
    mut acts: MessageWriter<PulseAction>,
    mut sacts: MessageWriter<SummaryAction>,
    mut scmds: MessageWriter<SoundcheckCmd>,
    mut notes: MessageWriter<AudioNote>,
) {
    let Some(spec) = d.specs.get(d.idx) else {
        return;
    };
    let scene = spec.scene;
    let t_end = spec.t;
    if scene == Scene::Gallery {
        return;
    }
    // Hold the dispute line on screen while the capture settles (it fades with real time).
    if matches!(d.phase, 1 | 2)
        && let Dispute::Withdrawn { target, .. } = summary.dispute
    {
        summary.dispute = Dispute::Withdrawn {
            target,
            at_s: summary.now_s.max(time.elapsed_secs_f64()),
        };
    }
    if d.phase != 20 {
        return;
    }
    let step = d.step;
    d.step += 1;
    let soundcheck = matches!(scene, Scene::Beeps | Scene::Strum);
    let say = |notes: &mut MessageWriter<AudioNote>, e: SessionEvent| {
        notes.write(AudioNote(e));
    };
    match step {
        0 => {
            *summary = SummaryState::default();
            *sound = Soundcheck::default();
            flow.screen_on = true;
            flow.phase = Phase::Home;
            // A calibrated clock, so the round's strokes are judged; the guide scenes keep it
            // off (basis None) and show the instructions alone.
            if !matches!(scene, Scene::Guide(_)) {
                sound.basis = Some(Basis::Loopback);
            }
            flow.view = match scene {
                Scene::Round(v) | Scene::Guide(v) => v,
                _ => PulseView::D3,
            };
            flow.bpm = 80.0;
            flow.target_s = 24.0;
            clock.t = crate::LEAD_IN;
            clock.running = false;
            if soundcheck {
                scmds.write(SoundcheckCmd::Start { force: true });
            } else {
                acts.write(PulseAction::Play);
            }
        }
        1 if soundcheck => {
            say(&mut notes, SessionEvent::Prompt(Prompt::Checking));
        }
        2 if soundcheck => {
            say(&mut notes, SessionEvent::Prompt(Prompt::Quiet));
        }
        3 if soundcheck => {
            say(
                &mut notes,
                SessionEvent::Prompt(Prompt::WillBeep { count: 3 }),
            );
        }
        4 if soundcheck && scene == Scene::Strum => {
            say(
                &mut notes,
                SessionEvent::Prompt(Prompt::WillBeepLouder { count: 3 }),
            );
        }
        5 if soundcheck && scene == Scene::Strum => {
            say(
                &mut notes,
                SessionEvent::Prompt(Prompt::StrumMuted {
                    count_in: 0,
                    beat: 2,
                    of: 4,
                }),
            );
        }
        _ if soundcheck => {
            d.frames = 0;
            d.attempts = 0;
            d.phase = 1;
        }
        1 => {
            say(
                &mut notes,
                SessionEvent::RoundArmed {
                    anchor: 0,
                    fs_out: 48_000,
                },
            );
            clock.t = 0.0;
            d.cursor = 0;
        }
        _ => {
            // Replay the strokes that already happened, one per frame.
            let whole = matches!(scene, Scene::Summary | Scene::Details);
            let i = d.cursor;
            let last = |i: usize| i + 1 >= round.times.len();
            let due = round
                .times
                .get(i)
                .is_some_and(|t| whole || *t + 0.1 < t_end);
            if due {
                let dt = player(i);
                clock.t = round.times[i] + dt.unwrap_or(0.0) / 1000.0;
                let pos = (i % 4) as u8;
                let guide = matches!(scene, Scene::Guide(_));
                if dt.is_some() && !guide {
                    say(
                        &mut notes,
                        SessionEvent::Judge(JudgeOut::Flash {
                            id: Some(i as u32),
                            dt_ms: dt,
                        }),
                    );
                }
                if !guide {
                    say(
                        &mut notes,
                        SessionEvent::Judge(JudgeOut::Verdict(verdict_of(i, dt, pos))),
                    );
                }
                if i == 7 && !guide {
                    say(
                        &mut notes,
                        SessionEvent::Judge(JudgeOut::Extra { t_in: 0.0 }),
                    );
                }
                d.cursor += 1;
                if !last(i) || !whole {
                    return;
                }
            }
            // Past the last stroke due: the end of the scene.
            let n = d.cursor;
            match scene {
                Scene::Round(_) | Scene::Guide(_) => {
                    clock.t = t_end;
                    d.frames = 0;
                    d.attempts = 0;
                    d.phase = 1;
                }
                Scene::Dispute => {
                    // Press, then the session answers: the last final verdict leaves the count.
                    clock.t = t_end;
                    d.attempts += 1;
                    if d.attempts == 1 {
                        sacts.write(SummaryAction::Dispute);
                    } else if d.attempts == 3 {
                        say(
                            &mut notes,
                            SessionEvent::Judge(JudgeOut::Withdrawn {
                                id: n.saturating_sub(1) as u32,
                                why: Why::Disputed,
                            }),
                        );
                        d.frames = 0;
                        d.attempts = 0;
                        d.phase = 1;
                    }
                }
                Scene::Summary | Scene::Details => {
                    clock.t = round.length_s;
                    say(
                        &mut notes,
                        SessionEvent::RoundDone {
                            stats: stats_of(n),
                            better: vec![Improvement::MoreHeard {
                                from: 0.7,
                                to: 0.88,
                            }],
                            suggest: Some(Level::Band),
                            interrupted: false,
                        },
                    );
                    if scene == Scene::Details {
                        sacts.write(SummaryAction::ToggleDetails);
                    }
                    d.frames = 0;
                    d.attempts = 0;
                    d.phase = 1;
                }
                _ => {}
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut d: ResMut<Driver>,
    chart: Res<Chart>,
    mut clock: ResMut<SongClock>,
    mut view: ResMut<ActiveView>,
    mut pending: ResMut<PendingChart>,
    mut zone: ResMut<Zone>,
    mut flow: ResMut<PulseFlow>,
    target: Option<Res<ShotTarget>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(target) = target else { return };
    let Some(spec) = d.specs.get(d.idx) else {
        exit.write(AppExit::Success);
        return;
    };
    let path = d.dir.join(format!("{}.png", spec.name));
    match d.phase {
        0 => {
            if spec.scene == Scene::Gallery && chart.name != spec.chart {
                pending.0 = Some(spec.chart);
                d.phase = 10; // wait one frame for the chart switch
                return;
            }
            d.phase = 11;
        }
        10 => d.phase = 0,
        11 if spec.scene != Scene::Gallery => {
            d.step = 0;
            d.phase = 20;
        }
        11 => {
            flow.screen_on = false;
            view.0 = spec.view;
            clock.t = spec.t;
            clock.running = false;
            zone.snap = true;
            d.frames = 0;
            d.attempts = 0;
            d.phase = 1;
        }
        // The Pulse script (system `script`) owns this phase.
        20 => {}
        1 => {
            d.frames += 1;
            if d.frames >= SETTLE_FRAMES {
                let out_path = path.clone();
                let state = Arc::new(AtomicU8::new(PENDING));
                d.cap = state.clone();
                commands.spawn(Screenshot::image(target.0.clone())).observe(
                    move |cap: On<ScreenshotCaptured>| {
                        let Ok(img) = cap.image.clone().try_into_dynamic() else {
                            state.store(BLANK, Ordering::Release);
                            return;
                        };
                        let rgb = img.to_rgb8();
                        let bright = rgb.pixels().filter(|p| p.0.iter().any(|c| *c > 90)).count();
                        if bright < 400 {
                            warn!(
                                "blank capture rejected ({bright} bright pixels, first px {:?})",
                                rgb.get_pixel(5, 5).0
                            );
                            state.store(BLANK, Ordering::Release);
                            return;
                        }
                        if let Err(e) = rgb.save(&out_path) {
                            error!("save failed: {e}");
                        }
                        state.store(DONE, Ordering::Release);
                    },
                );
                d.waited = time.elapsed_secs();
                d.phase = 2;
            }
        }
        _ => {
            if path.exists() {
                info!("captured {}", path.display());
                d.idx += 1;
                d.phase = 0;
            } else if match d.cap.load(Ordering::Acquire) {
                PENDING => time.elapsed_secs() - d.waited > LOST_AFTER_S,
                // a failed save leaves no file: retry it too
                _ => true,
            } {
                d.attempts += 1;
                if d.attempts > MAX_ATTEMPTS {
                    error!("giving up on {}", path.display());
                    exit.write(AppExit::Error(
                        std::num::NonZeroU8::new(1).expect("nonzero"),
                    ));
                    return;
                }
                d.frames = SETTLE_FRAMES - 10;
                d.phase = 1;
            }
        }
    }
}
