//! Pulse mode: the playable timing screen.
//!
//! * `flow`: phases (home, count-in, playing, between), offers, counters. No window.
//! * `feedback`: what each judge answer looks like; drawing of the marks.
//! * `assets`: meshes and materials (no text; errors are white or grey).
//! * `view3d`, `view2d`: the two playfields; `view2d` also holds the HUD (play button,
//!   count-in dots, next-stroke arrow, chips, plug).
//!
//! Turn it on with `--pulse` (or insert `PulseFlow { screen_on: true, .. }`). Keys: Space or
//! Enter play, V view, Up/Down accept the tempo chip, `[` and `]` screen latency, Esc stop.
//! Backspace (dispute) is G4's.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::AppConfig;
use crate::gfx::{ActiveView, ViewCam};

pub mod assets;
pub mod feedback;
pub mod flow;
pub mod view2d;
pub mod view3d;

use assets::PulseView;
use flow::{Offer, Phase, PulseAction, PulseFlow, PulseLogicPlugin};

/// Seconds of look-ahead and look-behind drawn.
pub const LOOK_S: f32 = 4.0;
pub const PAST_S: f32 = 1.1;

/// Position of something `dt` seconds from now (positive: still to come), and whether it is in
/// the drawn range.
pub fn track(view: PulseView, dt: f32) -> (Vec3, bool) {
    // 2D keeps a margin at both screen edges; 3D drops a ring once it is well past the line,
    // so none is left cut by the bottom of the screen.
    let on = match view {
        PulseView::D2 => (-0.7..=3.6).contains(&dt),
        PulseView::D3 => (-0.7..=LOOK_S).contains(&dt),
    };
    let pos = match view {
        PulseView::D3 => Vec3::new(0.0, 0.02, -dt * view3d::SPEED),
        PulseView::D2 => Vec3::new(view2d::HIT_X + dt * view2d::SPEED, 0.0, 0.0),
    };
    (pos, on)
}

/// Where an extra stroke's dash sits, beside the line.
pub fn extra_offset(view: PulseView) -> Vec3 {
    match view {
        PulseView::D3 => Vec3::new(2.05, 0.0, 0.0),
        PulseView::D2 => Vec3::new(0.0, 1.1, 0.0),
    }
}

/// Visual metronome: 1 on a beat, decaying; and whether it is beat 1 of a bar. Defined for
/// negative time too (the count-in).
pub fn beat_pulse(t: f32, beat_s: f32) -> (f32, bool) {
    if beat_s <= 0.0 {
        return (0.0, false);
    }
    let n = (t / beat_s).floor();
    let phase = t / beat_s - n;
    let strong = (n as i64).rem_euclid(4) == 0;
    ((-phase * 7.0).exp(), strong)
}

/// An overlay (the summary with its details, or the soundcheck) is up: it owns the screen, so
/// the Home play ring is hidden and the play button does not answer behind it.
pub fn overlay_up(
    summary: Option<&crate::summary::SummaryState>,
    sound: Option<&crate::soundcheck::Soundcheck>,
) -> bool {
    summary.is_some_and(|s| s.visible && s.summary.is_some()) || sound.is_some_and(|s| s.shown)
}

/// Marks the camera of a Pulse view.
#[derive(Component)]
pub struct PulseCam(pub Option<PulseView>);

/// Called once by `build_app` after the base plugins and resources are in place.
pub fn install(app: &mut App, cfg: &AppConfig) {
    if !cfg.args.iter().any(|a| a == "--pulse") {
        return;
    }
    app.add_plugins(PulsePlugin);
    app.world_mut().resource_mut::<PulseFlow>().screen_on = true;
}

/// Logic and drawing.
pub struct PulsePlugin;

impl Plugin for PulsePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PulseLogicPlugin)
            .add_systems(
                Startup,
                (
                    setup_assets,
                    view3d::spawn,
                    view2d::spawn_scene,
                    view2d::spawn_hud,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    read_input,
                    view3d::respawn_round,
                    view2d::respawn_round,
                    feedback::apply_marks,
                    feedback::place_events,
                    feedback::place_pool,
                    view2d::update_hud,
                    view3d::pulse_hit,
                    view2d::pulse_hit,
                )
                    .chain(),
            )
            .add_systems(Last, apply_screen);
    }
}

fn setup_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(assets::build_shapes(&mut meshes));
    commands.insert_resource(assets::build_mats(&mut mats));
}

/// Keys and the mouse become actions.
fn read_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    flow: Res<PulseFlow>,
    summary: Option<Res<crate::summary::SummaryState>>,
    sound: Option<Res<crate::soundcheck::Soundcheck>>,
    mut acts: MessageWriter<PulseAction>,
) {
    if !flow.screen_on {
        return;
    }
    let overlay = overlay_up(summary.as_deref(), sound.as_deref());
    // Enter belongs to the summary's optional offer while it is on the table, and to the
    // soundcheck's "play now" while that is up: Space always plays.
    let offered = summary
        .as_deref()
        .is_some_and(|s| matches!(s.chip, crate::summary::Chip::Offered(_)) && s.visible);
    let enter_plays = !offered && !sound.as_deref().is_some_and(|s| s.shown);
    if keys.just_pressed(KeyCode::Space) || (enter_plays && keys.just_pressed(KeyCode::Enter)) {
        acts.write(PulseAction::Play);
    }
    if keys.just_pressed(KeyCode::KeyV) {
        acts.write(PulseAction::ToggleView);
    }
    if keys.just_pressed(KeyCode::ArrowUp) {
        acts.write(PulseAction::Accept(Offer::TempoUp));
    }
    if keys.just_pressed(KeyCode::ArrowDown) {
        acts.write(PulseAction::Accept(Offer::TempoDown));
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        acts.write(PulseAction::VideoNudge(-1));
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        acts.write(PulseAction::VideoNudge(1));
    }
    if keys.just_pressed(KeyCode::Escape) {
        acts.write(PulseAction::Stop);
    }
    if mouse.just_pressed(MouseButton::Left)
        && let Ok(w) = windows.single()
        && let Some(c) = w.cursor_position()
    {
        // The HUD camera is orthographic, 6 units tall.
        let (ww, wh) = (w.width().max(1.0), w.height().max(1.0));
        let p = Vec2::new((c.x / ww - 0.5) * 6.0 * ww / wh, (0.5 - c.y / wh) * 6.0);
        if matches!(flow.phase, Phase::Home | Phase::Between)
            && !overlay
            && p.distance(view2d::HOME_POS) < 1.0
        {
            acts.write(PulseAction::Play);
        }
        if !overlay
            && flow.offers.up
            && ((p - view2d::CHIP_UP) / Vec2::new(0.9, 0.45)).length() < 1.0
        {
            acts.write(PulseAction::Accept(Offer::TempoUp));
        }
        if !overlay
            && flow.offers.down
            && ((p - view2d::CHIP_DOWN) / Vec2::new(0.9, 0.45)).length() < 1.0
        {
            acts.write(PulseAction::Accept(Offer::TempoDown));
        }
    }
}

/// Which cameras run: the Pulse view and HUD while the screen is on, the gallery's otherwise.
#[allow(clippy::too_many_arguments)]
fn apply_screen(
    flow: Res<PulseFlow>,
    mut active: ResMut<ActiveView>,
    mut was_on: Local<bool>,
    mut pulse_cams: Query<(&mut Camera, &PulseCam)>,
    mut legacy: Query<&mut Camera, (With<ViewCam>, Without<PulseCam>)>,
) {
    for (mut cam, p) in &mut pulse_cams {
        let on = flow.screen_on
            && match p.0 {
                Some(v) => v == flow.view,
                None => true,
            };
        if cam.is_active != on {
            cam.is_active = on;
        }
    }
    if flow.screen_on {
        for mut cam in &mut legacy {
            if cam.is_active {
                cam.is_active = false;
            }
        }
    } else if *was_on {
        // Hand the screen back: the gallery re-applies its active view on change.
        active.set_changed();
    }
    *was_on = flow.screen_on;
}
