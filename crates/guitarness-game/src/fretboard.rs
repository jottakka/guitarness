//! Fretboard 3D view: frets across X, strings stacked in Y, gems fly in along Z.
//! Layout conventions only (zone band, panning camera); look and palette are ours.

use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::chart::Chart;
use crate::gfx::{ActiveView, HitLine, Mats, Shapes, ShowFrets, SongClock, View, ViewCam};
use crate::palette;

pub const SCALE: f32 = 20.0;
pub const NECK_FRETS: i32 = 16;
const EASE_RATE: f32 = 3.0;

/// Position of fret wire `n` along the neck (shrinks like a real neck).
pub fn fret_pos(n: f32) -> f32 {
    SCALE * (1.0 - (-n / 12.0).exp2())
}

/// Centre of fret cell `f` (f >= 1).
pub fn fret_x(f: i32) -> f32 {
    0.5 * (fret_pos(f as f32 - 1.0) + fret_pos(f as f32))
}

/// Low E is lowest (nearest the player).
pub fn string_y(s: usize) -> f32 {
    0.3 + s as f32 * 0.38
}

/// Hand-position zone in fret numbers, eased toward the target ahead of time.
#[derive(Resource)]
pub struct Zone {
    pub lo: f32,
    pub hi: f32,
    pub snap: bool,
}

#[derive(Component)]
pub enum ZoneBand {
    Plane,
    Floor,
}

#[derive(Component)]
pub struct FretNum(pub i32);

/// Frets (about four wide) the left hand needs for events in `from..=to`.
pub fn zone_target(chart: &Chart, from: f32, to: f32) -> Option<(f32, f32)> {
    let mut lo = f32::MAX;
    let mut hi = 0.0f32;
    for ev in chart.events.iter().filter(|e| e.t >= from && e.t <= to) {
        for g in chart.glyphs(ev).iter().filter(|g| g.fret > 0) {
            lo = lo.min(g.fret as f32);
            hi = hi.max(g.fret as f32);
        }
    }
    if hi == 0.0 {
        return None;
    }
    if hi - lo < 3.0 {
        lo = ((lo + hi) / 2.0 - 1.5).max(1.0);
        hi = lo + 3.0;
    }
    Some((lo, hi))
}

pub fn spawn_camera(commands: &mut Commands) {
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(palette::BG),
            ..default()
        },
        Tonemapping::TonyMcMapface,
        Bloom::NATURAL,
        DistanceFog {
            color: palette::BG,
            falloff: FogFalloff::Linear {
                start: 8.0,
                end: 30.0,
            },
            ..default()
        },
        Transform::from_xyz(2.0, 1.9, 5.0).looking_at(Vec3::new(2.0, 0.9, 0.0), Vec3::Y),
        ViewCam(View::Fret3),
        View::Fret3.layer(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2500.0,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
        RenderLayers::layer(2),
    ));
}

/// Static board: strings, fret wires on the hit plane, floor wires, inlays, hit line, zone band.
pub fn spawn_board(commands: &mut Commands, shapes: &Shapes, mats: &Mats) {
    let view = View::Fret3;
    let vm = mats.of(view);
    let layer = view.layer();
    let neck = fret_pos(NECK_FRETS as f32);
    let top = string_y(5) + 0.2;
    for s in 0..6usize {
        let th = 0.034 - 0.004 * s as f32;
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.string.clone()),
            Transform::from_xyz(neck / 2.0, string_y(s), 0.0).with_scale(Vec3::new(neck, th, th)),
            layer.clone(),
        ));
    }
    for n in 0..=NECK_FRETS {
        let x = fret_pos(n as f32);
        let wire = if n == 0 { 0.045 } else { 0.018 };
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.lane.clone()),
            Transform::from_xyz(x, (top + 0.1) / 2.0, 0.0).with_scale(Vec3::new(
                wire,
                top - 0.1,
                0.02,
            )),
            layer.clone(),
        ));
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.beat_strong.clone()),
            Transform::from_xyz(x, 0.0, -45.0).with_scale(Vec3::new(0.014, 0.01, 100.0)),
            layer.clone(),
        ));
    }
    // Inlay dots under the strings.
    for (n, count) in [(3, 1), (5, 1), (7, 1), (9, 1), (12, 2)] {
        for k in 0..count {
            let dx = if count == 2 {
                (k as f32 - 0.5) * 0.35
            } else {
                0.0
            };
            commands.spawn((
                Mesh3d(shapes.disc.clone()),
                MeshMaterial3d(vm.lane.clone()),
                Transform::from_xyz(fret_x(n) + dx, -0.2, 0.0).with_scale(Vec3::splat(0.13)),
                layer.clone(),
            ));
        }
    }
    // Hit line at the near edge, across the whole neck.
    let base = Vec3::new(neck, 0.02, 0.07);
    commands.spawn((
        Mesh3d(shapes.cube.clone()),
        MeshMaterial3d(vm.hit.clone()),
        Transform::from_xyz(neck / 2.0, 0.012, 0.0).with_scale(base),
        HitLine {
            base,
            thick: Vec3::new(0.0, 0.6, 0.6),
        },
        layer.clone(),
    ));
    for band in [ZoneBand::Plane, ZoneBand::Floor] {
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.zone.clone()),
            Transform::default(),
            band,
            layer.clone(),
        ));
    }
    for n in 1..=NECK_FRETS {
        commands.spawn((
            FretNum(n),
            Text::new(n.to_string()),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(Color::srgba(0.6, 0.72, 0.95, 0.75)),
            TextLayout::justify(Justify::Center),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(30.0),
                ..default()
            },
            Visibility::Hidden,
        ));
    }
}

#[allow(clippy::type_complexity)]
pub fn update_zone(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    time: Res<Time<Real>>,
    mut zone: ResMut<Zone>,
    mut cam: Query<(&mut Transform, &ViewCam), Without<ZoneBand>>,
    mut bands: Query<(&ZoneBand, &mut Transform), Without<ViewCam>>,
) {
    if let Some((lo, hi)) = zone_target(&chart, clock.t + 0.3, clock.t + 2.4) {
        if zone.snap {
            zone.lo = lo;
            zone.hi = hi;
        } else {
            let k = 1.0 - (-EASE_RATE * time.delta_secs()).exp();
            zone.lo += (lo - zone.lo) * k;
            zone.hi += (hi - zone.hi) * k;
        }
        zone.snap = false;
    }
    let x0 = fret_pos(zone.lo - 1.0);
    let x1 = fret_pos(zone.hi);
    let cx = (x0 + x1) / 2.0;
    let w = x1 - x0;
    for (mut tf, v) in &mut cam {
        if v.0 == View::Fret3 {
            let d = ((w / 2.0 + 1.6) / 0.68).max(5.5);
            *tf = Transform::from_xyz(cx, 4.4, d).looking_at(Vec3::new(cx, 0.5, -1.5), Vec3::Y);
        }
    }
    for (band, mut tf) in &mut bands {
        match band {
            ZoneBand::Plane => {
                *tf = Transform::from_xyz(cx, 1.1, -0.02).with_scale(Vec3::new(w, 2.2, 0.01));
            }
            ZoneBand::Floor => {
                *tf = Transform::from_xyz(cx, 0.004, -20.0).with_scale(Vec3::new(w, 0.004, 44.0));
            }
        }
    }
}

pub fn update_fret_nums(
    zone: Res<Zone>,
    show: Res<ShowFrets>,
    view: Res<ActiveView>,
    cams: Query<(&Camera, &GlobalTransform, &ViewCam)>,
    mut nums: Query<(&FretNum, &mut Node, &mut Visibility)>,
) {
    let Some((cam, gt, _)) = cams.iter().find(|(_, _, v)| v.0 == View::Fret3) else {
        return;
    };
    for (n, mut node, mut vis) in &mut nums {
        let f = n.0 as f32;
        let inside = f >= zone.lo - 0.5 && f <= zone.hi + 0.5;
        let pos = Vec3::new(fret_x(n.0), -0.4, 0.2);
        match cam.world_to_viewport(gt, pos) {
            Ok(p) if show.0 && inside && view.0 == View::Fret3 => {
                node.left = Val::Px(p.x - 15.0);
                node.top = Val::Px(p.y - 10.0);
                *vis = Visibility::Inherited;
            }
            _ => *vis = Visibility::Hidden,
        }
    }
}
