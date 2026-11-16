//! Lanes view: six lanes receding into fog.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::gfx::{self, HitLine, Mats, Shapes, View, ViewCam};
use crate::palette;

pub fn spawn(commands: &mut Commands, shapes: &Shapes, mats: &Mats) {
    let view = View::D3;
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
                start: 12.0,
                end: 60.0,
            },
            ..default()
        },
        Transform::from_xyz(0.0, 2.7, 5.2).looking_at(Vec3::new(0.0, 0.0, -3.0), Vec3::Y),
        ViewCam(view),
        view.layer(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2500.0,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
        view.layer(),
    ));
    let vm = mats.of(view);
    for s in 0..6 {
        let x = (s as f32 - 2.5) * gfx::LANE3;
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.lane.clone()),
            Transform::from_xyz(x, 0.0, -48.0).with_scale(Vec3::new(0.025, 0.025, 104.0)),
            view.layer(),
        ));
    }
    let base = Vec3::new(5.0 * gfx::LANE3 + 0.7, 0.03, 0.07);
    commands.spawn((
        Mesh3d(shapes.cube.clone()),
        MeshMaterial3d(vm.hit.clone()),
        Transform::from_xyz(0.0, 0.015, 0.0).with_scale(base),
        HitLine {
            base,
            thick: Vec3::new(0.0, 0.6, 0.6),
        },
        view.layer(),
    ));
}
