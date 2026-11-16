//! Pulse 2D: a timeline slides left into the hit marker (after `docs/arte/minimal/timing-2d.png`),
//! and the HUD shared by both views: play button, count-in dots, next-stroke arrow, progress
//! line, tempo chips and the plug pictogram. No text anywhere.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use guitarness_core::chart::Dir;

use super::assets::{
    ErrorMark, LAYER_UI, POOL, PoolMark, PulseMats, PulseShapes, PulseView, RoundEntity,
    grid_scale, spawn_event,
};
use super::flow::{Offer, Phase, PulseFlow, PulseRound};
use super::view3d::HitPulse;
use super::{PulseCam, beat_pulse};
use crate::audio::{AudioStatus, Link};
use crate::gfx::SongClock;
use crate::palette;

/// The hit marker's x, and world units per second.
pub const HIT_X: f32 = -3.2;
pub const SPEED: f32 = 2.0;
pub const HOME_POS: Vec2 = Vec2::new(0.0, 0.2);
pub const CHIP_UP: Vec2 = Vec2::new(1.5, -1.7);
pub const CHIP_DOWN: Vec2 = Vec2::new(-1.5, -1.7);

const VIEW: PulseView = PulseView::D2;
const TICK_H: f32 = 1.5;

fn ortho() -> Projection {
    Projection::Orthographic(OrthographicProjection {
        scaling_mode: bevy::camera::ScalingMode::FixedVertical {
            viewport_height: 6.0,
        },
        ..OrthographicProjection::default_3d()
    })
}

pub fn spawn_scene(mut commands: Commands, shapes: Res<PulseShapes>, mats: Res<PulseMats>) {
    let layer = VIEW.layer();
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(palette::BG),
            is_active: false,
            ..default()
        },
        ortho(),
        Tonemapping::TonyMcMapface,
        Bloom::NATURAL,
        Transform::from_xyz(0.0, 0.0, 10.0),
        PulseCam(Some(VIEW)),
        layer.clone(),
    ));
    // The time line.
    commands.spawn((
        Mesh3d(shapes.bar.clone()),
        MeshMaterial3d(mats.edge.clone()),
        Transform::from_xyz(0.0, 0.0, -0.01).with_scale(Vec3::new(11.0, 0.035, 1.0)),
        layer.clone(),
    ));
    // The hit marker: a bar with two small brackets.
    let base = Vec3::new(0.06, 2.0, 1.0);
    commands.spawn((
        Mesh3d(shapes.bar.clone()),
        MeshMaterial3d(mats.hit.clone()),
        Transform::from_xyz(HIT_X, 0.0, 0.0).with_scale(base),
        HitPulse {
            base,
            grow: Vec3::new(0.08, 0.0, 0.0),
            view: VIEW,
        },
        layer,
    ));
}

/// Spawns the round for the 2D view (rings on the time line, ticks, pools).
pub fn respawn_round(
    mut commands: Commands,
    round: Res<PulseRound>,
    shapes: Res<PulseShapes>,
    mats: Res<PulseMats>,
    old: Query<(Entity, &RoundEntity)>,
    mut seen: Local<u32>,
) {
    if *seen == round.rev {
        return;
    }
    *seen = round.rev;
    for (e, r) in &old {
        if r.0 == VIEW {
            commands.entity(e).despawn();
        }
    }
    let small = round.sizes.iter().any(|s| *s < 1.0);
    for (i, t) in round.times.iter().enumerate() {
        spawn_event(
            &mut commands,
            &shapes,
            &mats,
            VIEW,
            i,
            *t,
            round.dirs[i],
            round.strong[i],
            small && !round.strong[i],
            Quat::IDENTITY,
        );
    }
    let beats = (round.length_s / round.beat_s.max(0.01)).ceil() as i32 + 1;
    for b in -(super::flow::COUNT_IN_BEATS as i32)..beats {
        let strong = b.rem_euclid(4) == 0;
        commands.spawn((
            Mesh3d(shapes.bar.clone()),
            MeshMaterial3d(if strong {
                mats.grid_strong.clone()
            } else {
                mats.grid.clone()
            }),
            Transform::from_translation(Vec3::new(0.0, 0.0, -0.005)).with_scale(grid_scale(
                strong,
                if strong { TICK_H } else { 0.7 },
                VIEW,
            )),
            Visibility::Hidden,
            super::assets::PulseGrid {
                t: b as f32 * round.beat_s,
                strong,
                view: VIEW,
            },
            RoundEntity(VIEW),
            VIEW.layer(),
        ));
    }
    for slot in 0..POOL {
        for extra in [false, true] {
            let (mesh, mat, scale) = if extra {
                (
                    shapes.bar.clone(),
                    mats.error(ErrorMark::ExtraDash).clone(),
                    Vec3::new(0.09, 0.5, 1.0),
                )
            } else {
                (
                    shapes.tri_down.clone(),
                    mats.fill.clone(),
                    Vec3::new(0.4, 0.4, 1.0),
                )
            };
            commands.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(mat),
                Transform::from_scale(scale),
                Visibility::Hidden,
                PoolMark {
                    extra,
                    slot,
                    view: VIEW,
                },
                RoundEntity(VIEW),
                VIEW.layer(),
            ));
        }
    }
}

pub fn pulse_hit(
    clock: Res<SongClock>,
    round: Res<PulseRound>,
    flow: Res<PulseFlow>,
    mut q: Query<(&HitPulse, &mut Transform)>,
) {
    for (h, mut tf) in &mut q {
        if h.view != VIEW {
            continue;
        }
        let (p, strong) = beat_pulse(clock.t, round.beat_s);
        let k = if flow.screen_on {
            p * if strong { 1.6 } else { 0.8 }
        } else {
            0.0
        };
        tf.scale = h.base + h.grow * k;
    }
}

// ------------------------------------------------------------------ HUD

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudKind {
    HomeRing,
    HomePlay,
    /// Parent of the count-in dots.
    Strip,
    SlotOff(u8),
    SlotOn(u8),
    Next(Dir),
    ProgBase,
    ProgFill,
    ChipRing(Offer),
    ChipArrow(Offer),
    Plug,
}

#[derive(Component, Clone, Copy, Debug)]
pub struct Hud(pub HudKind);

pub fn spawn_hud(mut commands: Commands, shapes: Res<PulseShapes>, mats: Res<PulseMats>) {
    let layer = bevy::camera::visibility::RenderLayers::layer(LAYER_UI);
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 10,
            clear_color: ClearColorConfig::None,
            is_active: false,
            ..default()
        },
        ortho(),
        Tonemapping::TonyMcMapface,
        Bloom::NATURAL,
        Transform::from_xyz(0.0, 0.0, 10.0),
        PulseCam(None),
        layer.clone(),
    ));
    let mut put = |kind: HudKind,
                   mesh: &Handle<Mesh>,
                   mat: &Handle<StandardMaterial>,
                   pos: Vec3,
                   scale: Vec3| {
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(pos).with_scale(scale),
            Visibility::Hidden,
            Hud(kind),
            layer.clone(),
        ));
    };
    // Home: a big ring and the play triangle.
    put(
        HudKind::HomeRing,
        &shapes.ring_thick,
        &mats.ring_strong,
        HOME_POS.extend(0.0),
        Vec3::new(1.9, 1.9, 1.0),
    );
    put(
        HudKind::HomePlay,
        &shapes.tri_right,
        &mats.fill,
        (HOME_POS + Vec2::new(0.06, 0.0)).extend(0.01),
        Vec3::new(0.8, 0.8, 1.0),
    );
    // Next stroke arrow and the progress line.
    put(
        HudKind::Next(Dir::Down),
        &shapes.tri_down,
        &mats.fill,
        Vec3::new(0.0, 2.2, 0.0),
        Vec3::splat(0.7),
    );
    put(
        HudKind::Next(Dir::Up),
        &shapes.tri_up,
        &mats.fill,
        Vec3::new(0.0, 2.2, 0.0),
        Vec3::splat(0.7),
    );
    put(
        HudKind::ProgBase,
        &shapes.bar,
        &mats.grid,
        Vec3::new(0.0, 2.85, 0.0),
        Vec3::new(8.0, 0.03, 1.0),
    );
    put(
        HudKind::ProgFill,
        &shapes.bar,
        &mats.hit,
        Vec3::new(-4.0, 2.85, 0.01),
        Vec3::new(0.001, 0.05, 1.0),
    );
    // Chips: an ellipse and the arrow of the tempo change.
    for (o, pos, arrow) in [
        (Offer::TempoUp, CHIP_UP, &shapes.tri_up),
        (Offer::TempoDown, CHIP_DOWN, &shapes.tri_down),
    ] {
        put(
            HudKind::ChipRing(o),
            &shapes.ring_thin,
            &mats.chip,
            pos.extend(0.0),
            Vec3::new(1.8, 0.9, 1.0),
        );
        put(
            HudKind::ChipArrow(o),
            arrow,
            &mats.chip,
            pos.extend(0.01),
            Vec3::splat(0.4),
        );
    }
    // Count-in dots: four slots, the first a diamond (beat 1: another shape and size).
    commands
        .spawn((
            Transform::from_xyz(0.0, -2.6, 0.0),
            Visibility::Hidden,
            Hud(HudKind::Strip),
            layer.clone(),
        ))
        .with_children(|c| {
            for i in 0..4u8 {
                let x = (f32::from(i) - 1.5) * 0.8;
                let (mesh, s) = if i == 0 {
                    (shapes.diamond.clone(), 0.62)
                } else {
                    (shapes.disc.clone(), 0.4)
                };
                c.spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(mats.dot_off.clone()),
                    Transform::from_xyz(x, 0.0, 0.0).with_scale(Vec3::splat(s)),
                    Hud(HudKind::SlotOff(i)),
                    layer.clone(),
                ));
                c.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(mats.dot_on.clone()),
                    Transform::from_xyz(x, 0.0, 0.01).with_scale(Vec3::splat(s)),
                    Visibility::Hidden,
                    Hud(HudKind::SlotOn(i)),
                    layer.clone(),
                ));
            }
        });
    // The plug pictogram (device gone): body, two prongs, cable. Grey.
    let grey = mats.error(ErrorMark::DimRing).clone();
    let bright = mats.error(ErrorMark::LockBody).clone();
    commands
        .spawn((
            Transform::from_xyz(0.0, 0.0, 0.0),
            Visibility::Hidden,
            Hud(HudKind::Plug),
            layer.clone(),
        ))
        .with_children(|c| {
            let mut bar = |x: f32, y: f32, w: f32, h: f32, m: &Handle<StandardMaterial>| {
                c.spawn((
                    Mesh3d(shapes.bar.clone()),
                    MeshMaterial3d(m.clone()),
                    Transform::from_xyz(x, y, 0.0).with_scale(Vec3::new(w, h, 1.0)),
                    layer.clone(),
                ));
            };
            bar(0.0, 0.0, 1.0, 0.8, &bright);
            bar(-0.24, 0.7, 0.14, 0.6, &bright);
            bar(0.24, 0.7, 0.14, 0.6, &bright);
            bar(0.0, -1.1, 0.14, 1.4, &grey);
        });
}

#[allow(clippy::too_many_arguments)]
pub fn update_hud(
    clock: Res<SongClock>,
    flow: Res<PulseFlow>,
    round: Res<PulseRound>,
    status: Option<Res<AudioStatus>>,
    time: Res<Time>,
    shooting: Option<Res<crate::shots::Shooting>>,
    summary: Option<Res<crate::summary::SummaryState>>,
    sound: Option<Res<crate::soundcheck::Soundcheck>>,
    contracts: Option<Res<crate::contract::RoundContracts>>,
    mut q: Query<(&Hud, &mut Transform, &mut Visibility)>,
) {
    let overlay = super::overlay_up(summary.as_deref(), sound.as_deref());
    // One focal point: in 2D the arrow and the beat dots sit on the hit marker's column; in
    // 3D the arrow is on the lane's axis and the dots stay in the top-left corner, clear of
    // the strokes that just passed at the bottom.
    let (focus_x, strip) = match flow.view {
        PulseView::D2 => (HIT_X, Vec3::new(HIT_X, -2.6, 0.0)),
        PulseView::D3 => (0.0, Vec3::new(-3.9, 2.2, 0.0)),
    };
    let strip_scale = if flow.view == PulseView::D2 { 0.8 } else { 0.6 };
    let t = clock.t;
    let (pulse, _) = beat_pulse(t, round.beat_s);
    let slot = if round.beat_s > 0.0 {
        ((t / round.beat_s).floor() as i64).rem_euclid(4) as u8
    } else {
        0
    };
    let plugged_out = status
        .as_ref()
        .is_some_and(|s| matches!(s.link, Link::Gone | Link::Unavailable));
    let idle = matches!(flow.phase, Phase::Home | Phase::Between) && !plugged_out && !overlay;
    let running = matches!(flow.phase, Phase::Counting | Phase::Playing) && !plugged_out;
    // The next-stroke arrow is an instruction: only for a stroke that asks for a direction.
    let next = round
        .next_after(t)
        .filter(|i| crate::contract::asks_direction(contracts.as_deref().and_then(|c| c.get(*i))))
        .map(|i| round.dirs[i]);
    let progress = if round.length_s > 0.0 && flow.phase == Phase::Playing {
        (t / round.length_s).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Screenshots must be byte-identical between runs: no real-time breathing there.
    let breathe = if shooting.is_some() {
        1.0
    } else {
        1.0 + 0.04 * (time.elapsed_secs() * 2.0).sin()
    };
    for (h, mut tf, mut vis) in &mut q {
        let show = match h.0 {
            HudKind::HomeRing | HudKind::HomePlay => idle,
            HudKind::Strip | HudKind::SlotOff(_) => running,
            HudKind::SlotOn(i) => running && i == slot,
            HudKind::Next(d) => running && next == Some(d),
            HudKind::ProgBase => flow.phase == Phase::Playing,
            HudKind::ProgFill => flow.phase == Phase::Playing && progress > 0.0,
            HudKind::ChipRing(o) | HudKind::ChipArrow(o) => {
                // With the summary up the offers are in its panel (one clear next step).
                flow.phase == Phase::Between
                    && !overlay
                    && match o {
                        Offer::TempoUp => flow.offers.up,
                        Offer::TempoDown => flow.offers.down,
                    }
            }
            HudKind::Plug => plugged_out,
        };
        let want = if flow.screen_on && show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
        match h.0 {
            HudKind::HomeRing => tf.scale = Vec3::new(1.9 * breathe, 1.9 * breathe, 1.0),
            HudKind::Strip => {
                let (pos, s) = if flow.phase == Phase::Counting {
                    (Vec3::new(0.0, 0.2, 0.0), 1.8)
                } else {
                    (strip, strip_scale)
                };
                tf.translation = pos;
                tf.scale = Vec3::splat(s);
            }
            HudKind::SlotOn(i) => {
                let base = if i == 0 { 0.62 } else { 0.4 };
                let k = base * (1.0 + 0.5 * pulse);
                tf.scale = Vec3::splat(k);
            }
            HudKind::Next(_) => {
                let k = 0.7 * (1.0 + 0.25 * pulse);
                tf.scale = Vec3::splat(k);
                tf.translation.x = focus_x;
            }
            HudKind::ProgFill => {
                let w = (progress * 8.0).max(0.001);
                tf.scale.x = w;
                tf.translation.x = -4.0 + w / 2.0;
            }
            _ => {}
        }
    }
}
