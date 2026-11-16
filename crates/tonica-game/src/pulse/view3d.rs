//! Pulse 3D: rings come down a lane to the hit line (after `docs/arte/minimal/timing-3d.png`).

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use super::assets::{
    ErrorMark, POOL, PoolMark, PulseMats, PulseShapes, PulseView, RoundEntity, grid_scale,
    spawn_event,
};
use super::flow::{PulseFlow, PulseRound};
use super::{PulseCam, beat_pulse};
use crate::gfx::SongClock;
use crate::palette;

/// World units per second along the lane.
pub const SPEED: f32 = 3.0;
pub const LANE_W: f32 = 3.3;

const VIEW: PulseView = PulseView::D3;

/// Lays flat parts on the lane (local +Y points down the lane, away from the camera).
fn flat() -> Quat {
    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
}

/// The hit line grows a little on each beat; beat 1 more.
#[derive(Component)]
pub struct HitPulse {
    pub base: Vec3,
    pub grow: Vec3,
    pub view: PulseView,
}

pub fn spawn(mut commands: Commands, shapes: Res<PulseShapes>, mats: Res<PulseMats>) {
    let layer = VIEW.layer();
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(palette::BG),
            is_active: false,
            ..default()
        },
        Tonemapping::TonyMcMapface,
        Bloom::NATURAL,
        DistanceFog {
            color: palette::BG,
            falloff: FogFalloff::Linear {
                start: 7.0,
                end: 24.0,
            },
            ..default()
        },
        Transform::from_xyz(0.0, 3.1, 5.4).looking_at(Vec3::new(0.0, 0.0, -2.2), Vec3::Y),
        PulseCam(Some(VIEW)),
        layer.clone(),
    ));
    // Lane edges.
    for side in [-1.0, 1.0] {
        commands.spawn((
            Mesh3d(shapes.bar.clone()),
            MeshMaterial3d(mats.edge.clone()),
            Transform::from_xyz(side * LANE_W / 2.0, 0.0, -28.0)
                .with_rotation(flat())
                .with_scale(Vec3::new(0.03, 64.0, 1.0)),
            layer.clone(),
        ));
        // The "//" ticks beside the hit line.
        for k in 0..2 {
            commands.spawn((
                Mesh3d(shapes.bar.clone()),
                MeshMaterial3d(mats.hit.clone()),
                Transform::from_xyz(side * (LANE_W / 2.0 + 0.42 + 0.14 * k as f32), 0.02, 0.0)
                    // Mirrored: "//" on the left, "\\" on the right.
                    .with_rotation(flat() * Quat::from_rotation_z(0.45 * side))
                    .with_scale(Vec3::new(0.035, 0.32, 1.0)),
                layer.clone(),
            ));
        }
    }
    let base = Vec3::new(LANE_W, 0.05, 1.0);
    commands.spawn((
        Mesh3d(shapes.bar.clone()),
        MeshMaterial3d(mats.hit.clone()),
        Transform::from_xyz(0.0, 0.03, 0.0)
            .with_rotation(flat())
            .with_scale(base),
        HitPulse {
            base,
            grow: Vec3::new(0.0, 0.12, 0.0),
            view: VIEW,
        },
        layer,
    ));
}

/// Spawns the round's rings, beat lines and small pools when a new round is set.
#[allow(clippy::too_many_arguments)]
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
    spawn_round(&mut commands, &round, &shapes, &mats);
}

fn spawn_round(
    commands: &mut Commands,
    round: &PulseRound,
    shapes: &PulseShapes,
    mats: &PulseMats,
) {
    let small = round.sizes.iter().any(|s| *s < 1.0);
    for (i, t) in round.times.iter().enumerate() {
        spawn_event(
            commands,
            shapes,
            mats,
            VIEW,
            i,
            *t,
            round.dirs[i],
            round.strong[i],
            small && !round.strong[i],
            flat(),
        );
    }
    // The beat lines, count-in included.
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
            Transform::from_rotation(flat()).with_scale(grid_scale(strong, LANE_W, VIEW)),
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
                Transform::from_rotation(flat()).with_scale(scale),
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
    let on = flow.screen_on;
    for (h, mut tf) in &mut q {
        if h.view != VIEW {
            continue;
        }
        let (p, strong) = beat_pulse(clock.t, round.beat_s);
        let k = if on {
            p * if strong { 1.6 } else { 0.8 }
        } else {
            0.0
        };
        tf.scale = h.base + h.grow * k;
    }
}
