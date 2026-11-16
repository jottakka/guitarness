//! Orthographic side-scroller: gems run right to left into a vertical hit line.

use bevy::camera::ScalingMode;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::gfx::{self, HitLine, Mats, Shapes, View, ViewCam};
use crate::palette;

pub fn spawn(commands: &mut Commands, shapes: &Shapes, mats: &Mats) {
    let view = View::D2;
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(palette::BG),
            is_active: false,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: 6.0,
            },
            ..OrthographicProjection::default_3d()
        }),
        Tonemapping::TonyMcMapface,
        Bloom::NATURAL,
        Transform::from_xyz(0.0, 0.0, 10.0),
        ViewCam(view),
        RenderLayers::layer(1),
    ));
    let vm = mats.of(view);
    for s in 0..6 {
        let y = (s as f32 - 2.5) * gfx::LANE2;
        commands.spawn((
            Mesh3d(shapes.cube.clone()),
            MeshMaterial3d(vm.lane.clone()),
            Transform::from_xyz(15.0, y, 0.0).with_scale(Vec3::new(40.0, 0.02, 0.01)),
            view.layer(),
        ));
    }
    let base = Vec3::new(0.05, 5.0 * gfx::LANE2 + 0.8, 0.04);
    commands.spawn((
        Mesh3d(shapes.cube.clone()),
        MeshMaterial3d(vm.hit.clone()),
        Transform::from_xyz(gfx::HIT_X, 0.0, 0.05).with_scale(base),
        HitLine {
            base,
            thick: Vec3::new(1.0, 0.0, 0.5),
        },
        view.layer(),
    ));
}
